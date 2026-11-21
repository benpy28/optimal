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
| `core-boot` | entry, early CPU setup, the boot protocol |
| `core-arch` | GDT/TSS/IDT, interrupt and syscall entry stubs, context switch, CPU feature control, MSR access, the APIC, I/O APIC and the timer hardware |
| `core-mm` | frame allocator, page tables, kernel heap, TLB shootdown, user-copy routines |
| `core-io` | typed MMIO, port I/O, IRQ and DMA handles, the IOMMU, the TPM driver |
| `core-sync` | spinlocks, per-CPU data, preemption and IRQ tokens |

Everything else (scheduler policy, file systems, network stack, drivers)
lives in crates whose root says `#![forbid(unsafe_code)]`.

CI enforces, on every change:

- A script checks every non-tier-0 crate root has `#![forbid(unsafe_code)]`
  and fails the build otherwise.
- Clippy lints at deny level in tier-0 crates:
  `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`,
  `missing_safety_doc`, and the compiler lint `unsafe_op_in_unsafe_fn`.
  In all other kernel crates: `unwrap_used`, `expect_used`, `panic`,
  `indexing_slicing` (use `get` and handle the error) and
  `arithmetic_side_effects` in parsing code.
- **Unsafe inventory.** A checked-in file lists every `unsafe` block with a
  hash of its text and the name of its reviewer. Any changed or new block
  without an updated entry fails CI, so no unsafe change slips in unreviewed.
- **Size budget.** Tier-0 crates are capped at 15,000 lines of code in
  total, reported on the scorecard each build. Exceeding the cap fails CI
  until the cap is raised by a written design change explaining why the code
  can't live outside the core.
- **Toolchain.** `rust-toolchain.toml` pins an exact stable version (for
  example `1.100.0`), never the floating `stable` channel. Bumping it is its
  own change with the full test, proof and benchmark suites green. rustc and
  LLVM are named axioms in [verification.md](verification.md).
- **Dependencies.** `cargo-deny` (licences, security advisories, banned
  crates) and `cargo-vet` (every dependency audited, with a recorded reason
  for each `unsafe` it contains) gate every change. Kernel crates may not
  depend on crates with `build.rs` scripts or procedural macros unless the
  crate is vendored and audited, because both run arbitrary code at build
  time.

## What a safe component can still break, and the API answers

| Hazard | Core API answer |
|---|---|
| Taking a spinlock that an interrupt handler also takes (deadlock) | lock types carry an interrupt class: `IrqSpinLock` can only be locked with an `IrqDisabled` token; plain `SpinLock` can't be used from interrupt context (checked by a context token on handler entry) |
| Holding per-CPU data across preemption or migration | per-CPU data is only reachable through a `PreemptDisabled` token, scoped by a closure |
| Lock ordering deadlocks | locks declare a level; acquiring out of order panics in debug, test and soak builds (a lockdep-style checker). Phase 2 done-when: zero lock-order reports in the hour-long stress run |
| DMA to arbitrary memory | drivers never see physical or bus addresses; see [drivers.md](drivers.md) |
| User pointers | `UserPtr<T>` can never be dereferenced, only copied in or out through the core's copy routines, which run with SMAP briefly lifted and recover from faults through an exception table. Each user value is copied once and validated on the copy, so a second thread changing it can't cause a check-then-use bug |
| Spectre v1 on attacker-chosen indices | attacker-influenced indices come in as `Untrusted<usize>`; the only way to index with one is a masking accessor ([threat-model.md](threat-model.md)) |
| Stack overflow | every kernel stack has a guard page; the double-fault handler runs on its own IST stack, so an overflow produces a crash report instead of a triple fault |
| Panics | see the panic policy below |
| Allocation failure | the kernel's allocation APIs are fallible (`try_new`, `try_reserve`); the infallible ones are unavailable outside the core, so a component can't abort the kernel by running out of memory |

## Panic policy

- **Tier 0 and tier 1:** a panic is a kernel crash: crash report on serial,
  framebuffer and (later) the persistent crash log, then reboot. These tiers
  are kept small and heavily tested precisely because of this.
- **Tier 2:** the kernel is built with unwinding enabled for tier-2
  components only. A tier-2 component runs on its own kernel threads; a
  panic unwinds to its entry point, where the core marks the component
  poisoned, revokes everything it holds (see restart in
  [drivers.md](drivers.md)) and optionally restarts it. A tier-2 component
  may not hold core spinlocks (it gets only sleeping locks, which the core
  can recover), so unwinding can't leave a core lock held.
- **Tier 3:** a panic is a process crash, handled like any other.
- The cost of unwind tables is measured (kernel image size and any runtime
  cost) and published.

## Outside audits

The unsafe core is audited by someone outside the project at the end of Phase 1
(memory management, boot, interrupts), at the end of Phase 3 (syscall entry,
user copy, capabilities) and at the end of Phase 4 (IOMMU, DMA, the driver
boundary, TPM), then again before 1.0 (Phase 11). Findings and fixes are
published. Each audit is a done-when item of its phase; arranging one needs
money or a volunteer, which is listed as an owner need in the roadmap.
