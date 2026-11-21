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
rank above T; a mix of B and M is reported as "B+M". Levels are written
**own/effective**, for example "P/T" for a P proof over tested shims, and
once when both are equal. The ledger script in CI computes the effective
level from `proofs.toml` and `axioms.md` and fails if it differs from the
expected value in the table below; every roadmap proof gate names the
target's own level with the expected effective level next to it, and the
scorecard reports both.

## Tools and how proofs see the real code

- **Kani** (MIT/Apache-2.0): bounded model checking of real Rust functions.
  It can't handle inline assembly, threads or weak memory, doesn't check
  aliasing rules (Stacked or Tree Borrows), and compiles for the host with
  its own toolchain.
- **Verus** (MIT): unbounded proofs of invariants over abstract state
  machines plus refinement of the real code. Starts in **Phase 3**, first on
  the capability table (target 5), then page tables (target 1 moves from B
  to P). Verus needs its `builtin`, `builtin_macros` and `vstd` crates;
  they are vendored and audited under the procedural-macro rule in
  [core.md](core.md). Spec and proof code is erased at compile time. Shipped
  crates may use only the erased ghost-only `vstd` items (`Tracked`,
  `Ghost` and spec functions); CI rejects any executable `vstd` item
  (`PPtr`, `PAtomicU64` and the like) reached from a shipped crate. Verus
  reasons about the real `core::sync::atomic` operations (page-table
  entries are `AtomicU64`) through `assume_specification` items, each a
  named toolchain axiom citing the Rust atomics documentation. Verus proves
  functional properties of safe code; it does not prove the absence of
  undefined behaviour in `unsafe` code, which stays at B (Kani) plus Miri
  (target 3a). Verus's own rustc version is recorded in `proofs.toml`.
- **loom** (MIT): exhaustive interleaving checks of concurrent code compiled
  for the host, with the preemption bound of each model recorded in
  `proofs.toml`; this is level M. Data shared with interrupt handlers is
  declared through loom's cell and atomic types under `cfg(loom)`, so loom
  sees every access. **shuttle** (Apache-2.0) explores randomised
  schedules of larger configurations; that is level T. Both start in Phase 2.
- **Miri**: runs the host test suite of every pure tier-0 crate and reports
  undefined behaviour. Lands in Phase 1a with the proofs job.
- Property-based tests and fuzzing check the same properties on the real
  kernel in QEMU, so a proof and a test disagreeing is caught.
- **Two toolchains.** Each tool pins its own rustc. CI builds and runs the
  tests of every proved crate with the shipping toolchain and with each
  tool's toolchain, on every change, so a crate that only one compiler
  accepts, or that behaves differently under one, fails.

**Proved code is host-compilable.** Every proved module is split into a pure
crate (no `asm!`, no target-specific `cfg`, no hardware access) containing
the logic, and a thin shim that does the hardware part (writing CR3,
`INVLPG`, MMIO). CI fails if a proved crate contains `asm!` or `cfg(target_*)`.
Every tier-0 module with `asm!`, a target-specific `cfg` or hardware access
is a shim. Each shim function has a written contract (what the instruction
does, from the manual section it cites). The contract is an our-code
contract in the ledger, discharged by target 3b (QEMU test per function plus
the unsafe review) at T, so it lowers the effective level of every target
that rests on it. Shim code is capped at 3,000 lines and `core-crypto` at
4,000 lines, both inside the 15,000-line tier-0 cap, with a per-crate shim
estimate in [core.md](core.md); all three counts are on the scorecard. So
the proofs check the same source that ships; the remaining gap is the
compiler and the shims.

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
states. Target 1 covers the mapping rules and target 3b the shim.

## The assumption ledger

Each proof's assumptions are listed in `design/axioms.md` (created with the
first proof), in two separate sections. Every assumption is either:

- **discharged** by another target's guarantee (assume-guarantee: target 1
  assumes "only `core-mm` writes page tables", which target 3a and the crate
  boundary guarantee). The **our-code contracts** section holds the
  only contracts on our own code that proofs rely on, the shim contracts,
  each discharged by target 3b; or
