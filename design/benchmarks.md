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

## The regression gate

- **Locked machine state**, recorded with every result: CPU model, microcode
  revision, firmware version, fixed energy-performance preference, turbo and
  SMT state, isolated benchmark cores, and the exact kernel builds.
- **Interleaved A/B runs.** The base build and the candidate build alternate
  on the same machine, at least 30 runs each, so drift affects both equally.
- **Decision rule.** A change fails the gate when the bootstrap 95%
  confidence interval of the median difference lies entirely beyond 3% worse
  on any tracked metric, and an automatic rerun reproduces it.
- **Noise ceiling.** Each metric must have a run-to-run coefficient of
  variation of at most 1% on its machine (tails: enough samples per run for
  a stable P99.9, at least 10^5). If the harness can't get under that, the
  harness gets fixed; the gate is never widened to fit the noise.

## The Linux baseline

- **Two Linux kernels per scorecard release:** the newest longterm (LTS)
  kernel and the newest stable kernel, each pinned by exact version for that
  release, with the full kernel command line and config published.
- **Two configurations of each:** *stock* (the distribution's generic
  config, default boot options) and *matched*: `preempt=full`, the same
  side-channel mitigations Optimal uses on that CPU, the same IOMMU mode
  (strict), and the same energy-performance preference. "Beats Linux" claims
  are made against **matched**; stock is shown for context.
- **Inside a VM,** Linux's paravirtual features that Optimal doesn't have
  (kvm-clock, paravirtual spinlocks, paravirtual TLB flush and EOI, haltpoll)
  are switched off for the matched column, or Optimal implements them; the
  column says which.

## What counts as winning

- The **desktop set** (responsiveness table below plus app launch and idle
  power) is frozen and versioned before Phase 5. Changing it is a reviewed
  change and starts a new trend line.
- Optimal **beats Linux** on a machine when: every desktop metric is at
  parity or better within its confidence interval, the geometric mean of the
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
  S3 and "runs on almost anything").
- Each is listed by exact model, CPU, microcode, RAM, storage and firmware
  version on the scorecard.
- The desktop is needed from Phase 1 (real-hardware boot) and as a
  dedicated runner from Phase 2; the rest by Phase 5. These are owner needs.

## Provisional targets (tighten only, never loosen)

Absolute targets are committed now so a threshold can't be chosen after
seeing the results. They can only be made stricter.

| Metric | Target (reference desktop) | From |
|---|---|---|
| Wake-up latency of a real-time thread with all cores saturated | P99 < 50 µs, P99.9 < 100 µs | 2 |
| Longest preempt-off or IRQ-off section | < 50 µs (20 µs from Phase 10) | 2 |
| Audio underruns at 128 frames, 48 kHz, under full CPU and I/O load | 0 per hour (also reported at 64 and 256 frames) | 6 |
| Input-to-display latency, P99.9 under full CPU load | no more than 1.5 times its idle P99.9, and no worse than matched Linux | 7 |
| Frame time P99 in the frame-pacing test at 60 and 144 Hz | ≤ refresh period + 1 ms | 7 |
| Foreground input latency P99 while a large copy or compile runs | ≤ 1.25 times idle | 4 |
| Boot time, firmware handoff to kernel ready | < 300 ms | 1 |

## Responsiveness (the main event)

| Metric | Reported as | What it shows | From |
|---|---|---|---|
| Input-to-display latency | P50, P99, P99.9 | how quickly a key press or mouse move reaches the screen | 7 |
| Frame time | P50, P99, variance | consistency of frame pacing, idle and under load | 7 |
| Audio underruns | per hour at 64, 128 and 256 frames (48 kHz), idle and under load | real-time scheduling quality | 6 |
| Scheduling latency under saturation | P99, P99.9 wake-up latency of a real-time thread | whether interactive work cuts through a full system | 2 |
| Foreground responsiveness during background I/O | input latency and app launch time while a large copy or compile runs | whether background work makes the desktop feel slow | 4 |
| Memory pressure | input latency P99 and frame time while a 50-tab browser and a compile overcommit 8 GiB | the biggest real cause of stutter | 5 |
| App launch time | cold and warm, P50/P99 | first impression speed | 5 |

## Core kernel costs

| Metric | From |
|---|---|
| Boot time (firmware handoff to kernel ready, and to desktop) | 1 |
| Timer and interrupt latency | 1 |
| Context switch and thread wake-up, **kernel threads, Optimal against itself only** | 2 |
| Context switch, wake-up and messaging **against Linux**, using identical static binaries on both (pipe ping-pong, futex ping-pong, hackbench, schbench, a cyclictest equivalent), with the switch including an address-space change and full XSAVE | 5 |
| Syscall round trip; vDSO `clock_gettime` | 3 |
| Page fault cost (4 KiB and 2 MiB, minor and major), mmap/munmap, page-fault and mmap scaling across cores | 3 |
| TLB shootdown latency against core count | 2 |
| Memory allocation (kernel and user) | 1 (kernel), 3 (user) |
| Cross-NUMA-node memory latency and bandwidth | 2 |
| Storage set (see [storage.md](storage.md)): fio 4k random read QD1/QD32, 4k random overwrite QD32, 1M sequential write QD8, fsync P50/P99/P99.9, SQLite WAL, pgbench, git checkout and status, untar, metadata microbenchmarks; on aged file systems with encryption on for both | 4 |
| Driver tier cost: per-I/O latency and throughput for the same device in tier 1, 2 and 3 | 4 |
| IOMMU strict-invalidation cost | 4 |
| Network throughput and latency (TCP and UDP, 1 and 8 streams, on a named NIC at a named link speed) | 6 |
| Overall throughput: kernel-size C compile, `cargo build` of a large crate, Zstd compression, Blender CPU render | 5 |
| Browser: Speedometer 3, JetStream 2 | 7 |
| Games: at least 5 titles from the game list, average and 1% low frame rate | 8 |
| Suspend and resume time | 10 |

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
Measured on each reference machine and on the CI VM. Targets: total OS memory
at an idle desktop at most 60% of the lightest mainstream Linux desktop
measured on the same machine, tightened as data comes in.

## Idle CPU, power and battery, with defined windows

- **Idle CPU:** percentage of CPU time not idle, averaged over a 10-minute
  window starting 5 minutes after the desktop is ready, no input, network
  connected. Target: at most matched Linux's, and below 0.5%.
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
