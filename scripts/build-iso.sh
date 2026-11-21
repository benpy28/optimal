#!/usr/bin/env bash
# Builds the kernel and a bootable ISO (UEFI + BIOS) at build/optimal.iso.
# CI runs this; you never need to run it by hand.
set -euo pipefail
cd "$(dirname "$0")/.."

LIMINE_TAG="v9.6.7-binary"   # Limine bootloader, BSD-2-Clause
BUILD=build
ISO_ROOT="$BUILD/iso_root"

(cd kernel && cargo build --release)

if [ ! -d "$BUILD/limine" ]; then
    git clone --depth 1 --branch "$LIMINE_TAG" https://github.com/limine-bootloader/limine.git "$BUILD/limine"
fi
make -C "$BUILD/limine" >/dev/null

rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp kernel/target/x86_64-unknown-none/release/optimal-kernel "$ISO_ROOT/boot/optimal-kernel"
cp boot/limine.conf "$ISO_ROOT/boot/limine/"
cp "$BUILD"/limine/limine-bios.sys "$BUILD"/limine/limine-bios-cd.bin "$BUILD"/limine/limine-uefi-cd.bin "$ISO_ROOT/boot/limine/"
cp "$BUILD"/limine/BOOTX64.EFI "$BUILD"/limine/BOOTIA32.EFI "$ISO_ROOT/EFI/BOOT/"

xorriso -as mkisofs -R -r -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 \
    --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
    "$ISO_ROOT" -o "$BUILD/optimal.iso"
"$BUILD/limine/limine" bios-install "$BUILD/optimal.iso"

echo "Built $BUILD/optimal.iso"
