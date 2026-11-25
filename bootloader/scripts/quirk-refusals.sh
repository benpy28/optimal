#!/usr/bin/env bash
# quirk-refusals.sh
# The firmware-quirk injector must refuse, not quietly run a weaker test,
# when QUIRKS.TXT is wrong or asks for hardware the machine lacks. Each
# case boots it with one bad file and requires its named FATAL message
# and no kernel run.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
FW=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
cargo build -q --release -p optimal-boot-quirk --target x86_64-unknown-uefi
QUIRK_EFI=$PWD/target/x86_64-unknown-uefi/release/optimal-boot-quirk.efi
OB_REVISION=5 OB_EXPECT_CMDLINE="" OB_EXPECT_MODULES=0 scripts/build-conformance.sh "$OUT/conformance-refusal"
KERNEL="$OUT/conformance-refusal" scripts/build-images.sh >/dev/null
esp="$OUT/disk.img@@1M"
mmove -i "$esp" ::EFI/BOOT/BOOTX64.EFI ::EFI/BOOT/NEXT.EFI
mcopy -i "$esp" "$QUIRK_EFI" ::EFI/BOOT/BOOTX64.EFI
# contents | message that must appear
CASES=(
  "pic bogus|bad QUIRKS.TXT word \"bogus\""
  "apic-off lapic|bad QUIRKS.TXT word \"apic-off together with lapic or x2apic\""
  "# only a comment|bad QUIRKS.TXT word \"(no quirks listed)\""
  "pic vtd|quirk vtd asked for, but this machine has no such hardware"
  "pic amdvi|quirk amdvi asked for, but this machine has no such hardware"
  "acpi-dsdt-clash acpi-dsdt-in-ecam|bad QUIRKS.TXT word \"acpi-dsdt-clash together with acpi-dsdt-in-ecam\""
  "acpi-xsdt-signature acpi-rsdp-checksum|bad QUIRKS.TXT word \"acpi-xsdt-signature together with acpi-rsdp-checksum\""
  "acpi-madt-junk|ACPI quirks asked for, but the MADT needs 3 Processor Local APIC entries and one entry after them (use -smp 3 or more)"
)
status=0
for c in "${CASES[@]}"; do
    IFS='|' read -r text want <<<"$c"
    printf '%s\n' "$text" > "$OUT/QUIRKS.TXT"
    mcopy -o -i "$esp" "$OUT/QUIRKS.TXT" ::EFI/BOOT/QUIRKS.TXT
    log="$OUT/quirk-refusal.log"
    scripts/run-qemu.sh "$log" 120 -machine q35 -m 512M -drive if=pflash,format=raw,readonly=on,file="$FW" -drive format=raw,file="$OUT/disk.img"
    if grep -aqF "OPTIMAL-QUIRK: FATAL: $want" "$log" && ! grep -aq "OPTIMAL-CONFORMANCE" "$log"; then
        echo "refused \"$text\": ok"
    else
        echo "refused \"$text\": WRONG (wanted: $want)"; tail -5 "$log"; status=1
    fi
done
# No file at all.
mdel -i "$esp" ::EFI/BOOT/QUIRKS.TXT
scripts/run-qemu.sh "$OUT/quirk-refusal.log" 120 -machine q35 -m 512M -drive if=pflash,format=raw,readonly=on,file="$FW" -drive format=raw,file="$OUT/disk.img"
if grep -aqF 'OPTIMAL-QUIRK: FATAL: cannot read \EFI\BOOT\QUIRKS.TXT' "$OUT/quirk-refusal.log"; then
    echo "refused a missing QUIRKS.TXT: ok"
else
    echo "refused a missing QUIRKS.TXT: WRONG"; tail -5 "$OUT/quirk-refusal.log"; status=1
fi
exit $status
