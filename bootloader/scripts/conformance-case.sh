#!/usr/bin/env bash
# conformance-case.sh <case> [loaders]
# Boots the conformance kernel for one test case under each loader
# (default "optimal limine") and fails unless every run prints
# "OPTIMAL-CONFORMANCE: PASS". Limine is the reference: a check that fails
# under Limine is a bug in the check (or a Limine bug, to be confirmed
# against the spec), and one that fails only under ours is a bug in ours.
#
# Cases: r3 r4 r5 r6 pie entry la57 tpm iommu-intel iommu-amd smp big small tiny
#        headless old-cpu
#        quirk-r3 quirk quirk-x2apic quirk-apic-off quirk-iommu-intel
#        quirk-iommu-amd
#
# The quirk-* cases boot through the firmware-quirk injector (quirk/), which
# leaves PICs, I/O APIC pins, the local APIC and IOMMUs the way sloppy
# firmware would, and only count if its report shows every quirk took.
set -euo pipefail
case_name="$1"; loaders="${2:-optimal limine}"
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
FW=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}

# The module every run carries: byte i = (i*167 + 13) mod 256, 100003 bytes.
python3 -c "import sys; sys.stdout.buffer.write(bytes((i*167+13)&0xff for i in range(100003)))" > "$OUT/pattern"
export OB_EXPECT_CMDLINE="conformance case=$case_name" OB_EXPECT_MODULES=1 OB_EXPECT_VOLUME=gpt
export OB_REVISION=3 OB_PIE=0 OB_PAGING=4 OB_ENTRY_REQ=0
QEMU=(-machine q35 -m 512M)
TPM=0
QUIRKS=""
case "$case_name" in
    r3) ;;
    r4) OB_REVISION=4 ;;
    r5) OB_REVISION=5 ;;
    r6) OB_REVISION=6 ;;
    pie) OB_PIE=1 OB_REVISION=6 ;;
    entry) OB_ENTRY_REQ=1 ;;
    la57) OB_PAGING=5 OB_REVISION=5; QEMU+=(-cpu max,+la57); export QEMU_ACCEL=tcg ;;
    tpm) OB_REVISION=5; TPM=1 ;;
    iommu-intel) OB_REVISION=5; QEMU=(-machine q35,kernel-irqchip=split -m 512M -device intel-iommu,intremap=on) ;;
    iommu-amd) OB_REVISION=5; QEMU+=(-device amd-iommu) ;;
    smp) OB_REVISION=5; QEMU=(-machine q35 -m 2G -smp 8) ;;
    big) OB_REVISION=4; QEMU=(-machine q35 -m 6G) ;;
    small) QEMU=(-machine q35 -m 160M) ;;
    tiny) QEMU=(-machine q35 -m 96M) ;;
    headless) OB_REVISION=5; QEMU+=(-vga none) ;;
    old-cpu) QEMU+=(-cpu qemu64,-nx,-pdpe1gb); export QEMU_ACCEL=tcg ;;
    quirk-r3) QUIRKS="pic ioapic lapic" ;;
    quirk) OB_REVISION=5; QUIRKS="pic ioapic lapic" ;;
    quirk-x2apic) OB_REVISION=6; QUIRKS="pic ioapic x2apic lapic"; QEMU+=(-cpu max,+x2apic)
        # QEMU 8.2's emulator has no x2APIC; refuse rather than run weaker.
        if [ ! -w /dev/kvm ] || [ "${QEMU_ACCEL:-auto}" = tcg ]; then
            echo "quirk-x2apic needs KVM (QEMU 8.2 cannot emulate x2APIC)"; exit 1
        fi ;;
    # Limine v11.4.1 leaves a globally disabled local APIC disabled (base
    # revision 5 says it must be enabled with SVR 0x1FF), so this case runs
    # under Optimal Boot only unless loaders are given explicitly.
    # QEMU's emulated APIC cannot be re-enabled once disabled, so this needs
    # KVM's in-kernel APIC to test anything.
    quirk-apic-off) OB_REVISION=5; QUIRKS="pic ioapic apic-off"; loaders="${2:-optimal}"
        if [ ! -w /dev/kvm ] || [ "${QEMU_ACCEL:-auto}" = tcg ]; then
            echo "quirk-apic-off needs KVM (QEMU's emulated APIC cannot be re-enabled)"; exit 1
        fi ;;
    quirk-iommu-intel) OB_REVISION=5; QUIRKS="pic ioapic lapic vtd"
        QEMU=(-machine q35,kernel-irqchip=split -m 512M -device intel-iommu,intremap=on) ;;
    quirk-iommu-amd) OB_REVISION=5; QUIRKS="pic ioapic lapic amdvi"; QEMU+=(-device amd-iommu) ;;
    *) echo "unknown case $case_name"; exit 2 ;;
