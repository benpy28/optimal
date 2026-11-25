#!/usr/bin/env bash
# Tough but realistic conditions: boots the kernel (with its in-kernel test
# suite) on machines that are deliberately awkward, the way real PCs are.
#
#   - CPUs from 2008 Core 2 to current Xeon and EPYC, with security features
#     missing, 5-level paging, Atom/E-core parts, emulated (slow) CPUs
#   - Odd core topologies, NUMA layouts including a node with no memory
#   - Odd memory: tiny, huge, not a round number, split around the 4 GiB hole,
#     extra DIMMs
#   - Firmware quirks: no HPET, no ACPI, different OVMF builds, Secure Boot
#   - A host that is fighting us for CPU: every vCPU squeezed onto one host
#     core, and the whole host loaded with stress-ng
#
# A "boot" scenario passes only if the full kernel test suite passes with zero
# failures, the kernel's ACPI line is there and reports no warnings, and
# interrupts came on with no warnings; a "boot:<regex>[ && <regex>...]"
# scenario instead needs every regex to match a line of the log (the
# machine's expected ACPI summary and warnings), and the same `interrupts:`
# line unless one of its regexes names its own. A "refuse" scenario (Secure Boot attacks) passes only if the
# firmware or bootloader refuses it with the expected message and the kernel
# never runs. Nothing may hang, and nothing may panic.
#
# The kernel test suite run in every scenario includes fault injection
# (failed frame allocations, 1b) and checks of the ACPI reader (1c). Quirky
# ACPI tables (duplicates, unknown signatures, a long root table, a 64 KiB
# table) are added with -acpitable, and each such run must print exactly the
# ACPI summary and warnings expected. Broken ACPI tables (bad checksums, a
# broken XSDT or RSDP, junk in the MADT, a misaligned HPET, FADT pointers
# that disagree or point into PCI configuration space) are made in RAM by
# the firmware-quirk injector (bootloader/quirk) just before the kernel
# starts, on UEFI disk images from scripts/quirk-image.sh; each must print
# exactly the expected summary and warnings too. Interrupt controllers left
# dirty (8259s on the BIOS vectors and unmasked, every I/O APIC pin live,
# local APIC LVTs live with a raised TPR, x2APIC on, the local APIC
# globally disabled) must be cleaned up and reported exactly. More
# conditions join as the kernel grows: timer jitter (1c3), disk power cuts
# and IO errors (Phase 4).
#
# REPEAT=N runs every scenario N times (the nightly soak uses a bigger N).
# ONLY=<regex> runs only scenarios whose name matches.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/qemu.sh

REPEAT="${REPEAT:-1}"
# Evidence (CI, 2026-11-21): with 64 vCPUs on a 4-core runner, KVM on the
# Ubuntu 24.04 runners now and then stops the VM with "internal-error"
# while the firmware (SeaBIOS or OVMF) is still setting up PCI, before any
# bootloader runs; it has failed two attempts in a row once. It has also
# stopped (run 44901087659) after Limine's menu but before the kernel's
# first line, still on the firmware's page tables (CR3 0x7f801000, RIP
# 0x7ecffc24, firmware code selector 0x38). That matches
# a host-side race between vCPUs and firmware memory remapping, not
# anything in this repository, so such failures get up to 3 attempts.
PLATFORM_ATTEMPTS="${PLATFORM_ATTEMPTS:-3}"
ONLY="${ONLY:-.}"
LOGDIR=build/tough-logs
mkdir -p "$LOGDIR"
ISO=build/optimal-ktest.iso
KVM=no
if [ -w /dev/kvm ]; then KVM=yes; fi

# Record the host, so platform failures can be matched to runner types.
echo "host: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//'), $(nproc) cores, kernel $(uname -r), $(qemu-system-x86_64 --version | head -1)"

pass=0
fail=0
skipped=0
failed=()
platform=()

