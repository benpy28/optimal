# Benchmarks and the public scorecard

Every performance and security claim Optimal makes has to be falsifiable. CI
publishes a scorecard that puts Optimal and Linux side by side on the same
hardware, running the same workloads. Numbers that look bad are published too.
An entry that no job measures yet is shown as **not yet measured**, never left
out and never filled in by hand.

## Where numbers come from

| Source | Used for | Not used for |
|---|---|---|
| **Dedicated bare-metal machines** (the reference PCs below, configured as self-hosted CI runners) | every comparison with Linux; the regression gate | — |
| KVM on a dedicated host | trend lines between bare-metal runs | claims against Linux |
| GitHub-hosted runners (shared VMs) | functional tests only: does it build, boot, pass tests | any performance number |
| TCG (QEMU without KVM) | functional tests only | any performance number |

Shared CI VMs change host CPU between runs and carry noisy neighbours, with
typical microbenchmark noise of 5 to 20%, so a 3% gate there either fires at
random or never. That's why the gate needs dedicated hardware.

**Until the first dedicated machine is online**, the gate runs on KVM as a
trend report only (it warns, it doesn't block), and the scorecard publishes
no "beats Linux" claim of any kind. This is a hardware dependency, listed in
the roadmap as an owner need.

**The rig.** The self-hosted runner software lives on a controller host (a
Raspberry Pi or similar), never on the machine under test, so a crashed or
hostile kernel can't touch the runner. Per machine under test the controller
drives UEFI HTTP or PXE boot of the build under test, a power relay, and a
serial line; results come back over serial with a per-record checksum (a
record with a bad checksum is discarded and the run repeated), and a
watchdog power-cycles the machine when no record arrives within the run's
stated time limit. Only commits a maintainer has approved run on the rig; the
rig sits on an isolated VLAN with no route to anything else and holds no
secrets (no signing keys, no tokens with write access).

## The regression gate

- **Locked machine state**, recorded with every result: CPU model, microcode
  revision, firmware version, fixed energy-performance preference, turbo and
  SMT state (the shipped default per CPU model, [scheduler.md](scheduler.md)),
  THP mode, the isolated benchmark cores (the `isolcpus=` set in
  [scheduler.md](scheduler.md), from Phase 2), and the exact kernel builds.
- **SMIs are counted.** Every run reads the SMI count before and after
  (`MSR_SMI_COUNT` on Intel; the SMIs-received core PMC event on AMD).
  Microbenchmark runs during which an SMI arrived are discarded and rerun,
  and the discard count is published; if more than 5% of a metric's runs are
  discarded the machine is flagged and its firmware settings are fixed.
  Tail-latency and zero-event runs keep every sample and publish the SMI
  count, because SMIs hit both kernels and real users alike.
- **Interleaved A/B boots.** The base build and the candidate build boot
  alternately on the same machine, so drift affects both equally, and each
  boot runs several repetitions of each metric. Boot-to-boot variation
  (physical memory layout, cache colouring) is larger than run-to-run, so the
  statistics treat the boot as a random effect: the bootstrap resamples
  whole boots, then runs within each resampled boot, and SE(diff) below is
  that hierarchical estimate.
- **Runs per metric come from measured noise.** The number of boots and runs
  per boot for each metric is computed from its between-boot and within-boot
  variance over the last 20 base-build boots, as the smallest that gives
  SE(diff) ≤ 0.2%, with at least 10 boots and 30 runs per build. At
  SE(diff) = 0.2% and the strictest Holm level of a run with up to 50
  metrics (z = 3.3), the corrected-CI branch below detects a true 2%
  regression with at least 90% probability (a run with more metrics lowers
  the SE target to keep that); each metric's achieved SE(diff) and power at
  2% are published with its result.
- **Time budget per tier.** Each change: at most 45 minutes of rig time per
  machine. Nightly: at most 8 hours. Weekly and each release: the 24-hour
  zero-event runs and the soak. If the budget caps a metric's runs before it
  reaches SE(diff) ≤ 0.2%, its achieved power is published as below target.
- **Decision rule.** A change fails the gate on a metric when the bootstrap
  confidence interval of the difference, Holm–Bonferroni corrected for the
  number of metrics in the run, lies entirely beyond 1% worse; or, only for
  a metric whose SE(diff) satisfies 2% ≥ 4 × SE(diff), when the median
  difference is more than 2% worse. For any other metric only the corrected
  interval counts, because an uncorrected point estimate with a larger SE
  fires on noise. Either way an automatic rerun must reproduce it (its own
  median difference more than 1% worse), so tracking more metrics doesn't
  make false alarms more common.
- **Cumulative budget.** Small regressions that each pass the gate still
  add up, so each release is also compared with the last release: no tracked
  metric may be more than 3% worse in total, and the geometric mean of the
  desktop set may not be worse at all.
- **Which runs gate what.** Each change: microbenchmarks (core kernel costs).
  Nightly: tail latencies and the responsiveness set. Weekly and each
  release: the 24-hour zero-event runs (audio underruns). Each release:
  power, battery and the long soak. A nightly, weekly or release failure
  blocks the next release until it is fixed or the change is reverted.
- **Noise ceiling.** Each metric must have a run-to-run coefficient of
  variation of at most 1% on its machine; tail percentiles at most 5%, with
  at least 10^5 samples per run for a P99.9 (input latency, where the rig
  limits the event rate, follows the rule below). If the harness can't get
  under that, the harness gets fixed; the gate is never widened to fit the
  noise.
- **Input latency.** The rig sends one event every 50 to 70 ms (uniformly
  random, so events don't lock to the refresh phase), about 16 events per
  second. The gate is on **P99 per run**, each run at least 10^4 events
  (about 10 minutes). **P99.9** is computed from at least 10^5 events pooled
  across at least 10 runs (about 1 hour 40 minutes per configuration), with a
  bootstrap CI that resamples runs. Hours per gate, for 4 load conditions
  (idle, full CPU load, background copy or compile, memory pressure) on
  Optimal and matched Linux: nightly P99, 8 runs, about 1 hour 20 minutes;
  each release, P99.9, about 13 hours 20 minutes.
- **Direction and normalisation.** Every metric states whether lower or
  higher is better and what it is normalised by (per operation, per second,
  per watt). Count metrics with a zero target (underruns, dropped frames,
  lock-order reports) are pass or fail and are not part of any geometric
  mean.
- **Zero-event targets** need at least 24 hours of runtime per configuration
  (for audio, per buffer size), and the scorecard publishes the 95% upper
  bound on the event rate, not just "0 seen".

## The Linux baseline

- **Two Linux kernels per scorecard release:** the newest longterm (LTS)
  kernel and the newest stable kernel, each pinned by exact version for that
  release, with the full kernel command line and config published.
- **Two configurations of each:** *stock* (the distribution's generic
  config, default boot options) and *matched*: `preempt=full`, the same
  side-channel mitigations Optimal uses on that CPU, the same SMT setting
  (`nosmt` on CPUs where Optimal ships SMT off, [scheduler.md](scheduler.md)),
  the same IOMMU mode (strict), the same energy-performance preference, and
  the same THP mode (`transparent_hugepage=never` until Optimal's THP lands
  in Phase 5, then `always` with `defrag=madvise`, so neither kernel compacts
  on fault). "Beats Linux" claims are made against
  **matched**; stock is shown for context.
- **Pinned user space.** Linux runs a pinned distribution image (Debian
  stable, from a dated snapshot archive, recorded per scorecard release), so
  the kernel is the only variable. The desktop for desktop metrics is
  GNOME on Wayland from that image; "the lightest mainstream Linux desktop"
  for the memory target is whichever of GNOME, KDE Plasma, Xfce and LXQt
  uses the least total OS memory on that image and machine, measured and
  named each scorecard release.
- **Matching idle states.** Until Optimal has deep idle states (Phase 10),
  the scorecard adds a third Linux column limited to C1 (`idle=halt`), so a
  power or latency difference that comes only from C-states is visible.
- **Inside a VM,** Linux's paravirtual features that Optimal doesn't have
  (kvm-clock, paravirtual spinlocks, paravirtual TLB flush and EOI, haltpoll)
  are switched off for the matched column, or Optimal implements them; the
  column says which.

## What counts as winning

- The **desktop set** (responsiveness table below plus app launch and idle
  power) is frozen and versioned before Phase 5. Changing it is a reviewed
  change and starts a new trend line.
- Optimal **beats Linux** on a machine when: every desktop metric is at
  parity or better (parity means the 95% confidence interval of the
  difference includes zero and is no wider than ±2%), the geometric mean of the
  desktop set is ahead, and no single desktop metric is more than 5% worse.
  Winning "most" metrics doesn't count.
- On server-style throughput the aim is to stay within 5% of matched Linux,
  and the scorecard says which workloads are which.

## Reference hardware

- **Reference desktop:** recent Intel hybrid CPU (P and E cores), 8+ cores,
  discrete GPU from the family chosen in Phase 8, NVMe.
- **Reference AMD desktop:** dual-CCD X3D CPU (tests the cache-die policy).
- **Reference laptop:** mainstream, on battery, with a TPM and IOMMU.
- **Reference old PC:** around 2012, 4 GiB, no IOMMU (tests hardware tier
  D2 and "runs on almost anything").
- **Reference multi-node machine:** a 2-socket server, or an AMD EPYC set to
  NPS2 or NPS4, so NUMA placement has real distances to measure. Until it is
  online, NUMA is tested for function only (QEMU NUMA topologies, including
  a memoryless node) and every NUMA performance entry is "not yet measured".
- **Benchmark controller host** (the rig above): one controller with a power
  relay and serial line per machine under test, needed with the first
  dedicated runner in Phase 2.
- Each is listed by exact model, CPU, microcode, RAM, storage and firmware
  version on the scorecard.
- **Input-latency rig:** a microcontroller that acts as a USB HID device and
  sends input, plus a photodiode on the screen, timestamped by the same
  microcontroller at 0.1 ms resolution or better, at the event rate and run
  sizes in "Input latency" above. Needed by Phase 7 (owner need).
- The desktop is needed from Phase 1 (real-hardware boot) and as a
  dedicated runner from Phase 2, with the controller host; the multi-node
  machine as soon as it can be had; the rest by Phase 5. These are owner
  needs.

## Provisional targets (tighten only, never loosen)

Absolute targets are committed now so a threshold can't be chosen after
seeing the results. They can only be made stricter. In every table here,
**From** is the phase whose done-when the entry gates (a target) or from
which it is measured and published (a metric); before that phase it is a
trend line at most, never a claim.

| Metric | Target (reference desktop) | From |
|---|---|---|
| Wake-up latency of a real-time thread with all cores saturated | P99 < 50 µs, P99.9 < 100 µs | 2 |
| Longest preempt-off or IRQ-off section | < 50 µs (20 µs from Phase 10) | 2 |
| Audio underruns at 128 frames, 48 kHz, under full CPU and I/O load | 0 per hour (also reported at 64 and 256 frames) | 6 |
| Input-to-display latency under full CPU load | P99 per run no more than 1.5 times its idle P99 (nightly gate); pooled P99.9 no more than 1.5 times its idle P99.9 (release gate); both no worse than matched Linux | 7 |
| Frame time P99 in the frame-pacing test at 60 Hz (virtio-gpu and llvmpipe, against Linux on the same graphics stack) | ≤ refresh period + 1 ms | 7 |
| Frame time P99 at 144 Hz on the reference discrete GPU | ≤ refresh period + 1 ms | 8 |
| Foreground input latency P99 while a large copy or compile runs | ≤ 1.25 times idle | 7 |
| Memory pressure test (`mem=8G`, 50 browser pages and a compile, [scheduler.md](scheduler.md)) | input latency P99 ≤ 1.5 times idle; frame time P99 ≤ refresh period + 2 ms; 0 OOM kills of the foreground app or the compositor; each no worse than matched Linux | 7 |
| Boot time, firmware handoff to kernel ready | < 300 ms (a Phase 1 done-when item) | 1 |
| Total OS memory at an idle desktop (Phase 7 done-when) | ≤ 60% of the lightest mainstream Linux desktop on the same machine | 7 |
| Idle CPU over the defined window (Phase 7 done-when) | ≤ matched Linux and < 0.5% | 7 |
| Latency tracer cost when off, on the syscall round-trip microbenchmark | ≤ 0.5% against a build with the tracer compiled out | 3 |

## Responsiveness (the main event)

| Metric | Reported as | What it shows | From |
|---|---|---|---|
| Input-to-display latency | P50, P99, P99.9 | how quickly a key press or mouse move reaches the screen | 7 |
| Frame time | P50, P99, variance | consistency of frame pacing, idle and under load | 7 (60 Hz, virtio-gpu and llvmpipe), 8 (144 Hz, discrete GPU) |
| Audio underruns | per hour at 64, 128 and 256 frames (48 kHz), idle and under load | real-time scheduling quality | 6 |
| Scheduling latency under saturation | P99, P99.9 wake-up latency of a real-time thread | whether interactive work cuts through a full system | 2 |
| Foreground responsiveness during background I/O | input latency and app launch time while a large copy or compile runs | whether background work makes the desktop feel slow | 7 |
| Memory pressure | input latency P99, frame time and OOM kills with `mem=8G`, 50 pages from a pinned offline corpus and a pinned compile ([scheduler.md](scheduler.md)); target above | the biggest real cause of stutter | 7 |
| App launch time | cold and warm, P50/P99 | first impression speed | 5 |

## Core kernel costs

| Metric | From |
|---|---|
| Boot time (firmware handoff to kernel ready, and to desktop) | 1 |
| Timer and interrupt latency | 1 |
| Context switch and thread wake-up, **kernel threads, Optimal against itself only** | 2 |
| Context switch, wake-up and messaging **against Linux**, using identical static binaries on both (pipe ping-pong, futex ping-pong, hackbench, schbench, a cyclictest equivalent), with the switch including an address-space change and full XSAVE | 5 |
| Syscall round trip; vDSO `clock_gettime` | 3 |
| fork+exec of a static binary with 10, 10^4 and 10^5 open fds (the open-file-description layer in [capabilities.md](capabilities.md)), within 10% of matched Linux at each count | 5 |
| Page fault cost (4 KiB from 3, 2 MiB from 5 when THP lands; minor and major), mmap/munmap, page-fault and mmap scaling across cores | 3, 5 |
| TLB shootdown latency against core count | 2 |
| Memory allocation (kernel and user) | 1 (kernel), 3 (user) |
| Cross-NUMA-node memory latency and bandwidth (function tests in QEMU from 2; numbers only on the multi-node reference machine) | 2 |
| Storage set: the table below | 4a, 5 |
| Driver tier cost: per-I/O latency and throughput for the same device in tier 1, 2 and 3 | 4a |
| IOMMU strict-invalidation cost | 4a |
| Network throughput and latency (TCP and UDP, 1 and 8 streams, on a named NIC at a named link speed) | 6 |
| Overall throughput: kernel-size C compile, `cargo build` of a large crate, Zstd compression, Blender CPU render | 5 |
| Browser: Speedometer 3, JetStream 2 | 7 |
| Games: at least 5 titles from the game list, average and 1% low frame rate | 8 |
| Suspend and resume time | 10 |

## Storage set

The one list of storage benchmarks; [storage.md](storage.md) links here and
sets the conditions (aged images restored per run, drive preconditioning,
encryption on for both, ext4 default mount options). Every entry compared
against Linux runs the same binary on both (a static binary in 4a). The
Phase 4a done-when is "every 4a entry compared against Linux is within 10%
of matched Linux".

| Benchmark | Against Linux | From |
|---|---|---|
| fio `psync`, 4k random read, QD1 | yes | 4a |
| fio `psync`, 4k random overwrite, QD1 | yes | 4a |
| fio `psync`, 1M sequential write, QD1 | yes | 4a |
| fio `psync`, 4k random read and 4k random overwrite at QD32, as 32 forked static `psync` processes (fio's default process mode, `numjobs=32`) | yes | 4a |
| fsync latency P50, P99, P99.9 | yes | 4a |
| SQLite in WAL mode, static binary (insert and point-query mix) | yes | 4a |
| untar of a pinned source tree, static binary | yes | 4a |
| metadata: create, stat, unlink of 1 million small files | yes | 4a |
| in-kernel block benchmark at QD32 | no: Optimal against itself only (there is no identical program on Linux) | 4a |
| fio `io_uring`, 4k random read and overwrite at QD32 | yes | 5 |
| pgbench | yes | 5 |
| git checkout and `git status` on a pinned large repository | yes | 5 |
| the whole set above, rerun on a per-user home volume ([storage.md](storage.md)) | yes (Linux: ext4 on dm-crypt over `dm-linear` with the same extents) | 5 |

Power is reported next to every throughput result (RAPL package power where
the CPU has it), so a win bought with extra watts is visible.

## Memory, split so it can't mislead

1. **Kernel memory:** kernel image, page tables, kernel heap and caches the
   kernel can't drop.
2. **Background services:** everything in user space that runs with no app
   open.
3. **Total OS memory:** 1 + 2, at an idle desktop, measured 2 minutes after
   login settles.
4. **Available to applications:** what a new app can actually allocate
   without swapping, at that same moment.

Reclaimable caches are reported separately and never counted as "used".
Memory is reported from Phase 3 (kernel only) and in full from Phase 7, when
there is a desktop to measure; the idle-desktop target below is a Phase 7
done-when item.
Measured on each reference machine and on the CI VM. Targets: total OS memory
at an idle desktop at most 60% of the lightest mainstream Linux desktop
measured on the same machine, tightened as data comes in.

## Idle CPU, power and battery, with defined windows

- **Idle CPU:** percentage of CPU time not idle, averaged over a 10-minute
  window starting 5 minutes after the desktop is ready, no input, network
  connected. Target: at most matched Linux's, and below 0.5%, a Phase 7
  done-when item.
- **Wake-ups per second** over the same window.
- **Idle power** over the same window at fixed screen brightness.
- **Battery life** on the reference laptop at fixed brightness for three
  workloads: idle desktop, a 1080p video loop, and a scripted web-browsing
  loop. Target: at least matched Linux on all three by Phase 10 (one target,
  the same for idle power and battery).

## Security scorecard

Published next to the performance numbers, **one column per hardware tier**
from [threat-model.md](threat-model.md), and counting a protection only if it
is on in the image people actually download:

- Every row of the attack-path table in the threat model: pass, fail, or not
  yet built.
- Which protections are on by default (NX, W^X, SMEP, SMAP, UMIP, KASLR with
  measured entropy, guard pages, CET, IOMMU, verified and measured boot,
  disk encryption stage, side-channel mitigations) and the measured cost of
  each.
- Microcode revision and the mitigation chosen per CPU vulnerability.
- The tough-conditions and Secure Boot attack results.
- Fuzzing hours and executions per target, and open crash bugs found by
  fuzzing.
- Proof targets with their assurance level ([verification.md](verification.md)).
- Lines in tier-0 crates against the cap, and the unsafe-block count.
- Driver-interrupt maskings and driver restarts seen in the soak.
- Open security bugs by severity.
