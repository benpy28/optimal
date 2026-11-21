#!/usr/bin/env bash
# Builds the Secure Boot test images (UEFI only) under build/sb/:
#
#   VARS.fd                 OVMF variable store with our own test keys enrolled
#                           (PK, KEK and db) and Secure Boot switched on
#   sb-signed.iso           Limine signed with our db key, limine.conf's BLAKE2B
#                           enrolled into Limine, and the kernel's BLAKE2B in
#                           limine.conf: the full verified chain. Must boot.
#   sb-unsigned.iso         stock, unsigned Limine. Firmware must refuse it.
#   sb-tampered-kernel.iso  signed chain, but one byte of the kernel changed
#                           after hashing. Limine must refuse it.
#   sb-tampered-config.iso  signed chain, but limine.conf edited after its hash
#                           was enrolled. Limine must refuse it.
#   sb-wrong-key.iso        Limine signed with a key the firmware doesn't trust.
#                           Firmware must refuse it.
#
# The keys are throwaway test keys made fresh on every run. Real release keys
# never live in CI logs or the repo.
#
# Needs: openssl, sbsigntool (sbsign), mtools (mcopy), b2sum, xorriso,
# python3-virt-firmware (virt-fw-vars), and Limine in build/limine.
set -euo pipefail
cd "$(dirname "$0")/.."

BUILD=build
SB=$BUILD/sb
LIMINE=$BUILD/limine
KERNEL="${1:-$BUILD/optimal-kernel-ktest}"
VARS_TEMPLATE="${OVMF_VARS_TEMPLATE:-/usr/share/OVMF/OVMF_VARS_4M.fd}"

mkdir -p "$SB"

newkey() {
    openssl req -new -x509 -newkey rsa:3072 -nodes -sha256 -days 30 \
        -subj "/CN=Optimal throwaway test $1/" \
        -keyout "$SB/$1.key" -out "$SB/$1.crt" 2>/dev/null
}
for k in PK KEK db rogue; do newkey "$k"; done

owner=$(python3 -c 'import uuid; print(uuid.uuid4())')
virt-fw-vars --input "$VARS_TEMPLATE" --output "$SB/VARS.fd" \
    --set-pk "$owner" "$SB/PK.crt" --add-kek "$owner" "$SB/KEK.crt" \
    --add-db "$owner" "$SB/db.crt" --secure-boot >/dev/null 2>&1

b2() { b2sum "$1" | cut -d' ' -f1; }

# write_config <file> <kernel hash>
write_config() {
    printf 'timeout: 0\nserial: yes\n\n/Optimal\n    protocol: limine\n    kernel_path: boot():/boot/optimal-kernel#%s\n' "$2" >"$1"
}

# make_sb_iso <out> <kernel> <limine.conf> <signed BOOTX64.EFI>
make_sb_iso() {
    local out="$1" kernel="$2" conf="$3" efi="$4" root
    root="$(mktemp -d "$BUILD/sbroot.XXXXXX")"
    mkdir -p "$root/boot/limine" "$root/EFI/BOOT"
    cp "$kernel" "$root/boot/optimal-kernel"
    cp "$conf" "$root/boot/limine/limine.conf"
    # The El Torito EFI image is a FAT disk with its own BOOTX64.EFI inside:
    # that is the copy the firmware actually runs, so it is the one replaced.
    cp "$LIMINE/limine-uefi-cd.bin" "$root/boot/limine/limine-uefi-cd.bin"
    mcopy -o -i "$root/boot/limine/limine-uefi-cd.bin" "$efi" ::/EFI/BOOT/BOOTX64.EFI
    cp "$efi" "$root/EFI/BOOT/BOOTX64.EFI"
    xorriso -as mkisofs -R -r -J \
        --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
        --protective-msdos-label "$root" -o "$out" 2>/dev/null
    rm -rf -- "$root"
    echo "Built $out"
}

# Limine with limine.conf's hash enrolled, so it rejects any edited config.
enrolled_limine() {
    local conf="$1" out="$2"
    cp "$LIMINE/BOOTX64.EFI" "$out"
    "$LIMINE/limine" enroll-config --quiet "$out" "$(b2 "$conf")"
}

sign() { sbsign --key "$SB/$1.key" --cert "$SB/$1.crt" --output "$3" "$2" 2>/dev/null; }

# The good chain.
write_config "$SB/limine.conf" "$(b2 "$KERNEL")"
enrolled_limine "$SB/limine.conf" "$SB/BOOTX64.enrolled.efi"
sign db "$SB/BOOTX64.enrolled.efi" "$SB/BOOTX64.signed.efi"
make_sb_iso "$SB/sb-signed.iso" "$KERNEL" "$SB/limine.conf" "$SB/BOOTX64.signed.efi"

# Unsigned bootloader (stock Limine, nothing enrolled).
make_sb_iso "$SB/sb-unsigned.iso" "$KERNEL" "$SB/limine.conf" "$LIMINE/BOOTX64.EFI"

# Signed by a key that isn't in db.
sign rogue "$SB/BOOTX64.enrolled.efi" "$SB/BOOTX64.rogue.efi"
make_sb_iso "$SB/sb-wrong-key.iso" "$KERNEL" "$SB/limine.conf" "$SB/BOOTX64.rogue.efi"

# Kernel changed after it was hashed: flip one byte in the middle of .text.
cp "$KERNEL" "$SB/kernel.tampered"
size=$(stat -c %s "$KERNEL")
printf '\xcc' | dd of="$SB/kernel.tampered" bs=1 seek=$((size / 2)) conv=notrunc status=none
make_sb_iso "$SB/sb-tampered-kernel.iso" "$SB/kernel.tampered" "$SB/limine.conf" "$SB/BOOTX64.signed.efi"

# Config edited after its hash was enrolled (an attacker adding a cmdline).
cp "$SB/limine.conf" "$SB/limine.tampered.conf"
printf '    cmdline: crashtest=panic\n' >>"$SB/limine.tampered.conf"
make_sb_iso "$SB/sb-tampered-config.iso" "$KERNEL" "$SB/limine.tampered.conf" "$SB/BOOTX64.signed.efi"
