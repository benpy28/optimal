#!/usr/bin/env bash
# bios-case.sh [case...]
# Boots Optimal Boot's BIOS stage under QEMU's SeaBIOS for each case and
# checks what it reports on COM1. Until the BIOS handoff lands, a good boot
# ends with "OPTIMAL-BOOT: BIOS stage: kernel verified and loaded" followed
# by the fatal "cannot hand over to the kernel yet"; a refusal case must
# end with its own named fatal error and must NOT reach that line.
#
# Layouts and file systems:  gpt (FAT32)  gpt-fat16  gpt-fat12  mbr (FAT16)
#                            mbr-fat32  mbr-fat12
# Controllers and machines:  i440fx (PIIX IDE)  virtio  nvme  usb  scsi
# Memory and options:        tiny (128 MiB)  big (6 GiB: files above 4 GiB)
#                            kaslr-off  menu (2 entries, 1 s countdown)
# Damage and tampering:      tampered-kernel  tampered-module tampered-config
#                            missing-sig  foreign-config  damaged-stage2
#                            stage2-missing  not-installed  gpt-primary-damaged
#                            gpt-both-damaged
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
ALL="gpt gpt-fat16 gpt-fat12 mbr mbr-fat32 mbr-fat12 i440fx virtio nvme usb scsi tiny big kaslr-off menu
     tampered-kernel tampered-module tampered-config missing-sig foreign-config damaged-stage2 stage2-missing
     not-installed gpt-primary-damaged gpt-both-damaged"
cases="${*:-$ALL}"
SIGN=target/release/optimal-boot-sign
GOOD="OPTIMAL-BOOT: BIOS stage: kernel verified and loaded"
NOT_YET="cannot hand over to the kernel yet"

python3 -c "import sys; sys.stdout.buffer.write(bytes((i*167+13)&0xff for i in range(100003)))" > "$OUT/pattern"
export OB_REVISION=5 OB_EXPECT_CMDLINE="conformance case=bios" OB_EXPECT_MODULES=1 OB_EXPECT_VOLUME=gpt
kernel="$OUT/conformance-bios"
scripts/build-conformance.sh "$kernel"
cargo build -q --release -p optimal-boot-sign

# flip <file> <byte offset>: XOR one byte with 0x01.
flip() {
    python3 - "$1" "$2" <<'PY'
import sys
f = open(sys.argv[1], 'r+b'); f.seek(int(sys.argv[2])); b = f.read(1); f.seek(int(sys.argv[2])); f.write(bytes([b[0] ^ 1]))
PY
}
# The boot volume inside the image (mtools offset syntax).
vol_of() { [ "${LAYOUT:-gpt}" = mbr ] && echo "$1@@1M" || echo "$1@@2M"; }