- a **named axiom** in the **hardware, vendor and toolchain axioms**
  section: an Intel SDM or AMD APM section (4-level and 5-level
  paging walk, TLB and PCID behaviour, `INVLPG`, x86-TSO memory ordering),
  the IOMMU and TPM 2.0 specifications, the `assume_specification` items
  for `core::sync::atomic`, "RNG output uniform; salt collision <= 2^-64
  per volume lifetime" (used only by target 12c, which is therefore
  reported "B, probabilistic"), "the page-table walker reads
  each entry with a single-copy-atomic 64-bit load" (which is why the shims
  write entries through `AtomicU64`, never plain stores: Rust code follows
  the C++20 memory model, not x86-TSO, and a plain store raced by the
  hardware walker would be a data race), "the walker may set the Accessed
  and Dirty bits of any present entry at any time with a locked
  read-modify-write" (SDM Vol. 3A §4.8, which is why present entries change
  only through `swap`, `compare_exchange` or `fetch_and`, never load then
  store, so a Dirty bit the walker set can't be lost), rustc and LLVM compile correctly,
  each tool's own compiler front end (Kani, Verus and Miri pin their own
  rustc versions, recorded in `design/proofs.toml`) treats the code the same
  as the shipping compiler (narrowed by the two-toolchain CI job), and Verus's
  erasure of spec and proof code (`verus!` and `builtin_macros`) leaves the
  executable code unchanged. Kani, Verus and loom are assumed sound.

A CI script fails if any proof cites an assumption that is neither discharged
nor on the axiom list.

The ledger also lists what each tool does not check, so the scorecard can't
imply more: Kani, paths beyond each loop's unwinding bound, concurrency,
inline assembly and aliasing rules (Stacked or Tree Borrows); loom,
interleavings beyond the recorded preemption bound
and `Relaxed` behaviours it doesn't generate (load buffering); Verus, every
`external_body`, `external` and `assume_specification` item (each also a
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
  that.
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
levels and phases, and fails if any design file calls a target "proved"
when it isn't at P (or M for concurrency). Phase 1 is split into 1a (CPU
setup and exceptions), 1b (memory), 1c (ACPI, PCI, interrupts, IOMMU and
time) and 1d (console); Phase 4 into 4a (storage) and 4b (trusted boot
chain). The file system track starts after 4a.

