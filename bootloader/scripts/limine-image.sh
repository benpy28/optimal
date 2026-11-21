#!/usr/bin/env bash
# limine-image.sh <kernel> <out.iso>
# The same kernel, command line and modules as build-images.sh, booted by
# Limine instead, so the conformance kernel's checks can be validated
# against the reference implementation. Env: LIMINE (a Limine binary
# release directory, default ../build/limine), CMDLINE, MODULES.
set -euo pipefail
kernel=$(realpath "$1"); out=$(realpath -m "$2")
cd "$(dirname "$0")/.."
LIMINE=$(realpath "${LIMINE:-../build/limine}")
root=$(mktemp -d)
mkdir -p "$root/boot/limine" "$root/EFI/BOOT"
cp "$kernel" "$root/boot/optimal-kernel"
{
    echo "timeout: 0"
    echo "serial: yes"
    echo "/Optimal"
    echo "    protocol: limine"
    echo "    kernel_path: boot():/boot/optimal-kernel"
    [ -n "${CMDLINE:-}" ] && echo "    cmdline: $CMDLINE"
    for m in ${MODULES:-}; do
        name=$(basename "$m")
        cp "$m" "$root/boot/$name"
        echo "    module_path: boot():/boot/$name"
        echo "    module_string: $name"
    done
} > "$root/boot/limine/limine.conf"
cp "$LIMINE"/limine-bios.sys "$LIMINE"/limine-bios-cd.bin "$LIMINE"/limine-uefi-cd.bin "$root/boot/limine/"
cp "$LIMINE"/BOOTX64.EFI "$root/EFI/BOOT/"
xorriso -as mkisofs -R -r -J -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
    --protective-msdos-label "$root" -o "$out" >/dev/null 2>&1
rm -rf "$root"
