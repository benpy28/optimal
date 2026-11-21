#!/usr/bin/env bash
# Boots build/optimal.iso in QEMU (BIOS, then UEFI if OVMF is installed) and
# passes only if the kernel prints its boot line over serial without panicking.
set -uo pipefail
cd "$(dirname "$0")/.."

ISO=build/optimal.iso
MARKER="OPTIMAL: kernel booted"
status=0

boot() {
    local name="$1"; shift
    local log="build/serial-$name.log"
    rm -f "$log"
    # The kernel halts after booting, so QEMU is stopped by the timeout.
    timeout 20 qemu-system-x86_64 -M q35 -m 512M -cdrom "$ISO" \
        -display none -serial "file:$log" -no-reboot "$@" || true
    echo "----- $name serial output -----"
    cat "$log" 2>/dev/null
    if grep -q "$MARKER" "$log" 2>/dev/null && ! grep -q "KERNEL PANIC" "$log"; then
        echo "PASS: $name"
    else
        echo "FAIL: $name"
        status=1
    fi
}

boot bios

OVMF=""
for f in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
    if [ -f "$f" ]; then OVMF="$f"; break; fi
done
if [ -n "$OVMF" ]; then
    boot uefi -drive "if=pflash,format=raw,readonly=on,file=$OVMF"
else
    echo "SKIP: uefi (no OVMF firmware found)"
fi

exit $status
