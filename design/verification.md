# Formal verification plan

Testing shows bugs are present; it can't show they're absent. For the small
part of the kernel where one bug breaks every security promise, we also prove
properties with machine-checked proofs.

## What proofs do and don't give us

A proof shows that a specific piece of code meets a specific property, under
stated assumptions. Proving many functions correct is **not** a proof that
the kernel is secure. Bugs can still live in:

- code that isn't proved (most of the kernel, for years),
- the assumptions (hardware errata, firmware, a wrong reading of the manual),
- the gaps between proved pieces (two correct parts used together wrongly),
- the specification itself (proving the wrong property),
- the tools (compiler, proof checker, linker).

So proofs sit on top of testing, fuzzing and review, never in place of them.

We're not the first to do this. seL4 proved a whole microkernel correct, and
mainstream systems verify parts of themselves: Microsoft's Static Driver
Verifier (built on SLAM) checks Windows drivers against kernel API rules, AWS
uses formal methods on parts of its infrastructure, and Linux has a formal
memory model and research work verifying parts of its BPF verifier. What
we're aiming for is to make proofs a routine part of the core from day one.

## Assurance levels (so nothing is called "proved" that isn't)

Every target states its level, and the scorecard reports it exactly.

| Level | Meaning |
|---|---|
| **P** proved, unbounded | holds for all inputs and all sequences of operations (Verus) |
| **B** bounded | holds for all inputs up to a stated bound (Kani, with the bound recorded) |
| **M** model-checked concurrency | all interleavings of a stated small configuration explored (loom or shuttle) |
| **T** tested only | property tests and fuzzing, no proof |

A target that is level T or B today says so; it is never described as
"proved" until it reaches P (or M for concurrency properties).

## Tools and how proofs see the real code

- **Kani** (MIT/Apache-2.0): bounded model checking of real Rust functions.
  It can't handle inline assembly, threads or weak memory, and it compiles
  for the host with its own toolchain.
- **Verus** (MIT): unbounded proofs of invariants over abstract state
  machines plus refinement of the real code. Starts in **Phase 3**, first on
  the capability table (target 5), then page tables (target 1 moves from B
  to P).
- **loom / shuttle** (MIT): exhaustive or randomised interleaving checks of
  concurrent code compiled for the host. Starts in Phase 2.
- **Miri**: runs the host test suite of every tier-0 crate and reports
  undefined behaviour. Phase 1.
- Property-based tests and fuzzing check the same properties on the real
  kernel in QEMU, so a proof and a test disagreeing is caught.

**Proved code is host-compilable.** Every proved module is split into a pure
crate (no `asm!`, no target-specific `cfg`, no hardware access) containing
the logic, and a thin shim that does the hardware part (writing CR3,
`INVLPG`, MMIO). CI fails if a proved crate contains `asm!` or `cfg(target_*)`.
Each shim has a written contract (what the instruction does, from the
manual section it cites) and a QEMU test. So the proofs check the same
source that ships; the remaining gap is the compiler, which is a named axiom.

## The assumption ledger

Each proof's assumptions are listed in `design/axioms.md` (created with the
first proof). Every assumption is either:

- **discharged** by another target's guarantee (assume-guarantee: target 1
  assumes "only `core-mm` writes page tables", which target 3 and the crate
  boundary guarantee), or
- a **named axiom**: an Intel SDM or AMD APM section (4-level and 5-level
  paging walk, TLB and PCID behaviour, `INVLPG`, x86-TSO memory ordering),
  a shim contract, the IOMMU specifications, "the page-table walker reads
  each entry with a single-copy-atomic 64-bit load" (which is why the shims
  write entries through `AtomicU64`, never plain stores: Rust code follows
  the C++20 memory model, not x86-TSO, and a plain store raced by the
  hardware walker would be a data race), rustc and LLVM compile correctly,
  and each tool's own compiler front end (Kani, Verus and Miri pin their own
  rustc versions, recorded in `design/proofs.toml`) treats the code the same
  as the shipping compiler. Kani, Verus and loom are assumed sound.

A CI script fails if any proof cites an assumption that is neither discharged
nor on the axiom list.

## Keeping proofs in sync with the code

- A required **proofs** CI job runs every harness on every change. Each
  harness has a time budget (default 10 minutes); exceeding it fails, so
  proofs can't silently become too slow to run.
- `design/proofs.toml` records each harness's bounds and the number of
  `kani::assume` calls. Loosening a bound or adding an assumption changes
  that file, which needs review like any design change.
- Each proved module lists, in `proofs.toml`, the functions its harnesses
  cover by name. A change to a function in a proved module that isn't on a
  list fails CI.
- The whole proofs job has a budget of 2 hours per change; beyond that,
  slow harnesses move to the nightly run and the change waits for it.

## Proof targets

Each target is a property, not a module. A target is "done" at its stated
level when it runs in CI on every change and its assumptions are in the
ledger. **Every target, and every move to a higher level, is a done-when item
of the phase in the "gate" column**; see the [roadmap](../ROADMAP.md). Phase 1
is split into 1a (CPU setup and exceptions), 1b (memory), 1c (ACPI, PCI,
interrupts, IOMMU and time) and 1d (console).

