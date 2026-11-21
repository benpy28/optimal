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
# failures. A "refuse" scenario (Secure Boot attacks) passes only if the
# firmware or bootloader refuses it with the expected message and the kernel
# never runs. Nothing may hang, and nothing may panic.
#
# More conditions join as the kernel grows: failed allocations (1b), quirky
# ACPI tables, timer jitter and spurious interrupts (1c), disk power cuts and
# IO errors (Phase 4).
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
# <expect> is "boot" or "refuse:<text the refusal must contain>".
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
            if [ "$expect" = boot ]; then
                if grep -qE "OPTIMAL: ktest [0-9]+ passed, 0 failed" "$log" &&
                    ! grep -qE "KTEST FAIL|KERNEL PANIC|EXCEPTION" "$log"; then
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
scenario "slow-tcg-smp16" boot "$ISO" bios q35 1G 16 40 tcg
scenario "slow-tcg-pc-smp1" boot "$ISO" bios pc 256M 1 40 tcg

# ---------------------------------------------------------------------------
# Core topologies and NUMA.
# ---------------------------------------------------------------------------
scenario "topo-2s-2c-2t" boot "$ISO" bios q35 1G 8,sockets=2,cores=2,threads=2 30 kvm
scenario "topo-6-cores-not-pow2" boot "$ISO" bios q35 1G 6,sockets=2,cores=3,threads=1 30 kvm
scenario "topo-2-of-8-present" boot "$ISO" bios q35 1G 2,maxcpus=8 30 kvm
scenario "topo-dies" boot "$ISO" bios q35 1G 8,sockets=1,dies=2,cores=4,threads=1 30 kvm
# 64 vCPUs run under TCG. QEMU itself warns that 64 exceeds what KVM
# supports on the 4-core CI runners, and there KVM stops the firmware with
# an internal error on most attempts: in OVMF at the same address (RIP
# 0x7ecffc20/24) and in SeaBIOS's VGA option ROM (CS=c000, port writes to
# 0x3c4/0x3ce), three attempts in a row in runs 44902042113 and
# 44903476305, long before Limine runs. TCG still boots the kernel with all
# 64 CPUs on both firmwares, without the host bug; heavy oversubscription
# under KVM is covered by the contention scenarios.
scenario "topo-64-cores" boot "$ISO" bios q35 2G 64 30 tcg
has_uefi && scenario "topo-64-cores-uefi" boot "$ISO" uefi q35 2G 64 30 tcg

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
scenario "fw-no-hpet" boot "$ISO" bios q35,hpet=off 512M 2 30 kvm
has_uefi && scenario "fw-no-hpet-uefi" boot "$ISO" uefi q35,hpet=off 512M 2 30 kvm
scenario "fw-no-hpet-pc" boot "$ISO" bios pc,hpet=off 512M 2 30 kvm
scenario "fw-no-acpi" boot "$ISO" bios pc,acpi=off 512M 1 30 kvm
scenario "fw-pc-no-smbus-no-sata" boot "$ISO" bios pc,smbus=off,sata=off 256M 1 30 kvm
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
done

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
