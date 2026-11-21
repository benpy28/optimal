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
assumptions. Contracts on our own code (every shim contract) are
assumptions discharged by target 3b, which is at T, so a target that rests
on a shim is effective T; only hardware, vendor and toolchain axioms (see
the ledger) leave the level unchanged. P ranks above B and M, which both
rank above T; a mix of B and M is reported as "B+M". Levels are always
written **own/effective**, for example "P/T" for a P proof over tested
shims, and "T/T" when both are T. The ledger script in CI computes the
effective level from `proofs.toml` and `axioms.md` and fails if it differs
from the expected value in the table below; every roadmap proof gate names
both, and the scorecard reports both. Because target 3b is T, the
effective level alone says little, so the table's "Rests on" column names
the shim contracts and targets each target rests on, generated from
`proofs.toml`, and the scorecard prints it next to the level. The
gate-table check (below) also
fails if any design file or the roadmap gives a target's level in any other
form.

## Tools and how proofs see the real code

- **Kani** (MIT/Apache-2.0): bounded model checking of real Rust functions.
  It can't handle inline assembly, threads or weak memory, doesn't check
  aliasing rules (Stacked or Tree Borrows), and compiles for the host with
  its own toolchain.
- **Verus** (MIT): unbounded proofs of invariants over abstract state
  machines plus refinement of the real code. Starts in **Phase 3**, first on
  the capability table (target 5), then page tables (target 1 moves from B
  to P for its sequential effects) and IOMMU tables (target 10). Verus
  models `&T` as immutable, so it can't reason about interior mutability
  through `&core::sync::atomic::AtomicU64` or a reference into mutable
  memory; its only sound route is vstd's permission-carrying types. So the
  shipping kernel depends on a **vendored, pinned, audited exec subset of
  `vstd`**: `atomic` (`PAtomicU64`, an `AtomicU64` plus a tracked
  permission), `simple_pptr` (`PPtr` with a tracked points-to permission)
  and `cell`, plus `verus_builtin` (which holds `Ghost` and `Tracked`) and
  `builtin_macros`, all under the procedural-macro rule in
  [core.md](core.md). Page-table and IOMMU entries are `PAtomicU64`, and
  the memory-access backend returns `PPtr`s with their permissions. Ghost
  and tracked code erases in non-Verus builds, leaving the plain atomics
  and pointers. A CI check fails if the shipping `cargo tree` of any kernel
  or loader binary contains `vstd` other than as that subset at its pinned
  hash. Each `external_body` item of the subset (the atomic operations, the
  pointer reads and writes) is a named toolchain axiom citing the Rust
  atomics and pointer documentation. **Phase 3 done-when:** a spike proves
  one real `map` end to end through this route, the shipped code and not a
  copy; if it fails, target 1's sequential part is reported **P+B/T** (P
  over the abstract PTE-array model, its refinement to the code by Kani)
  and the scorecard says so. These specifications are sequential: they
  can't track a value the hardware walker changes concurrently, so the
  A/D-bit and walker clause of target 1 stays at B (Kani with a
  nondeterministic walker) plus M (loom with a walker thread), never P.
  Verus proves functional properties of safe code; it does not prove the
  absence of undefined behaviour in `unsafe` code (target 3a). Verus's own
  rustc version is recorded in `proofs.toml`.
- **loom** (MIT): exhaustive interleaving checks of concurrent code compiled
  for the host, with the preemption bound of each model recorded in
  `proofs.toml`; this is level M. Data shared with interrupt handlers is
  declared through loom's cell and atomic types under `cfg(loom)`, so loom
  sees every access; these `cfg(loom)` type swaps mean loom checks a build
  whose cell and atomic types differ from the shipping ones, which the
  ledger lists as not checked. loom starts in Phase 1c with the
  single-CPU interrupt re-entry models (the LAPIC timer and the clocksource
  watchdog handlers preempting the code they share data with) and the
  target 1 walker thread; multi-CPU models start in Phase 2. **shuttle**
  (Apache-2.0) explores randomised schedules of larger configurations; that
  is level T. It starts in Phase 2.
- **Miri**: runs the host test suite of every pure tier-0 crate and reports
  undefined behaviour. Lands in Phase 1a with the proofs job.
- Property-based tests and fuzzing check the same properties on the real
  kernel in QEMU, so a proof and a test disagreeing is caught.