| # | Target | Level and gate | Property |
|---|---|---|---|
| 1 | Page-table manipulation | B at 1b, P at 3 | `map`/`unmap`/`protect` only change the entries they were asked to; never create a writable+executable kernel mapping; never map one frame writable at two virtual addresses unless asked; intermediate tables stay well-formed; translation matches the hardware walk (axiom: SDM paging chapter) |
| 1s | TLB shootdown protocol | M at 2 | written as a state machine with per-CPU acknowledgement generations: a frame is returned to the allocator only after every CPU that may cache a translation for it has acknowledged an invalidation issued after the unmap. The only axiom is "an IPI that is sent is delivered and its handler runs". Plus a QEMU test that delays acknowledgements and checks the frame isn't reused |
| 2 | Physical frame allocator | B at 1b, P at 4 | a frame is never handed out twice without being freed; freed frames were allocated; never returns reserved or out-of-range memory; every frame has exactly one owner (allocator, a page table, a memory object, or a `DmaBuffer`), plus any number of recorded DMA pins on frames owned by a memory object; a pinned frame is never freed |
| 3 | Soundness of tier-0 safe APIs | B per API from 1a, M for interrupt re-entry at 2, P for the capability and memory APIs at 5 | every `pub` safe item of a tier-0 crate is sound for **arbitrary safe callers**, including ones that leak, `forget`, panic or re-enter ([core.md](core.md)). Kani harnesses drive each API with nondeterministic call sequences including `forget`; loom models interrupt re-entry as a preempting thread gated by the interrupt-flag state; compile-fail tests check that tokens and guards can't escape or cross threads; Miri runs every pure crate's tests; every `unsafe impl Send`/`Sync` is inventoried with a justification |
| 4 | Memory isolation between processes | P at 3 for the sharing paths that exist then; extended at 5 (shared memory, memfd, `SCM_RIGHTS`, `ptrace`, `process_vm_*`, io_uring buffers), 7 (dma-buf) and 8 (GPU buffers) | a user mapping in process A reaches a frame only if A holds a capability for the memory object that owns it; the sharing paths are the closed list in `proofs.toml` ([capabilities.md](capabilities.md)); kernel memory is never user-accessible |
| 5 | Capability derivation and revocation | P at 3 | derived capabilities never have more rights than their parent; revocation removes the whole subtree; a revoked handle behaves as defined in [capabilities.md](capabilities.md) |
| 6 | User memory only through the copy layer | B per entry point at 3, T across all at 3 | no syscall or driver entry point dereferences user memory except through `UserPtr` copy routines, and each user value is read once; plus a per-syscall property test that no input yields a capability or mapping the caller didn't hold |
| 7 | Resource accounting | B at 2 | memory, CPU and interrupt budgets ([drivers.md](drivers.md)) are charged and released exactly once, never go negative, and a component can't exceed its budget |
| 8 | Scheduler invariants | M at 2 | every runnable thread is on exactly one run queue; class order, real-time throttling and priority inheritance rules in [scheduler.md](scheduler.md) hold. (A thread running on two CPUs is prevented by the core's move-only `RunnableThread`, not by this policy-level proof, so a policy bug can't cause undefined behaviour) |
| 9 | Driver message boundary | B at 4 | ring code never reads a descriptor field twice; never uses a driver-supplied index without bounds-checking it; every buffer handle resolves only to a buffer the kernel granted to that driver |
| 10 | IOMMU tables and DMA ownership | B at 1c, P at 4 | a device's domain maps only frames owned by its live `DmaBuffer`s or pinned for it by a streaming map; context and domain tables stay well-formed; interrupt remapping entries only target vectors the core allocated. The invalidation-before-free order is proved at level M at 2, as part of the same state machine as target 1s, with "a queued invalidation's wait descriptor completes only after the invalidation" as the axiom |
| 11 | SMP locks and per-CPU data | M at 2 | spinlocks give mutual exclusion and are IRQ-safe as typed; per-CPU access tokens are never held across a context switch |
| 12 | Storage invariants | B at 4 | the block allocator never double-allocates; the XTS sector-to-tweak mapping is injective; a commit is issued only after the writes it covers have completed; after a driver restart, writes completed since the last flush are treated as not durable ([storage.md](storage.md)); AEAD nonces are never reused under one key (file system track). The own file system's further proofs are in `fs/docs/DESIGN.md` |
| 13 | GPU address-space isolation | B at 8 | a GPU command stream from process A can only reach GPU virtual addresses mapped in A's GPU address space, which map only buffer objects A holds a capability for |

## Not provable here, and tested instead

Lockdown (every row of the table in [threat-model.md](threat-model.md)), the
Linux ABI's behaviour, driver logic against real hardware, side-channel
resistance, and anything that depends on firmware. These have tests and
fuzzing, and are reported as level T.
