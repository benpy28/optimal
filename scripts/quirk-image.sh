#!/usr/bin/env bash
# quirk-image.sh <kernel ELF> <out.img> <quirk word>...
# A UEFI disk (GPT, one FAT ESP) that boots the firmware-quirk injector
# (bootloader/quirk) first. It breaks the machine the way the words say
# (ACPI tables in RAM, interrupt controllers, IOMMUs), then chain-loads
# Limine from \EFI\BOOT\NEXT.EFI, which boots the kernel as usual. The
# injector prints "OPTIMAL-QUIRK: applied: <words>" on COM1 once done, and
# refuses to boot (OPTIMAL-QUIRK: FATAL) if a word can't be applied here.
# Env: QUIRK_EFI (default: the release build in bootloader/target).
set -euo pipefail
kernel="$1"; out="$2"; shift 2
[ "$#" -gt 0 ] || { echo "quirk-image.sh: no quirk words" >&2; exit 1; }
cd "$(dirname "$0")/.."
QUIRK_EFI="${QUIRK_EFI:-bootloader/target/x86_64-unknown-uefi/release/optimal-boot-quirk.efi}"
[ -f "$QUIRK_EFI" ] || { echo "quirk-image.sh: $QUIRK_EFI not built" >&2; exit 1; }
root="$(mktemp -d build/quirk_root.XXXXXX)"
esp="$(mktemp build/quirk_esp.XXXXXX)"
trap 'rm -rf "$root" "$esp"' EXIT
mkdir -p "$root/boot/limine" "$root/EFI/BOOT"
cp "$kernel" "$root/boot/optimal-kernel"
# No menu wait: the scenario timeouts are for the kernel.
sed 's/^timeout: .*/timeout: 0/' boot/limine.conf > "$root/boot/limine/limine.conf"
cp build/limine/BOOTX64.EFI "$root/EFI/BOOT/NEXT.EFI"
cp "$QUIRK_EFI" "$root/EFI/BOOT/BOOTX64.EFI"
printf '%s\n' "$*" > "$root/EFI/BOOT/QUIRKS.TXT"
size_mb=$(( ($(du -sk "$root" | cut -f1) + 8192 + 1023) / 1024 ))
[ "$size_mb" -lt 34 ] && size_mb=34   # FAT32's minimum
truncate -s "${size_mb}M" "$esp"
mkfs.vfat -F 32 -n OPTIMAL "$esp" >/dev/null
mcopy -s -i "$esp" "$root/EFI" "$root/boot" ::/
rm -f "$out"
truncate -s $(( size_mb + 2 ))M "$out"
printf 'label: gpt\nstart=2048, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI system"\n' | sfdisk -q "$out" >/dev/null
dd if="$esp" of="$out" bs=1M seek=1 conv=notrunc status=none