- **Two toolchains.** Each tool pins its own rustc. CI builds and runs the
  tests of every proved crate with the shipping toolchain and with each
  tool's toolchain, on every change, so a crate that only one compiler
  accepts, or that behaves differently under one, fails.

**Proved code is host-compilable.** Every proved module is split into a pure
crate (no `asm!`, no target-specific `cfg`, no hardware access, no
dereference of a pointer into physical memory) containing the logic, and a thin shim that does the
hardware part (writing CR3, `INVLPG`, MMIO). CI fails if a proved crate
contains `asm!` or `cfg(target_*)`. Every tier-0 module with `asm!`, a
target-specific `cfg`, hardware access or a raw-pointer dereference of
physical memory is a shim. One shim class is the **memory-access backend**:
the page-table code, the frame allocator's metadata and the heap reach
physical memory only through a `MemoryBackend` trait (phys-to-virt
translation through the direct map, and accessors that return a reference
to one page-table entry or one metadata record) that the pure crates are
generic over. The shipping kernel implements it with direct-map raw
pointers in about 150 lines of `core-mm`; Kani, Miri and loom use a host
implementation over an array. A CI check fails if a pure `core-mm` crate
dereferences a raw pointer, calls `ptr::read`/`ptr::write` or names the
direct-map base other than through the trait. Each shim function, including
every backend method, has a written contract (what the instruction or
access does, from the manual section it cites, and for the backend which
frames the returned reference may alias). The contract is an our-code
contract in the ledger, discharged by target 3b (QEMU test per function plus
the unsafe review) at T, so it lowers the effective level of every target
that rests on it. Shim code is capped at 3,000 lines and `core-crypto` at
6,000 lines, both inside the 15,000-line tier-0 cap, with a per-crate shim
estimate in [core.md](core.md); all three counts are on the scorecard. So
the proofs check the same source that ships, except for the shims, the
`cfg(loom)` type swaps and the compiler.

**Text poking.** Trace points ([scheduler.md](scheduler.md)) are the only
kernel text that changes after boot, through one `core-arch` shim. Each
trace point has exactly two precompiled states (the 5-byte no-op and the
jump to the tracer); the shim refuses any other byte string. It writes
through a temporary private mapping: a non-executable writable alias of the
one text page, installed only in a poking page table that no other CPU
loads, and removed with a local invalidation before the shim returns. The
sequence is the cross-modifying-code protocol of SDM Vol. 3A §8.1.3: write
`int3` over the first byte, serialise every CPU with an IPI, write the
remaining four bytes, serialise again, write the first byte of the new
state, serialise again. A CPU that hits the `int3` meanwhile resumes after
the 5-byte site, as if the no-op ran (the tracer misses one event). Direct-map aliases of kernel text are read-only.
Lockdown ([threat-model.md](threat-model.md)) permits only these two
states. Target 1 covers the mapping rules (extended at Phase 2, when the
poke code lands) and target 3b the shim.

## The assumption ledger

Each proof's assumptions are listed in `design/axioms.md` (created with the
first proof), in two separate sections. Every assumption is either:

- **discharged** by another target's guarantee (assume-guarantee: target 4
  assumes the capability table's guarantee from target 5). The **our-code
  contracts** section holds the only contracts on our own code that proofs
  rely on, the shim contracts (memory-access backend included), each
  discharged by target 3b; or
