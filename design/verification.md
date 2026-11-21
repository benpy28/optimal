# Formal verification plan

Testing shows bugs are present; it can't show they're absent. For the small
part of the kernel where one bug breaks every security promise, we also check
properties with proofs and bounded checks, each at a stated level.

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
| **M** model-checked concurrency | all interleavings of a stated small configuration explored up to a recorded preemption bound (loom) |
| **T** tested only | property tests, fuzzing and randomised schedules (shuttle), no proof |

A target that is level T or B today says so; it is never described as
"proved" until it reaches P (or M for concurrency properties).

**Effective level.** A target's effective level is the weakest of its own
level and the effective levels of every target that discharges one of its
assumptions: target 1 at P, assuming a target-3a guarantee that is at B, is
effective B. P ranks above B and M, which both rank above T; a mix of B and
M is reported as "B+M". The ledger script in CI computes it from `proofs.toml` and
`axioms.md`, and the scorecard reports both the own and the effective level.

## Tools and how proofs see the real code

- **Kani** (MIT/Apache-2.0): bounded model checking of real Rust functions.
  It can't handle inline assembly, threads or weak memory, and it compiles
  for the host with its own toolchain.
- **Verus** (MIT): unbounded proofs of invariants over abstract state
  machines plus refinement of the real code. Starts in **Phase 3**, first on
  the capability table (target 5), then page tables (target 1 moves from B
  to P). Verus needs its `builtin`, `builtin_macros` and `vstd` crates;
  they are vendored and audited under the procedural-macro rule in
  [core.md](core.md). Spec and proof code is
  erased at compile time, and CI rejects any executable `vstd` item reached
  from a shipped crate. Verus's own rustc version is recorded in
  `proofs.toml`.
- **loom** (MIT): exhaustive interleaving checks of concurrent code compiled
  for the host, with the preemption bound of each model recorded in
  `proofs.toml`; this is level M. Data shared with interrupt handlers is
  declared through loom's cell and atomic types under `cfg(loom)`, so loom
  sees every access. **shuttle** (Apache-2.0) explores randomised
  schedules of larger configurations; that is level T. Both start in Phase 2.
- **Miri**: runs the host test suite of every pure tier-0 crate and reports
  undefined behaviour. Phase 1.
- Property-based tests and fuzzing check the same properties on the real
  kernel in QEMU, so a proof and a test disagreeing is caught.

**Proved code is host-compilable.** Every proved module is split into a pure
crate (no `asm!`, no target-specific `cfg`, no hardware access) containing
the logic, and a thin shim that does the hardware part (writing CR3,
`INVLPG`, MMIO). CI fails if a proved crate contains `asm!` or `cfg(target_*)`.
Every tier-0 module with `asm!`, a target-specific `cfg` or hardware access
is a shim. Each shim function has a written contract (what the instruction
does, from the manual section it cites), which is a named axiom in the
ledger; its soundness is target 3b (QEMU test per function plus the unsafe
review). Shim code is capped at 3,000 lines inside the 15,000-line tier-0
cap ([core.md](core.md)) and both counts are on the scorecard. So the proofs
check the same source that ships; the remaining gap is the compiler and the
shims.

## The assumption ledger

Each proof's assumptions are listed in `design/axioms.md` (created with the
first proof). Every assumption is either:

- **discharged** by another target's guarantee (assume-guarantee: target 1
  assumes "only `core-mm` writes page tables", which target 3a and the crate
  boundary guarantee), or
- a **named axiom**: an Intel SDM or AMD APM section (4-level and 5-level
  paging walk, TLB and PCID behaviour, `INVLPG`, x86-TSO memory ordering),
  a shim contract, the IOMMU specifications, "the page-table walker reads
  each entry with a single-copy-atomic 64-bit load" (which is why the shims
  write entries through `AtomicU64`, never plain stores: Rust code follows
  the C++20 memory model, not x86-TSO, and a plain store raced by the
  hardware walker would be a data race), "the walker may set the Accessed
  and Dirty bits of any present entry at any time with a locked
  read-modify-write" (SDM Vol. 3A §4.8, which is why present entries change
  only through `swap`, `compare_exchange` or `fetch_and`, never load then
  store, so a Dirty bit the walker set can't be lost), rustc and LLVM compile correctly,
  and each tool's own compiler front end (Kani, Verus and Miri pin their own
  rustc versions, recorded in `design/proofs.toml`) treats the code the same
  as the shipping compiler. Kani, Verus and loom are assumed sound.

