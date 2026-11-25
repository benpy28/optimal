#!/usr/bin/env bash
# run-qemu.sh <log> <timeout-seconds> [qemu args...]
# Boots OVMF + whatever drives the args give, serial to <log>, and stops as
# soon as the kernel or the loader reports it is done.
set -uo pipefail
log="$1"; timeout="$2"; shift 2
DONE='OPTIMAL: kernel booted|KERNEL PANIC|OPTIMAL-BOOT: FATAL|OPTIMAL-QUIRK: FATAL|OPTIMAL-CONFORMANCE: (PASS|FAIL)|Access Denied|Security Violation'
rm -f "$log"
ACCEL=()
# QEMU_ACCEL=tcg forces the emulator (to test CPU features the host lacks).
if [ "${QEMU_ACCEL:-auto}" = auto ] && [ -w /dev/kvm ]; then ACCEL=(-accel kvm); fi
qemu-system-x86_64 "${ACCEL[@]}" -display none -serial "file:$log" -no-reboot "$@" >/dev/null 2>&1 &
qemu=$!
waited=0
while kill -0 "$qemu" 2>/dev/null && [ "$waited" -lt $((timeout * 10)) ]; do
    if [ -f "$log" ] && sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$log" | grep -qE "$DONE"; then
        sleep 0.5
        break
    fi
    sleep 0.1
    waited=$((waited + 1))
done
kill "$qemu" 2>/dev/null
wait "$qemu" 2>/dev/null
# Clean log: no ANSI escapes or carriage returns.
if [ -f "$log" ]; then
    sed -i 's/\x1b\[[0-9;?]*[a-zA-Z]//g; s/\r//g' "$log"
fi
exit 0
