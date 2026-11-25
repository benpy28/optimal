#!/usr/bin/env python3
"""Writes the extra ACPI tables the tough-conditions quirky-ACPI scenarios
hand to QEMU with -acpitable file=... (QEMU appends each to the root table
after its own tables and fixes its checksum).

Usage: acpi-quirk-tables.py <output directory>

  madt-dup.bin     a second MADT, as some firmware lists one twice, full of
                   junk: a duplicate APIC ID, the broadcast ID 0xff and a
                   zero-length entry. The kernel must use the first MADT and
                   say there were two.
  odd-<n>.bin      tables with signatures nobody knows, odd lengths and
                   revision 0, which must be listed and otherwise ignored.
  ssdt-<n>.bin     40 small SSDTs: a long root table.
  dmar-x2apic-opt-out.bin  a DMAR whose only flag is X2APIC_OPT_OUT.
  ssdt-big.bin     a 65535-byte SSDT (the most QEMU allows): 16 pages (on SeaBIOS
                   every page goes through the kernel's read window).
"""
import os
import struct
import sys


def table(sig: bytes, body: bytes, rev: int = 1) -> bytes:
    t = bytearray(sig + struct.pack("<IBB", 36 + len(body), rev, 0))
    t += b"OPTIML" + b"QUIRKTBL" + struct.pack("<I", 1) + b"OPTM" + struct.pack("<I", 1)
    t += body
    t[9] = (-sum(t)) & 0xFF
    return bytes(t)


def main() -> None:
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)

    def write(name: str, data: bytes) -> None:
        with open(os.path.join(out, name), "wb") as f:
            f.write(data)

    # MADT body: local APIC address, flags (PCAT_COMPAT), then entries.
    madt = struct.pack("<II", 0xFEE00000, 1)
    madt += bytes([0, 8, 0, 0, 1, 0, 0, 0])  # CPU UID 0, APIC ID 0
    madt += bytes([0, 8, 1, 0, 1, 0, 0, 0])  # CPU UID 1, APIC ID 0 again
    madt += bytes([0, 8, 2, 0xFF, 1, 0, 0, 0])  # APIC ID 0xff (broadcast)
    madt += bytes([7, 0])  # zero-length entry
    write("madt-dup.bin", table(b"APIC", madt, rev=3))
    for i, (sig, n) in enumerate([(b"OPTQ", 3), (b"ZZZZ", 0), (b"\x01\x02ab", 77)]):
        write(f"odd-{i}.bin", table(sig, bytes(range(n)), rev=0))
    # An AML DefinitionBlock with an empty body: valid, does nothing.
    for i in range(40):
        write(f"ssdt-{i}.bin", table(b"SSDT", b"", rev=2))
    # 65535 bytes in all, filled with AML Noop (0xa3) opcodes.
    write("ssdt-big.bin", table(b"SSDT", b"\xa3" * (65535 - 36), rev=2))
    # DMAR with no remapping units: width 39 bits (stored 38), flags
    # X2APIC_OPT_OUT only. QEMU's own intel-iommu DMAR never sets that flag.
    write("dmar-x2apic-opt-out.bin", table(b"DMAR", bytes([38, 0b10]) + bytes(10), rev=1))


main()