# scenario <name> <expect> <iso> <firmware> <machine> <mem> <smp> <timeout> <accel: kvm|tcg> [-- extra qemu args]
# <expect> is "boot", "boot:<regex>[ && <regex>...]" or
# "refuse:<text the refusal must contain>".
scenario() {
    local name="$1" expect="$2" iso="$3" fw="$4" machine="$5" mem="$6" smp="$7" timeout="$8" accel="$9"
    shift 9
    [ "${1:-}" = "--" ] && shift
    [[ "$name" =~ $ONLY ]] || return 0
    if [ ! -f "$iso" ]; then
        echo "SKIP: $name ($iso not built)"
        skipped=$((skipped + 1))
        return 0
    fi
    QEMU_EXTRA=("$@")
    if [ "$accel" = tcg ] || [ "$KVM" = no ]; then
        QEMU_ACCEL=(-accel "tcg,thread=multi")
        # TCG is several times slower; give it room, but still catch hangs.
        timeout=$((timeout * 3))
    else
        QEMU_ACCEL=(-accel kvm)
        # Default to the host CPU unless the scenario picked a model.
        if ! printf '%s\n' "$@" | grep -qx -- -cpu; then QEMU_ACCEL+=(-cpu host); fi
    fi

    local run log ok attempt why
    for run in $(seq 1 "$REPEAT"); do
        log="$LOGDIR/$name-run$run.log"
        local start=$SECONDS
        for attempt in $(seq 1 "$PLATFORM_ATTEMPTS"); do
            run_qemu "$iso" "$log" "$fw" "$machine" "$mem" "$smp" "$timeout"
            ok=no
            if [[ "$expect" == boot* ]]; then
                if boot_ok "$log" "$expect"; then
                    ok=yes
                fi
            else
                why="${expect#refuse:}"
                if grep -qF "$why" "$log" && ! grep -q "Optimal kernel" "$log"; then
                    ok=yes
                fi
            fi
            [ "$ok" = yes ] && break
            # A failure is only blamed on the platform (firmware, hypervisor
            # or the third-party bootloader) when it provably happened before
            # our kernel ran: no kernel output at all, and the VM died with a
            # hypervisor internal error or the CPU never reached 64-bit mode. Such a
            # run is retried, up to PLATFORM_ATTEMPTS attempts in all; if every
            # attempt fails the scenario fails. Our code never ran on a failed
            # attempt, so a retry can't hide a kernel bug: the kernel's tests
            # still have to pass in full on the attempt that boots.
            why=$(platform_failure "$log")
            if [ "$attempt" -lt "$PLATFORM_ATTEMPTS" ] && [ -n "$why" ]; then
                echo "PLATFORM: $name run $run attempt $attempt: $why; retrying"
                platform+=("$name-run$run attempt $attempt: $why")
                mv "$log" "$log.platform-attempt$attempt"
                continue
            fi
            break
        done
        if [ "$ok" = yes ]; then
            pass=$((pass + 1))
            echo "PASS: $name run $run ($((SECONDS - start))s)"
            rm -f "$log"
        else
            fail=$((fail + 1))
            failed+=("$name-run$run")
            echo "FAIL: $name run $run ($((SECONDS - start))s)"
            # The start (firmware and Limine) and the end (the hang report)
            # say where it died.
            head -n 25 "$log" 2>/dev/null
            echo "[...]"
            grep -E "KVM internal error|Suberror|emulation failure|extra data" "$log" 2>/dev/null
            tail -n 40 "$log" 2>/dev/null
        fi
    done
    QEMU_EXTRA=()
    QEMU_ACCEL=("${ACCEL[@]}")
    QEMU_WRAP=()
}

has_uefi() { [ -n "$OVMF" ]; }

# boot_ok <log> <expect>: the kernel test suite passed in full, and the ACPI
# expectations hold (see the top of this file). Prints what is missing.
boot_ok() {
    local log="$1" expect="$2" rest re
    grep -qE "OPTIMAL: ktest [0-9]+ passed, 0 failed" "$log" || return 1
    ! grep -qE "KTEST FAIL|KERNEL PANIC|EXCEPTION" "$log" || return 1
    if [ "$expect" = boot ]; then
        rest='^acpi: (XSDT|RSDT) rev .*; 0 warnings$'
    else
        rest="${expect#boot:}"
    fi
    if [[ "$rest" != *"interrupts:"* ]]; then
        rest="$rest && ^interrupts: on; 0 warnings\$"
    fi
    while :; do
        re="${rest%% && *}"
        if ! grep -qE -- "$re" "$log"; then
            echo "EXPECTED a line matching: $re"
            grep -E "^(acpi|apic|ioapic|pic|interrupts|irqdirt|OPTIMAL-QUIRK)" "$log"
            return 1
        fi
        [ "$re" = "$rest" ] && return 0
        rest="${rest#* && }"
    done
}

# Extra ACPI tables for the quirky-ACPI scenarios.
AQ=build/acpi-quirks
python3 scripts/acpi-quirk-tables.py "$AQ"

# Prints why a failed boot is the platform's fault, or nothing if it isn't
# provably so (see the comment in scenario).
platform_failure() {
    local log="$1"
    # Any line from our kernel means our code ran: never the platform's fault.
    # (Limine's own menu prints the entry name "Optimal", so that word alone
    # is not evidence; the kernel's first line is "Optimal kernel <version>".)
    if grep -qE "Optimal kernel|OPTIMAL:|KERNEL PANIC|EXCEPTION|KTEST" "$log"; then
        return 0
    fi
    local where="inside the firmware"
    grep -q "Limine" "$log" && where="inside the firmware or Limine, before the kernel ran"
    if grep -q "VM status: paused (internal-error)" "$log"; then
        echo "hypervisor internal error $where"
    elif sed -n '/^--- hang report/,$p' "$log" | grep -q "^EFER=0000000000000000"; then
        echo "firmware crashed before entering 64-bit mode"
    fi
}

