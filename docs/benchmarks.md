# Benchmarks and the public scorecard

Every performance and security claim Optimal makes has to be falsifiable. From
the first bootable build, CI publishes a scorecard that puts Optimal and Linux
side by side on the same virtual (and later real) hardware, running the same
workloads. Numbers that look bad are published too.

## Ground rules

- **Same machine, same workload.** Linux and Optimal run on identical QEMU
  configurations in CI (same CPU model, cores, RAM, firmware, KVM on) and,
  from Phase 1, on the same real reference PCs.
- **Linux baseline:** the current Ubuntu LTS kernel with its default config,
  plus the newest stable mainline kernel. Both are reported.
- **Repeated runs, distributions not averages.** Every metric reports P50,
  P99 and P99.9 (or min/median/max for short runs) over enough runs to be
  stable; the number of runs is published with the result.
- **Regressions block merges.** Any change that makes a tracked metric more
  than 3% worse (beyond its measured run-to-run noise) fails CI.
- **We optimise for the desktop.** The goal is to beat Linux on the workloads
  a desktop user feels: input latency, frame pacing, audio, app launch,
  responsiveness under background load, and idle power. On server-style
  throughput workloads we aim to be close to Linux, not necessarily ahead, and
  the scorecard says which is which.

## Reference hardware

- **CI virtual PCs:** q35, KVM, `-cpu host`, 4 cores, 8 GiB, OVMF. Exact
  QEMU and firmware versions are recorded with each result.
- **Real PCs** (from Phase 1, as they become available): one recent desktop
  (8+ cores, hybrid CPU), one mainstream laptop on battery, one old machine
  (around 2012, 4 GiB). Each is listed by exact model, CPU, RAM, storage and
  firmware version on the scorecard.

## Responsiveness (the main event)

| Metric | Reported as | What it shows | From |
|---|---|---|---|
| Input-to-display latency | P50, P99, P99.9 | How quickly a key press or mouse move reaches the screen; P50 is typical feel, P99/P99.9 is the worst moments | Phase 7 |
| Frame time | P50, P99, variance | Consistency of frame pacing in a game or animation, idle and under load | Phase 7 |
| Audio dropouts | underruns per hour at 5 ms and 10 ms buffers, idle and under load | Real-time scheduling quality | Phase 6 |
| Scheduling latency under saturation | P99, P99.9 wake-up latency of a high-priority thread while all cores are busy | Whether interactive work cuts through a full system | Phase 2 |
| Foreground responsiveness during background IO | input latency and app launch time while a large copy or compile runs | Whether background work makes the desktop feel slow | Phase 4 |
| App launch time | cold and warm, P50/P99 | First impression speed | Phase 5 |

No absolute promises like "never stutters": the targets are numbers, for
example "P99.9 input latency under full CPU load no more than 2x idle", set
once we have our first measurements and Linux's, and tightened over time.

## Core kernel costs

| Metric | From |
|---|---|
| Boot time (firmware handoff to kernel ready, and to desktop) | Phase 1 |
| Timer and interrupt latency | Phase 1 |
| Context switch (same core, cross core) | Phase 2 |
| Thread wake-up latency | Phase 2 |
| Syscall round trip | Phase 3 |
| Page fault cost (minor, major), mmap/munmap | Phase 3 |
| Memory allocation (kernel and user) | Phase 1 (kernel), Phase 3 (user) |
| File and block throughput and latency | Phase 4 |
| Network throughput and latency | Phase 6 |
| Overall throughput (compile benchmark, compression, rendering) | Phase 5 |

## Memory, split so it can't mislead

A single "uses 1.2 GB" number hides more than it shows. We report four:

1. **Kernel memory:** kernel image, page tables, kernel heap and caches the
   kernel can't drop.
2. **Background services:** everything user-space that runs with no app open.
3. **Total OS memory:** 1 + 2, at an idle desktop, measured 2 minutes after
   login settles.
4. **Available to applications:** what a new app can actually allocate
   without swapping, at that same moment.

Reclaimable caches are reported separately and never counted as "used".
Measured on each reference machine and on the CI VM; the targets are set
against Linux desktops on the same machine.

## Idle CPU and power, with a defined window

- **Idle CPU:** percentage of CPU time not in the idle state, averaged over a
  10-minute window that starts 5 minutes after the desktop is ready, with no
  user input, network connected, on each reference machine.
- **Wake-ups per second** over the same window (fewer means less power).
- **Idle power:** wall or battery power over the same window on the laptop and
  desktop reference machines, with screen on at fixed brightness.

## Security scorecard

Published next to the performance numbers, from the first bootable build:

- Which protections are on by default: NX, W^X kernel mappings, SMEP, SMAP,
  UMIP, KASLR, guard pages, CET, IOMMU, Secure Boot chain, disk encryption,
  side-channel mitigations (and the measured cost of each).
- The tough-conditions and Secure Boot attack results (`scripts/tough-test.sh`).
- Fuzzing hours and open crash bugs found by fuzzing.
- Proof targets done and their assumptions ([verification.md](verification.md)).
- Lines of `unsafe` code in the core and outside it.
- Every open security bug count, by severity.
