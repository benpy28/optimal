#!/usr/bin/env bash
# build-conformance.sh <out-file>
# Builds the conformance kernel with the OB_* knobs from the environment:
#   OB_REVISION=3..6   base revision to request (default 3)
#   OB_PAGING=5        ask for 5-level paging
#   OB_ENTRY_REQ=1     enter through the Entry Point request
#   OB_PIE=1           position independent, linked in the lower half
#   OB_EXPECT_CMDLINE  the exact command line the kernel must receive
#   OB_EXPECT_MODULES  the number of modules it must receive
set -euo pipefail
out="$1"
cd "$(dirname "$0")/.."
if [ "${OB_PIE:-0}" = 1 ]; then
    export CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" -C relocation-model=pie"
fi
log=$(mktemp)
if ! cargo build --release -p optimal-boot-conformance --target x86_64-unknown-none --target-dir target/conformance-${OB_PIE:-0} >"$log" 2>&1; then
    cat "$log"; rm -f "$log"; exit 1
fi
grep -E "^warning" -A5 "$log" || true
rm -f "$log"
mkdir -p "$(dirname "$out")"
cp target/conformance-${OB_PIE:-0}/x86_64-unknown-none/release/conformance "$out"