# ---------------------------------------------------------------------------
# CPU models. Emulated (TCG) so each gets exactly its own feature set, even
# features the CI host lacks, and so every one of them is also a slow CPU.
# ---------------------------------------------------------------------------
for cpu in \
    qemu64 core2duo Nehalem Westmere SandyBridge IvyBridge Haswell Broadwell \
    Skylake-Client Skylake-Server Cascadelake-Server Icelake-Server \
    SapphireRapids GraniteRapids Denverton Snowridge SierraForest \
    Opteron_G1 Opteron_G3 Opteron_G5 phenom EPYC EPYC-Rome EPYC-Milan EPYC-Genoa \
    max; do
    scenario "cpu-$cpu-bios" boot "$ISO" bios q35 512M 2 40 tcg -- -cpu "$cpu"
done
if has_uefi; then
    for cpu in qemu64 Nehalem Skylake-Client EPYC-Genoa SierraForest max; do
        scenario "cpu-$cpu-uefi" boot "$ISO" uefi q35 512M 2 40 tcg -- -cpu "$cpu"
    done
fi
# Security features switched off or missing, as some firmware and VMs do.
scenario "cpu-no-smep-smap-umip" boot "$ISO" bios q35 512M 2 40 tcg -- -cpu max,-smep,-smap,-umip
scenario "cpu-no-nx" boot "$ISO" bios q35 512M 2 40 tcg -- -cpu qemu64,-nx
# 5-level paging (57-bit addresses), on newer Xeons and EPYCs.
scenario "cpu-la57-bios" boot "$ISO" bios q35 512M 2 40 tcg -- -cpu max,+la57
has_uefi && scenario "cpu-la57-uefi" boot "$ISO" uefi q35 512M 2 40 tcg -- -cpu max,+la57
# 1 and 16 emulated cores on old and new chipsets: slow CPUs, lots of them.
scenario "slow-tcg-smp16" "boot:^acpi: .*; 16 CPUs \(0 off.*; 0 warnings$" "$ISO" bios q35 1G 16 40 tcg
scenario "slow-tcg-pc-smp1" "boot:^acpi: RSDT .*; 1 CPUs \(0 off.*; 0 warnings$" "$ISO" bios pc 256M 1 40 tcg

# ---------------------------------------------------------------------------
# Core topologies and NUMA.
# ---------------------------------------------------------------------------
scenario "topo-2s-2c-2t" boot "$ISO" bios q35 1G 8,sockets=2,cores=2,threads=2 30 kvm
scenario "topo-6-cores-not-pow2" boot "$ISO" bios q35 1G 6,sockets=2,cores=3,threads=1 30 kvm
scenario "topo-2-of-8-present" "boot:^acpi: .*; 2 CPUs \(6 off, 0 hot-add.*; 0 warnings$" "$ISO" bios q35 1G 2,maxcpus=8 30 kvm
scenario "topo-dies" boot "$ISO" bios q35 1G 8,sockets=1,dies=2,cores=4,threads=1 30 kvm
# 64 vCPUs run under TCG. QEMU itself warns that 64 exceeds what KVM
# supports on the 4-core CI runners, and there KVM stops the firmware with
# an internal error on most attempts: in OVMF at the same address (RIP
# 0x7ecffc20/24) and in SeaBIOS's VGA option ROM (CS=c000, port writes to
# 0x3c4/0x3ce), three attempts in a row in runs 44902042113 and
# 44903476305, long before Limine runs. TCG still boots the kernel with all
# 64 CPUs on both firmwares, without the host bug; heavy oversubscription
# under KVM is covered by the contention scenarios.
scenario "topo-64-cores" "boot:^acpi: .*; 64 CPUs \(0 off.*; 0 warnings$" "$ISO" bios q35 2G 64 30 tcg
has_uefi && scenario "topo-64-cores-uefi" "boot:^acpi: XSDT .*; 64 CPUs \(0 off.*; 0 warnings$" "$ISO" uefi q35 2G 64 30 tcg

numa2=(-object memory-backend-ram,id=m0,size=1G -object memory-backend-ram,id=m1,size=1G
    -numa node,nodeid=0,cpus=0-1,memdev=m0 -numa node,nodeid=1,cpus=2-3,memdev=m1)
