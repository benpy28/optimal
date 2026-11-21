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

- `unsafe fn`, raw pointers, physical addresses and raw page-table entries
  never appear in a tier-0 crate's public API. They are `pub(crate)` or
  behind sealed traits.
- **The core never relies on `Drop` for safety.** If a guard's destructor
  doesn't run (it was leaked), the result may be a resource leak, never
  unsoundness. A guard that must run (for example, re-enabling interrupts)
  is a closure-scoped API (`with_irqs_disabled(|token| ...)`), not a value
  the caller could forget.
- Every review of a tier-0 API asks: is this still sound if Drop never runs,
  if the caller panics halfway, and if an interrupt arrives between any two
  instructions?

## Crate layout and mechanical enforcement

The kernel is a Cargo workspace. Tier-0 crates are the only ones allowed
`unsafe`:

| Tier-0 crate | Contents |
|---|---|
| `core-boot` | entry, early CPU setup, the boot protocol, the SMP start-up trampoline |
| `core-arch` | GDT/TSS/IDT, interrupt and syscall entry stubs, exception fixup tables, context switch, signal-frame and XSAVE save/restore, CPU feature control, MSR access, microcode loading, the APIC, I/O APIC and timer hardware |
| `core-mm` | frame allocator, page tables, kernel heap, TLB shootdown, user-copy routines |
| `core-io` | typed MMIO, port I/O, IRQ and DMA handles, PCI config access and MSI/MSI-X programming, the IOMMU, the raw TPM and ACPI table mappings |
| `core-sync` | spinlocks, sleeping locks, per-CPU data, preemption and IRQ tokens |
| `core-crypto` | the few primitives that need CPU intrinsics (AES-NI/VAES, SHA extensions, carry-less multiply), behind safe APIs |

Parsing stays out of tier 0 even when the data comes from hardware: ACPI
tables, the TPM protocol and its event log, the boot information and device
descriptors are parsed by safe crates working on byte slices that tier 0
hands them, so only the mapping is `unsafe`.

Everything else (scheduler policy, file systems, network stack, drivers)
lives in crates whose root says `#![forbid(unsafe_code)]`.

CI enforces, on every change, from Phase 1 (when the kernel is split into
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
  met by `core-crypto` or by writing the code safely.
- **Size budget.** Tier-0 code is capped at 15,000 lines per architecture
  (x86_64 now; ARM64 gets its own cap for its arch code, while
  architecture-independent tier-0 code counts once), including tier-0
  dependencies, reported on the scorecard each build. Inside it, shim code
  (every tier-0 module with `asm!`, a target-specific `cfg` or hardware
  access; [verification.md](verification.md)) is capped at 3,000 lines and
  reported separately. Exceeding either cap fails CI until the cap is raised
  by a written design change explaining why the code can't live outside the
  core.
- **Panic lints everywhere.** `unwrap_used`, `expect_used`, `panic` and
  `indexing_slicing` are denied in tier-0 crates as well as the rest (in
  tier 0 a panic is a crash), with each deliberate panic carrying an
  `#[allow]` and a reason. Integer overflow checks are on in every build
  profile, release included; their cost is measured, and code that wants
  wrapping arithmetic says so explicitly. "Parsing crates", where
  `arithmetic_side_effects` is also denied and `Untrusted<T>` is required,
  are listed in `design/crates.toml`.
- **Toolchain.** `kernel/rust-toolchain.toml` must pin an exact stable
  version (for example `1.100.0`), never the floating `stable` channel; the
  Phase 0 file still says `stable`, and the exact pin lands in Phase 1 with
  a CI check that rejects a channel name. The Limine release the build
  fetches is pinned by SHA-256 of its files, checked before use, from the
  same phase. Bumping either is its own change with the full test, proof and
  benchmark suites green. rustc and LLVM are named axioms in
  [verification.md](verification.md).
- **Dependencies.** `cargo-deny` (licences, security advisories, banned
  crates) and `cargo-vet` (every dependency audited, with a recorded reason
  for each `unsafe` it contains) gate every change. Kernel crates may not
  depend on crates with `build.rs` scripts or procedural macros unless the
  crate is vendored and audited, because both run arbitrary code at build
  time. Verus's `builtin` and `builtin_macros` crates are vendored and
  audited on this rule ([verification.md](verification.md)).

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
- **Tier 3:** a process crash, handled like any other.

## Outside audits

The unsafe core is audited by someone outside the project at the end of Phase 1
(memory management, boot, interrupts), at the end of Phase 3 (syscall entry,
user copy, capabilities), and at the end of Phase 4b (IOMMU, DMA and the
driver boundary from 4a; the TPM service and loader from 4b), then again before 1.0 (Phase 11). Findings and fixes are
published. Each audit is a done-when item of its phase; arranging one needs
money or a volunteer, which is listed as an owner need in the roadmap.
