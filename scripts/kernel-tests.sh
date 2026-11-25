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

# KASLR: the test build prints where the kernel, the direct map and the
# boot stack landed. Across the ktest boots above, each must have moved at
# least once (all equal would mean no randomisation; four boots all landing
# on the same spot by chance is vanishingly unlikely).
for what in kernel "direct map" "boot stack"; do
    seen=$(grep -h "^kaslr: kernel" "$LOGDIR"/ktest-*.log 2>/dev/null |
        sed -E "s/.*$what (0x[0-9a-f]+).*/\1/" | sort -u | wc -l)
    boots=$(grep -l "^kaslr: kernel" "$LOGDIR"/ktest-*.log 2>/dev/null | wc -l)
    name="kaslr-$what-moves"
    name="${name// /-}"
    if [ "$boots" -ge 2 ] && [ "$seen" -ge 2 ]; then
        pass=$((pass + 1))
        echo "PASS: $name ($seen different addresses in $boots boots)"
    else
        fail=$((fail + 1))
        failed+=("$name")
        echo "FAIL: $name ($seen different addresses in $boots boots)"
        grep -h "^kaslr:" "$LOGDIR"/ktest-*.log
    fi
done

echo "===== kernel tests: $pass passed, $fail failed ====="
for f in "${failed[@]}"; do echo "  failed: $f"; done
[ "$fail" -eq 0 ]
