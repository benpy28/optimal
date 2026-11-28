#!/usr/bin/env bash
# bios-image.sh <out.img>
# Builds Optimal Boot's BIOS stage and a disk image it boots from:
#   LAYOUT=gpt (default)  GPT: BIOS boot partition (1 MiB) + FAT boot volume
#   LAYOUT=mbr            MBR: one FAT partition at 1 MiB, stage 2 in the gap
#   FAT=12|16|32          the boot volume's FAT type (default 32 on GPT, 16 on MBR)
# and, like build-images.sh: KERNEL (required), CMDLINE, MODULES, KEYDIR,
# CONFIG_EXTRA, plus TIMEOUT (menu seconds) and SECOND_ENTRY=1. Fixed GUIDs and disk id, so the tests can check what the
# loader reports.
set -euo pipefail
out=$(realpath -m "$1")
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
KEYDIR=${KEYDIR:-$OUT/keys}
SIGN=target/release/optimal-boot-sign
INSTALL=target/release/optimal-boot-install
LAYOUT=${LAYOUT:-gpt}
FAT=${FAT:-$([ "$LAYOUT" = mbr ] && echo 16 || echo 32)}
: "${KERNEL:?set KERNEL to the kernel ELF to boot}"

cargo build -q --release -p optimal-boot-sign -p optimal-boot-install
if [ ! -f "$KEYDIR/config.secret" ]; then
    mkdir -p "$KEYDIR"
    "$SIGN" keygen "$KEYDIR/config.secret" "$KEYDIR/config.pub"
fi
OPTIMAL_BOOT_PUBKEYS="$KEYDIR/config.pub" cargo build -q --release -p optimal-boot-bios --target x86_64-unknown-none
STAGES=target/x86_64-unknown-none/release/optimal-boot-bios

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/boot"
cp "$KERNEL" "$STAGE/boot/optimal-kernel"
{
    echo "optimal-boot-config 1"
    echo "timeout ${TIMEOUT:-0}"
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
    if [ "${SECOND_ENTRY:-0}" = 1 ]; then
        echo "entry other"
        echo "  title Other"
        echo "  kernel /boot/optimal-kernel sha512:$("$SIGN" hash "$STAGE/boot/optimal-kernel")"
    fi
} > "$STAGE/boot/optimal-boot.conf"
"$SIGN" sign "$KEYDIR/config.secret" "$STAGE/boot/optimal-boot.conf"

# The boot volume. FAT12 needs fewer than 4085 clusters, FAT32 at least
# 65525: size each so mkfs.vfat picks the asked-for type.
payload_kb=$(du -sk "$STAGE" | cut -f1)
case "$FAT" in
    12) vol_mb=$(( payload_kb / 1024 + 4 )); [ "$vol_mb" -gt 15 ] && { echo "payload too big for FAT12"; exit 1; } ;;
    16) vol_mb=$(( payload_kb / 1024 + 20 )) ;;
    32) vol_mb=$(( payload_kb / 1024 + 40 )); [ "$vol_mb" -lt 40 ] && vol_mb=40 ;;
    *) echo "FAT must be 12, 16 or 32"; exit 2 ;;
esac
vol="$STAGE/vol.img"
truncate -s "${vol_mb}M" "$vol"
case "$FAT" in
    12) mkfs.vfat -F 12 -s 8 -n OPTIMAL "$vol" >/dev/null ;;
    16) mkfs.vfat -F 16 -s 4 -n OPTIMAL "$vol" >/dev/null ;;
    32) mkfs.vfat -F 32 -s 1 -n OPTIMAL "$vol" >/dev/null ;;
esac
mcopy -s -i "$vol" "$STAGE/boot" ::/
cp "$STAGE/boot/optimal-boot.conf" "$OUT/optimal-boot-bios.conf"

rm -f "$out"
case "$LAYOUT" in
    gpt)
        truncate -s $(( vol_mb + 3 ))M "$out"
        printf 'label: gpt\nlabel-id: 0B0071A1-0000-4000-8000-00000000D15C\nstart=2048, size=2048, type=21686148-6449-6E6F-744E-656564454649, uuid=0B0071A1-0000-4000-8000-0000000B105B, name="BIOS boot"\nstart=4096, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=0B0071A1-0000-4000-8000-0000000AA27A, name="Optimal boot"\n' | sfdisk -q "$out" >/dev/null
        dd if="$vol" of="$out" bs=1M seek=2 conv=notrunc status=none
        part=2 ;;
    mbr)
        truncate -s $(( vol_mb + 2 ))M "$out"
        type=$([ "$FAT" = 32 ] && echo c || { [ "$FAT" = 16 ] && echo e || echo 1; })
        printf 'label: dos\nlabel-id: 0x0b0071a1\nstart=2048, type=%s, bootable\n' "$type" | sfdisk -q "$out" >/dev/null
        dd if="$vol" of="$out" bs=1M seek=1 conv=notrunc status=none
        part=1 ;;
    *) echo "LAYOUT must be gpt or mbr"; exit 2 ;;
esac
"$INSTALL" bios --image "$STAGES" --disk "$out" --partition "$part"