| # | Target | Level and gate (own/effective) | Property |
|---|---|---|---|
| 1 | Page-table manipulation | B/T at 1b; P/T at 3; writeback clause and its QEMU test extended at 4a (P/T) | `map`/`unmap`/`protect` only change the entries they were asked to; present entries change only by `swap`/`compare_exchange`/`fetch_and`, and the Dirty bit of a replaced or write-protected entry is moved to its page before the entry is gone (the Kani harness runs a nondeterministic walker that may set A/D bits between any two steps); outside the direct map, never create a writable+executable kernel mapping and never map one frame writable at two virtual addresses unless asked; the only writable alias of a kernel-text frame is the text-poke mapping, which is non-executable, present only in the poking page table and only during a poke; direct-map aliases of kernel text are read-only; the direct map is no-execute, supervisor-only and never reachable through an entry with the user bit set; intermediate tables stay well-formed; translation matches the hardware walk (axiom: SDM paging chapter). Extended at 4a, when the page cache lands: writeback of a page starts only after the shootdown of its write-protect completes, plus a QEMU test that writes through a file mapping while its page is under writeback and checks no write is lost |
| 1s | TLB shootdown protocol | M/T at 2; re-gated at M/T at 3 (PCIDs and user address spaces) and at 5 (INVLPGB and transparent huge pages) | written as a state machine with per-CPU acknowledgement generations: **no CPU can use a stale translation** or paging-structure entry for a frame, including a freed page-table page, once the frame is returned to the allocator. The two obligations, using the may-cache set and PCID generations of [scheduler.md](scheduler.md): every CPU in the address space's may-cache set at the unmap has acknowledged an invalidation issued after it, and on every other CPU each PCID slot holding that address space has a recorded generation older than the unmap, so reloading it forces a full flush first; a PCID or global ASID is reassigned only after a full flush. Axioms: an IPI that is sent is delivered and its handler runs; `INVLPG` and `INVPCID` semantics and `MOV CR3` with bit 63 set not flushing the new PCID (SDM Vol. 3A §4.10.4); paging-structure caches are invalidated by the same operations as leaf entries (§4.10.3); a mapping whose page size changes is invalidated before reuse (§4.10.2.3); `INVLPGB` followed by `TLBSYNC` completes the invalidation on every CPU (AMD APM Vol. 3). Plus a QEMU test that delays acknowledgements and checks the frame isn't reused |
| 2 | Physical frame allocator | B/T at 1b (owners: allocator and page tables); owner kinds extended at 3 (memory objects) and 4a (`DmaBuffer` and DMA pins); P/T at 4a | a frame is never handed out twice without being freed; freed frames were allocated; never returns reserved or out-of-range memory; every frame has exactly one owner among the kinds that exist at that gate (allocator, a page table, a memory object, a `DmaBuffer`), plus any number of recorded DMA pins on frames owned by a memory object; a pinned frame is never freed |
| 3a | Soundness of tier-0 safe APIs (pure crates) | B per API from 1a; M for interrupt re-entry at 2 (B+M from then); P/B+M at 5 for the functional invariants of the safe-only pure capability and memory APIs | every `pub` safe item of a tier-0 crate is sound for **arbitrary safe callers**, including ones that leak, `forget`, panic or re-enter ([core.md](core.md)). Kani harnesses drive each API with nondeterministic call sequences including `forget`; loom models interrupt re-entry as a preempting thread gated by the interrupt-flag state; compile-fail tests check that tokens and guards can't escape or cross threads; Miri runs every pure crate's tests; every `unsafe impl Send`/`Sync` is inventoried with a justification. Freedom from undefined behaviour in `unsafe` code stays at B plus Miri at every phase (Kani doesn't check aliasing; Miri covers the aliasing rules on the paths the tests run); the P step at 5 proves functional invariants of APIs whose implementation is safe code only. Also a 3a obligation from 3: user page-table entries are installed only through the single `core-mm` user-PTE API (target 4) |
| 3b | Shim soundness | T from 1a; text-poke shim at 2 | each shim function does what its contract (an our-code contract citing the manual section) says, for every argument its safe wrapper can pass: a QEMU test per shim function plus the unsafe-inventory review of its module. Shim code stays within its 3,000-line cap. Includes the text-poke shim from 2 (a QEMU test toggles a trace point under a load running on every CPU and checks no CPU faults or executes a mixed state) |
| 4 | Memory isolation between processes | P/T at 3 for the sharing paths that exist then; extended at 5 (shared memory, memfd, `SCM_RIGHTS`, `ptrace`, `process_vm_*`, io_uring buffers), 7 (dma-buf) and 8 (GPU buffers) | a user mapping in process A reaches a frame only if A holds a capability for the memory object that owns it; kernel memory is never user-accessible. Proved over one `core-mm` API that installs user page-table entries and takes a memory-object capability witness, plus the capability table; every syscall, the page-fault handler, copy-on-write and file mappings reach user PTEs only through it, and the sharing paths are the closed list in `proofs.toml` ([capabilities.md](capabilities.md)). A CI call-graph check fails if anything other than that API reaches the PTE-writing shim |
| 5 | Capability derivation and revocation | P/T at 3; the per-syscall test T at 3 | derived capabilities never have more rights than their parent; revocation removes the whole subtree, including every open file description (OFD) derived from the object and every fd slot that shares an OFD through `dup`, `fork` or `SCM_RIGHTS`, and every mount derived from a view grant, including copies made by `CLONE_NEWNS`; a revoked handle behaves as defined in [capabilities.md](capabilities.md). Plus a per-syscall property test that no input yields a capability, OFD or mapping the caller didn't hold |
| 6 | User-copy routines | B/T at 3 | the pure part of the `UserPtr` copy routines (range checks, chunking, the read-once wrapper) only touches the user range it was given, never a kernel address, turns a fault into an error, and reads each user value once; the `stac`/`rep movsb`/`clac` instructions and their exception-table fixup are a shim (target 3b). That no syscall or driver entry point reaches user memory any other way follows from `UserPtr<T>` having no dereference and raw pointers not existing outside tier 0 (target 3a) |
| 7 | Resource accounting | B/T at 2 for scheduler CPU accounting; extended at 4a (B/T) to memory and interrupt budgets and the driver real-time cap | budgets ([drivers.md](drivers.md)) and the driver real-time cap ([scheduler.md](scheduler.md)) are charged and released exactly once, never go negative, and a component can't exceed its budget |
| 8 | Scheduler invariants | M/T at 2 for run queues and priority inheritance (loom); B at 2 for real-time throttling (Kani over an abstract clock) | every runnable thread is on exactly one run queue; class order and priority inheritance rules in [scheduler.md](scheduler.md) hold; real-time threads never run past their throttling budget in any period, for every sequence of clock readings up to the bound. (A thread running on two CPUs is prevented by the core's move-only `RunnableThread`, not by this policy-level proof, so a policy bug can't cause undefined behaviour) |
| 9 | Driver message boundary | B/B+M at 4a | ring code never reads a descriptor field twice; never uses a driver-supplied index without bounds-checking it; every buffer handle resolves only to a buffer the kernel granted to that driver |
| 10 | IOMMU tables and DMA ownership | B/T at 1c; invalidation-before-free order M/T at 2; streaming maps extended at 4a; P/T at 4a | a device's domain maps only frames owned by its live `DmaBuffer`s or, from 4a, pinned for it by a streaming map; context and domain tables stay well-formed, with every unused context entry blocking and every unused AMD-Vi device table entry set to block DMA and interrupts; interrupt remapping entries only target vectors the core allocated. The invalidation-before-free order is checked at level M/T at 2, as part of the same state machine as target 1s, with "a queued invalidation's wait descriptor completes only after the invalidation" as the axiom |
| 11 | SMP locks and per-CPU data | M/T at 2 | spinlocks give mutual exclusion and are IRQ-safe as typed; per-CPU access tokens are never held across a context switch |
| 12a | Storage: allocator, commit order, restart | B at 4a | the block allocator never double-allocates ([storage.md](storage.md)); a commit is issued only after the writes it covers have completed; after a driver restart, every write not covered by a completed flush is re-sent and treated as not durable |
| 12b | Storage: volume map and XTS tweak | B at 5 | the per-user volume map's extents never overlap and stay inside the device; the XTS sector-to-tweak mapping is injective per key |
| 12c | Own file system: AEAD nonces | B, probabilistic; gated by the file system track's done-when | no AEAD nonce repeats under one key, including after a crash, a fallback to an older commit or a replayed image: each mount draws a 128-bit random prefix and counts writes with a 64-bit counter; XChaCha20-Poly1305 uses the 192-bit prefix and counter directly, and a 96-bit-nonce suite (AES-256-GCM) uses a per-mount subkey HKDF(key, prefix), the key being the per-file key for class-B files and the volume or class key otherwise, with the counter zero-padded to 96 bits as its nonce; a counter about to wrap draws a new prefix first, so no (key, nonce) pair repeats. Rests on the RNG axiom (collision at most 2^-64 per volume lifetime). The own file system's further proofs are in `fs/docs/DESIGN.md` |
| 13 | GPU address-space isolation | B/T at 8 | a GPU command stream from process A can only reach GPU virtual addresses mapped in A's GPU address space, which map only buffer objects A holds a capability for; GPU page-table entries are built only from `DmaBuffer` or `Iova` handles, on D1 and D2 ([drivers.md](drivers.md)) |
| 14 | Kernel cryptography | T from 4a | `core-crypto` and the kernel's other cryptographic code (AES-XTS, AES-GCM and GHASH, SHA-256, ChaCha20-Poly1305, HKDF, Argon2id) match the NIST CAVP vectors, the Wycheproof vectors and the RFC vectors where they exist, and agree with a reference implementation in differential tests over random inputs; every function that handles key or plaintext bytes has no secret-dependent branch or memory index, checked by running it under a ctgrind-style tool with secrets marked undefined. Verified generated code (HACL*/libcrux-derived, fiat-crypto) is used wherever it exists for the algorithm and instruction set, and counts in the `core-crypto` cap |
| 15 | Network packet parsers | B at 6 | every in-kernel packet parser (Ethernet, ARP, IPv4, IPv6 and its extension headers, ICMPv4 and ICMPv6, UDP, TCP with options, DHCPv4 and DHCPv6, and DNS in the resolver) never panics, never indexes out of range and never reads beyond the frame, for every frame up to the bound recorded in `proofs.toml` (at least 128 bytes, which covers every fixed header and its options up to that length); longer frames are fuzzed |
| 16 | Anti-rollback floor state machine | B at 4b | over an abstract TPM NV counter with a power cut injected at every step: the floor never decreases; an image is accepted only if its SVN is at least the floor; counter minus base never overflows or underflows; a raise increments the counter only after every sealed object in [boot.md](boot.md) is resealed at the new state, test-unsealed and kept in two copies; after a power cut at any step, the machine boots either the old or the new state without the recovery key |

## Not provable here, and tested instead

Lockdown (every row of the table in [threat-model.md](threat-model.md)), the
Linux ABI's behaviour, driver logic against real hardware, side-channel
resistance, and anything that depends on firmware. These have tests and
fuzzing, and are reported as level T.
