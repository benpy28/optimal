#!/usr/bin/env bash
# Builds Optimal Boot and bootable test images:
#   build/ob/esp.img      FAT EFI system partition (loader, kernel, signed config)
#   build/ob/disk.img     GPT disk with that ESP
#   build/ob/optimal.iso  UEFI El Torito CD image with that ESP
# Environment:
#   KERNEL   kernel ELF to boot (default: build the repo's kernel)
#   CMDLINE  kernel command line (default: empty)
#   MODULES  space-separated module files to add (default: none)
#   KEYDIR   where the signing key lives (default build/ob/keys; made if missing)
#   CONFIG_EXTRA  extra global config lines (e.g. "kaslr off")
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=../build/ob
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
KEYDIR=${KEYDIR:-$OUT/keys}
SIGN=target/release/optimal-boot-sign

cargo build -q --release -p optimal-boot-sign
if [ ! -f "$KEYDIR/config.secret" ]; then
    mkdir -p "$KEYDIR"
    "$SIGN" keygen "$KEYDIR/config.secret" "$KEYDIR/config.pub"
fi
OPTIMAL_BOOT_PUBKEYS="$KEYDIR/config.pub" cargo build -q --release -p optimal-boot --target x86_64-unknown-uefi
LOADER=target/x86_64-unknown-uefi/release/optimal-boot.efi

if [ -z "${KERNEL:-}" ]; then
    (cd ../kernel && cargo build -q --release)
    KERNEL=../kernel/target/x86_64-unknown-none/release/optimal-kernel
fi

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/EFI/BOOT" "$STAGE/boot"
cp "$LOADER" "$STAGE/EFI/BOOT/BOOTX64.EFI"
cp "$KERNEL" "$STAGE/boot/optimal-kernel"
{
    echo "optimal-boot-config 1"
    echo "timeout 0"
    [ -n "${CONFIG_EXTRA:-}" ] && printf '%b\n' "$CONFIG_EXTRA"
    echo "entry optimal"
    echo "  title Optimal"
    echo "  kernel /boot/optimal-kernel sha512:$("$SIGN" hash "$STAGE/boot/optimal-kernel")"
    echo "  cmdline ${CMDLINE:-}"
    for m in ${MODULES:-}; do
        name=$(basename "$m")
        cp "$m" "$STAGE/boot/$name"
        echo "  module /boot/$name sha512:$("$SIGN" hash "$m") $name"
    done
} > "$STAGE/boot/optimal-boot.conf"
"$SIGN" sign "$KEYDIR/config.secret" "$STAGE/boot/optimal-boot.conf"

# FAT image sized to the payload plus headroom.
size_kb=$(( $(du -sk "$STAGE" | cut -f1) + 8192 ))
size_mb=$(( (size_kb + 1023) / 1024 ))
[ "$size_mb" -lt 34 ] && size_mb=34
rm -f "$OUT/esp.img"
truncate -s "${size_mb}M" "$OUT/esp.img"
mkfs.vfat -F 32 -n OPTIMAL "$OUT/esp.img" >/dev/null 2>&1 || mkfs.vfat -n OPTIMAL "$OUT/esp.img" >/dev/null
mcopy -s -i "$OUT/esp.img" "$STAGE/EFI" "$STAGE/boot" ::/
cp "$STAGE/boot/optimal-boot.conf" "$OUT/optimal-boot.conf"

# GPT disk: 1 MiB alignment, one ESP partition. Fixed GUIDs, so the
# conformance kernel's ob-boot-volume check can compare what it is told.
rm -f "$OUT/disk.img"
truncate -s $(( size_mb + 2 ))M "$OUT/disk.img"
printf 'label: gpt\nlabel-id: 0B0071A1-0000-4000-8000-00000000D15C\nstart=2048, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=0B0071A1-0000-4000-8000-0000000AA27A, name="EFI system"\n' | sfdisk -q "$OUT/disk.img" >/dev/null
dd if="$OUT/esp.img" of="$OUT/disk.img" bs=1M seek=1 conv=notrunc status=none

# El Torito CD (UEFI only for now; BIOS boot is a later stage).
rm -rf "$STAGE/iso" && mkdir -p "$STAGE/iso"
cp "$OUT/esp.img" "$STAGE/iso/esp.img"
xorriso -as mkisofs -R -J -e esp.img -no-emul-boot -isohybrid-gpt-basdat -o "$OUT/optimal.iso" "$STAGE/iso" >/dev/null 2>&1
echo "Built $OUT/{esp.img,disk.img,optimal.iso}"
