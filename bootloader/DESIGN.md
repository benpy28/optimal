# Optimal Boot: design

Optimal Boot is Optimal's own UEFI bootloader. This file covers its
internals: the boot sequence, its file formats, what it measures, the state
it hands to the kernel, how it is tested, and how each planned boot-chain
mechanism will be built inside it. The whole-system requirements it has to
meet (the chain, the TPM policy, updates, recovery) are in
[design/boot.md](../design/boot.md), and the threats in
[design/threat-model.md](../design/threat-model.md). Where this file and
boot.md disagree, boot.md sets the requirement and this file is wrong.

Every mechanism below has a status:

- **Done**: in the loader and tested in CI.
- **Planned**: designed here, not built yet.

Rules for the code:

- Clean room, written from the UEFI 2.10, ACPI 6.5, TCG PC Client and Limine
  protocol specifications. No code from other bootloaders.
- No GPL code. Crypto comes from audited RustCrypto and dalek crates
  (MIT, Apache-2.0 or BSD-3-Clause).
- Stable Rust only.
- Everything that parses untrusted bytes lives in `core/`, with no firmware
  calls, so it can be host tested and fuzzed.

## Boot sequence (done)

1. Gather entropy. In order: `EFI_RNG_PROTOCOL` (credited 256 bits), then
   `RDSEED` and `RDRAND` (8 draws each, credited 32 bits a draw), then TSC
   jitter around `Stall(1)` (mixed in, credited 0). Everything goes into a
   SHA-512 hash DRBG (`core/src/rng.rs`), so one good source is enough and a
   bad source cannot weaken the others.
2. Read `SecureBoot` and `SetupMode`, and report them.
3. Read `/boot/optimal-boot.conf` (at most 64 KiB) and
   `/boot/optimal-boot.conf.sig` from the loader's own volume. Verify the
   signature against the keys compiled into the loader. There is no build
   or runtime option that skips this check.
4. Parse the config strictly. Unknown statements, duplicates and bad values
   are errors, never warnings.
5. Measure the config into PCR 8.
6. Pick the entry: the default, or the menu if `timeout` is above 0. The
   menu chooses between signed entries only. It cannot edit a command line.
7. Measure the chosen entry's id and command line into PCR 8.
8. Read the kernel and every module. Compare each one's SHA-512 with the
   config, in constant time, and measure it into PCR 9.
9. Parse the kernel ELF, scan its Limine requests, check the base revision
   (3 to 6) and the paging mode, then pick KASLR placements. Load and
   relocate the kernel.
10. Build the page tables, the memory map and every response. Take the
    final memory map and call `ExitBootServices`, retrying up to 16 times if
    the map key goes stale.
11. Set the machine state (next section) and leave through the trampoline.

Any failure stops the boot with `OPTIMAL-BOOT: FATAL: <reason>` and returns
to the firmware, so it can try the next boot option. Nothing unverified ever
runs.

## File formats (done)

**Config** (`core/src/config.rs`). Printable ASCII, one statement per line,
`#` starts a comment. The first statement must be `optimal-boot-config 1`.

| Limit | Value |
|---|---|
| Config size | 64 KiB |
| Entries | 16 |
| Modules per entry | 16 |
| Path length | 255 bytes |
| Command line length | 4096 bytes |
| Title length | 64 bytes |
| Entry id length | 32 bytes |
| Timeout | 60 s |

Paths are absolute, plain ASCII, and may not contain `..`. Every `kernel` and
`module` line carries `sha512:<128 hex digits>`.

**Signature** (`core/src/sig.rs`). The file is 104 bytes:

| Bytes | Contents |
|---|---|
| 0..8 | magic `OPTBSIG1` |
| 8..40 | key id: SHA-256 of the 32-byte Ed25519 public key |
| 40..104 | Ed25519 signature over `"Optimal Boot config signature v1\0" ‖ SHA-512(config)` |

The domain prefix stops a signature made for anything else from being
replayed as a config signature.

**Trusted keys.** These are 1 to 8 Ed25519 public keys, compiled in from
`OPTIMAL_BOOT_PUBKEYS` at build time. Changing them means a new, signed
loader binary.