esac
CMDLINE="$OB_EXPECT_CMDLINE"
kernel="$OUT/conformance-$case_name"
scripts/build-conformance.sh "$kernel"
if [ -n "$QUIRKS" ]; then
    cargo build -q --release -p optimal-boot-quirk --target x86_64-unknown-uefi
    export QUIRK_EFI=$PWD/target/x86_64-unknown-uefi/release/optimal-boot-quirk.efi QUIRKS
fi

status=0
for l in $loaders; do
    log="$OUT/conformance-$case_name-$l.log"
    extra=()
    if [ "$TPM" = 1 ]; then
        tpmdir=$(mktemp -d)
        swtpm socket --tpmstate dir="$tpmdir" --ctrl type=unixio,path="$tpmdir/sock" --tpm2 &
        sleep 1
        extra=(-chardev socket,id=chrtpm,path="$tpmdir/sock" -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0)
    fi
    if [ "$l" = optimal ]; then
        KERNEL="$kernel" CMDLINE="$CMDLINE" MODULES="$OUT/pattern" scripts/build-images.sh >/dev/null
        if [ -n "$QUIRKS" ]; then
            # Put the injector in front: it chain-loads \EFI\BOOT\NEXT.EFI.
            printf '%s\n' "$QUIRKS" > "$OUT/QUIRKS.TXT"
            esp="$OUT/disk.img@@1M"
            mmove -i "$esp" ::EFI/BOOT/BOOTX64.EFI ::EFI/BOOT/NEXT.EFI
            mcopy -i "$esp" "$QUIRK_EFI" ::EFI/BOOT/BOOTX64.EFI
            mcopy -i "$esp" "$OUT/QUIRKS.TXT" ::EFI/BOOT/QUIRKS.TXT
        fi
        drive=(-drive format=raw,file="$OUT/disk.img")
    elif [ -n "$QUIRKS" ]; then
        # A GPT disk rather than a CD, so the injector can sit in front.
        CMDLINE="$CMDLINE" MODULES="$OUT/pattern" scripts/limine-image.sh "$kernel" "$OUT/limine-$case_name.img"
        drive=(-drive format=raw,file="$OUT/limine-$case_name.img")
    else
        CMDLINE="$CMDLINE" MODULES="$OUT/pattern" scripts/limine-image.sh "$kernel" "$OUT/limine-$case_name.iso"
        drive=(-cdrom "$OUT/limine-$case_name.iso")
    fi
    scripts/run-qemu.sh "$log" 600 "${QEMU[@]}" -drive if=pflash,format=raw,readonly=on,file="$FW" "${drive[@]}" "${extra[@]}"
    result=$(grep -a "OPTIMAL-CONFORMANCE: \(PASS\|FAIL\)" "$log" || echo "no result (hang or crash)")
    echo "$case_name under $l: $result"
    if ! grep -aq "OPTIMAL-CONFORMANCE: PASS" "$log"; then
        grep -a "^FAIL\|FATAL" "$log" || tail -20 "$log"
        status=1
    fi
    if [ -n "$QUIRKS" ]; then
        # The test only counts if every quirk really took.
        applied=$(grep -a "^OPTIMAL-QUIRK: applied:" "$log" || true)
        echo "  ${applied:-OPTIMAL-QUIRK: no report (the injector did not run)}"
        grep -a "^OPTIMAL-QUIRK: detail:" "$log" | sed 's/^/  /' || true
        if grep -aq "^skip rev5-lapic" "$log"; then
            echo "  the kernel skipped the local APIC checks under $l: this run proves nothing about them"
            status=1
        fi
        for q in $QUIRKS; do
            if ! grep -qw -- "$q" <<<"${applied#*applied:}"; then
                echo "  quirk $q did not take under $l: this run proves nothing about it"
                status=1
            fi
        done
    fi
done
exit $status
