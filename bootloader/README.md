# Optimal Boot

Optimal's own UEFI bootloader, written from the UEFI and Limine protocol
specifications. It will replace Limine once it passes the full boot matrix.

- `core/`: the pure logic (config, signatures, ELF loading, memory map, page
  tables, ACPI, KASLR), tested and fuzzed on the host.
- `loader/`: the UEFI application (coming next).

Design and security model: [DESIGN.md](DESIGN.md) (coming with the loader).