## Measurements (done, with SVN planned)

| PCR | Event (EV_IPL, SHA-256 bank required) | Order |
|---|---|---|
| 8 | the loader's SVN (planned, see below) | first |
| 8 | the whole config file | 2 |
| 8 | `optimal-boot entry <id> cmdline <cmdline>` | 3 |
| 9 | the kernel file, then each module file in config order | after PCR 8 |

The firmware measures the loader binary itself into PCR 4, and the Secure
Boot state and certificates into PCR 7.

If the SHA-256 bank is off, or a measurement fails, the loader boots with a
warning: sealed secrets will not unlock, so failing safe is automatic. If
there is no TPM, it boots unmeasured and says so. CI (`tpm` case) replays
the event log against the PCRs, and checks that the PCR 9 kernel digest is
the SHA-256 of the kernel.

## Handoff (done)

The loader speaks the Limine protocol, base revisions 3 to 6, and answers
these requests:

- Bootloader info, firmware type, executable command line, executable file
  and executable address.
- Stack size, HHDM, paging mode (4 or 5 levels), entry point and memory map.
- Framebuffer, modules, RSDP, SMBIOS, EFI system table, EFI memory map, TPM
  event log, date at boot, bootloader performance, TSC frequency, entropy,
  keep IOMMU, and MP (see "Multiprocessor start" below).

**Memory protection beyond the spec:**

- No page is ever both writable and executable.
- The HHDM is non-executable.
- The only executable page outside the kernel is one read-only handoff page
  at `kernel_virt_base - 4 KiB`, which the trampoline leaves through and
  where the parked APs wait.

**KASLR:**

- The virtual base comes from at least 512 equally likely 2 MiB slots for
  PIE kernels.
- The physical placement is random.
- The HHDM base is random.

**Machine state at entry:**

- RFLAGS is exactly `0x2`.
- PICs masked. On the I/O APICs, Fixed and Lowest Priority pins are masked,
  plus NMI and ExtINT pins from revision 5.
- From revision 5, the local APIC is enabled with SVR `0x1FF`, TPR 0, its
  LVTs masked and its LINT NMIs wired from the MADT. Every VT-d unit has
  translation and interrupt remapping off, and AMD-Vi is off, unless the
  kernel asks to keep the IOMMU.

The local APIC counts as present if CPUID says so **or** a MADT exists,
because CPUID's APIC bit mirrors the global enable that firmware may have
cleared. A re-enable that does not stick (some CPUs need a reset) means the
APIC is not available, and the loader leaves it alone.

## How it is tested (done)

- **Host tests** of `core/`, in debug (overflow checks on) and release:
  - config, ELF and relocations
  - memory map and page tables
  - Limine request scanning
  - ACPI
  - KASLR ranges and the DRBG

  Seeded random tests cover the config parser, the ELF parser and
  relocator, the request scanner, memory maps and page tables.
- **Conformance kernel** (`conformance/`). This kernel checks every handoff
  promise from the hardware's side: page-table walks, W^X probes caught by
  its own IDT, PIC, I/O APIC, local APIC and IOMMU registers, ACPI
  placement, the TPM log, the TSC frequency against the PIT, modules and the
  command line.

  Every CI case boots it under Optimal Boot **and** under Limine v11.4.1,
  the reference. A check that fails only under ours is our bug. One that
  fails under Limine is a bug in the check, or a Limine bug confirmed
  against the spec.

  Under Limine only spec promises are checked. Optimal Boot's stronger
  promises are the `ob-` checks.
- **Firmware-quirk injector** (`quirk/`). OVMF is tidier than real firmware,
  so this test-only EFI app boots first. Through a hooked ExitBootServices,
  it leaves the machine the way sloppy firmware would:
  - PICs and every I/O APIC pin unmasked
  - odd TPR, SVR and LVTs
  - x2APIC on, or the APIC globally off
  - VT-d and AMD-Vi on

  Then it chain-loads the loader under test. A run only counts if the
  injector's report shows every requested quirk took.
