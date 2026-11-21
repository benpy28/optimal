#!/usr/bin/env bash
# Runs the in-kernel test suite and every deliberate crash test, on BIOS and
# UEFI, and on 1 and 4 CPU cores. Fails on any test failure or wrong report.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/qemu.sh
source scripts/crashtests.sh

TIMEOUT="${BOOT_TIMEOUT:-30}"
LOGDIR=build/test-logs
mkdir -p "$LOGDIR"
pass=0
fail=0
failed=()

firmwares=(bios)
if [ -n "$OVMF" ]; then firmwares+=(uefi); fi

check() {
    local name="$1" log="$2" ok="$3"
    if [ "$ok" = yes ]; then
        pass=$((pass + 1))
        echo "PASS: $name"
    else
        fail=$((fail + 1))
        failed+=("$name")
        echo "FAIL: $name"
        cat "$log" 2>/dev/null
    fi
}

for fw in "${firmwares[@]}"; do
    for smp in 1 4; do
        # In-kernel test suite: needs the zero-failure summary line.
        name="ktest-$fw-smp$smp"
        log="$LOGDIR/$name.log"
        run_qemu build/optimal-ktest.iso "$log" "$fw" q35 512M "$smp" "$TIMEOUT"
        ok=no
        if grep -qE "OPTIMAL: ktest [0-9]+ passed, 0 failed" "$log" && ! grep -qE "KTEST FAIL|KERNEL PANIC|EXCEPTION" "$log"; then
            ok=yes
        fi
        check "$name" "$log" "$ok"
        grep "KTEST PASS" "$log" 2>/dev/null | sed 's/^/    /'

        # Crash tests: the report must name the right fault and the kernel must halt.
        for test in "${!CRASHTESTS[@]}"; do
            name="crash-$test-$fw-smp$smp"
            log="$LOGDIR/$name.log"
            run_qemu "build/crash-$test.iso" "$log" "$fw" q35 512M "$smp" "$TIMEOUT"
            ok=no
            if grep -qF "${CRASHTESTS[$test]}" "$log" 2>/dev/null && ! grep -q "did NOT fault" "$log"; then
                ok=yes
            fi
            check "$name" "$log" "$ok"
        done
    done
done

echo "===== kernel tests: $pass passed, $fail failed ====="
for f in "${failed[@]}"; do echo "  failed: $f"; done
[ "$fail" -eq 0 ]