scenario "numa-2-nodes" boot "$ISO" bios q35 2G 4 30 kvm -- "${numa2[@]}"
has_uefi && scenario "numa-2-nodes-uefi" boot "$ISO" uefi q35 2G 4 30 kvm -- "${numa2[@]}"
# Lopsided nodes, a node with CPUs but no memory, and custom distances.
numa4=(-object memory-backend-ram,id=m0,size=256M -object memory-backend-ram,id=m1,size=1792M
    -object memory-backend-ram,id=m2,size=1G
    -numa node,nodeid=0,cpus=0,memdev=m0 -numa node,nodeid=1,cpus=1-3,memdev=m1
    -numa node,nodeid=2,cpus=4-5 -numa node,nodeid=3,cpus=6-7,memdev=m2
    -numa dist,src=0,dst=1,val=21 -numa dist,src=0,dst=2,val=31 -numa dist,src=0,dst=3,val=41
    -numa dist,src=1,dst=2,val=21 -numa dist,src=1,dst=3,val=31 -numa dist,src=2,dst=3,val=21)
scenario "numa-4-lopsided-memoryless" boot "$ISO" bios q35 3G 8 30 kvm -- "${numa4[@]}"
has_uefi && scenario "numa-4-lopsided-memoryless-uefi" boot "$ISO" uefi q35 3G 8 30 kvm -- "${numa4[@]}"

# ---------------------------------------------------------------------------
# Odd memory.
# ---------------------------------------------------------------------------
scenario "mem-96M-tiny" boot "$ISO" bios q35 96M 1 30 kvm
scenario "mem-129M-odd" boot "$ISO" bios pc 129M 2 30 kvm
scenario "mem-4097M-over-hole" boot "$ISO" bios q35 4097M 2 30 kvm
scenario "mem-split-1G-below-4G" boot "$ISO" bios q35,max-ram-below-4g=1G 6G 2 30 kvm
has_uefi && scenario "mem-split-1G-below-4G-uefi" boot "$ISO" uefi q35,max-ram-below-4g=1G 6G 2 30 kvm
# 32 GiB, more than the CI host has: reserve=off lets the host back it lazily
# (the guest only touches what it uses), so this tests a big memory map.
big=(-object memory-backend-ram,id=big,size=32G,reserve=off)
scenario "mem-32G-large" boot "$ISO" bios q35,memory-backend=big 32G 4 30 kvm -- "${big[@]}"
has_uefi && scenario "mem-32G-large-uefi" boot "$ISO" uefi q35,memory-backend=big 32G 4 30 kvm -- "${big[@]}"
dimms=(-object memory-backend-ram,id=d0,size=512M -device pc-dimm,memdev=d0
    -object memory-backend-ram,id=d1,size=256M -device pc-dimm,memdev=d1)
scenario "mem-extra-dimms" boot "$ISO" bios q35 1G,slots=4,maxmem=8G 2 30 kvm -- "${dimms[@]}"
has_uefi && scenario "mem-extra-dimms-uefi" boot "$ISO" uefi q35 1G,slots=4,maxmem=8G 2 30 kvm -- "${dimms[@]}"

# ---------------------------------------------------------------------------
# Firmware quirks.
# ---------------------------------------------------------------------------
scenario "fw-no-hpet" "boot:^acpi: .*, HPET none, .*; 0 warnings$" "$ISO" bios q35,hpet=off 512M 2 30 kvm
has_uefi && scenario "fw-no-hpet-uefi" "boot:^acpi: XSDT .*, HPET none, .*; 0 warnings$" "$ISO" uefi q35,hpet=off 512M 2 30 kvm
scenario "fw-no-hpet-pc" "boot:^acpi: .*, HPET none, .*; 0 warnings$" "$ISO" bios pc,hpet=off 512M 2 30 kvm
# No ACPI means no MADT: the local APIC and the 8259s are still set up,
# but no I/O APIC is known, which is the one warning.
scenario "fw-no-acpi" "boot:^acpi: none \(no RSDP from the bootloader\)$ && ^apic: warning: no MADT: no I/O APIC routes, LINT pins masked$ && ^interrupts: on; 1 warnings$" "$ISO" bios pc,acpi=off 512M 1 30 kvm
scenario "fw-pc-no-smbus-no-sata" boot "$ISO" bios pc,smbus=off,sata=off 256M 1 30 kvm
# Quirky ACPI tables, added with -acpitable (scripts/acpi-quirk-tables.py).
# A second MADT full of junk, plus tables with unknown signatures: the first
# MADT is used, the duplicate is reported, the unknown ones are listed.
dup=(-acpitable "file=$AQ/madt-dup.bin" -acpitable "file=$AQ/odd-0.bin"
    -acpitable "file=$AQ/odd-1.bin" -acpitable "file=$AQ/odd-2.bin")
