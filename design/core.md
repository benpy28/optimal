# The core and the soundness boundary

Optimal's architecture only means something if the line between the small
`unsafe` core and the safe-Rust rest of the kernel is real and enforced by
tools, not by convention. This file defines that line.

## The rule

**Every public safe API of a tier-0 crate is sound for any safe caller.** No
sequence of safe calls, in any order, from any number of threads, including
callers that leak values, `mem::forget` guards, panic at any point or
re-enter from an interrupt, can cause undefined behaviour or break a core
invariant. Checking "every caller in the tree today is fine" is not enough,
because every new component would reopen the question.

Consequences:

- `unsafe fn`, raw pointers, physical addresses, raw page-table entries and
  the vendored `vstd` types (`PPtr`, `PAtomicU64`, `PCell`, sound only
  under Verus's permission checks) never appear in a tier-0 crate's public
  API. They are `pub(crate)` or behind sealed traits; `PPtr` is wrapped in
  a newtype private to the `core-mm` crates
  ([verification.md](verification.md)).
- **The core never relies on `Drop` for safety.** If a guard's destructor
  doesn't run (it was leaked), the result may be a resource leak, never
  unsoundness. A guard that must run (for example, re-enabling interrupts)
  is a closure-scoped API (`with_irqs_disabled(|token| ...)`), not a value
  the caller could forget.
- Every review of a tier-0 API asks: is this still sound if Drop never runs,
  if the caller panics halfway, and if an interrupt arrives between any two
  instructions? Pure-crate APIs are checked by proof target 3a, and APIs
  whose implementation calls a shim (`with_irqs_disabled`, MMIO and DMA
  handles, the context switch, `UserPtr` copies) by target 3c: compile-fail
  tests plus QEMU tests that `forget` every guard, panic at every callback
  and inject an interrupt at every IF=1 instruction boundary of the shim
  ([verification.md](verification.md)).

## Crate layout and mechanical enforcement

The kernel is a Cargo workspace. Tier-0 crates are the only ones allowed
`unsafe`:

| Tier-0 crate | Contents |
|---|---|
| `core-boot` | entry, early CPU setup, the boot protocol, the SMP start-up trampoline |
| `core-arch` | GDT/TSS/IDT, interrupt and syscall entry stubs, exception fixup tables, context switch, signal-frame and XSAVE save/restore, CPU feature control, MSR access, microcode loading, the APIC, I/O APIC and timer hardware, the text-poke shim for trace points ([verification.md](verification.md)) |
| `core-mm` | frame allocator, page tables, kernel heap, TLB shootdown, user-copy routines, the memory-access backend (the only code that turns a physical address into a reference, through the direct map; [verification.md](verification.md)), and the one API that installs user page-table entries, which takes a memory-object capability witness (proof target 4) |
| `core-io` | typed MMIO, port I/O, IRQ and DMA handles, PCI config access and MSI/MSI-X programming (tier 3 writes the other registers on a shared MSI-X page through `mmio_write(handle, offset, width, value)`, never by trapping), the IOMMU, the raw TPM and ACPI table mappings |
| `core-sync` | spinlocks, sleeping locks, per-CPU data, preemption and IRQ tokens |
| `core-crypto` | all cipher, hash, MAC, KDF, signature and random-generator code in the kernel and the loader, behind safe APIs: AES-XTS, AES-GCM and GHASH, ChaCha20-Poly1305 and XChaCha20-Poly1305, SHA-1 (TOTP only), SHA-256, SHA-512, BLAKE2b, BLAKE3 (keyed, `derive_key`, XOF; the own file system's KDF), HMAC, HKDF, PBKDF2, Argon2id, Ed25519 verification, the TPM session crypto (ECDH P-256, KDFa and KDFe, AES-128-CFB) and the kernel generator (below), each with an intrinsic backend (AES-NI/VAES, SHA extensions, carry-less multiply, AVX2/AVX-512) where one exists and a constant-time portable backend for CPUs without; verified generated code (HACL*/libcrux-derived, fiat-crypto) wherever it exists for the algorithm and instruction set; no other crate implements a primitive; proof target 14 |

Parsing stays out of tier 0 even when the data comes from hardware: ACPI
tables, the TPM protocol and its event log, the boot information and device
descriptors are parsed by safe crates working on byte slices that tier 0
hands them, so only the mapping is `unsafe`. The TPM service is such a safe
kernel crate and the only holder of `core-io`'s raw TPM access; user space
reaches the TPM only through the service's broker API.

Everything else (scheduler policy, file systems, network stack, drivers)
lives in crates whose root says `#![forbid(unsafe_code)]`.

CI enforces, on every change, from Phase 1a (when the kernel is split into
these crates; the Phase 0 kernel is one crate with only build, lint and boot
tests):

- A script checks every non-tier-0 crate root has `#![forbid(unsafe_code)]`
  and fails the build otherwise.
- Clippy lints at deny level in tier-0 crates:
  `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`,
  `missing_safety_doc`, and the compiler lint `unsafe_op_in_unsafe_fn`.
  In all other kernel crates: `unwrap_used`, `expect_used`, `panic`,
  `indexing_slicing` (use `get` and handle the error) and
  `arithmetic_side_effects` in parsing code.
- **Unsafe inventory.** A checked-in file lists every module that contains
  `unsafe`, with a hash of the **whole module** (safe code next to an
  `unsafe` block often upholds its invariants, so a change anywhere in the
  module needs re-review) and the name of its reviewer, plus every
  `unsafe impl Send` and `unsafe impl Sync` with its justification. Any change
  without an updated entry fails CI.
- **Dependencies with `unsafe` are tier 0.** A dependency containing any
  `unsafe` code (including the `limine` crate and any crypto, compression
  or hashing crate) is either counted as tier 0 (inventoried, inside the
  size cap, covered by proof target 3a, and by 3b and the shim cap for any
  module that touches hardware) or not used. Most such needs are
  met by `core-crypto` or by writing the code safely. Cryptography is
  always `core-crypto`: the loader's current `ed25519-dalek` and `sha2`
  dependencies are replaced by it before Phase 4b, so the kernel and the
  loader run one implementation with one set of tests.
- **Size budget.** Tier-0 code is capped at 15,000 lines per architecture
  (x86_64 now; ARM64 gets its own cap for its arch code, while
  architecture-independent tier-0 code counts once), including tier-0
  dependencies, reported on the scorecard each build. Inside it, shim code
  (every tier-0 module with `asm!`, a target-specific `cfg` or hardware
  access; [verification.md](verification.md)) is capped at 3,000 lines,
  and `core-crypto`, whose intrinsic backends are all shim by that rule, has
  its own cap of 7,500 lines instead; both are reported separately. The
  `core-crypto` estimate: AES with AES-NI, VAES and bitsliced portable
  backends 1,100; XTS, GCM and GHASH 700; ChaCha20, Poly1305 and XChaCha
  900; SHA-1, SHA-256 and SHA-512 550; BLAKE2b 250; BLAKE3 with portable
  and AVX2 backends 600; HMAC, HKDF and PBKDF2 150; Argon2id 350; Ed25519
  verification over fiat-crypto field arithmetic 1,300; ECDH P-256 over
  fiat-crypto field arithmetic 1,000; KDFa, KDFe and AES-CFB 100; the
  generator 250; 7,250 in all. The kernel shim
  estimate per crate, checked against the actual counts on the
  scorecard: `core-boot` 300, `core-arch` 1,120 (entry stubs, context
  switch, XSAVE and signal frames, APIC and timer, MSRs, `RDSEED`/`RDRAND`,
  text poke), `core-mm` 400 (CR3, `INVLPG`/`INVPCID`/`INVLPGB`, the
  user-copy instructions, and about 150 lines of memory-access backend),
  `core-io` 870 (MMIO and port access, PCI config, MSI-X, VT-d and AMD-Vi
  register and queue programming, the `write_dte_half` `cmpxchg16b` shim,
  TPM CRB/TIS access), `core-sync` 100; 2,790 in all, leaving 210 lines of
  headroom. Exceeding
  any cap fails CI until the cap is raised by a written design change
  explaining why the code can't live outside the core.
- **Panic lints everywhere.** `unwrap_used`, `expect_used`, `panic` and
  `indexing_slicing` are denied in tier-0 crates as well as the rest (in
  tier 0 a panic is a crash), with each deliberate panic carrying an
  `#[allow]` and a reason. Integer overflow checks are on in every build
  profile, release included; their cost is measured, and code that wants
  wrapping arithmetic says so explicitly. "Parsing crates", where
  `arithmetic_side_effects` is also denied and `Untrusted<T>` is required,
  are listed in `design/crates.toml`.
- **Toolchain.** `kernel/rust-toolchain.toml` and
  `bootloader/rust-toolchain.toml` must pin the same exact stable version
  (for example `1.100.0`), never the floating `stable` channel; both files
  still say `stable`, and the exact pin lands in Phase 1a with a CI check
  that rejects a channel name in any `rust-toolchain.toml` in the
  repository. The Limine release the build
  fetches is pinned by SHA-256 of its files, checked before use, from the
  same phase. Bumping either is its own change with the full test, proof and
  benchmark suites green. rustc and LLVM are named axioms in
  [verification.md](verification.md).
- **Dependencies.** `cargo-deny` (licences, security advisories, banned
  crates) and `cargo-vet` (every dependency audited, with a recorded reason
  for each `unsafe` it contains) gate every change. Kernel crates may not
  depend on crates with `build.rs` scripts or procedural macros unless the
  crate is vendored and audited, because both run arbitrary code at build
  time. Verus's `verus_builtin` crate, the exec subset of `vstd` the
  shipping kernel uses (`atomic`, `simple_pptr`, `cell` and their full
  module closure) and the whole procedural-macro chain (`builtin_macros`,
  `verus_syn`, `proc-macro2`, `quote`) are vendored, pinned by hash and
  audited on this rule; their ghost code erases in non-Verus builds. The
  `vstd` subset counts as a tier-0 dependency: inventoried, and its lines
  that survive erasure count in the 15,000-line cap. It is byte-identical
  to the pinned Verus `vstd`, so it is exempt from the tier-0 lints by name,
  with a review note per module hash. Only verified crates and the
  memory-access backend crate may depend on it (CI checks in
  [verification.md](verification.md)).
  Build-time-only procedural-macro crates emit code but never ship, so they
  are outside the tier-0 count and cap; in exchange, CI expands each
  verified crate in an erased build and fails if any ghost or tracked item
  is left, which tests the erasure axiom.

## The bootloader workspace

Optimal Boot (`bootloader/`) is in the trusted computing base, so every
rule above applies to it from Phase 1a, with its own numbers: tier-0 crates
(the UEFI application's protocol calls, its page tables and hand-off, the
pre-boot IOMMU deny-all programming and its TPM access) are the only ones
allowed `unsafe`, capped at 3,000 lines, of which shim code is at most
1,500; the pure crates (`optimal-boot-core`: config, signatures, ELF,
memory map, KASLR) are `#![forbid(unsafe_code)]`. `core-crypto` and the
kernel's pure IOMMU-table crate (which builds the loader's pre-boot tables,
proof target 10) are shared and counted once, in the kernel's caps. The same lints, unsafe inventory,
dependency rules and toolchain pin apply, its harnesses run in the proofs
job, and its floor code is in proof target 16's scope. Configs are signed
with Ed25519 (RFC 8032, verified by `core-crypto`). The whole loader's
size budget is in the TCB table of [threat-model.md](threat-model.md).

## Kernel random generator

Every key the system makes (recovery key, hibernation and resume keys, the
TOTP key, file-system nonce prefixes, `getrandom`) comes from one generator
in `core-crypto`, proof target 14 from Phase 3.

- **Inputs.** A BLAKE2b-512 pool absorbs the 256-bit seed Optimal Boot takes
  from `EFI_RNG_PROTOCOL`, `RDSEED` output, TSC jitter samples (timing of a
  fixed memory-access loop, taken until the credit target is reached on
  machines with no other credited source), and, uncredited, `RDRAND`,
  `TPM2_GetRandom`, interrupt timings and the seed file written at shutdown
  ([boot.md](boot.md) lists the same sources). A source is credited at the
  SP 800-90B min-entropy estimate published for it (for the EFI seed, the
  lowest estimate measured across the reference firmware), and only while
  its samples pass the SP 800-90B repetition-count and adaptive-proportion
  tests; `RDRAND` and the TPM are credited 0, and `RDSEED` is credited 0
  until the running microcode revision meets the mitigation table's
  minimum. Test (Phase 3): an emulated stuck `RDSEED` is credited 0.
- **Construction.** When 256 bits are credited, the base generator is keyed
  with 256 bits of pool output. It is ChaCha20 with fast key erasure: each
  request generates a block batch whose first 32 bytes replace the key
  before any output leaves, so a later state compromise can't recover
  earlier output. Each CPU has its own generator, rekeyed from the base
  when the base's generation number has advanced, so the hot path takes no
  lock.
- **Reseeding.** The base reseeds from the pool every 60 s, and before any
  caller can draw again after resume from suspend, a hibernation resume
  (which also mixes in 256 bits the boot kernel passes in the reserved
  hand-over page, [storage.md](storage.md)) and an ACPI VM Generation ID
  change. Each reseed bumps the generation, so every per-CPU generator
  rekeys before its next output.
- **Before the seed exists.** Kernel callers and `getrandom` block until
  256 bits are credited (`GRND_NONBLOCK` returns `EAGAIN`), and the kernel
  starts PID 1 only after that, so user space always sees a ready
  generator and `/dev/random`, `/dev/urandom` and `GRND_INSECURE` behave as
  on an initialised Linux. The TSC jitter source makes readiness reachable
  on every machine; the Phase 3 boot-matrix test publishes the time to
  readiness and fails above 2 s.
- **Fork and snapshot safety.** Generator state lives only in kernel memory.
  The vDSO `getrandom` state that user space holds is in pages that are
  wiped on fork and carries the generation number, so a forked child, a
  resumed image or a restored VM snapshot rekeys from the kernel before its
  next output.

## What a safe component can still break, and the API answers

| Hazard | Core API answer |
|---|---|
| Taking a spinlock that an interrupt handler also takes (deadlock) | lock types carry an interrupt class: `IrqSpinLock` can only be locked with an `IrqDisabled` token; plain `SpinLock` can't be used from interrupt context (checked by a context token on handler entry) |
| Holding per-CPU data across preemption, migration or a sleep | per-CPU data is only reachable through a `PreemptDisabled` token. Tokens are `!Send`, `!Sync`, not `Copy`, and branded with a unique closure-scoped lifetime, so they can't escape the closure that created them (compile-fail tests check it). Because a type can't express "no token is held", every sleeping API also checks the preempt count at run time and crashes on a violation |
| Running one thread on two CPUs | the context-switch API only accepts a move-only `RunnableThread`, and switching to it atomically claims the thread's on-CPU flag; a second claim is a crash, never undefined behaviour. So a bug in the safe scheduler policy can hurt fairness, not memory safety |
| Lock ordering deadlocks | locks declare a level; acquiring out of order panics in debug, test and soak builds (a lockdep-style checker). Phase 2 done-when: zero lock-order reports in the hour-long stress run |
| DMA to arbitrary memory | drivers never see physical or bus addresses; see [drivers.md](drivers.md) |
| User pointers | `UserPtr<T>` can never be dereferenced, only copied in or out through the core's copy routines, which run with SMAP briefly lifted and recover from faults through an exception table. Each user value is copied once and validated on the copy, so a second thread changing it can't cause a check-then-use bug |
| Spectre v1 on attacker-chosen indices | attacker-influenced indices come in as `Untrusted<usize>`; the only way to index with one is a masking accessor ([threat-model.md](threat-model.md)) |
| Stack overflow | every kernel stack has a guard page; the double-fault handler runs on its own IST stack, so an overflow produces a crash report instead of a triple fault |
| Panics | see the panic policy below |
| Allocation failure | the kernel's allocation APIs are fallible (`try_new`, `try_reserve`); the infallible ones are unavailable outside the core, so a component can't abort the kernel by running out of memory |

## Panic policy

Rust's panic strategy is one per binary, and unwinding in a `no_std` kernel
needs unstable compiler features. Optimal doesn't unwind. The kernel is built
`panic = "abort"` on stable Rust, and the single panic handler decides what a
panic means:

- **Tier 0 and tier 1:** a kernel crash: crash report on serial, framebuffer
  and the persistent crash log, then reboot. These tiers are kept small and
  tested hardest because of this.
- **Tier 2:** every tier-2 component runs only on its own kernel threads, and
  each thread keeps a count of how deep it is inside core calls. If a tier-2
  thread panics (or overflows its stack, caught by its guard page and the
  IST double-fault handler) **while outside the core** (count 0), the
  handler doesn't return to it: it marks the component poisoned, takes the
  thread off every run queue for good, restores the CPU's preempt count and
  interrupt state, and runs the stop sequence in [drivers.md](drivers.md).
  No destructors run, which is safe because the core never relies on `Drop`
  for safety. The component's memory is reclaimed by freeing its whole
  arena; its sleeping locks are marked poisoned so any waiter gets an error;
  it can't hold spinlocks. If the count is above 0 (the panic happened in
  core code on that thread), the core's state may be half-updated, so it is
  a kernel crash.
- **Tier-2 watchdog.** A looping tier-2 thread is stopped without a panic:
  a forced-preemption IPI parks it at the interrupt return if its depth
  count is 0, and otherwise sets a park-on-core-exit flag, so it is parked
  when its outermost core call returns and never inside the core. Core
  calls a tier-2 thread can make never wait on driver-controlled state
  without a timeout. A tier-2 thread holding preemption off for 10 ms is a
  stop event for its component, not a crash. The stop sequence and its
  fault-injection tests are in [drivers.md](drivers.md) (Phase 4a).
- **Tier 3:** a process crash, handled like any other.

## Outside audits

The unsafe core is audited by someone outside the project at the end of Phase 1
(memory management, boot, interrupts), at the end of Phase 3 (syscall entry,
user copy, capabilities), at the end of Phase 4a (the IOMMU, DMA and the
driver boundary), at the end of Phase 4b (the TPM service, the loader and the
core's changes since Phase 3), then again before 1.0 (Phase 11). Findings and fixes are
published. Each audit is a done-when item of its phase; arranging one needs
money or a volunteer, which is listed as an owner need in the roadmap.
