# Scheduler, preemption, time and memory management

Performance is pillar 1, and for a desktop that mostly means latency: how
fast interactive work gets a CPU, how evenly frames are paced, and whether
audio keeps up under load. This file is the design that the targets in
[benchmarks.md](benchmarks.md) are measured against.

## Preemption model

- **The kernel is fully preemptible.** Kernel code can be preempted anywhere
  except inside explicit non-preemptible sections (holding a spinlock,
  interrupts off, per-CPU access). Each CPU keeps a preempt count; the core's
  types enforce it ([core.md](core.md): per-CPU data needs a
  `PreemptDisabled` token).
- **Bounded non-preemptible sections.** No section with preemption or
  interrupts disabled may last longer than **50 µs** on the reference
  desktop. A built-in tracer (always compiled in) records the longest
  sections. When off, each trace point is a patched-out jump (a no-op until
  the tracer is switched on); its cost when off is at most 0.5% on the
  syscall round-trip microbenchmark against a build with the tracer compiled
  out, a Phase 3 gate ([benchmarks.md](benchmarks.md)). In virtual machines
  the host can stall a vCPU at any moment, so there the tracer only reports;
  the limit is enforced (CI fails above it) on the bare-metal reference
  machines, which run the stress suites nightly from Phase 2. The target
  tightens to 20 µs in Phase 10.
- **Interrupt handlers are threaded.** Hard-IRQ handlers live in the core and
  only acknowledge the device and wake the driver's handler thread, which is
  an ordinary schedulable thread with its own priority. Audio, input and
  timer threads get real-time priority, so a slow disk handler can't delay
  them.
- **Priority inheritance.** Kernel sleeping locks are rt-mutexes with
  priority inheritance. User space gets PI futexes (`FUTEX_LOCK_PI`), which
  glibc's `PTHREAD_PRIO_INHERIT` mutexes need (milestone 2 in
  [compat.md](compat.md)).

## Scheduling classes (the Linux ABI, implemented)

Highest first. Each class only runs when no higher class has a runnable
thread on that CPU.

| Class | Linux policy | Algorithm | Phase |
|---|---|---|---|
| Deadline | `SCHED_DEADLINE` | earliest deadline first with constant-bandwidth admission control | 10 (until then `sched_setattr` with `SCHED_DEADLINE` returns `EINVAL`, documented) |
| Real-time | `SCHED_FIFO`, `SCHED_RR`, priorities 1 to 99 | fixed-priority, per-CPU run queues, push and pull balancing | 2 (kernel threads), 5 (user ABI) |
| Fair | `SCHED_OTHER`, `SCHED_BATCH` | EEVDF (earliest eligible virtual deadline first) with `nice` weights and a per-thread requested slice (`sched_attr.sched_runtime`, as in Linux 6.12 and later, clamped to 0.1 to 100 ms); see below | 2 |
| Idle | `SCHED_IDLE` | runs only when nothing else wants the CPU | 2 |

- **Real-time throttling.** RT threads may use at most 95% of each CPU per
  1 s period by default (the Linux default), so a runaway RT thread can't
  lock the machine. Apps get RT priority only through the rtkit-compatible
  portal, which caps the priority and requires `RLIMIT_RTTIME`.
