# Optimal Boot

Optimal's own UEFI bootloader, written from the UEFI and Limine protocol
specifications. It will replace Limine once it passes the full boot matrix.

- `core/`: the pure logic (config, signatures, ELF loading, memory map, page
  tables, ACPI, KASLR), tested and fuzzed on the host.
- `loader/`: the UEFI application (`optimal-boot.efi`).
- `sign/`: `optimal-boot-sign`, which makes keys and signs the boot config.
- `conformance/`: a test kernel that checks every promise of the Limine
  protocol handoff (and Optimal Boot's stronger ones: no writable+executable
  page, a non-executable HHDM, only the kernel executable). CI boots it under
  Optimal Boot and under Limine for each case in `scripts/conformance-case.sh`.

Run one case locally (needs QEMU, OVMF, xorriso, mtools, dosfstools, fdisk,
swtpm, and a Limine binary release in `../build/limine`):

    scripts/conformance-case.sh r5

Design and security model: [DESIGN.md](DESIGN.md) (coming with the loader).
