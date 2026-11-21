#!/usr/bin/env bash
# Boots build/optimal.iso in QEMU across a matrix of virtual PCs and passes only
# if every boot prints the kernel's boot line without panicking.
#
# Matrix: firmware (BIOS, UEFI) x chipset (old i440fx "pc", modern "q35")
#         x RAM (128 MiB .. 8 GiB) x CPU cores (1 .. 16).
#
# REPEAT=N boots every configuration N times (the nightly soak uses a big N to
# catch rare, timing-dependent failures). Default 1.
set -uo pipefail
cd "$(dirname "$0")/.."

ISO=build/optimal.iso
MARKER="OPTIMAL: kernel booted"
REPEAT="${REPEAT:-1}"
TIMEOUT="${BOOT_TIMEOUT:-20}"
LOGDIR=build/boot-logs
mkdir -p "$LOGDIR"

OVMF=""
for f in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/ovmf/OVMF.fd; do
    if [ -f "$f" ]; then OVMF="$f"; break; fi
done

ACCEL=()
if [ -w /dev/kvm ]; then ACCEL=(-accel kvm -cpu host); fi

pass=0
fail=0
failed=()

boot_once() {
    local fw="$1" machine="$2" mem="$3" smp="$4" run="$5"
    local name="${fw}-${machine}-${mem}-smp${smp}-run${run}"
    local log="$LOGDIR/$name.log"
    local fw_args=()
    if [ "$fw" = uefi ]; then
        fw_args=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF")
    fi
    rm -f "$log"
    qemu-system-x86_64 "${ACCEL[@]}" -M "$machine" -m "$mem" -smp "$smp" \
        -cdrom "$ISO" -display none -serial "file:$log" -no-reboot "${fw_args[@]}" \
        >/dev/null 2>&1 &
    local qemu=$!
    # The kernel halts once booted, so stop QEMU as soon as the boot line (or
    # a panic) shows up, or when the timeout runs out.
    local waited=0
    while kill -0 "$qemu" 2>/dev/null && [ "$waited" -lt $((TIMEOUT * 10)) ]; do
        if grep -q -e "$MARKER" -e "KERNEL PANIC" "$log" 2>/dev/null; then
            sleep 0.3 # let any trailing output land
            break
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    kill "$qemu" 2>/dev/null
    wait "$qemu" 2>/dev/null
    if grep -q "$MARKER" "$log" 2>/dev/null && ! grep -q "KERNEL PANIC" "$log"; then
        pass=$((pass + 1))
        rm -f "$log" # keep only failing logs
    else
        fail=$((fail + 1))
        failed+=("$name")
        echo "FAIL: $name"
        cat "$log" 2>/dev/null
    fi
}

firmwares=(bios)
if [ -n "$OVMF" ]; then firmwares+=(uefi); else echo "WARN: no OVMF, skipping UEFI"; fi

for run in $(seq 1 "$REPEAT"); do
    for fw in "${firmwares[@]}"; do
        for machine in pc q35; do
            for mem in 128M 512M 2G 8G; do
                for smp in 1 2 4 16; do
                    boot_once "$fw" "$machine" "$mem" "$smp" "$run"
                done
            done
        done
    done
    echo "after run $run/$REPEAT: $pass passed, $fail failed"
done

echo "===== boot matrix: $pass passed, $fail failed ====="
for f in "${failed[@]}"; do echo "  failed: $f"; done
[ "$fail" -eq 0 ]
