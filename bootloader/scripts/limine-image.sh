#!/usr/bin/env bash
# limine-image.sh <kernel> <out.iso>
# The same kernel, command line and modules as build-images.sh, booted by
# Limine instead, so the conformance kernel's checks can be validated
# against the reference implementation. Env: LIMINE (a Limine binary
# release directory, default ../build/limine), CMDLINE, MODULES.
# An <out> ending in .img makes a GPT disk with one FAT ESP instead of a CD;
# with QUIRK_EFI and QUIRKS set, the firmware-quirk injector boots first and
# chain-loads Limine from \EFI\BOOT\NEXT.EFI.
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
if [[ "$out" == *.img ]]; then
    if [ -n "${QUIRK_EFI:-}" ]; then
        cp "$LIMINE"/BOOTX64.EFI "$root/EFI/BOOT/NEXT.EFI"
        cp "$QUIRK_EFI" "$root/EFI/BOOT/BOOTX64.EFI"
        printf '%s\n' "${QUIRKS:?QUIRKS must be set with QUIRK_EFI}" > "$root/EFI/BOOT/QUIRKS.TXT"
    else
        cp "$LIMINE"/BOOTX64.EFI "$root/EFI/BOOT/"
    fi
    size_mb=$(( ($(du -sk "$root" | cut -f1) + 8192 + 1023) / 1024 ))
    [ "$size_mb" -lt 34 ] && size_mb=34
    esp=$(mktemp)
    truncate -s "${size_mb}M" "$esp"
    mkfs.vfat -F 32 -n LIMINE "$esp" >/dev/null
    mcopy -s -i "$esp" "$root/EFI" "$root/boot" ::/
    rm -f "$out"
    truncate -s $(( size_mb + 2 ))M "$out"
    printf 'label: gpt\nstart=2048, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI system"\n' | sfdisk -q "$out" >/dev/null
    dd if="$esp" of="$out" bs=1M seek=1 conv=notrunc status=none
    rm -rf "$root" "$esp"
    exit 0
fi
cp "$LIMINE"/limine-bios.sys "$LIMINE"/limine-bios-cd.bin "$LIMINE"/limine-uefi-cd.bin "$root/boot/limine/"
cp "$LIMINE"/BOOTX64.EFI "$root/EFI/BOOT/"
xorriso -as mkisofs -R -r -J -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
    --protective-msdos-label "$root" -o "$out" >/dev/null 2>&1
rm -rf "$root"