- a **named axiom** in the **hardware, vendor and toolchain axioms**
  section: an Intel SDM or AMD APM section (4-level and 5-level
  paging walk, TLB and PCID behaviour, `INVLPG`, x86-TSO memory ordering),
  the IOMMU and TPM 2.0 specifications (for target 10: VT-d context-entry
  and PASID-entry update rules and caching and coherency, where a walk
  with `ECAP.C`=0 is non-coherent and reads an entry only after it is
  flushed with `clflush`; the AMD-Vi device-table-entry update rules; each
  multi-word entry (VT-d context 128 bits legacy or 256 bits scalable, VT-d PASID 512 bits, AMD-Vi DTE 256 bits), read
  without single-copy atomicity across its words, so it is valid only
  while its present or valid bit is written last and cleared first; for
  target 13, one GPU page-table format and walk axiom per vendor, citing
  that vendor's published MMU documentation; for target 17, the TPM 2.0
  Part 1 policy chapter's policy-digest semantics), the `external_body`
  items of the vendored `vstd` subset, "XTS ciphertext and unused AEAD
  slots are indistinguishable from CSPRNG output" (F3's deniability rests
  on it, [features.md](features.md)), "each hardware and firmware entropy source
  (`RDSEED`, the `EFI_RNG_PROTOCOL` seed, the TSC jitter samples) has at
  least its published SP 800-90B min-entropy estimate" (the kernel's own
  generator is our code, target 14, never an axiom), "the page-table walker reads
  each entry with a single-copy-atomic 64-bit load" (which is why entries
  are written through `PAtomicU64`, never plain stores: Rust code follows
  the C++20 memory model, not x86-TSO, and a plain store raced by the
  hardware walker would be a data race), "the walker may set the Accessed
  and Dirty bits of any present entry at any time with a locked
  read-modify-write" (SDM Vol. 3A §4.8, which is why present entries change
  only through `swap`, `compare_exchange` or `fetch_and`, never load then
  store, so a Dirty bit the walker set can't be lost), rustc and LLVM compile correctly
  (including privacy and `#![forbid(unsafe_code)]`, which is how "only
  `core-mm` writes page tables", assumed by target 1, holds: raw entries
  are `pub(crate)` in `core-mm` and no other crate may use `unsafe`),
  each tool's own compiler front end (Kani, Verus and Miri pin their own
  rustc versions, recorded in `design/proofs.toml`) treats the code the same
  as the shipping compiler (narrowed by the two-toolchain CI job), and Verus's
  erasure of spec and proof code (`verus!` and `builtin_macros`) leaves the
  executable code unchanged. Kani, Verus and loom are assumed sound.

A third section of `axioms.md` holds **written lemmas**: paper arguments no
tool checks, each naming the targets and axioms it rests on and reported at
the level of its weakest input. The first is target 12c's birthday bound,
resting on target 14.

A CI script fails if any proof cites an assumption that is neither discharged
nor on the axiom list. **The discharge graph is acyclic:** the script builds
the graph of "target X rests on target Y" edges from `proofs.toml` and
fails on any cycle, because circular assume-guarantee is unsound without an
induction rule. Where two properties really depend on each other, they are
proved jointly as one target. (Target 4's mapping property and target 5's
per-syscall test were such a pair; the mapping half now lives in target 4
alone.)

The ledger also lists what each tool does not check, so the scorecard can't
imply more: Kani, paths beyond each loop's unwinding bound, concurrency,
inline assembly and aliasing rules (Stacked or Tree Borrows); loom,
interleavings beyond the recorded preemption bound, `Relaxed` behaviours it
doesn't generate (load buffering) and the shipping types it replaces under
`cfg(loom)`; all host tools, the shipping `MemoryBackend` implementation; Verus, every
`external_body`, `external` and `assume_specification` item, the `vstd`
subset's included (each also a
named axiom) and undefined behaviour in `unsafe` code;
Miri, paths the tests don't execute and aliasing rules beyond its current
model; shuttle, every schedule it didn't sample.

## Keeping proofs in sync with the code

- A required **proofs** CI job, landing in Phase 1a with Miri, runs every
  per-change harness on every change. Each
  harness has a time budget (default 10 minutes); exceeding it fails, so
  proofs can't silently become too slow to run.
- `design/proofs.toml` records each harness's bounds and counts every
  `kani::assume`, Verus `assume`, `admit`, `external`, `external_body` and
  `assume_specification`. In shipped proofs, `admit`, `assume` and
  `external` are banned except where the item is a named axiom in the
  ledger; CI fails on any other. Loosening a bound or adding an assumption
  changes that file, which needs review like any design change.
- Each target names, in `proofs.toml`, the crates and functions in its
  scope, by name. A change to a function in a proved module that isn't on a
  list fails CI.
- The whole proofs job has a budget of 2 hours per change; beyond that,
  slow harnesses move to the nightly run and the change waits for it.
  Nightly-only harnesses are labelled as such in `proofs.toml` and on the
  scorecard, and are at most 20% of each target's harnesses; CI fails above
  that. So every target runs on every change, but up to 20% of its
  harnesses run nightly instead.
- **The loader is in scope.** `bootloader/` follows the rules of
  [core.md](core.md) and this file: its harnesses run in the same proofs job,
  its functions are named in the same `proofs.toml`, and its floor code is
  in target 16's scope.
