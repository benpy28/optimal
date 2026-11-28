#!/usr/bin/env bash
# bios-mutants.sh [mutant...]
# Deliberate breaks of the BIOS stage. Each mutant makes one literal edit,
# rebuilds, and runs the bios-case.sh case that must catch it; the script
# fails if any mutant survives (its case still passes), no longer applies
# (the code moved: update the mutant), or breaks the build (that proves
# nothing about the tests). Files are restored after every mutant.
#
# Not here: removing the E820 attribute preset in mem.rs. It only matters
# on firmware that reports 24-byte entries but writes 20, and SeaBIOS
# writes all 24, so no case can catch it (core's e820::parse tests cover
# the 20-byte path).
set -uo pipefail
cd "$(dirname "$0")/.."
B=bios/src/boot.rs M=bios/src/mem.rs D=bios/src/disk.rs N=bios/src/menu.rs
# name | file | text to find | replacement | case that must fail
MUTANTS=(
  "no-self-crc|$B|if c.finish() != h.crc {|if false {|damaged-stage2"
  "no-file-hash|$B|if !sig::ct_eq(&sig::sha512(&pages.bytes()[..size as usize]), want) {|if false {|tampered-kernel"
  "sig-ignored|$B|sig::verify(cfg_bytes, sig_bytes, &TRUSTED_KEYS)|Ok::<usize, sig::SigError>(0)|tampered-config"
  "no-backup-warning|$B|if f.used_backup {|if false {|gpt-primary-damaged"
  "volume-one-short|$B|f.part.end_lba - f.part.start_lba + 1,|f.part.end_lba - f.part.start_lba,|gpt"
  "alloc-low-memory|$M|const LOW_LIMIT: u64 = 0x10_0000;|const LOW_LIMIT: u64 = 0;|gpt"
  "no-map-above-4g|$M|m.map(&mut src, start, start, end - start, rwx)|Ok::<(), optimal_boot_core::paging::MapError>(())|big"
  "bounce-offset|$D|core::slice::from_raw_parts(BOUNCE as *const u8, bytes)|core::slice::from_raw_parts((BOUNCE + 512) as *const u8, bytes)|gpt"
  "no-menu|$N|if cfg.timeout == 0 \\|\\| n == 0 {|if true {|menu"
)
want=("$@")
backup=$(mktemp -d)
for f in $B $M $D $N; do cp "$f" "$backup/$(basename "$f")"; done
restore() { for f in $B $M $D $N; do cp "$backup/$(basename "$f")" "$f"; done; }
trap 'restore; rm -rf "$backup"' EXIT
killed=0; survived=(); stale=()
for m in "${MUTANTS[@]}"; do
    IFS='|' read -r name file from to case_name <<<"$(printf '%s' "$m" | sed 's/\\|/\x01/g')"
    from=${from//$'\x01'/|}; to=${to//$'\x01'/|}
    if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " == *" $name "* ]]; then continue; fi
    restore
    if ! FROM="$from" TO="${to//\\&/&}" python3 - "$file" <<'PY'
import os, sys
p = sys.argv[1]; s = open(p).read(); a = os.environ["FROM"]
if s.count(a) != 1: sys.exit(1)
open(p, "w").write(s.replace(a, os.environ["TO"]))
PY
    then
        echo "mutant $name: STALE (its text is not in $file exactly once)"
        stale+=("$name"); continue
    fi
    out="../build/ob/bios-mutant-$name.out"
    if scripts/bios-case.sh "$case_name" >"$out" 2>&1; then
        echo "mutant $name: SURVIVED ($case_name still passes)"
        survived+=("$name")
    elif grep -aq "^error\(\[E[0-9]*\]\)\?:" "$out"; then
        echo "mutant $name: does not build, so it proves nothing (fix the mutant)"
        survived+=("$name")
    else
        echo "mutant $name: killed by $case_name"
        killed=$((killed + 1))
    fi
done
total=$((killed + ${#survived[@]} + ${#stale[@]}))
echo "BIOS stage mutants: $killed of $total killed"
[ ${#survived[@]} -eq 0 ] && [ ${#stale[@]} -eq 0 ]
