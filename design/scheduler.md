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
  sections. When off, each trace point is a 5-byte no-op; switching the
  tracer on or off rewrites trace points between exactly two precompiled
  states (no-op and jump to the trace stub), never any other bytes. The
  rewrite is the core's text-poke path: the target page is mapped writable
  only at a temporary private address on the poking CPU, the first byte is
  replaced with `int3`, every CPU is synchronised by IPI with a serialising
  instruction, the remaining bytes are written, a second IPI round runs,
  then the first byte is written and a third round runs (the
  cross-modifying-code protocol of the Intel SDM Vol. 3A §8.1.3). Direct-map
  aliases of kernel text are read-only, so kernel text is never writable at
  two addresses. The mechanism is covered by
  [verification.md](verification.md) target 1 and listed in the shim
  inventory; lockdown permits only the two precompiled states. Its cost when
  off is at most 0.5% on the
  syscall round-trip microbenchmark against a build with the tracer compiled
  out, both built with the fixed code alignment of
  [benchmarks.md](benchmarks.md) so the gate measures the trace points, not
  link layout, a Phase 3 gate. The tracer reads the
  SMI count (`MSR_SMI_COUNT` on Intel, the SMIs-received PMC event on AMD) at
  the entry and exit of each section; sections an SMI overlapped are
  firmware time, so they are reported separately with their count published
  and excluded from the 50 µs gate (the NMI watchdog likewise leaves SMI
  time out of its 10 ms and only reports in VMs, [drivers.md](drivers.md)).
- **Device stalls.** A CPU waiting on a non-posted MMIO read takes no
  interrupt until the read completes, whatever the interrupt flag says, so
  a surprise-removed device stalls that CPU for its whole Completion
  Timeout. Three rules bound it ([drivers.md](drivers.md)): Completion
  Timeout defaults to the 1 to 10 ms range, and is set to 50 to 100 µs
  only on devices measured safe with ASPM L1 substates off (a 24-hour soak
  with 0 spurious timeouts gates each one); no MMIO read happens in
  any non-preemptible section (spinlock held, preemption or interrupts
  off), so hard-IRQ handlers only write; and handler threads of
  hot-removable devices (behind a hotplug-capable or Thunderbolt/USB4 port)
  are affine to one "removable-device core" (the highest-numbered
  non-isolated core, with its SMT sibling; on hybrid CPUs an E-core), on
  which the real-time class never places a thread while any other CPU can
  take it, so audio, input and other real-time threads don't share a CPU
  with a stall (on machines with fewer than 4 CPUs this is off, and the
  scorecard says so). Test (Phase 4a): the
  surprise-removal latency test pulls a QEMU NVMe device behind a hotplug
  root port during reads, with the real-time wake-up load running, and
  publishes the worst CPU stall and the worst real-time wake-up latency.
- **Where the 50 µs limit is enforced.** In virtual machines the host can
  stall a vCPU at any moment, so there the tracer only reports; the limit
  is enforced (CI fails above it) on the bare-metal reference machines, in
  dedicated stress boots from Phase 2 (their own slot in the machine-hours
  budget, [benchmarks.md](benchmarks.md)). Benchmark boots run with the
  tracer off. The target tightens to 20 µs in Phase 10.
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
  their fair share, so it can't starve background work. The optional focus
  boost (a `cpu.weight` multiplier for the focused app's group) and
  background freezing (`cgroup.freeze` on hidden apps' groups, with
  exemptions) are F9 in [features.md](features.md); per-app power from
  RAPL and per-app `cpu.max` power caps are F10.
- **Groups.** cgroup v2 `cpu.weight` and `cpu.max` are implemented with
  hierarchical fair scheduling (Phase 5, needed for milestone 6). The desktop
  puts each app in its own group, so an app with 200 threads gets one app's
  share, not 200 threads' worth.
- **Benchmark core isolation** (Phase 2). The boot option `isolcpus=<set>`
  removes those CPUs from load balancing, from unbound kernel threads and
  timer migration, and from the default destination of device interrupts:
  the kernel programs every I/O APIC and MSI destination to the other CPUs
  and rejects affinity changes that would add an isolated CPU. A Phase 2
  test runs one spinning thread per isolated CPU for 10 s and counts
  interrupts per CPU by source: 0 device (I/O APIC and MSI) interrupts, at
  most 10 LAPIC timer interrupts (the tick-stop bound below) and at most 10
  IPIs, each IPI logged by type, none from load balancing or timer
  migration. They stop
  their tick when they run one thread (the single-thread tick stop below,
  Phase 2). Only threads placed there by affinity run on them. The harness
  uses it only for the per-change microbenchmark tier; every desktop,
  real-time-latency and throughput metric runs on the default boot with no
  isolated CPUs ([benchmarks.md](benchmarks.md)). For that tier matched
  Linux uses `isolcpus=domain,managed_irq,<set> nohz_full=<set>` with the
  same set.
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
  the calibration source. On KVM (the Phase 1 gate: under 1 ppm over
  10 minutes, on the dedicated KVM host, an owner need from Phase 1) QEMU
  runs with CPUID level at least 0x15 and an explicit `tsc-frequency`,
  which the guest reads from CPUID 0x15; once a second the kernel writes its clock reading
  as a line on the 16550 UART (polled, no DMA), and a host-side program
  timestamps each line's arrival with `CLOCK_MONOTONIC_RAW`; drift is the
  slope of a least-squares fit of guest time against host time, so serial
  latency jitter averages out. On bare metal (Phase 2, when the rig exists)
  the controller host, disciplined by a GPS receiver, drives a PPS pulse
  onto the UART's DCD pin; the kernel timestamps each DCD edge from the
  modem-status interrupt and fits the same slope (the result is the board
  crystal's error, which no kernel can remove without an outside reference;
  published, and at most 50 ppm).
- **High-resolution timers and a tickless kernel** are part of the Phase 2
  design, not Phase 10: timers are kept in per-CPU structures with
  nanosecond expiry on the timer event source above, and an idle CPU takes
  no periodic tick. A CPU running one thread also stops its tick
  (single-thread tick stop, Phase 2) and arms no slice timer: a thread
  alone on its CPU has no one to yield to, so when a second thread is
  queued there the enqueuing CPU sends an IPI and the target re-arms the
  slice timer. The clocksource watchdog, RT-throttle bookkeeping and RAPL
  sampling run only on housekeeping CPUs (the boot CPU, or the
  non-isolated CPUs), so a tick-stopped CPU arms only timers its own
  thread asked for and, from Phase 5, the `cpu.max` quota-expiry timer of
  its thread's group (one per period while the group has a quota, which
  the bound counts). A Phase 2 test runs one spinning thread on a CPU for
  10 s, logs every interrupt there by source, and requires at most 10
  timer interrupts (with a `cpu.max` quota from Phase 5: at most 10 plus
  one per quota period). Phase 10 only tunes it.