- **No vacuous proofs.** Every Kani harness has `kani::cover!` statements for the
  states its property is about (a full table, a split huge page, a revoked
  subtree); an unreachable cover fails CI, so an over-strong `kani::assume`
  can't make a harness pass by excluding everything. Every Verus `requires`
  clause has a satisfiability witness: a proof function that constructs a
  concrete state meeting it, checked by Verus, so an unsatisfiable
  precondition fails CI. Every loom model asserts that each state listed
  for it in `proofs.toml` (for example "the acknowledgement arrives after the
  frame is queued for freeing") is reached in at least one explored
  interleaving; an unreached state fails CI. Each target also lists
  seeded bug injections in `proofs.toml` (known-wrong changes to its code,
  such as dropping a bounds check); the nightly run applies them one at a
  time, and each must make a harness at the target's own level fail (a
  Verus proof for a P target, a Kani harness for B, a loom model for M); a
  failing test alone doesn't count, except for targets at T.

## Proof targets

Each target is a property, not a module. A target is "done" at its stated
level when its harnesses run in CI per change or nightly, as labelled in
`proofs.toml`, and its assumptions are in the ledger. **Every target, every
move to a higher level and every extension ("extended at N": the property
gains the parts whose code lands in Phase N) is a done-when item of the
phase in the "gate" column**, named there by target number and
own/effective level; see the [roadmap](../ROADMAP.md). A CI script checks
that this table and the roadmap's proof-gate lines name the same targets,
levels and phases, fails if any design file or the roadmap writes a
target's level other than as own/effective, and fails if any design file
calls a target "proved" when it isn't at P (or M for concurrency). Phase 1 is split into 1a (CPU
setup and exceptions), 1b (memory), 1c (ACPI, PCI, interrupts, IOMMU and
time) and 1d (console); the storage and trusted-boot phase into 4a
(storage) and 4b (trusted boot chain). The file system track starts after 4a.