dup_expect='boot:^acpi: .* tables \[FACP APIC .*APIC OPTQ ZZZZ \?\?\?\? (BGRT )?DSDT\]; 2 CPUs \(0 off, 0 hot-add, 0 x2APIC\), 1 I/O APIC, 5 overrides, .*; 1 warnings$ && ^acpi: warning: 2 APIC tables listed; using the first$'
scenario "acpi-dup-madt-odd-tables" "$dup_expect" "$ISO" bios q35 512M 2 30 kvm -- "${dup[@]}"
has_uefi && scenario "acpi-dup-madt-odd-tables-uefi" "$dup_expect" "$ISO" uefi q35 512M 2 30 kvm -- "${dup[@]}"
# A long root table: 40 more SSDTs.
many=()
for i in $(seq 0 39); do many+=(-acpitable "file=$AQ/ssdt-$i.bin"); done
scenario "acpi-46-tables" "boot:^acpi: RSDT rev 0, 46 tables .*; 0 warnings$" "$ISO" bios q35 512M 2 30 kvm -- "${many[@]}"
has_uefi && scenario "acpi-47-tables-uefi" "boot:^acpi: XSDT rev 2, 47 tables .*; 0 warnings$" "$ISO" uefi q35 512M 2 30 kvm -- "${many[@]}"
# A 64 KiB table (QEMU's limit): 16 pages through the read window on
# SeaBIOS, and 64 KiB more ACPI reclaimable memory to give back on OVMF
# (72 KiB without it).
big_ssdt=(-acpitable "file=$AQ/ssdt-big.bin")
scenario "acpi-64K-table" "boot:^acpi: RSDT .* SSDT DSDT\]; .*; 0 warnings$" "$ISO" bios q35 512M 2 30 kvm -- "${big_ssdt[@]}"
has_uefi && scenario "acpi-64K-table-uefi" "boot:^acpi: XSDT .* SSDT BGRT DSDT\]; .*; (1[3-9][0-9]|[2-9][0-9]{2}) KiB reclaimed; 0 warnings$" "$ISO" uefi q35 512M 2 30 kvm -- "${big_ssdt[@]}"

# Broken ACPI tables, made in RAM by the firmware-quirk injector after
# ExitBootServices (so no firmware code can repair them), UEFI only. QEMU
# builds the tables and OVMF installs them, so every OVMF build lays them
# out the same way. A run passes only if the injector reports every word
# applied, the kernel's test suite passes in full, and the kernel prints
# exactly the expected summary and warnings.
QUIRK_EFI="${QUIRK_EFI:-bootloader/target/x86_64-unknown-uefi/release/optimal-boot-quirk.efi}"
QDIR=build/quirk-images
mkdir -p "$QDIR"
# quirk_img <name> <words...>: builds $QDIR/<name>.img (the ktest kernel).
quirk_img() {
    local name="$1"
    shift
    QUIRK_EFI="$QUIRK_EFI" scripts/quirk-image.sh build/optimal-kernel-ktest "$QDIR/$name.img" "$@"
}
# What every injector run must print: every word applied, none missed.
quirk_lines() { echo "^OPTIMAL-QUIRK: applied: $*\$ && ^OPTIMAL-QUIRK: not applied:\$"; }
X='^acpi: XSDT rev 2, '
W='^acpi: warning: '
# The ACPI words, each on its own, each with the one warning it must cause.
# word | CPUs | expected lines (after the injector's own)
ACPI_CASES=(
    "acpi-madt-checksum|2|${X}.*; 1 warnings\$ && ${W}APIC checksum wrong \(used anyway\)\$"
    "acpi-fadt-checksum|2|${X}.*; 1 warnings\$ && ${W}FACP checksum wrong \(used anyway\)\$"
    "acpi-xsdt-signature|2|^acpi: RSDT rev 2, .*; 1 warnings\$ && ${W}XSDT unusable; using the RSDT\$"
    "acpi-rsdp-checksum|2|^acpi: RSDT rev 2, .*; 1 warnings\$ && ${W}RSDP extended part unusable \(length or checksum\); using the RSDT\$"
    "acpi-madt-junk|4|${X}.*; 2 CPUs \(0 off, 0 hot-add, 0 x2APIC\), 1 I/O APIC, 5 overrides, 0 LAPIC NMI, .*; 2 warnings\$ && ${W}MADT: 2 entries skipped \(short, duplicate or invalid\)\$ && ${W}MADT entry list truncated\$"
    "acpi-hpet-misaligned|2|${X}.*, HPET none, .*; 1 warnings\$ && ${W}HPET table unusable: BadField\$"
    "acpi-dsdt-clash|2|${X}.* DSDT\]; .*; 1 warnings\$ && ${W}FADT DSDT and X_DSDT disagree; using X_DSDT\$"
    "acpi-dsdt-in-ecam|2|${X}[0-9]+ tables \[FACP APIC HPET MCFG WAET( BGRT)?\]; .*; 1 warnings\$ && ${W}DSDT at 0x[0-9a-f]+ is in PCI configuration space \(MCFG\); refused\$"
)
# Everything at once, two ways (the RSDT fallback and the DSDT edit each
# have two words that would hide each other).
ALL1="acpi-madt-junk acpi-hpet-misaligned acpi-dsdt-clash acpi-madt-checksum acpi-fadt-checksum acpi-xsdt-signature"
ALL1_EXPECT="^acpi: RSDT rev 2, .* DSDT\]; 2 CPUs \(0 off, 0 hot-add, 0 x2APIC\), 1 I/O APIC, 5 overrides, 0 LAPIC NMI, HPET none, .*; 7 warnings\$ && ${W}XSDT unusable; using the RSDT\$ && ${W}APIC checksum wrong \(used anyway\)\$ && ${W}FACP checksum wrong \(used anyway\)\$ && ${W}FADT DSDT and X_DSDT disagree; using X_DSDT\$ && ${W}MADT: 2 entries skipped \(short, duplicate or invalid\)\$ && ${W}MADT entry list truncated\$ && ${W}HPET table unusable: BadField\$"
ALL2="acpi-madt-junk acpi-hpet-misaligned acpi-dsdt-in-ecam acpi-madt-checksum acpi-fadt-checksum acpi-rsdp-checksum"
ALL2_EXPECT="^acpi: RSDT rev 2, [0-9]+ tables \[FACP APIC HPET MCFG WAET( BGRT)?\]; 2 CPUs \(0 off, 0 hot-add, 0 x2APIC\), 1 I/O APIC, 5 overrides, 0 LAPIC NMI, HPET none, .*; 7 warnings\$ && ${W}RSDP extended part unusable \(length or checksum\); using the RSDT\$ && ${W}APIC checksum wrong \(used anyway\)\$ && ${W}FACP checksum wrong \(used anyway\)\$ && ${W}DSDT at 0x[0-9a-f]+ is in PCI configuration space \(MCFG\); refused\$ && ${W}MADT: 2 entries skipped \(short, duplicate or invalid\)\$ && ${W}MADT entry list truncated\$ && ${W}HPET table unusable: BadField\$"
if has_uefi && [ ! -f "$QUIRK_EFI" ]; then
    # Never a quiet skip: CI builds the injector, so a missing one is a
    # broken job, not a weaker test.
    echo "FAIL: broken-ACPI scenarios ($QUIRK_EFI not built)"
    fail=$((fail + 1))
    failed+=("acpi-ram-*: injector not built")