- **Why EEVDF.** It gives each thread a fair share and also a deadline that
  depends on its requested time slice, so short, latency-sensitive work
  (input handling, the compositor, a game's render thread) runs sooner
  without starving anything. It is published research with no licence
  concern; our implementation is our own.
- **Desktop awareness.** The compositor tells the scheduler which app is in
  the foreground (through the seat service). Threads of the foreground app
  that haven't set `sched_runtime` themselves get half the default slice
  (an earlier virtual deadline), and background apps' threads twice it; a
  slice an app sets explicitly wins. This changes *when* threads run, not
  their fair share, so it can't starve background work.
- **Groups.** cgroup v2 `cpu.weight` and `cpu.max` are implemented with
  hierarchical fair scheduling (Phase 5, needed for milestone 6). The desktop
  puts each app in its own group, so an app with 200 threads gets one app's
  share, not 200 threads' worth.
- **Benchmark core isolation** (Phase 2). The boot option `isolcpus=<set>`
  removes those CPUs from load balancing, from unbound kernel threads and
  timer migration, and from the default destination of device interrupts
  (I/O APIC and MSI destinations go to the other CPUs; interrupt remapping
  enforces it); they run tickless when they have one thread. Only threads
  placed there by affinity run on them. The benchmark harness uses it from
  Phase 2, and matched Linux uses `isolcpus=domain,managed_irq,<set>
  nohz_full=<set>` with the same set.
- **Affinity** (`sched_setaffinity`) and `sched_yield`, `nice`,
  `getpriority`/`setpriority`, `sched_getattr`/`sched_setattr` are all
  implemented with Linux semantics (Phase 5).

## Topology, load balancing, hybrid CPUs, NUMA

- **Topology** comes from CPUID leaves 0xB/0x1F (and 0x8000_0026 on AMD),
  the ACPI MADT, PPTT and SRAT, building scheduling domains: SMT siblings,
  cores sharing a cache, dies or CCDs, NUMA nodes. Load balancing works up
  this hierarchy, preferring to move work within the closest domain.
- **Hybrid CPUs (Intel P/E cores), in two steps.** Phase 2: core types and
  relative capacity from CPUID 0x1A and ACPI CPPC, and capacity-aware
  placement (a thread that keeps a big core busy moves to a big core).
  Phase 10: Intel Thread Director / HFI feedback and CPPC preferred-core
  rankings.
- **AMD X3D (cache dies with no hardware hint).** Phase 10: a policy, shown
  in Settings, to prefer the cache die for the foreground app (default for
  games) or the frequency die.
- **NUMA.** The frame allocator is node-aware from Phase 1b (allocations come
  from the requesting CPU's node first), the scheduler keeps threads near
  their memory, and memory-only and CPU-only nodes are handled (the tough
  tests include a memoryless node). `mbind`, `set_mempolicy` and
  `get_mempolicy` are implemented in Phase 5; automatic NUMA balancing is a
  Phase 10 item. NUMA is tested for function only (QEMU NUMA topologies)
  until the multi-node reference machine ([benchmarks.md](benchmarks.md)) is
  online; the cross-node bandwidth and latency benchmark and any NUMA
  placement claim wait for it.
- **SMT and side channels.** On CPUs affected by MDS, L1TF, TAA or RFDS (the
  models are listed in the mitigation matrix in
  [threat-model.md](threat-model.md)), **SMT is off by default**, because a
  sibling can read kernel data and core scheduling can't prevent that. Core
  scheduling (threads of different apps never on sibling hyperthreads at the
  same time) is app-to-app protection only: opt-in on affected CPUs, where
  turning SMT on is a named exposure, and on by default only on CPUs where
  siblings can't leak kernel data. It lands in Phase 3, when there are user
  processes to separate. The Phase 3 leak test runs on the shipped default
  for each CPU model, plus a separate test with SMT and core scheduling
  turned on; the cost of each setting is measured and published, and the
  Linux baseline boots with the same SMT setting (`nosmt` on affected CPUs).

## Time and idle

- **Time sources** (Phase 1c). The clock is the TSC when CPUID
  0x8000_0007 reports it invariant and the boot-time sync check across CPUs
  (with `IA32_TSC_ADJUST` where present) passes; otherwise HPET, then the
  ACPI PM timer. TSC frequency comes from CPUID 0x15/0x16 when they give it,
  else is calibrated against HPET or the PM timer, else the PIT. A
  clocksource watchdog compares the TSC with HPET or the PM timer every
  0.5 s and, if the TSC deviates by more than 100 ppm over 10 consecutive
  checks, switches to that source and logs it. Timer events use the LAPIC
  TSC-deadline mode, falling back to LAPIC one-shot, then HPET, then the
  PIT. The drift test measures against an independent reference only, never
  the calibration source: on KVM against the host's `CLOCK_MONOTONIC_RAW`
  read over virtio-serial (under 1 ppm over 10 minutes, the Phase 1 gate),
  and on bare metal against a PPS or NTP reference (the result is the board
  crystal's error, which no kernel can remove without an outside reference;
  published, and at most 50 ppm).
- **High-resolution timers and a tickless kernel** are part of the Phase 2
  design, not Phase 10: timers are kept in per-CPU structures with
  nanosecond expiry on the timer event source above, and an idle CPU takes
  no periodic tick. A CPU running one thread can also stop its tick (adaptive
  tick) once Phase 10 tunes it.
- **Frequency control before any real-hardware comparison.** In Phase 2,
  before the first bare-metal benchmark against Linux, Optimal enables hardware
  P-states (Intel HWP, AMD CPPC) and sets the same energy-performance
  preference as the Linux baseline. Otherwise every comparison measures the
  firmware's default frequency, not the kernel. Idle states beyond C1 (with
  an idle governor) come in Phase 10, along with suspend.

## Memory management performance

Memory pressure is the main cause of desktop stutter, so the MM design is
explicit:

- **Frame allocator.** Buddy allocator per NUMA node with per-CPU caches of
  free frames, so most allocations take no lock. 4 KiB, 2 MiB and 1 GiB
  frames.
- **TLB: PCID allocation** (Phase 3, with user address spaces). PCID is used
  whenever the CPU has it (essential to keep page-table isolation cheap).
  Each CPU keeps 6 dynamic PCID slots, each holding an address-space id and
  the address space's flush generation at the time the slot was last valid;
  with page-table isolation the user half uses the same PCID with bit 11
  set. Switching to an address space already in a slot whose generation
  matches loads CR3 with the no-flush bit (bit 63); otherwise the least
  recently used slot is taken and CR3 is loaded with a flush. Every unmap
  bumps the address space's generation and a CPU that completes a shootdown
  records the new generation in its slot, so a CPU can never reuse a stale
  PCID without flushing.
- **TLB: "may cache" tracking.** Each address space has a CPU set of CPUs
  that may cache its translations: a CPU joins when it loads the address
  space's CR3, and stays while it runs a kernel thread with that CR3 still
  loaded (lazy switch). A shootdown goes to every CPU in the set; a lazy CPU
  that receives one switches to the kernel-only page table and leaves the
  set. A CPU leaves the set otherwise only when it loads another address
  space's CR3; translations it still holds under an unloaded PCID are covered
  by the generation rule above. This set is exactly what
  [verification.md](verification.md) target 1s means by "may cache".
- **TLB: shootdown paths.** Remote shootdowns are batched per unmap
  operation. The IPI path lands in Phase 2 and the INVLPGB path in Phase 5.
  Without INVLPGB, the initiator sends an IPI to every other CPU in the set
  and waits for each acknowledgement generation. On AMD CPUs with INVLPGB,
  address spaces that run on more than one CPU get a **global ASID**: one
  PCID, the same on every CPU, from a global pool (PCID values 7 to 2047,
  above the 6 per-CPU dynamic slots), because INVLPGB invalidates by PCID on
  all CPUs at once. Every CPU with INVLPGB is immune to Meltdown, so
  page-table isolation is off there and no user-half PCID needs a second
  invalidation. The initiator, with preemption disabled so it can't migrate,
  issues INVLPGB for each range in the batch (with the page-structure-cache
  bit set whenever page tables were freed) and then one TLBSYNC, which waits
  until those invalidations have completed everywhere. When the global pool
  runs out, freed global ASIDs are recycled only after a full flush of those
  ASIDs on every CPU (INVLPGB by ASID, then TLBSYNC); until then the address
  space falls back to a dynamic PCID and IPIs. A frame is reused only after
  its shootdown has completed on every CPU in the set
  ([verification.md](verification.md) target 1s: level M at Phase 2 for the
  IPI path, re-gated at M in Phase 3 with PCID and lazy switch and in Phase
  5 with INVLPGB and THP).
- **Huge pages** (Phase 5). Transparent huge pages for anonymous memory on
  fault where the region is aligned and large enough, plus background
  collapse; a fault never waits for compaction (if no 2 MiB frame is free it
  maps 4 KiB and leaves the region to background collapse), the same as
  Linux's `always` with `defrag=madvise`. Page cache huge folios for large
  files in Phase 10.
- **Reclaim.** A multi-generation LRU style reclaim with refault detection
  (pages that come back soon after eviction were evicted wrongly, and the
  policy adapts). The foreground app's pages are protected from reclaim
  longer than background apps'.
- **Swap and compression.** Compressed in-memory swap (zram-style, our own
  implementation, LZ4 or Zstd from permissively licensed crates) is on by
  default, with optional encrypted disk swap. Phase 5.
- **Pressure information and OOM.** Pressure stall information
  (`/proc/pressure/*`, needed by systemd-oomd-style daemons and by the
  desktop) is implemented in Phase 5. The kernel OOM killer is the last
  resort; a user-space OOM service acts earlier, killing the least important
  background app first, never the compositor or the foreground app.
- **Benchmarks** (added to [benchmarks.md](benchmarks.md)): TLB shootdown
  latency against core count (Phase 2); 4 KiB fault cost, mmap/munmap and
  page-fault scaling across cores (Phase 3); 2 MiB fault cost (Phase 5); and a memory pressure test in
  Phase 7, when there is a desktop and a browser to run: the machine is
  limited to 8 GiB (`mem=8G` on both kernels), a browser opens 50 pages from
  a pinned offline corpus served locally, and a pinned compile runs
  alongside, measuring input latency P99, frame time and OOM kills while it
  runs, against the memory-pressure target in
  [benchmarks.md](benchmarks.md).

## Driver budgets and real-time work

Driver CPU and interrupt budgets ([drivers.md](drivers.md)) are charged to the
driver's handler thread, which runs under the scheduler like any other thread.
A driver's real-time threads may use at most 5% of a CPU per 100 ms window
(the cap in [drivers.md](drivers.md)); past that they drop to the fair class
until the window ends. So a driver over budget is slowed by the scheduler
(it gets its share and no more) rather than by masking its interrupt, which would cause audio underruns
or I/O timeouts. Interrupt masking is reserved for genuine storms (the
per-class limits over 10 ms windows in [drivers.md](drivers.md)) and is
reported on the scorecard whenever it happens.

## Proof and test obligations

- [verification.md](verification.md) target 8 (level M at Phase 2) covers:
  every runnable thread is on exactly one run queue; a higher class always
  runs before a lower one on the same CPU; RT throttling never lets RT use
  more than its budget; priority inheritance chains are acyclic and restore
  priorities when released. Run-queue membership, class order and priority
  inheritance are model-checked with loom on the host (preemption bound
  recorded in the ledger); RT throttling is checked with Kani over an
  abstract clock; property tests run in QEMU. (No thread runs on two CPUs is
  guaranteed by the core's move-only `RunnableThread`, not by this check.)
- Target 7 at level B in Phase 2 covers the scheduler's CPU accounting only
  (the RT throttle and the driver real-time cap above); memory and interrupt
  budgets join it in Phase 4a.
- Phase 2 tests: priority-inheritance chains (nested and across classes),
  capacity-aware placement on a simulated hybrid topology, tickless idle
  (no periodic tick on an idle CPU over 10 s) and timer expiry accuracy.
- The latency tracer and the stress suite run in every CI boot from Phase 2
  (report-only in VMs, enforced on bare metal, as above).