- **Secure Boot and tamper tests** (`scripts/secureboot-case.sh`). These run
  on OVMF's Secure Boot build, with throwaway PK, KEK and db keys made fresh
  on each run:
  - The db-signed loader boots, and reports Secure Boot on.
  - With swtpm, PCR 8 and 9 as the loader reads them back equal a replay
    computed on the host from the config, entry, kernel and module bytes.
  - The firmware refuses (`Access Denied`, and our banner never appears):
    - an unsigned loader
    - a loader signed by an untrusted key
    - a signed loader with one byte changed
    - a signed loader whose Authenticode hash is in dbx
  - The loader refuses, each with its own message:
    - a changed kernel or module
    - an edited config
    - a config signed by a key it does not trust
    - a missing signature
- **Mutants** (`scripts/loader-mutants.sh`). There are 15 one-line breaks of
  the machine-state code, and each must make its quirk case fail.
- **Known Limine divergence.** With the APIC globally disabled by firmware,
  Limine v11.4.1 leaves it disabled (SVR `0xF`). Revision 5 requires it
  enabled with SVR `0x1FF`, so the `quirk-apic-off` case runs under Optimal
  Boot only.

## Planned mechanisms

The eight items below are the loader's share of boot.md. Each lists its
format, its order and its test. The tests are written before the code, and
each must fail before its code lands.

### 1. Merkle root of the system image on the command line

- The system image's root hash, geometry (block size, data size, tree
  offset) and salt go on the entry's command line, as
  `optimal.image=sha256:<64 hex>,bs=4096,size=<bytes>,tree=<offset>,salt=<64 hex>`.
  The command line is inside the signed config, so the value is both
  verified (the signature) and measured (PCR 8 event 3). The loader needs no
  code of its own for this. The kernel parses the value and checks the
  blocks.
- One loader rule is added: an entry whose `kernel` has the
  `requires-image` flag must have exactly one `optimal.image=` on its
  command line, or the config is refused at parse time. This stops a signed
  config that forgets the root from booting an unverified image.
- Tests: a host test refuses a config missing the root or with two roots. A
  boot with one changed hex digit of the root fails the signature check. A
  boot of a correctly signed config whose image has a flipped block is
  refused by the kernel (Phase 4 test).

### 2. Security version number and anti-rollback

- **Config:** the global statement `svn <n>` (a u64) is already parsed, but
  nothing enforces or measures it yet. It becomes required from config
  format 2.
- **Loader:** a built-in `MIN_CONFIG_SVN` is compiled in next to the trusted
  keys. The loader also measures its own SVN (`LOADER_SVN`) as the first
  PCR 8 event: `optimal-boot svn loader=<n> config-key=<key id>`.
- **Floor:**
  - B1: a TPM NV counter, with its index and policy as in boot.md.
  - B2 (no TPM): an authenticated, boot-services-only UEFI variable
    `OptimalBootFloor`. If the variable is missing or corrupt, the floor is
    the highest SVN among the configs on the ESP.
  - After a TPM clear, the floor is
    `max(MIN_CONFIG_SVN, highest SVN present)`.
- **Rule:** a config whose `svn` is below the floor is refused before
  anything is read. A config at or above the floor boots.
- **Raises:** the loader never raises the floor. User space does, after a
  health check, exactly as boot.md orders it.
- **Key revocation:** a revoked config-signing key is removed from the
  trusted set by a loader SVN bump. Its key id is in PCR 8, so the TPM
  policy also sees which key signed.
- **Tests (swtpm):**
  - A config below the floor is refused.
  - A config at the floor boots.
  - After a TPM clear, an old config below `MIN_CONFIG_SVN` is refused.
  - The B2 variable missing gives the highest-present rule.
  - A config signed by a revoked key is refused.
  - A PCR 8 replay shows the SVN event first.

### 3. A/B slots, a 3-try counter and a recovery entry

- **State** lives in a non-volatile UEFI variable `OptimalBootState` (32
  bytes, CRC-32 protected): `magic`, `version`, `slot_a_tries`,
  `slot_b_tries`, `slot_a_good`, `slot_b_good`, `active`, `last_booted`. It
  is readable and writable at runtime, so user space can mark a slot good.
  It is not trusted for security: a forged state can only pick between
  signed, above-floor slots.