elif has_uefi; then
    for c in "${ACPI_CASES[@]}"; do
        IFS='|' read -r word cpus expect <<<"$c"
        name="acpi-ram-${word#acpi-}"
        quirk_img "$name" "$word"
        scenario "$name" "boot:$(quirk_lines "$word") && $expect" "$QDIR/$name.img" uefi q35 512M "$cpus" 30 kvm
    done
    # shellcheck disable=SC2086 # one word per argument
    quirk_img acpi-ram-all-1 $ALL1
    # shellcheck disable=SC2086
    quirk_img acpi-ram-all-2 $ALL2
    scenario "acpi-ram-all-1" "boot:$(quirk_lines "$ALL1") && $ALL1_EXPECT" "$QDIR/acpi-ram-all-1.img" uefi q35 512M 4 30 kvm
    scenario "acpi-ram-all-2" "boot:$(quirk_lines "$ALL2") && $ALL2_EXPECT" "$QDIR/acpi-ram-all-2.img" uefi q35 512M 4 30 kvm
    # The same on an emulated CPU, which also makes the firmware slow.
    scenario "acpi-ram-all-1-tcg" "boot:$(quirk_lines "$ALL1") && $ALL1_EXPECT" "$QDIR/acpi-ram-all-1.img" uefi q35 512M 4 40 tcg -- -cpu qemu64
fi