| # | Target | Level and gate (own/effective) | Rests on (shim contracts, targets) | Property |
|---|---|---|---|---|
| 1 | Page-table manipulation | B/T at 1b; A/D-bit and walker clause B+M/T at 1c (loom walker thread added); text-poke clauses extended at 2 (B/T); P/T at 3 for the sequential effects through the `vstd`-subset route (P+B/T if the Phase 3 spike fails, see Verus above), with the A/D-bit and walker clause staying B+M/T; writeback clause and its QEMU test extended at 4a (P/T) | backend; CR3, `INVLPG`/`INVPCID`; text poke (from 2) | `map`/`unmap`/`protect` only change the entries they were asked to; outside the direct map, never create a writable+executable kernel mapping and never map one frame writable at two virtual addresses unless asked; the only writable alias of a kernel-text frame is the text-poke mapping, which is non-executable, present only in the poking page table and only during a poke; direct-map aliases of kernel text are read-only; the direct map is no-execute, supervisor-only and never reachable through an entry with the user bit set; intermediate tables stay well-formed; translation matches the hardware walk (axiom: SDM paging chapter). **A/D-bit and walker clause** (B with Kani's nondeterministic walker, which may set A/D bits between any two steps, plus M with a loom walker thread; never P, see Verus above): present entries change only by `swap`/`compare_exchange`/`fetch_and`, and the Dirty bit of a replaced or write-protected entry is moved to its page before the entry is gone. Extended at 4a, when the page cache lands: writeback of a page starts only after the shootdown of its write-protect completes, plus a QEMU test that writes through a file mapping while its page is under writeback and checks no write is lost |
| 1s | TLB shootdown protocol | M/T at 2; re-gated at M/T at 3 (PCIDs, user address spaces, copy-on-write and `mprotect` downgrades, lazy-CPU teardown on the IPI path) and at 5 (INVLPGB with its lazy-CPU teardown, transparent huge pages) | CR3, `INVLPG`/`INVPCID`, IPI send; `INVLPGB`/`TLBSYNC` (from 5) | written as a state machine with per-CPU acknowledgement generations: **no CPU uses a translation with more rights than the current PTE once the operation returns**, and no CPU uses any translation or paging-structure entry for a frame, including a freed page-table page, once the frame is returned to the allocator. Any change that removes a permission (write-protect for copy-on-write, `mprotect` to read-only, executable or `PROT_NONE`) or changes a frame bumps the address space's generation and shoots down before returning, exactly as an unmap does. The two obligations, using the may-cache set and PCID generations of [scheduler.md](scheduler.md): every CPU in the address space's may-cache set at the change has acknowledged an invalidation issued after it, and on every other CPU each PCID slot holding that address space has a recorded generation older than the change, so reloading it forces a full flush first; a PCID or global ASID is reassigned only after a full flush. At address-space teardown, and whenever a top-level table is freed, every lazy CPU in the may-cache set is sent an IPI that switches it to the kernel-only table before the table is freed, on the IPI and the INVLPGB paths alike (the IPI path part of the gate at 3, the INVLPGB path part of the gate at 5). Axioms: an IPI that is sent is delivered and its handler runs; `INVLPG` and `INVPCID` semantics and `MOV CR3` with bit 63 set not flushing the new PCID (SDM Vol. 3A §4.10.4); paging-structure caches are invalidated by the same operations as leaf entries (§4.10.3); a mapping whose page size changes is invalidated before reuse (§4.10.2.3); `INVLPGB` followed by `TLBSYNC` completes the invalidation on every CPU (AMD APM Vol. 3). Plus QEMU tests: one delays acknowledgements and checks the frame isn't reused (at 2); one forks a process while its other threads write to every page and checks the parent never writes into a frame now shared with the child (at 3) |
| 2 | Physical frame allocator | B/T at 1b (owners: allocator and page tables); owner kinds extended at 3 (memory objects, B/T) and 4a (`DmaBuffer` and DMA pins, B/T); P/T at 4a | backend | a frame is never handed out twice without being freed; freed frames were allocated; never returns reserved or out-of-range memory; every frame has exactly one owner among the kinds that exist at that gate (allocator, a page table, a memory object, a `DmaBuffer`), plus any number of recorded DMA pins on frames owned by a memory object; a pinned frame is never freed |
| 3a | Soundness of tier-0 safe APIs (pure crates) | B/T per API from 1a for panics, bounds and functional behaviour; T/T from 1a for aliasing (Miri); interrupt re-entry M/T at 1c (single CPU) and at 2 (multi-CPU); P/T at 5 for the functional invariants of the safe-only pure capability and memory APIs | backend (`core-mm` crates); interrupt-flag shim (M part) | every `pub` safe item of a tier-0 pure crate is sound for **arbitrary safe callers**, including ones that leak, `forget`, panic or re-enter ([core.md](core.md)). **B part:** Kani harnesses drive each API with nondeterministic call sequences including `forget`, and check no panic, overflow or out-of-bounds access. **T part:** Kani doesn't check aliasing, so freedom from aliasing violations in `unsafe` code rests on Miri running every pure crate's tests (Tree Borrows) and stays T at every phase. **M part:** loom models interrupt re-entry as a preempting thread gated by the interrupt-flag state. Compile-fail tests check that tokens and guards can't escape or cross threads; every `unsafe impl Send`/`Sync` is inventoried with a justification. The P step at 5 proves functional invariants of APIs whose implementation is safe code only. Also a 3a obligation from 3: user page-table entries are installed only through the single `core-mm` user-PTE API (target 4) |
| 3b | Shim soundness | T/T from 1a; extended at 1b (memory-access backend), 1c (IOMMU register and queue programming and the `clflush` of IOMMU table lines for non-coherent walks, PCI config, MSI-X), 2 (text poke), 3 (user-copy instructions, PCID and `INVPCID`), 4b (TPM CRB/TIS, the loader's shims) and 5 (`INVLPGB`/`TLBSYNC`), each T/T | none: it discharges every shim contract | each shim function does what its contract (an our-code contract citing the manual section) says, for every argument its safe wrapper can pass: a QEMU test per shim function plus the unsafe-inventory review of its module. For the memory-access backend, the tests also check that each returned reference lies inside the frame it was asked for and the direct map. Kernel shim code stays within its 3,000-line cap and the loader's within its own ([core.md](core.md)). The text-poke test toggles a trace point under a load running on every CPU and checks no CPU faults or executes a mixed state |
| 3c | Soundness of shim-backed safe APIs | T/T from 1a (`with_irqs_disabled`, IRQ and preemption tokens); extended at 1c (MMIO, port I/O, IRQ and DMA handles), 2 (context switch, `RunnableThread`) and 3 (`UserPtr` copies), each T/T | the shim each API calls (interrupt flag, MMIO, port I/O, DMA, context switch, user copy) | every `pub` safe item whose implementation calls a shim is sound for arbitrary safe callers: compile-fail tests check its tokens and handles can't escape, be copied or cross threads, and QEMU tests call it with a `forget` of every guard and handle it returns, a panic at every callback point it offers, and an interrupt injected at each instruction boundary of its shim (single-stepped under QEMU's gdbstub), checking no undefined behaviour and no broken core invariant (the interrupt state is restored, the token count matches, no frame is leaked into two owners) |
| 4 | Memory isolation between processes | P/T at 3 for the sharing paths that exist then, plus the per-syscall mapping test (T/T); extended at 5 (shared memory, memfd, `SCM_RIGHTS`, `ptrace`, `process_vm_*`, io_uring buffers), 7 (dma-buf) and 8 (GPU buffers), each P/T | target 5; backend; CR3 | a user mapping in process A reaches a frame only if A holds a capability for the memory object that owns it; kernel memory is never user-accessible. Proved over one `core-mm` API that installs user page-table entries and takes a memory-object capability witness, plus the capability table (target 5); every syscall, the page-fault handler, copy-on-write and file mappings reach user PTEs only through it, and the sharing paths are the closed list in `proofs.toml` ([capabilities.md](capabilities.md)). A CI call-graph check fails if anything other than that API reaches the PTE-writing shim. Plus a per-syscall property test that no input yields a mapping of a memory object the caller didn't hold |
| 5 | Capability derivation and revocation | P/T at 3; the per-syscall test T/T at 3 | backend (the table lives in the kernel heap) | derived capabilities never have more rights than their parent; revocation removes the whole subtree, including every open file description (OFD) derived from the object and every fd slot that shares an OFD through `dup`, `fork` or `SCM_RIGHTS`, and every mount derived from a view grant, including copies made by `CLONE_NEWNS`; a revoked handle behaves as defined in [capabilities.md](capabilities.md). Plus a per-syscall property test that no input yields a capability or OFD the caller didn't hold (mappings are target 4's, so this target rests on no other target; it rests on the memory-access backend contract, discharged by 3b at T, hence P/T) |
| 6 | User-copy routines | B/T at 3 | user-copy instructions and their exception-table fixup | the pure part of the `UserPtr` copy routines (range checks, chunking, the read-once wrapper) only touches the user range it was given, never a kernel address, turns a fault into an error, and reads each user value once; the `stac`/`rep movsb`/`clac` instructions and their exception-table fixup are a shim (target 3b). That no syscall or driver entry point reaches user memory any other way follows from `UserPtr<T>` having no dereference and raw pointers not existing outside tier 0 (targets 3a and 3c) |
| 7 | Resource accounting | B/T at 2 for scheduler CPU accounting; extended at 4a (B/T) to memory and interrupt budgets and the driver real-time cap | timer and clock shim | budgets ([drivers.md](drivers.md)) and the driver real-time cap ([scheduler.md](scheduler.md)) are charged and released exactly once, never go negative, and a component can't exceed its budget |
| 8 | Scheduler invariants | M/T at 2 for run queues and priority inheritance (loom); B/T at 2 for real-time throttling (Kani over an abstract clock; effective T because the clock comes from the timer shim) | timer and clock shim; context switch | every runnable thread is on exactly one run queue; class order and priority inheritance rules in [scheduler.md](scheduler.md) hold; real-time threads never run past their throttling budget in any period, for every sequence of clock readings up to the bound. (A thread running on two CPUs is prevented by the core's move-only `RunnableThread`, not by this policy-level proof, so a policy bug can't cause undefined behaviour) |
| 9 | Driver message boundary | B/T at 4a | DMA handles (the shared rings) | ring code never reads a descriptor field twice; never uses a driver-supplied index without bounds-checking it; every buffer handle resolves only to a buffer the kernel granted to that driver |
| 10 | IOMMU tables and DMA ownership | B/T at 1c; invalidation-before-free order M/T at 2; streaming maps extended at 4a (B/T); P/T at 4a | backend; IOMMU register and queue programming; `clflush` (non-coherent walks) | a device's domain maps only frames owned by its live `DmaBuffer`s or, from 4a, pinned for it by a streaming map; context and domain tables stay well-formed, with every unused context entry blocking and every unused AMD-Vi device table entry set to block DMA and interrupts; interrupt remapping entries only target vectors the core allocated. **Multi-word entries** (Kani clause, B/T at 1c): a VT-d context or PASID entry and an AMD-Vi device table entry are written with the present or valid bit cleared first and set last, each word ordered by a release store, and on VT-d with `ECAP.C`=0 every written cache line is flushed with `clflush` before the invalidation is queued (axioms: the VT-d and AMD-Vi entry-update and coherency rules in the ledger); plus a QEMU `intel-iommu` test in non-coherent mode at 1c that rewrites live context entries under DMA and checks no transaction uses a mixed entry. The invalidation-before-free order is checked at level M/T at 2, as part of the same state machine as target 1s, with "a queued invalidation's wait descriptor completes only after the invalidation" as the axiom |
| 11 | SMP locks and per-CPU data | M/T at 2 | interrupt-flag shim; context switch | spinlocks give mutual exclusion and are IRQ-safe as typed; per-CPU access tokens are never held across a context switch |
| 12a | Storage: allocator, commit order, restart | B/T at 4a | targets 2, 9 and 10 | the block allocator never double-allocates ([storage.md](storage.md)); a commit is issued only after the writes it covers have completed; after a driver restart, every write not covered by a completed flush is re-sent from the replay log's exact device bytes and treated as not durable |
| 12b | Storage: volume map and XTS tweak | B/T at 5 | target 14 (XTS) | the per-user volume map's extents never overlap and stay inside the device; the XTS sector-to-tweak mapping is injective per key |
| 12c | Own file system: AEAD nonces | Kani part B/T, probabilistic; birthday-bound lemma T/T; both gated by the file system track's done-when | target 14 (generator), through the written birthday-bound lemma | **Kani part:** no AEAD nonce repeats under one key, including after a crash, a fallback to an older commit, a replayed image or a hibernation resume: each mount (and each resume) draws a 128-bit random prefix from the kernel generator (target 14) and counts writes with a 64-bit counter; XChaCha20-Poly1305 uses the 192-bit prefix and counter directly, and a 96-bit-nonce suite (AES-256-GCM) uses a per-mount subkey HKDF(key, prefix), the key being the per-file key for class-B files and the volume or class key otherwise, with the counter zero-padded to 96 bits as its nonce; the second DUP metadata copy (copy 1) is encrypted under the per-copy subkey HKDF(key, `optimal-copy-1`) with the same nonce, so no (key, nonce) pair repeats across the two copies ([storage.md](storage.md)); a counter about to wrap draws a new prefix first, so no (key, nonce) pair repeats within one prefix: the prefix and counter are unique per mount, and a new prefix is drawn before the counter wraps. **Written lemma** (in `axioms.md`, resting on target 14, hence T/T): two prefixes under one key collide with probability at most 2^-64 per volume lifetime (2^32 mounts, birthday bound on 128 bits), given that the generator's output is indistinguishable from random. Kani can't establish a probability; this bound is the paper argument. The own file system's further proofs are in `fs/docs/DESIGN.md` |
| 13 | GPU address-space isolation | B/T at 8 | targets 4 and 10; GPU MMIO and page-table shims | a GPU command stream from process A can only reach GPU virtual addresses mapped in A's GPU address space, which map only buffer objects A holds a capability for; GPU page-table entries are built only from `DmaBuffer`, `Iova` or `VramBuffer` handles (a `VramBuffer` names a VRAM range the kernel allocated to that buffer object and cleared, [drivers.md](drivers.md)), on D1 and D2; the entry format and walk rest on the per-vendor GPU page-table axiom in the ledger |
| 14 | Kernel and loader cryptography | T/T at 3 (the kernel generator, ChaCha20 and BLAKE2b); extended at 4a (AES-XTS, AES-GCM and GHASH, ChaCha20-Poly1305 and XChaCha20-Poly1305, SHA-256, HMAC-SHA-256, HKDF, PBKDF2-HMAC-SHA-256, Argon2id, T/T) and 4b (Ed25519 verification and SHA-512, HMAC-SHA-1 for TOTP, and the loader's build of every primitive it uses, T/T) | its intrinsic backends (AES-NI/VAES, SHA, carry-less multiply, AVX2/AVX-512), `RDSEED` | all of `core-crypto` ([core.md](core.md)), which is all cipher, hash, MAC, KDF, signature and random-generator code in the kernel and the loader, matches the NIST CAVP vectors, the Wycheproof vectors and the RFC vectors (RFC 8439, 7693, 5869, 8018, 9106, 8032, 6238) where they exist, and agrees with a reference implementation in differential tests over random inputs. The generator ([core.md](core.md)) is checked against RFC 8439 ChaCha20 vectors and known-answer vectors from a reference model of its construction (seed, reseed, fast key erasure, per-CPU rekey), its output passes the NIST SP 800-22 statistical suite over 10^9 bits, and tests check `getrandom` blocks until 256 bits are credited, a forked child and a resumed hibernation image never repeat the parent's output, and readiness on the slowest boot-matrix machine with no `RDSEED` and no firmware RNG. Every function that handles key or plaintext bytes has no secret-dependent branch or memory address, checked by a binary-level secret-taint checker (a DynamoRIO client we maintain, running natively on a CI machine with VAES and AVX-512, so every backend executes) with secrets marked tainted. Verified generated code (HACL*/libcrux-derived, fiat-crypto) is used wherever it exists for the algorithm and instruction set, and counts in the `core-crypto` cap |
| 15 | Network-path parsers | B/T at 4b for the pre-cap header-block parser; B/T at 6 for the network-path parsers | DMA handles (input buffers) | each parser never panics, never indexes out of range and never reads beyond its input, for every input up to the bound recorded in `proofs.toml`; longer inputs are fuzzed. **In the kernel:** Ethernet, ARP, IPv4, IPv6 and its extension headers, ICMPv4, ICMPv6 including neighbour discovery (neighbour solicitation and advertisement; router advertisements are passed whole to user space), UDP and TCP with options; bound at least 128 bytes, which covers every fixed header and its options up to that length. **In sandboxed user-space services** ([threat-model.md](threat-model.md)): DHCPv4 and DHCPv6 (DHCP client), router advertisements (the same client) and DNS (resolver); bound at least 512 bytes. **Pre-cap, in the kernel:** the parser of the fixed 16 KiB header block (two 8 KiB copies, [storage.md](storage.md)), over the whole 8 KiB copy; the parser and its fuzzing land at 4a, the proof at 4b |
| 16 | Anti-rollback floor state machine | B/T at 4b | TPM CRB/TIS | over an abstract TPM NV counter with a power cut injected at every step, covering both the kernel's raise code and the loader's floor code: the floor never decreases; an image is accepted only if its SVN is at least the floor; after a TPM clear the floor is max(the minimum config SVN built into the loader, the highest SVN present); the replay value is counter minus a write-once base and never overflows or underflows, across every reseal and every clear recovery; while the system is up, objects 1 to 3 in [boot.md](boot.md) are resealed at the new state, each new object's authPolicy matched against a trial-session policy digest, and kept in two copies; on the next boot the loader hands over its check of the other configs' signatures and SVNs, the kernel really unseals each new object before capping, and only then raises the floor (increments the counter); after a power cut at any step, the machine boots either the old or the new state without the recovery key |
| 17 | TPM policy composition | B/T at 4b | target 14 (SHA-256); TPM CRB/TIS | Kani over an abstract model of TPM policy-digest semantics (`PolicyPCR`, `PolicyOR`, `PolicyAuthorize`, `PolicySecret`, `PolicyNV`, `PolicyAuthValue`; axiom: the TPM 2.0 Part 1 policy chapter), instantiated with every sealed object, NV index and signing reference in [boot.md](boot.md) (`/f`, `-pm/f`, the `optimal-floor-delegate-v1` chain) and every digest each signing ceremony signs: for every object, every session that satisfies its authPolicy implies PCR 4 matched (the signed value or the per-machine value), replay counter <= base+f, the PIN-epoch NV index equal to its current value (objects 1 and 2), and the PIN authValue (objects 1, 2, 4 and 5). Seeded bugs: drop a `PolicyNV` step, add a `PolicyOR` branch without PCR 4, sign a digest under the wrong reference |
| 18 | Channel graph | B/T at 5; P/T at 7 | target 5 | over the pure graph crate of "Direct channels make one principal" ([capabilities.md](capabilities.md)), after any sequence of channel opens, grants, approvals, logouts and logins (grant-created edges rebuilt at login): no networked identity's upstream data set (the union of the data sets of every identity with a directed path to it) is outside a combined approval naming it; and an open or grant is refused or asked only when allowing it would break that. So A (data) → B → C (network) is refused at the second join, an approval is asked again when either side grows, and terminal reads add no edge. Seeded bugs: drop the downstream search on a grant, treat edges as undirected |

## Not provable here, and tested instead

Lockdown (every row of the table in [threat-model.md](threat-model.md)), the
Linux ABI's behaviour, driver logic against real hardware, side-channel
resistance, and anything that depends on firmware. These have tests and
fuzzing, and are reported as level T.
