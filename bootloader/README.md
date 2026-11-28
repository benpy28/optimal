# Optimal Boot

Optimal's own UEFI bootloader, written from the UEFI and Limine protocol
specifications. It will replace Limine once it passes the full boot matrix.

- `core/`: the pure logic (config, signatures, ELF loading, memory map, page
  tables, ACPI, KASLR), tested and fuzzed on the host.
- `loader/`: the UEFI application (`optimal-boot.efi`).
- `bios/`: the BIOS stage (MBR boot sector and stage 2), and `install/`,
  `optimal-boot-install`, which puts it on a disk. Part 1 reaches a
  verified, loaded kernel; the handoff is next.
- `sign/`: `optimal-boot-sign`, which makes keys and signs the boot config.
- `conformance/`: a test kernel that checks every promise of the Limine
  protocol handoff (and Optimal Boot's stronger ones: no writable+executable
  page, a non-executable HHDM, only the kernel executable). CI boots it under
  Optimal Boot and under Limine for each case in `scripts/conformance-case.sh`.
- `quirk/`: a test-only EFI app that boots first and leaves the machine the
  way sloppy firmware would (PICs and I/O APIC pins unmasked, odd local APIC
  state, x2APIC on or the APIC disabled, VT-d and AMD-Vi IOMMUs on), then
  chain-loads the loader under test. The `quirk-*` cases use it, and
  `scripts/loader-mutants.sh` checks that every deliberate break of the
  loader's machine-state code is caught by one of them.

Run one case locally (needs QEMU, OVMF, xorriso, mtools, dosfstools, fdisk,
swtpm, and a Limine binary release in `../build/limine`):

    scripts/conformance-case.sh r5

Design and security model: [DESIGN.md](DESIGN.md).
