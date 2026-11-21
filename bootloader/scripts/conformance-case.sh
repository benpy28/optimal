#!/usr/bin/env bash
# conformance-case.sh <case> [loaders]
# Boots the conformance kernel for one test case under each loader
# (default "optimal limine") and fails unless every run prints
# "OPTIMAL-CONFORMANCE: PASS". Limine is the reference: a check that fails
# under Limine is a bug in the check (or a Limine bug, to be confirmed
# against the spec), and one that fails only under ours is a bug in ours.
#
# Cases: r3 r4 r5 r6 pie entry la57 tpm iommu-intel iommu-amd smp big small
#        headless old-cpu
set -euo pipefail
case_name="$1"; loaders="${2:-optimal limine}"
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
FW=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}

# The module every run carries: byte i = (i*167 + 13) mod 256, 100003 bytes.
python3 -c "import sys; sys.stdout.buffer.write(bytes((i*167+13)&0xff for i in range(100003)))" > "$OUT/pattern"
export OB_EXPECT_CMDLINE="conformance case=$case_name" OB_EXPECT_MODULES=1
export OB_REVISION=3 OB_PIE=0 OB_PAGING=4 OB_ENTRY_REQ=0
QEMU=(-machine q35 -m 512M)
TPM=0
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
    headless) OB_REVISION=5; QEMU+=(-vga none) ;;
    old-cpu) QEMU+=(-cpu qemu64,-nx,-pdpe1gb); export QEMU_ACCEL=tcg ;;
    *) echo "unknown case $case_name"; exit 2 ;;
esac
CMDLINE="$OB_EXPECT_CMDLINE"
kernel="$OUT/conformance-$case_name"
scripts/build-conformance.sh "$kernel"

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
        drive=(-drive format=raw,file="$OUT/disk.img")
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
done
exit $status