- **Frequency control** (Phase 2). Optimal drives hardware P-states (Intel
  HWP, AMD CPPC) from Phase 2; it is a precondition of every comparison
  with Linux, from the Phase 4a storage set on. Otherwise a comparison
  measures the firmware's default frequency, not the kernel. Microbenchmarks
  run with turbo off and HWP (or CPPC) minimum = maximum = 80% of the CPU's
  guaranteed performance, on both kernels (Linux: `intel_pstate` or
  `amd_pstate` active mode, `no_turbo`, and the same min/max).
  Desktop, real-time-latency and throughput metrics run at shipped defaults,
  with the same energy-performance preference on both kernels. Idle states
  beyond C1 (with an idle governor) come in Phase 10, along with suspend.

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
  recently used slot is taken and CR3 is loaded with a flush. Every change
  that removes a permission or changes a frame (unmap; write-protect for
  copy-on-write at `fork`; `mprotect` that drops write, execute or user
  access, including `PROT_NONE` and W→X; page migration; huge-page split
  or collapse; a cache-type change) bumps the address space's generation
  and completes its shootdown before the operation returns, and a CPU that
  completes a shootdown records the new generation in its slot, so a CPU
  can never reuse a stale PCID without flushing. Only changes that add
  rights (a fault filling a PTE, an upgrade from read-only) skip the
  shootdown; a CPU then takes at most a spurious fault.
