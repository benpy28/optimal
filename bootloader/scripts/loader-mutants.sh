#!/usr/bin/env bash
# loader-mutants.sh [mutant...]
# Deliberate breaks of the loader's machine-state code. Each mutant edits
# one line, rebuilds, and boots the quirk case that must catch it under
# Optimal Boot; the script fails if any mutant survives (its case still
# passes) or no longer applies (the code it targets has moved: update it).
# The file is restored after every mutant, whatever happens.
set -uo pipefail
cd "$(dirname "$0")/.."
P=loader/src/platform.rs
B=loader/src/boot.rs
M=loader/src/mp.rs
# name # file # perl substitution # case that must fail
MUTANTS=(
  "no-pic-mask#$B#s/^\s*platform::mask_pics\(&plat\);\n//m#quirk"
  "no-ioapic-mask#$B#s/^\s*platform::mask_ioapics\(&plat, rev\);\n//m#quirk"
  "no-ioapic-mask-r3#$B#s/^\s*platform::mask_ioapics\(&plat, rev\);\n//m#quirk-r3"
  "ioapic-nmi-extint-rev6#$P#s/revision >= 5 && matches!\(mode, 4 \| 7\)/revision >= 6 \&\& matches!(mode, 4 | 7)/#quirk"
  "no-lapic-init#$B#s/^\s*platform::init_lapic\(&plat\);\n//m#quirk"
  "no-svr#$P#s/^\s*lapic\.write\(0xf0, 0x1ff\);\n//m#quirk"
  "no-tpr#$P#s/^\s*lapic\.write\(0x80, 0\);\n//m#quirk"
  "no-lvt-mask#$P#s/lapic\.write\(\*reg, v \| 1 << 16\);/lapic.write(*reg, v);/#quirk"
  "x2apic-ignored#$P#s/x2: apic_base & \(1 << 10\) != 0,/x2: false,/#quirk-x2apic"
  "no-iommu-off-intel#$B#s/^\s*platform::disable_iommus\(&plat\);\n//m#quirk-iommu-intel"
  "no-iommu-off-amd#$B#s/^\s*platform::disable_iommus\(&plat\);\n//m#quirk-iommu-amd"
  "vtd-keeps-ire#$P#s/for bit in \[TE, IRE\]/for bit in [TE]/#quirk-iommu-intel"
  "amdvi-left-on#$P#s/mmio_write64\(base \+ 0x18, ctrl & !1\);/mmio_write64(base + 0x18, ctrl);/#quirk-iommu-amd"
  "apic-trust-cpuid#$P#s/!hw::features\(\)\.apic && p\.madt\.is_none\(\)/!hw::features().apic/#quirk-apic-off"
  "no-apic-enable#$P#s/apic_base \|= 1 << 11;\n/\n/#quirk-apic-off"
  "mp-x2apic-kept#$M#s/\} else if !x2 && on \{/} else if false {/#quirk-x2apic-mp"
)
want=("$@")
backup=$(mktemp -d)
cp "$P" "$B" "$M" "$backup/"
restore() { cp "$backup/platform.rs" "$P"; cp "$backup/boot.rs" "$B"; cp "$backup/mp.rs" "$M"; }
trap 'restore; rm -rf "$backup"' EXIT
killed=0; survived=(); stale=()
for m in "${MUTANTS[@]}"; do
    IFS='#' read -r name file sub case_name <<<"$m"
    if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " == *" $name "* ]]; then continue; fi
    restore
    perl -0pi -e "$sub" "$file"
    if cmp -s "$file" "$backup/$(basename "$file")"; then
        echo "mutant $name: STALE (its substitution no longer matches $file)"
        stale+=("$name"); continue
    fi
    if scripts/conformance-case.sh "$case_name" optimal >"../build/ob/mutant-$name.out" 2>&1; then
        echo "mutant $name: SURVIVED ($case_name still passes)"
        survived+=("$name")
    else
        why=$(grep -a "^FAIL\|did not take\|no result\|needs KVM\|skipped the local APIC" "../build/ob/mutant-$name.out" | head -2 | tr '\n' ' ')
        echo "mutant $name: killed by $case_name: $why"
        if grep -aq "needs KVM\|did not take\|error\[" "../build/ob/mutant-$name.out"; then
            echo "  (but not by a check: the run could not test it)"; survived+=("$name")
        else
            killed=$((killed + 1))
        fi
    fi
done
total=$((killed + ${#survived[@]} + ${#stale[@]}))
echo "loader mutants: $killed of $total killed"
[ ${#survived[@]} -eq 0 ] && [ ${#stale[@]} -eq 0 ]