- **Config:** entries gain `slot a|b|recovery`. There is exactly one
  `recovery` entry, a signed read-only recovery system.
- **Order:**
  1. Read the state. If it is missing or bad, use slot A with 3 tries.
  2. If the active slot is not good, decrement its tries and write the
     state **before** reading any file.
  3. If its tries were already 0, switch to the other slot. If both are at
     0, boot the recovery entry.
  4. If the write fails, boot the current slot and report it on the console
     and in the handoff, never loop.
- **Loader updates** use their own trial: two loader directories, two
  `Boot####` entries, and `\EFI\BOOT\BOOTX64.EFI` as the fallback. A new
  loader is first started through `BootNext`. `BootOrder` changes only
  after user space marks it good.
- **Tests:**
  - A kernel that hangs before user space is back on the old slot within 3
    boots, with no key press.
  - Both slots bad boots the recovery entry.
  - A full variable store boots the current slot and reports it.
  - A power cut during the state write is safe (QEMU `-no-reboot` with the
    write interrupted).
  - A loader that hangs after `ExitBootServices` is back on the old loader
    on the next boot.

### 4. PIN entry page

- When the config's entry has `unlock pin`, the loader asks for the PIN on
  the UEFI text console, before `ExitBootServices`. It reads
  `SimpleTextInput` keys (this works with USB keyboards through the
  firmware), echoes `*`, and allows 4 to 64 characters.
- The PIN goes into one dedicated 4 KiB page of `LOADER_DATA`. A new
  Optimal-specific request hands its address and length to the kernel. The
  kernel zeroes the page after use.
- Every other copy (the key buffer, the line buffer) is zeroed with volatile
  writes before handoff. The page is not in the HHDM's executable set and
  is mapped non-executable.
- The PIN is never measured, logged or printed.
- **Tests:**
  - The conformance kernel finds exactly one copy of a test PIN in memory:
    in the PIN page, and nowhere else in the memory map's usable or
    bootloader-reclaimable memory.
  - The swtpm log holds no PIN bytes.
  - A PIN longer than 64 characters is refused before handoff.

### 5. Microcode module

- **Config:** a module line with the string `microcode` marks the vendor
  microcode blob. It is hash pinned like every module and measured into
  PCR 9 before the kernel's other modules. The loader itself never applies
  microcode: the kernel does it on every CPU, as boot.md says.
- The loader refuses a config with two `microcode` modules.
- **Tests:**
  - A host test refuses two microcode modules.
  - The conformance kernel checks that a `microcode` module arrives first
    in the module response, byte exact.

### 6. Release keys, TUF style

- The config-signing key is a delegated role under the offline root, as
  boot.md describes. The loader's part is to accept configs during a
  rotation.
- Signature format 2 (`OPTBSIG2`) holds 1 to 4 signatures, each as
  `key id ‖ Ed25519 signature`. It verifies if at least one is by a trusted,
  unrevoked key. Signatures by unknown keys are skipped, and malformed
  entries refuse the whole file.
- The rotation order is the one in boot.md:
  1. Re-sign the other slot and the recovery entry.
  2. Update the loader.
  3. Raise the floor.
- **Tests:**
  - A dual-signed config boots on a loader trusting either key.
  - A config signed only by a revoked key is refused.
  - A malformed second signature refuses the file.
  - A power cut at each step of the order leaves a bootable machine.

### 7. Secure Boot paths, shim and owner keys, with dbx and SBAT tests

- **Binary:**
  - The loader gets a `.sbat` PE section (`sbat,1`, then
    `optimal-boot,<generation>,...`).
  - It sets the `NX_COMPAT` DLL characteristic.
  - Every section is either writable or executable, never both. A CI check
    reads the PE headers to enforce this.
- **Under shim:** the loader is started by shim, which verifies it against
  the Optimal certificate. Shim's protocol is used to verify nothing else,
  because the loader checks the kernel and modules itself through the
  signed config.