# Other OVMF builds (older and newer edk2), fetched by scripts/fetch-firmware.sh.
for code in build/firmware/*/OVMF_CODE*.fd; do
    [ -f "$code" ] || continue
    ver="$(basename "$(dirname "$code")")"
    case "$ver" in
        edk2-2022.11-debian12)
            # Evidence (CI, 2026-11-21): with KVM and `-cpu host` this build
            # crashes in its own 32-bit PEI phase (CR0=0x33, EFER=0, EIP=0x1369,
            # wild addresses) before any bootloader runs, on every attempt,
            # while the same firmware and ISO pass under TCG. So it is a
            # firmware/host-CPU incompatibility, not ours: under KVM it runs
            # with a plain qemu64 CPU model instead.
            scenario "fw-ovmf-$ver-kvm-qemu64" boot "$ISO" "ovmf=$code" q35 512M 2 30 kvm -- -cpu qemu64 ;;
        *)
            scenario "fw-ovmf-$ver" boot "$ISO" "ovmf=$code" q35 512M 2 30 kvm ;;
    esac
    # Same firmware on an emulated CPU (different CPUID, no KVM): firmware
    # bugs often depend on exactly what the CPU reports.
    scenario "fw-ovmf-$ver-tcg-qemu64" boot "$ISO" "ovmf=$code" q35 512M 2 40 tcg -- -cpu qemu64
    # Every ACPI break at once, under this firmware's own RSDP and XSDT.
    if [ -f "$QDIR/acpi-ram-all-1.img" ]; then
        scenario "fw-ovmf-$ver-acpi-ram-all-1" "boot:$(quirk_lines "$ALL1") && $ALL1_EXPECT" "$QDIR/acpi-ram-all-1.img" "ovmf=$code" q35 512M 4 40 tcg -- -cpu qemu64
    fi
done

# ---------------------------------------------------------------------------
# Interrupt controllers left dirty.
# ---------------------------------------------------------------------------
# Limine masks the 8259s and the I/O APIC's fixed-delivery pins on its way
# to the kernel (its protocol promises it), so the injector's pic and
# ioapic words never reach the kernel, and Limine itself triple-faults on
# the ExtINT and NMI pins the ioapic word leaves live (seen under QEMU 8.2
# with OVMF, 2026-11-26). So the test kernel dirties them itself, after Limine
# (irqdirt=, kernel/src/irq/dirt.rs): 8259s on the BIOS vectors and
# unmasked, every I/O APIC pin live, the local APIC's LVTs live with TPR
# 0x20. The kernel must report exactly that, clean it all up, and pass
# its interrupt tests (the PIT must still arrive only on its I/O APIC
# route, never through an 8259).
DIRT_ISO=build/optimal-ktest-irqdirt.iso
dirt='^irqdirt: applied: pic ioapic lapic; PIC masks 0x00/0x00; 24 I/O APIC pins unmasked; [4-7] LVTs unmasked, TPR 0x20;$ && ^apic: firmware left: local APIC xAPIC, software enabled, TPR 0x20, [4-7] LVTs unmasked, 0 in service; PIC masks 0x00/0x00; I/O APIC pins unmasked 24/24$ && ^pic: 8259s remapped to 0x20/0x28 and masked$ && ^ioapic: ISA IRQs: 15 routed; moved 0>2; level 5 9 10 11; active low none; 0 NMI pins$'
scenario "irq-dirty-bios" "boot:$dirt" "$DIRT_ISO" bios q35 512M 2 30 kvm
has_uefi && scenario "irq-dirty-uefi" "boot:$dirt" "$DIRT_ISO" uefi q35 512M 2 30 kvm
scenario "irq-dirty-tcg-qemu64" "boot:$dirt" "$DIRT_ISO" bios q35 512M 2 40 tcg -- -cpu qemu64
scenario "irq-dirty-tcg-smp4-max" "boot:$dirt" "$DIRT_ISO" bios q35 1G 4 40 tcg -- -cpu max
scenario "irq-dirty-pc" "boot:$dirt" "$DIRT_ISO" bios pc 256M 1 30 kvm
# Which mode the local APIC ends in: x2APIC whenever the CPU has it...
scenario "apic-x2apic-host-cpu" "boot:^apic: x2APIC \(CPU supports it\), id 0, " "$ISO" bios q35 512M 2 30 kvm
# ...xAPIC when it does not...
scenario "apic-xapic-tcg-qemu64" "boot:^apic: xAPIC \(CPU has no x2APIC\), id 0, " "$ISO" bios q35 512M 2 40 tcg -- -cpu qemu64
# ...and xAPIC when the DMAR table opts out (an Intel IOMMU with interrupt
# remapping but without extended interrupt mode).
scenario "apic-dmar-x2apic-opt-out" "boot:^acpi: .* tables \[.*DMAR.*\]; .*; 0 warnings$ && ^apic: xAPIC \(firmware opted out of x2APIC \(DMAR\)\), " "$ISO" bios q35 512M 2 30 kvm -- -acpitable "file=$AQ/dmar-x2apic-opt-out.bin"
# A CPU with no local APIC at all: everything masked, interrupts stay off.
scenario "apic-none-tcg" "boot:^apic: firmware left: local APIC disabled; PIC masks 0xff/0xff; I/O APIC pins unmasked 0/24$ && ^interrupts: off \(the CPU has no local APIC \(IA32_APIC_BASE 0x100\)\); 0 warnings$" "$ISO" bios q35 512M 1 40 tcg -- -cpu qemu64,-apic
# Local APIC states from the firmware-quirk injector (applied after
# ExitBootServices; Limine leaves the local APIC as it finds it).
if has_uefi && [ -f "$QUIRK_EFI" ]; then
    quirk_img apic-lapic lapic
    quirk_img apic-x2apic x2apic
    quirk_img apic-x2apic-lapic x2apic lapic
    quirk_img apic-off apic-off
    scenario "apic-quirk-lapic" "boot:$(quirk_lines lapic) && ^apic: firmware left: local APIC xAPIC, software enabled, TPR 0x20, [4-7] LVTs unmasked, 0 in service; " "$QDIR/apic-lapic.img" uefi q35 512M 2 30 kvm
    scenario "apic-quirk-lapic-tcg" "boot:$(quirk_lines lapic) && ^apic: firmware left: local APIC xAPIC, software enabled, TPR 0x20, 6 LVTs unmasked, 0 in service; " "$QDIR/apic-lapic.img" uefi q35 512M 2 40 tcg -- -cpu qemu64
    scenario "apic-quirk-x2apic" "boot:$(quirk_lines x2apic) && ^apic: firmware left: local APIC x2APIC, software enabled, TPR 0x0, && ^apic: x2APIC \(firmware left it on\), " "$QDIR/apic-x2apic.img" uefi q35 512M 2 30 kvm
    scenario "apic-quirk-x2apic-lapic" "boot:$(quirk_lines lapic x2apic) && ^apic: firmware left: local APIC x2APIC, software enabled, TPR 0x20, [4-7] LVTs unmasked, 0 in service; && ^apic: x2APIC \(firmware left it on\), " "$QDIR/apic-x2apic-lapic.img" uefi q35 512M 2 30 kvm
    # Re-enabling a globally disabled local APIC works under KVM, as on
    # current CPUs; QEMU's emulated APIC only re-enables on reset, as on
    # some older ones, and then interrupts must stay off cleanly.
    scenario "apic-quirk-off" "boot:$(quirk_lines apic-off) && ^apic: firmware left: local APIC disabled; && ^apic: x2APIC \(CPU supports it\), && ^apic: warning: firmware left the local APIC disabled; switched it back on$ && ^interrupts: on; 1 warnings$" "$QDIR/apic-off.img" uefi q35 512M 2 30 kvm
    scenario "apic-quirk-off-tcg" "boot:$(quirk_lines apic-off) && ^apic: firmware left: local APIC disabled; && ^interrupts: off \(the local APIC is disabled and would not switch back on \(IA32_APIC_BASE 0xfee00100\)\); 0 warnings$" "$QDIR/apic-off.img" uefi q35 512M 2 40 tcg -- -cpu qemu64
fi

# ---------------------------------------------------------------------------
# Secure Boot (images from scripts/secureboot.sh).
# ---------------------------------------------------------------------------
if has_uefi && [ -f build/sb/VARS.fd ]; then
    scenario "sb-signed-chain" boot build/sb/sb-signed.iso uefi-sb q35 512M 2 30 kvm
    scenario "sb-unsigned-bootloader" "refuse:Access Denied" build/sb/sb-unsigned.iso uefi-sb q35 512M 2 30 kvm
    scenario "sb-wrong-signing-key" "refuse:Access Denied" build/sb/sb-wrong-key.iso uefi-sb q35 512M 2 30 kvm
    scenario "sb-tampered-kernel" "refuse:hash for URI" build/sb/sb-tampered-kernel.iso uefi-sb q35 512M 2 30 kvm
    scenario "sb-tampered-config" "refuse:CHECKSUM MISMATCH FOR CONFIG FILE" build/sb/sb-tampered-config.iso uefi-sb q35 512M 2 30 kvm
fi

# ---------------------------------------------------------------------------
# A host fighting us for CPU.
# ---------------------------------------------------------------------------
# Every vCPU of a 16-core VM squeezed onto one host core: vCPUs get preempted
# at random points, like a laptop under heavy load or an oversold cloud VM.
QEMU_WRAP=(taskset -c 0)
scenario "contention-16-vcpus-on-1-core" boot "$ISO" bios q35 1G 16 60 kvm
QEMU_WRAP=(taskset -c 0)
has_uefi && scenario "contention-16-vcpus-on-1-core-uefi" boot "$ISO" uefi q35 1G 16 60 kvm
QEMU_WRAP=()

if command -v stress-ng >/dev/null; then
    # Load every host core (CPU, cache and memory pressure) and boot through it.
    stress-ng --cpu 0 --cache 2 --vm 2 --vm-bytes 25% --timeout 0 --quiet &
    stress=$!
    scenario "loaded-host-smp4" boot "$ISO" bios q35 1G 4 60 kvm
    has_uefi && scenario "loaded-host-smp4-uefi" boot "$ISO" uefi q35 1G 4 60 kvm
    scenario "loaded-host-tcg-smp8" boot "$ISO" bios q35 1G 8 60 tcg
    QEMU_WRAP=(taskset -c 0)
    scenario "loaded-host-and-squeezed" boot "$ISO" bios q35 1G 8 60 kvm
    QEMU_WRAP=()
    kill "$stress" 2>/dev/null
    wait "$stress" 2>/dev/null
else
    echo "SKIP: loaded-host scenarios (stress-ng not installed)"
    skipped=$((skipped + 1))
fi

echo "===== tough conditions: $pass passed, $fail failed, $skipped skipped, ${#platform[@]} platform retries ====="
for p in "${platform[@]}"; do echo "  platform retry: $p"; done
for f in "${failed[@]}"; do echo "  failed: $f"; done
[ "$fail" -eq 0 ]
