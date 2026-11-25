# Shared QEMU helpers, sourced by the test scripts.

OVMF=""
for f in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
    if [ -f "$f" ]; then OVMF="$f"; break; fi
done

ACCEL=()
if [ -w /dev/kvm ]; then ACCEL=(-accel kvm -cpu host); fi

# Lines that mean the kernel has finished (it halts afterwards).
# The firmware/bootloader refusal lines end Secure Boot tests early.
DONE_PATTERN='OPTIMAL: kernel booted|OPTIMAL: system halted|KERNEL PANIC|OPTIMAL: ktest .* passed|KTEST FAIL|Access Denied|PANIC|hash .* does not match|CHECKSUM MISMATCH'

# Serial log as plain text: terminal escape codes and carriage returns removed.
clean_log() {
    sed 's/\x1b\[[0-9;=?]*[A-Za-z]//g' "$1" 2>/dev/null | tr -d '\r'
}

# hang_report <monitor socket> <log>: ask QEMU where the CPU is, and save a
# screenshot as <log>.ppm (uploaded with the failure logs).
hang_report() {
    python3 - "$1" "$2" <<'PY'
import socket, sys, time
sock, log = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
s.connect(sock)
s.settimeout(2)
def cmd(c):
    s.sendall(c.encode() + b"\n")
    time.sleep(1)
    out = b""
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            out += chunk
    except socket.timeout:
        pass
    return out.decode(errors="replace")
cmd("")
for c in ("info status", "info registers", "screendump " + log + ".ppm"):
    out = cmd(c).replace("\r", "")
    # Keep what QEMU printed, minus its line-editing noise.
    lines = [l for l in out.split("\n") if l.strip() and "\x1b" not in l and not l.startswith("(qemu)")]
    print("$ " + c)
    print("\n".join(lines[:40]))
PY
}

# Optional knobs, set by the caller before run_qemu (all default to empty):
#   QEMU_EXTRA=(...)   extra QEMU arguments (CPU model, NUMA layout, ...)
#   QEMU_ACCEL=(...)   replaces the accelerator, e.g. (-accel tcg) for a slow CPU
#   QEMU_WRAP=(...)    command prefix, e.g. (taskset -c 0) to squeeze every vCPU
#                      onto one host core
QEMU_EXTRA=()
QEMU_WRAP=()
QEMU_ACCEL=("${ACCEL[@]}")
OVMF_SECBOOT=/usr/share/OVMF/OVMF_CODE_4M.secboot.fd

# run_qemu <iso or .img> <log> <firmware> <machine> <mem> <smp> <timeout seconds>
# Firmware is one of:
#   bios              SeaBIOS, QEMU's default
#   uefi              OVMF from the distro
#   uefi-sb           OVMF with Secure Boot on and our test keys (build/sb/VARS.fd)
#   ovmf=<code.fd>    a specific OVMF build (older and newer firmware), with
#                     the OVMF_VARS.fd beside it if there is one
# Boots the ISO with serial output in <log>, and stops QEMU as soon as the
# kernel reports it is done, or when the timeout runs out.
run_qemu() {
    local iso="$1" log="$2" fw="$3" machine="$4" mem="$5" smp="$6" timeout="$7"
    local fw_args=()
    case "$fw" in
        bios) ;;
        uefi) fw_args=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF") ;;
        ovmf=*)
            # A specific OVMF build, with a fresh copy of its own variable
            # store (OVMF_VARS.fd next to the code) when it has one.
            local code="${fw#ovmf=}"
            fw_args=(-drive "if=pflash,format=raw,unit=0,readonly=on,file=$code")
            if [ -f "$(dirname "$code")/OVMF_VARS.fd" ]; then
                cp "$(dirname "$code")/OVMF_VARS.fd" "$log.vars"
                fw_args+=(-drive "if=pflash,format=raw,unit=1,file=$log.vars")
            fi
            ;;
        uefi-sb)
            # Secure Boot needs SMM so the OS can't rewrite the key store.
            machine="$machine,smm=on"
            cp build/sb/VARS.fd "$log.vars"
            fw_args=(-global driver=cfi.pflash01,property=secure,value=on
                -drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_SECBOOT"
                -drive "if=pflash,format=raw,unit=1,file=$log.vars")
            ;;
        *) echo "run_qemu: unknown firmware $fw" >&2; return 1 ;;
    esac
    rm -f "$log"
    # A .img is a disk (scripts/quirk-image.sh), booted from a throwaway
    # overlay so a run can never change it; anything else is a CD.
    local media=(-cdrom "$iso")
    [[ "$iso" == *.img ]] && media=(-drive "format=raw,file=$iso,snapshot=on")
    "${QEMU_WRAP[@]}" qemu-system-x86_64 "${QEMU_ACCEL[@]}" -M "$machine" -m "$mem" -smp "$smp" \
        "${media[@]}" -display none -serial "file:$log" -no-reboot "${fw_args[@]}" \
        -monitor "unix:$log.mon,server,nowait" -d guest_errors,cpu_reset \
        "${QEMU_EXTRA[@]}" >"$log.qemu" 2>&1 &
    local qemu=$!
    local waited=0 done=no
    while kill -0 "$qemu" 2>/dev/null && [ "$waited" -lt $((timeout * 10)) ]; do
        if clean_log "$log" | grep -qaE "$DONE_PATTERN"; then
            sleep 0.5 # let the rest of the report land
            done=yes
            break
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    if [ "$done" = no ] && kill -0 "$qemu" 2>/dev/null; then
        # Hung or silent: record where the CPU is and what the screen shows,
        # so a failure comes with evidence (firmware, bootloader or kernel?).
        hang_report "$log.mon" "$log" >"$log.hang" 2>&1
    fi
    kill "$qemu" 2>/dev/null
    wait "$qemu" 2>/dev/null
    # Firmware and Limine draw text with terminal escape codes, one character
    # at a time; store plain text so every check can grep it.
    clean_log "$log" >"$log.txt" && mv "$log.txt" "$log"
    # Keep QEMU's own complaints (bad option, unsupported feature) with the log.
    if [ -s "$log.qemu" ]; then
        { echo "--- qemu stderr ---"; cat "$log.qemu"; } >>"$log"
    fi
    if [ -s "$log.hang" ]; then
        { echo "--- hang report (no finish line within ${timeout}s) ---"; cat "$log.hang"; } >>"$log"
    fi
    rm -f "$log.qemu" "$log.vars" "$log.mon" "$log.hang"
    return 0
}
