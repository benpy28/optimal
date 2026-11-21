# Shared QEMU helpers, sourced by the test scripts.

OVMF=""
for f in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
    if [ -f "$f" ]; then OVMF="$f"; break; fi
done

ACCEL=()
if [ -w /dev/kvm ]; then ACCEL=(-accel kvm -cpu host); fi

# Lines that mean the kernel has finished (it halts afterwards).
DONE_PATTERN='OPTIMAL: kernel booted|OPTIMAL: system halted|KERNEL PANIC|OPTIMAL: ktest .* passed|KTEST FAIL'

# run_qemu <iso> <log> <firmware: bios|uefi> <machine> <mem> <smp> <timeout seconds>
# Boots the ISO with serial output in <log>, and stops QEMU as soon as the
# kernel reports it is done, or when the timeout runs out.
run_qemu() {
    local iso="$1" log="$2" fw="$3" machine="$4" mem="$5" smp="$6" timeout="$7"
    local fw_args=()
    if [ "$fw" = uefi ]; then
        fw_args=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF")
    fi
    rm -f "$log"
    qemu-system-x86_64 "${ACCEL[@]}" -M "$machine" -m "$mem" -smp "$smp" \
        -cdrom "$iso" -display none -serial "file:$log" -no-reboot "${fw_args[@]}" \
        >/dev/null 2>&1 &
    local qemu=$!
    local waited=0
    while kill -0 "$qemu" 2>/dev/null && [ "$waited" -lt $((timeout * 10)) ]; do
        if grep -qE "$DONE_PATTERN" "$log" 2>/dev/null; then
            sleep 0.5 # let the rest of the report land
            break
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    kill "$qemu" 2>/dev/null
    wait "$qemu" 2>/dev/null
    return 0
}