A CI script fails if any proof cites an assumption that is neither discharged
nor on the axiom list.

The ledger also lists what each tool does not check, so the scorecard can't
imply more: Kani, paths beyond each loop's unwinding bound, concurrency and
inline assembly; loom, interleavings beyond the recorded preemption bound
and `Relaxed` behaviours it doesn't generate (load buffering); Verus, every
`external_body` and `assume_specification` item (each also a named axiom);
Miri, paths the tests don't execute and aliasing rules beyond its current
model; shuttle, every schedule it didn't sample.

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
  Nightly-only harnesses are labelled as such in `proofs.toml` and on the
  scorecard.
- **No vacuous proofs.** Every harness has `kani::cover!` statements for the
  states its property is about (a full table, a split huge page, a revoked
  subtree); an unreachable cover fails CI, so an over-strong `kani::assume`
  can't make a harness pass by excluding everything. Each target also lists
  seeded bug injections in `proofs.toml` (known-wrong changes to its code,
  such as dropping a bounds check); the nightly run applies them one at a
  time, and each must make at least one of the target's harnesses or tests
  fail.

## Proof targets

Each target is a property, not a module. A target is "done" at its stated
level when it runs in CI on every change and its assumptions are in the
ledger. **Every target, and every move to a higher level, is a done-when item
of the phase in the "gate" column**, named there by target number and level;
see the [roadmap](../ROADMAP.md). A CI script checks that this table and the
roadmap's proof-gate lines name the same targets, levels and phases. Phase 1
is split into 1a (CPU setup and exceptions), 1b (memory), 1c (ACPI, PCI,
interrupts, IOMMU and time) and 1d (console); Phase 4 into 4a (storage) and
4b (trusted boot chain).