- **Owner keys:** the loader is signed directly by an owner db key.
- **The platform hierarchy:** if firmware left the TPM platform hierarchy
  enabled, the loader disables it (`TPM2_HierarchyControl`) before
  `ExitBootServices`.
- **Pre-boot DMA:** with the IOMMU kept, the loader sets deny-all
  translation tables before `ExitBootServices`. The only exceptions are the
  RMRRs and the firmware framebuffer, read-only. The order is:
  1. All firmware I/O.
  2. The deny-all tables.
  3. `ExitBootServices`.
  4. Re-check and re-arm, because EDK2's VT-d driver turns translation off
     in its own ExitBootServices handler.
- **Tests (OVMF secure-boot builds with enrolled variable stores):**
  - Done, with owner-style test keys (see the testing section): the signed
    loader boots; an unsigned, wrongly signed, tampered or dbx-listed loader
    is refused; a tampered kernel, module or config, a foreign config key
    and a missing signature are refused.
  - Planned: under Microsoft keys with shim, the signed loader boots.
  - Planned: an SBAT generation bump stops an old loader (path 1).
  - The VT-d DXE driver test shows translation still on at handoff when
    the kernel asked to keep the IOMMU.

### 8. Fuzz every parser

These parsers read untrusted bytes:

- config and signature
- ELF and relocations
- Limine request scanning
- ACPI: RSDP, XSDT, MADT, DMAR, IVRS, FADT
- the TCG event log
- the firmware memory map
- device paths, in the loader and the quirk injector

All of them live in `core/`. Device paths came last: the loader and the
quirk injector now measure a firmware path node header by node header with
`devpath::measure` (bounded at 4 KiB) and hand the measured bytes to
`core/src/devpath.rs`.

Coverage-guided fuzzers (libFuzzer, AFL) need a nightly toolchain or
sanitizer runtimes, which the stable-only rule rules out for CI gating. So
the gate is seeded structured mutation on stable:

- Each parser gets a generator for valid inputs and a mutator: byte flips,
  truncation, length-field lies, out-of-range offsets, duplicates.
- Each parser is checked against a property: it never panics, and every
  accepted input satisfies the parser's invariants (a re-check or
  re-serialisation round trip).
- Each PR runs 10^5 cases per parser. The nightly soak runs 10^8 with a new
  base seed each night, and prints the seed on failure. Signature cases sign
  and verify (about 100 times the cost of a case of the other parsers), so
  they run a tenth of that: 10^4 per PR, 10^7 nightly.
- The runs use the `fuzz` build profile: release speed with overflow checks
  and debug assertions on, so arithmetic that would wrap silently in the
  shipped loader panics and is caught.
- A crash found anywhere becomes a fixed regression case.

**Today:** config, ELF, request scanning, memory maps and page tables have
seeded random tests. The signature, ACPI, TCG log and device-path parsers
have the generator, mutator and property above (`core/src/fuzzkit.rs`). For
ACPI, TCG logs and device paths the generator also gives the answer a
correct parser must report, and the valid-input test checks it exactly.
Measured locally with the `fuzz` profile, 10^6 cases take 1 to 15 seconds
per parser test, one thread each. At that rate the slowest test needs about
25 minutes for 10^8 cases; the tests run in parallel, and the nightly job
has a 300-minute limit.

What this work found, each now a fixed regression test:

- ACPI: a table placed at the top of the 64-bit address space with a length
  running past 2^64 made `addr + offset` wrap. A debug build panicked; the
  release loader would have read low memory as if it were the table (for
  example as DMAR remapping units). Every range is now checked to fit before
  it is read.
- ACPI: a FACS was accepted on its length field alone, so a stray FADT
  pointer made random memory a range the kernel must keep mapped. The
  `FACS` signature is now required.
- TCG log: the header was not checked against the PC Client Platform
  Firmware Profile. A first event other than PCR 0 / EV_NO_ACTION, a
  duplicate algorithm, a digest size of 0, a size that contradicts the TCG
  Algorithm Registry for a known algorithm (SHA-256 declared as 20 bytes),
  or vendor info running past the event is now refused, since every later
  event is sized from this header. Event offsets near `usize::MAX` no longer
  wrap.