status=0
for c in $cases; do
    img="$OUT/bios-$c.img"
    log="$OUT/bios-$c.log"
    LAYOUT=gpt FAT="" EXTRA_ENV=()
    QEMU=(-machine q35 -m 512M)
    drive=(-drive format=raw,file="$img")
    expect="$GOOD"      # the line the case must print
    want=()             # further lines it must print
    want_re=""          # an extended regex it must print
    refuse=""           # set: the case must stop with this fatal instead
    case "$c" in
        gpt) want=("Boot volume: GPT partition 2" "Files: kernel and 1 module(s) match") ;;
        gpt-fat16) FAT=16 ;;
        gpt-fat12) FAT=12 ;;
        mbr) LAYOUT=mbr; want=("Boot volume: MBR partition 1") ;;
        mbr-fat32) LAYOUT=mbr FAT=32 ;;
        mbr-fat12) LAYOUT=mbr FAT=12 ;;
        i440fx) QEMU=(-machine pc -m 512M) ;;
        virtio) drive=(-drive format=raw,file="$img",if=virtio) ;;
        nvme) drive=(-drive format=raw,file="$img",if=none,id=d0 -device nvme,drive=d0,serial=ob0) ;;
        usb) drive=(-device qemu-xhci -drive format=raw,file="$img",if=none,id=d0 -device usb-storage,drive=d0) ;;
        scsi) drive=(-device virtio-scsi-pci -drive format=raw,file="$img",if=none,id=d0 -device scsi-hd,drive=d0) ;;
        tiny) QEMU=(-machine q35 -m 128M) ;;
        # Files are allocated from the top of RAM, so with 6 GiB they land
        # above 4 GiB, past what stage 2's boot page tables map.
        big) QEMU=(-machine q35 -m 6G); want_re="kernel file at 0x[1-9a-f][0-9a-f]{8,}\)" ;;
        kaslr-off) EXTRA_ENV=(CONFIG_EXTRA="kaslr off"); want=("KASLR off") ;;
        menu) EXTRA_ENV=(TIMEOUT=1 SECOND_ENTRY=1); want=("Booting \"Optimal\" in 1 s" "Booting: Optimal") ;;
        tampered-kernel) refuse="/boot/optimal-kernel: SHA-512 does not match the signed config" ;;
        tampered-module) refuse="/boot/pattern: SHA-512 does not match the signed config" ;;
        tampered-config) refuse="config signature is invalid" ;;
        missing-sig) refuse="optimal-boot.conf.sig: file not found (the config must be signed)" ;;
        foreign-config) refuse="config is signed by a key this loader does not trust" ;;
        damaged-stage2) refuse="stage 2 is damaged: its CRC does not match" ;;
        stage2-missing) refuse="stage 1: stage 2 missing" ;;
        not-installed) refuse="stage 1: not installed" ;;
        gpt-primary-damaged) want=("WARNING: the primary GPT is damaged; using the backup copy") ;;
        gpt-both-damaged) refuse="both GPT headers are damaged" ;;
        *) echo "unknown case $c"; exit 2 ;;
    esac
    env LAYOUT="$LAYOUT" ${FAT:+FAT=$FAT} "${EXTRA_ENV[@]}" KERNEL="$kernel" CMDLINE="$OB_EXPECT_CMDLINE" \
        MODULES="$OUT/pattern" scripts/bios-image.sh "$img" >/dev/null
    vol=$(LAYOUT=$LAYOUT vol_of "$img")
    case "$c" in
        tampered-kernel)
            mcopy -n -i "$vol" ::boot/optimal-kernel "$OUT/k.tmp"; flip "$OUT/k.tmp" 4096
            mcopy -o -i "$vol" "$OUT/k.tmp" ::boot/optimal-kernel ;;
        tampered-module)
            mcopy -n -i "$vol" ::boot/pattern "$OUT/m.tmp"; flip "$OUT/m.tmp" 50000
            mcopy -o -i "$vol" "$OUT/m.tmp" ::boot/pattern ;;
        tampered-config)
            mcopy -n -i "$vol" ::boot/optimal-boot.conf "$OUT/c.tmp"; echo "# edited" >> "$OUT/c.tmp"
            mcopy -o -i "$vol" "$OUT/c.tmp" ::boot/optimal-boot.conf ;;
        missing-sig) mdel -i "$vol" ::boot/optimal-boot.conf.sig ;;
        foreign-config)
            "$SIGN" keygen "$OUT/foreign.secret" "$OUT/foreign.pub" >/dev/null
            mcopy -n -i "$vol" ::boot/optimal-boot.conf "$OUT/c.tmp"
            "$SIGN" sign "$OUT/foreign.secret" "$OUT/c.tmp"
            mcopy -o -i "$vol" "$OUT/c.tmp.sig" ::boot/optimal-boot.conf.sig ;;
        # Stage 2 is at LBA 2048 (the BIOS boot partition): change a byte of its code.
        damaged-stage2) flip "$img" $(( 2048 * 512 + 0x4000 )) ;;
        stage2-missing) flip "$img" $(( 2048 * 512 )) ;;
        not-installed) printf '\0\0' | dd of="$img" bs=1 seek=$((0x1b0)) conv=notrunc status=none ;;
        gpt-primary-damaged) flip "$img" $(( 512 + 60 )) ;;
        gpt-both-damaged)
            flip "$img" $(( 512 + 60 ))
            flip "$img" $(( $(stat -c %s "$img") - 512 + 60 )) ;;
    esac
    scripts/run-qemu.sh "$log" 300 "${QEMU[@]}" "${drive[@]}"
    ok=1
    if [ -n "$refuse" ]; then
        grep -aqF "OPTIMAL-BOOT: FATAL: " "$log" && grep -a "OPTIMAL-BOOT: FATAL: " "$log" | grep -aqF "$refuse" || ok=0
        if grep -aqF "$GOOD" "$log"; then ok=0; fi
    else
        grep -aqF "$expect" "$log" || ok=0
        grep -a "OPTIMAL-BOOT: FATAL: " "$log" | grep -aqF "$NOT_YET" || ok=0
        if [ -n "$want_re" ] && ! grep -aqE "$want_re" "$log"; then echo "  missing: /$want_re/"; ok=0; fi
        for w in "${want[@]}"; do grep -aqF "$w" "$log" || { echo "  missing: $w"; ok=0; }; done
    fi
    if [ "$ok" = 1 ]; then
        echo "bios $c: ok"
    else
        echo "bios $c: FAILED"; tail -15 "$log" | sed 's/^/  | /'
        status=1
    fi
done
exit $status
