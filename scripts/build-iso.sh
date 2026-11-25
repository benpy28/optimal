#!/usr/bin/env bash
# Builds every bootable image CI tests (UEFI + BIOS hybrid ISOs):
#   build/optimal.iso            the normal kernel
#   build/optimal-ktest.iso      kernel with the in-kernel test suite
#   build/crash-<name>.iso       normal kernel told to crash on purpose
# CI runs this; you never need to run it by hand.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/crashtests.sh

LIMINE_TAG="v9.6.7-binary"   # Limine bootloader, BSD-2-Clause
BUILD=build
TARGET=kernel/target/x86_64-unknown-none/release/optimal-kernel

if [ ! -d "$BUILD/limine" ]; then
    git clone --depth 1 --branch "$LIMINE_TAG" https://github.com/limine-bootloader/limine.git "$BUILD/limine"
fi
make -C "$BUILD/limine" >/dev/null

# make_iso <kernel ELF> <output ISO> [kernel command line]
make_iso() {
    local kernel="$1" out="$2" cmdline="${3:-}"
    local root
    root="$(mktemp -d "$BUILD/iso_root.XXXXXX")"
    mkdir -p "$root/boot/limine" "$root/EFI/BOOT"
    cp "$kernel" "$root/boot/optimal-kernel"
    sed "s|^\(    kernel_path:.*\)$|\1\n    cmdline: ${cmdline}|" boot/limine.conf > "$root/boot/limine/limine.conf"
    cp "$BUILD"/limine/limine-bios.sys "$BUILD"/limine/limine-bios-cd.bin "$BUILD"/limine/limine-uefi-cd.bin "$root/boot/limine/"
    cp "$BUILD"/limine/BOOTX64.EFI "$BUILD"/limine/BOOTIA32.EFI "$root/EFI/BOOT/"
    xorriso -as mkisofs -R -r -J \
        -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
        -hfsplus -apm-block-size 2048 \
        --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
        "$root" -o "$out" 2>/dev/null
    "$BUILD/limine/limine" bios-install "$out" 2>/dev/null
    rm -rf "$root"
    echo "Built $out"
}

# Test-suite kernel first, then the normal one (so $TARGET ends as normal).
(cd kernel && cargo build --release --features ktest)
cp "$TARGET" "$BUILD/optimal-kernel-ktest"
(cd kernel && cargo build --release)
cp "$TARGET" "$BUILD/optimal-kernel"

make_iso "$BUILD/optimal-kernel" "$BUILD/optimal.iso"
make_iso "$BUILD/optimal-kernel-ktest" "$BUILD/optimal-ktest.iso"
# The test suite after the kernel itself dirties the 8259s, I/O APICs and
# local APIC the way sloppy firmware leaves them (kernel/src/irq/dirt.rs).
make_iso "$BUILD/optimal-kernel-ktest" "$BUILD/optimal-ktest-irqdirt.iso" "irqdirt=pic,ioapic,lapic"
for name in "${!CRASHTESTS[@]}"; do
    make_iso "$BUILD/optimal-kernel" "$BUILD/crash-$name.iso" "crashtest=$name"
done