- Device paths (found while moving the walker into `core/`, then pinned by
  a test): the quirk injector copied every instance of a multi-instance
  path before appending the next loader's file node, giving a path that
  names the wrong file. Both users now stop at the first End node, End
  Instance included, and the whole-disk prefix is the path before the first
  media node.

## Boot media (done)

Conformance cases `cd`, `usb`, `nvme` and `virtio` boot from each kind of
medium QEMU's q35 offers besides SATA (`small` and the rest use SATA), under
ours and under Limine. `ob-boot-volume` checks what the kernel is told about
the volume. These are QEMU's emulated controllers; real hardware is not
covered yet.

## Multiprocessor start (done)

With the MP request present, the loader starts every enabled processor in
the MADT and parks it for the kernel, as the Limine protocol describes.

- **Which processors:** enabled MADT entries (types 0 and 9) in table order,
  each APIC id once. Online-capable ones are left for the OS. In xAPIC mode
  ids above 254 are left out, because an xAPIC IPI cannot name them. The BSP
  is always reported; if the MADT does not list it, it comes first with UID
  `u32::MAX`. The logic is `core::mp::select`, unit tested and fuzzed
  against a model.
- **x2APIC:** on when the kernel asks and the CPU has it. If the firmware
  left it on and the kernel did not ask, it is turned off. If that is
  impossible (the platform locked it, or an APIC id needs it), the boot is
  refused before leaving the firmware, as the spec requires. Without the MP
  request the APIC mode is left as the firmware set it.
- **Start-up:** one AP at a time after ExitBootServices: INIT, 10 ms,
  start-up IPI, 200 us, a second start-up IPI if it has not run yet (Intel
  SDM Vol. 3A 8.4.4.1). That is about 10 ms per AP, so a 64-CPU machine
  spends about 0.65 s here. The start page is a loader-code page below
  1 MiB. Its real-mode code goes to long mode on the firmware's page tables
  (a below-4 GiB copy of the top level if needed). Then Rust code on the AP
  sets the APIC mode, copies the BSP's MTRRs (SDM 11.11.7.2 and 11.11.8),
  and from base revision 5 sets up the local APIC exactly as on the BSP.
  Finally it runs the handoff trampoline with a Block of its own: the final
  page tables, CR0, CR4, EFER, PAT, its own GDT (a loaded TSS descriptor is
  marked busy, so it cannot be shared), and the IDTR, LDTR and TR state.
- **Parking:** the AP ends in a loop in the handoff page. It stores its
  stack pointer in `reserved` (this tells the BSP it is parked) and waits
  for `goto_address`. It reloads CR3 before jumping, to drop TLB entries for
  the identity pages the BSP has removed since. Each AP gets a stack of the
  requested size in bootloader-reclaimable memory, with a 0 return address,
  like the BSP. The BSP removes the identity mappings only after every AP
  has parked.
- **Failures:** an AP that does not park within 5 s is sent INIT again,
  which stops it, and is left out of the response. The loader prints how
  many were left out. If there is no memory below 1 MiB, only the BSP is
  reported, with a printed warning.
- **Tests:** the conformance kernel releases each AP in turn and checks
  its RDI, stack, segments, CR0, CR3, CR4, EFER, PAT, RFLAGS, GDT, IDTR,
  LDTR and TR (base revision 5 and later), APIC mode and id, LAPIC state,
  MTRRs and `extra_argument`. It also checks the response against its own
  reading of the MADT. Cases: `smp` (8 CPUs, base revision 5), `smp-r3`
  (4 CPUs, base revision 3), and `smp-x2apic` (8 CPUs asking for x2APIC;
  needs KVM). Every other case has one CPU and checks a BSP-only response.
  All run under ours and under Limine.
- **Not covered yet:** more than 255 CPUs (x2APIC-only ids), firmware with
  x2APIC locked, and real hardware.

## Not yet: BIOS

- A BIOS stage is next. Until it lands, BIOS machines boot with Limine,
  which is boot tier B4 in boot.md.
