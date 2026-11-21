# Formal verification plan

Testing shows bugs are present; it can't show they're absent. For the small
part of the kernel where one bug breaks every security promise, we also prove
properties with machine-checked proofs.

## What proofs do and don't give us

Be precise about the claim. A proof shows that a specific piece of code meets
a specific property, under stated assumptions (for example "the CPU implements
4-level paging as the Intel SDM describes" or "no other code writes this page
table"). Proving many functions correct is **not** a proof that the kernel is
secure. Bugs can still live in:

- code that isn't proved (most of the kernel, for years),
- the assumptions (hardware errata, firmware, a wrong reading of the manual),
- the gaps between proved pieces (two correct parts used together wrongly),
- the specification itself (proving the wrong property),
- the tools (compiler, proof checker, linker).

So proofs sit on top of testing, fuzzing and review, never in place of them.
Every proof states its assumptions in the code next to it.

We're also not the first to do this. seL4 proved a whole microkernel correct,
and mainstream systems verify parts of themselves: Microsoft's Static Driver
Verifier (built on SLAM) checks Windows drivers against kernel API rules,
AWS uses formal methods on parts of its infrastructure, and Linux has a
formal memory model and research work verifying parts of its BPF verifier. What
we're aiming for is to make proofs a routine part of the core from day one,
not a one-off research effort.

## Tools

- **Kani** (model checker for Rust, MIT/Apache-2.0): bounded proofs of
  properties of real kernel functions, run in CI. It has its own pinned
  toolchain; the kernel itself still builds on stable Rust.
- **Verus** (MIT) later, for unbounded proofs of data-structure invariants
  that Kani's bounded checking can't cover (for example "for any sequence of
  map/unmap calls").
- Property-based tests and fuzzing check the same properties on the real
  kernel in QEMU, so a proof and a test disagreeing is caught.

## Proof targets, in the order we tackle them

Each target is a property, not a module. A target is "done" when the proof
runs in CI on every change and the assumptions are written down.

| # | Target | Starts | Property |
|---|---|---|---|
| 1 | Page-table manipulation | Phase 1b | `map`/`unmap`/`protect` only change the entries they were asked to; never create a writable+executable kernel mapping; never map one physical frame writable at two virtual addresses unless asked; leave intermediate tables well-formed; translate addresses the same way the hardware walk would. |
| 2 | Physical frame allocator | Phase 1b | A frame is never handed out twice without being freed; freed frames were allocated; allocation never returns reserved or out-of-range memory. |
| 3 | Unsafe abstractions in the core | Phase 1 onwards | Each `unsafe` API's documented safety contract holds for every caller in the tree (checked per call site where Kani can, reviewed where it can't). |
| 4 | Memory isolation between processes | Phase 3 | No user mapping in process A reaches a frame owned by process B unless both hold a capability to a shared object; kernel memory is never user-accessible. |
| 5 | Capability enforcement | Phase 3 | Every operation on a kernel object checks a capability with the right rights; capabilities can only be derived with equal or fewer rights; revocation removes all derived copies. |
| 6 | No privilege escalation through kernel interfaces | Phase 3 onwards | For every syscall and driver-interface entry point: user-supplied pointers are only dereferenced through checked user-copy routines, and no input lets a caller gain a capability or mapping it didn't hold. (Proved per entry point; fuzzed across all of them.) |
| 7 | Resource-accounting invariants | Phase 2 onwards | Memory, CPU-time and IRQ budgets (see [drivers.md](drivers.md)) are charged and released exactly once, never go negative, and a component can't exceed its budget. |
| 8 | Scheduler invariants | Phase 2 | Every runnable thread is on exactly one run queue; a thread is never running on two CPUs; priority rules hold. |
| 9 | Restricted-driver boundary | Phase 4 | The shared-memory ring code never reads a descriptor field twice (no time-of-check/time-of-use), never trusts an index from the driver without bounds-checking it, and only accepts DMA addresses inside the driver's IOMMU domain. |