- **TLB: "may cache" tracking.** Each address space has a CPU set of CPUs
  that may cache its translations: a CPU joins when it loads the address
  space's CR3, and stays while it runs a kernel thread with that CR3 still
  loaded (lazy switch). A shootdown goes to every CPU in the set; a lazy CPU
  that receives one switches to the kernel-only page table and leaves the
  set. A CPU leaves the set otherwise only when it loads another address
  space's CR3; translations it still holds under an unloaded PCID are covered
  by the generation rule above. When an address space is torn down or its
  top-level table is freed, every lazy CPU still in the set gets an IPI,
  switches to the kernel-only table and acknowledges before the table is
  freed, on both shootdown paths (INVLPGB sends no IPI, so without this a
  lazy CPU could keep a freed PML4 in CR3 and walk it speculatively).
  [verification.md](verification.md) target 1s states the property as "no
  CPU uses a translation with more rights than the current PTE once the
  operation returns", with two obligations at each such change: every CPU
  in the may-cache set has acknowledged, and every other PCID slot holding
  that address space has a generation older than the change, so reloading
  it flushes. Tests: a QEMU test forks while other threads of the parent
  write continuously and checks no write lands in a page the child sees
  (Phase 3); a teardown test tears down an address space with lazy CPUs
  and checks no CPU holds its freed top-level table in CR3: the IPI path
  in QEMU (Phase 3), the INVLPGB path on the bare-metal reference AMD
  desktop (Zen 3 or later; QEMU and KVM don't offer INVLPGB to guests),
  with the INVLPGB shootdown tests, a Phase 5 done-when item.
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
  ([verification.md](verification.md) target 1s: M/T at Phase 2 for the
  IPI path, re-gated at M/T in Phase 3 with PCID, lazy switch, permission
  downgrades and lazy-CPU teardown on the IPI path, and in Phase 5 with
  INVLPGB, its lazy-CPU teardown, and THP).
- **Huge pages** (Phase 5). Transparent huge pages for anonymous memory on
  fault where the region is aligned and large enough, plus background
  collapse; a fault never waits for compaction (if no 2 MiB frame is free it
  maps 4 KiB and leaves the region to background collapse), including in
  `MADV_HUGEPAGE` regions. That matches Linux's `always` with
  `defrag=defer` (`madvise` would compact directly in `MADV_HUGEPAGE`
  regions), which matched Linux uses. Page cache huge folios for large
  files in Phase 10.
- **Reclaim.** A multi-generation LRU style reclaim with refault detection
  (pages that come back soon after eviction were evicted wrongly, and the
  policy adapts). The foreground app's pages are protected from reclaim
  longer than background apps'.
- **Swap and compression.** Compressed in-memory swap (zram-style, our own
  implementation, LZ4 or Zstd from permissively licensed crates) is on by
  default, sized at half of usable RAM, LZ4 by default (lowest
  decompression latency), with swappiness 100 (anonymous and file pages
  weighted equally, as suits in-memory swap) and optional encrypted disk
  swap. Phase 5. Matched Linux in the memory-pressure test uses the same
  size, compressor and swappiness ([benchmarks.md](benchmarks.md)).
- **Pressure information and OOM.** Pressure stall information
  (`/proc/pressure/*`, needed by systemd-oomd-style daemons and by the
  desktop) is implemented in Phase 5. The kernel OOM killer is the last
  resort; a user-space OOM service acts earlier, when an app group's memory
  pressure (`full` avg10) stays at or above 50% for 20 s, killing the least
  important background app first, never the compositor or the foreground
  app. Matched Linux runs systemd-oomd with the same limit and duration on
  per-app scopes, so the test measures the kernel and the victim choice,
  not the thresholds.
- **Benchmarks** (added to [benchmarks.md](benchmarks.md)): TLB shootdown
  latency against core count (Phase 2); 4 KiB fault cost, mmap/munmap and
  page-fault scaling across cores (Phase 3); 2 MiB fault cost (Phase 5); and a memory pressure test in
  Phase 7, when there is a desktop and a browser to run: the machine is
  limited to 8 GiB of usable RAM on both kernels (frames handed to the page
  allocator, measured after boot and published; each kernel's limit option
  is tuned until it is within 1% of 8 GiB, because `mem=` caps the top
  physical address, not usable RAM), a browser opens 50 pages from
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
(it gets its share and no more) rather than by masking its interrupt,
which would cause audio underruns or I/O timeouts. Interrupt masking is
reserved for genuine storms (the per-class limits over 10 ms windows in
[drivers.md](drivers.md)) and is reported on the scorecard whenever it
happens. A looping tier-2 thread is stopped by the driver watchdog through
the scheduler: a forced-preemption IPI, then the thread is parked only if
its core-call depth is 0, otherwise it is flagged to park on core exit
(core calls are bounded); a tier-2 thread that stays preempt-off for 10 ms
is a stop event, not a crash ([drivers.md](drivers.md); Phase 4a test with
a loop inside a core call).

## Proof and test obligations

- [verification.md](verification.md) target 8 (M/T at Phase 2 for run
  queues, class order and priority inheritance; B/T at Phase 2 for real-time
  throttling) covers:
  every runnable thread is on exactly one run queue; a higher class always
  runs before a lower one on the same CPU; RT throttling never lets RT use
  more than its budget; priority inheritance chains are acyclic and restore
  priorities when released. Run-queue membership, class order and priority
  inheritance are model-checked with loom on the host (preemption bound
  recorded in the ledger); RT throttling is checked with Kani over an
  abstract clock; property tests run in QEMU. (No thread runs on two CPUs is
  guaranteed by the core's move-only `RunnableThread`, not by this check.)
- Target 7 at B/T in Phase 2 covers the scheduler's CPU accounting only
  (the RT throttle); it is extended at 4a, when drivers exist, to the driver
  real-time cap above and to memory and interrupt budgets.
- Phase 2 tests: priority-inheritance chains (nested and across classes),
  capacity-aware placement on a simulated hybrid topology, tickless idle
  (no periodic tick on an idle CPU over 10 s), single-thread tick stop and
  isolated-CPU interrupt counts (above, each logging every interrupt by
  source), timer expiry accuracy, and the
  bare-metal PPS drift test.
- The latency tracer and the stress suite run in every functional CI boot
  from Phase 2 (report-only in VMs) and in the bare-metal stress boots,
  where the limit is enforced; benchmark boots keep the tracer off (its
  cost when off is the Phase 3 gate above).
