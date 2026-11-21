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
source scripts/qemu.sh

ISO=build/optimal.iso
MARKER="OPTIMAL: kernel booted"
REPEAT="${REPEAT:-1}"
TIMEOUT="${BOOT_TIMEOUT:-20}"
LOGDIR=build/boot-logs
mkdir -p "$LOGDIR"

pass=0
fail=0
failed=()

firmwares=(bios)
if [ -n "$OVMF" ]; then firmwares+=(uefi); else echo "WARN: no OVMF, skipping UEFI"; fi

for run in $(seq 1 "$REPEAT"); do
    for fw in "${firmwares[@]}"; do
        for machine in pc q35; do
            for mem in 128M 512M 2G 8G; do
                for smp in 1 2 4 16; do
                    name="${fw}-${machine}-${mem}-smp${smp}-run${run}"
                    log="$LOGDIR/$name.log"
                    run_qemu "$ISO" "$log" "$fw" "$machine" "$mem" "$smp" "$TIMEOUT"
                    if grep -q "$MARKER" "$log" 2>/dev/null && ! grep -qE "KERNEL PANIC|EXCEPTION" "$log"; then
                        pass=$((pass + 1))
                        rm -f "$log" # keep only failing logs
                    else
                        fail=$((fail + 1))
                        failed+=("$name")
                        echo "FAIL: $name"
                        cat "$log" 2>/dev/null
                    fi
                done
            done
        done
    done
    echo "after run $run/$REPEAT: $pass passed, $fail failed"
done

echo "===== boot matrix: $pass passed, $fail failed ====="
for f in "${failed[@]}"; do echo "  failed: $f"; done
[ "$fail" -eq 0 ]
