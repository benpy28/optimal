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
  desktop. A built-in tracer (always compiled in, near-zero cost when off)
  records the longest sections; CI fails if any exceeds the limit during the
  stress suites. The target tightens to 20 µs in Phase 10.
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
| Fair | `SCHED_OTHER`, `SCHED_BATCH` | EEVDF (earliest eligible virtual deadline first) with `nice` weights and a per-thread latency hint (`latency-nice`); see below | 2 |
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
  the foreground (through the seat service). Threads of the foreground app get
  a lower latency hint (shorter virtual deadline), and background apps a
  higher one. This changes *when* threads run, not their fair share, so it
  can't starve background work.
- **Groups.** cgroup v2 `cpu.weight` and `cpu.max` are implemented with
  hierarchical fair scheduling (Phase 5, needed for milestone 6). The desktop
  puts each app in its own group, so an app with 200 threads gets one app's
  share, not 200 threads' worth.
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
  Phase 10 item. A cross-node memory bandwidth and latency benchmark joins
  the suite in Phase 2.
- **SMT and side channels.** On CPUs affected by L1TF/MDS-class bugs, core
  scheduling keeps threads of different apps (and the kernel's sensitive
  work) off sibling hyperthreads at the same time
  ([threat-model.md](threat-model.md)). The cost is measured and published.

## Time and idle

- **High-resolution timers and a tickless kernel** are part of the Phase 2
  design, not Phase 10: timers are kept in per-CPU structures with
  nanosecond expiry using the TSC deadline timer, and an idle CPU takes no
  periodic tick. A CPU running one thread can also stop its tick (adaptive
  tick) once Phase 10 tunes it.
- **Frequency control before any real-hardware comparison.** Before the
  first bare-metal benchmark against Linux, Optimal enables hardware
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
- **TLB.** PCID is used whenever the CPU has it (essential to keep
  page-table isolation cheap). Remote TLB shootdowns are batched per unmap
  operation, sent only to CPUs that may cache the address space (tracked per
  address space), and use INVLPGB on AMD CPUs that support broadcast
  invalidation. A frame is reused only after its shootdown has completed on
  every such CPU ([verification.md](verification.md) target 1).
- **Huge pages.** Transparent huge pages for anonymous memory on fault where
  the region is aligned and large enough, plus background collapse; page
  cache huge folios for large files in Phase 10.
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
- **Benchmarks** (added to [benchmarks.md](benchmarks.md)): 4 KiB and 2 MiB
  fault cost, mmap/munmap and page-fault scaling across cores, TLB shootdown
  latency against core count, and a memory pressure test (a browser with 50
  tabs plus a compile in 8 GiB of RAM) measuring input latency P99 and
  frame time while it runs.

## Driver budgets and real-time work

Driver CPU and interrupt budgets ([drivers.md](drivers.md)) are charged to the
driver's handler thread, which runs under the scheduler like any other thread.
So a driver over budget is slowed by the scheduler (it gets its share and no
more) rather than by masking its interrupt, which would cause audio underruns
or I/O timeouts. Interrupt masking is reserved for genuine storms (rate far
above anything the device class can legitimately produce) and is reported on
the scorecard whenever it happens.

## Proof and test obligations

- [verification.md](verification.md) target 8 covers: every runnable thread
  is on exactly one run queue; no thread runs on two CPUs; a higher class
  always runs before a lower one on the same CPU; RT throttling never lets RT
  use more than its budget; priority inheritance chains are acyclic and
  restore priorities when released. Checked with loom-style model checking
  of the run queue code on the host, plus property tests in QEMU.
- The latency tracer and the stress suite run in every CI boot from Phase 2.