| # | Target | Level and gate | Property |
|---|---|---|---|
| 1 | Page-table manipulation | B at 1b, P at 3 | `map`/`unmap`/`protect` only change the entries they were asked to; present entries change only by `swap`/`compare_exchange`/`fetch_and`, and the Dirty bit of a replaced or write-protected entry is moved to its page before the entry is gone (the Kani harness runs a nondeterministic walker that may set A/D bits between any two steps; writeback of a page starts only after the shootdown of its write-protect completes); outside the direct map, never create a writable+executable kernel mapping and never map one frame writable at two virtual addresses unless asked; the direct map is no-execute, supervisor-only and never reachable through an entry with the user bit set; intermediate tables stay well-formed; translation matches the hardware walk (axiom: SDM paging chapter). Plus a QEMU test that writes through a file mapping while its page is under writeback and checks no write is lost (gate 4a) |
| 1s | TLB shootdown protocol | M at 2; re-gated at M at 3 (PCIDs and user address spaces) and at 5 (INVLPGB and transparent huge pages) | written as a state machine with per-CPU acknowledgement generations: a frame, including a freed page-table page, is returned to the allocator only after every CPU that may cache a translation or paging-structure entry for it has completed an invalidation issued after the unmap. "May cache" follows the PCID and ASID allocation in [scheduler.md](scheduler.md): a CPU may cache every address space whose PCID it has loaded since that PCID's last full flush on that CPU, and a PCID or global ASID is reassigned only after a full flush. Axioms: an IPI that is sent is delivered and its handler runs; `INVLPG` and `INVPCID` semantics and `MOV CR3` with bit 63 set not flushing the new PCID (SDM Vol. 3A §4.10.4); paging-structure caches are invalidated by the same operations as leaf entries (§4.10.3); a mapping whose page size changes is invalidated before reuse (§4.10.2.3); `INVLPGB` followed by `TLBSYNC` completes the invalidation on every CPU (AMD APM Vol. 3). Plus a QEMU test that delays acknowledgements and checks the frame isn't reused |
| 2 | Physical frame allocator | B at 1b, P at 4a | a frame is never handed out twice without being freed; freed frames were allocated; never returns reserved or out-of-range memory; every frame has exactly one owner (allocator, a page table, a memory object, or a `DmaBuffer`), plus any number of recorded DMA pins on frames owned by a memory object; a pinned frame is never freed |
| 3a | Soundness of tier-0 safe APIs (pure crates) | B per API from 1a, M for interrupt re-entry at 2, P for the capability and memory APIs at 5 | every `pub` safe item of a tier-0 crate is sound for **arbitrary safe callers**, including ones that leak, `forget`, panic or re-enter ([core.md](core.md)). Kani harnesses drive each API with nondeterministic call sequences including `forget`; loom models interrupt re-entry as a preempting thread gated by the interrupt-flag state; compile-fail tests check that tokens and guards can't escape or cross threads; Miri runs every pure crate's tests; every `unsafe impl Send`/`Sync` is inventoried with a justification |
| 3b | Shim soundness | T from 1a | each shim function does what its contract (a named axiom citing the manual section) says, for every argument its safe wrapper can pass: a QEMU test per shim function plus the unsafe-inventory review of its module. Shim code stays within its 3,000-line cap |
| 4 | Memory isolation between processes | P at 3 for the sharing paths that exist then; extended at 5 (shared memory, memfd, `SCM_RIGHTS`, `ptrace`, `process_vm_*`, io_uring buffers), 7 (dma-buf) and 8 (GPU buffers) | a user mapping in process A reaches a frame only if A holds a capability for the memory object that owns it; the sharing paths are the closed list in `proofs.toml` ([capabilities.md](capabilities.md)); kernel memory is never user-accessible |
| 5 | Capability derivation and revocation | P at 3; the per-syscall test T at 3 | derived capabilities never have more rights than their parent; revocation removes the whole subtree, including every open file description (OFD) derived from the object and every fd slot that shares an OFD through `dup`, `fork` or `SCM_RIGHTS`, and every mount derived from a view grant, including copies made by `CLONE_NEWNS`; a revoked handle behaves as defined in [capabilities.md](capabilities.md). Plus a per-syscall property test that no input yields a capability, OFD or mapping the caller didn't hold |
| 6 | User-copy routines | B at 3 | the pure part of the `UserPtr` copy routines (range checks, chunking, the read-once wrapper) only touches the user range it was given, never a kernel address, turns a fault into an error, and reads each user value once; the `stac`/`rep movsb`/`clac` instructions and their exception-table fixup are a shim (target 3b). That no syscall or driver entry point reaches user memory any other way follows from `UserPtr<T>` having no dereference and raw pointers not existing outside tier 0 (target 3a) |
| 7 | Resource accounting | B at 2 for scheduler CPU accounting; B at 4a for memory and interrupt budgets | budgets ([drivers.md](drivers.md)) are charged and released exactly once, never go negative, and a component can't exceed its budget |
| 8 | Scheduler invariants | M at 2 for run queues and priority inheritance (loom); B at 2 for real-time throttling (Kani over an abstract clock) | every runnable thread is on exactly one run queue; class order and priority inheritance rules in [scheduler.md](scheduler.md) hold; real-time threads never run past their throttling budget in any period, for every sequence of clock readings up to the bound. (A thread running on two CPUs is prevented by the core's move-only `RunnableThread`, not by this policy-level proof, so a policy bug can't cause undefined behaviour) |
| 9 | Driver message boundary | B at 4a | ring code never reads a descriptor field twice; never uses a driver-supplied index without bounds-checking it; every buffer handle resolves only to a buffer the kernel granted to that driver |
| 10 | IOMMU tables and DMA ownership | B at 1c, P at 4a | a device's domain maps only frames owned by its live `DmaBuffer`s or pinned for it by a streaming map; context and domain tables stay well-formed; interrupt remapping entries only target vectors the core allocated. The invalidation-before-free order is checked at level M at 2, as part of the same state machine as target 1s, with "a queued invalidation's wait descriptor completes only after the invalidation" as the axiom |
| 11 | SMP locks and per-CPU data | M at 2 | spinlocks give mutual exclusion and are IRQ-safe as typed; per-CPU access tokens are never held across a context switch |
| 12 | Storage invariants | B at 4a | the block allocator never double-allocates; the XTS sector-to-tweak mapping is injective; a commit is issued only after the writes it covers have completed; after a driver restart, writes completed since the last flush are treated as not durable ([storage.md](storage.md)); AEAD nonces are never reused under one key, including after a crash, a fallback to an older commit or a replayed image (the per-mount salt mixed into each write epoch's subkey in [storage.md](storage.md); file system track). The own file system's further proofs are in `fs/docs/DESIGN.md` |
| 13 | GPU address-space isolation | B at 8 | a GPU command stream from process A can only reach GPU virtual addresses mapped in A's GPU address space, which map only buffer objects A holds a capability for |

## Not provable here, and tested instead

Lockdown (every row of the table in [threat-model.md](threat-model.md)), the
Linux ABI's behaviour, driver logic against real hardware, side-channel
resistance, and anything that depends on firmware. These have tests and
fuzzing, and are reported as level T.
