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
| KVM on a dedicated host (an owner need from Phase 1; QEMU with CPUID level ≥ 0x15 and an explicit `tsc-frequency`) | trend lines between bare-metal runs (virtio-gpu graphics is trend-only); the Phase 1 clock-drift gate ([scheduler.md](scheduler.md)) | claims against Linux |
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
stated time limit. From Phase 2 the controller also carries a GPS receiver
and drives its PPS pulse onto each machine's UART DCD pin, the reference for
the bare-metal clock-drift test ([scheduler.md](scheduler.md)). The serial
line is a native 16550 UART: every desktop reference machine must have a
COM header on a Super I/O chip (legacy I/O ports and IRQ, DCD wired), a
board-selection requirement, because USB-serial and network paths need
drivers that don't exist before Phase 6. The reference laptop has no UART,
so it joins the rig in Phase 6: results over a USB Ethernet adapter on the
rig VLAN (same checksummed records), reset by a relay that disconnects the
internal battery and the AC adapter together, power-on by a second relay
across the power-button contacts. Until then it runs functional tests by
hand only. Only commits
a maintainer has approved run on the rig; the
rig sits on an isolated VLAN with no route to anything else and holds no
secrets (no signing keys, no tokens with write access).

## The regression gate

- **Locked machine state**, recorded with every result: CPU model, microcode
  revision, firmware version, energy-performance preference, frequency
  setting, SMT state (the shipped default per CPU model,
  [scheduler.md](scheduler.md)), THP mode, the isolated cores, the latency
  tracer state (off in every benchmark boot; its enforcing stress boots
  have their own slot below), and the exact kernel builds. Microbenchmarks (the per-change tier) run with turbo off,
  HWP or CPPC minimum = maximum = 80% of guaranteed performance, and the
  `isolcpus=` set from [scheduler.md](scheduler.md), on both kernels. Every
  desktop, real-time-latency and throughput metric runs on the default boot:
  shipped frequency defaults (same energy-performance preference on both
  kernels) and no isolated CPUs, because that is what users run. Hardware
  P-state control (Phase 2) is a precondition of every comparison with
  Linux, from the Phase 4a storage set on. KASLR stays on in benchmark
  builds (the shipped configuration); the between-boot variance it adds is
  part of the measured noise.
- **SMIs are counted.** Every run reads the SMI count before and after
  (`MSR_SMI_COUNT` on Intel; the SMIs-received core PMC event on AMD).
  Microbenchmark runs during which an SMI arrived are discarded and rerun,
  and the discard count is published; if more than 5% of a metric's runs are
  discarded the machine is flagged and its firmware settings are fixed.
  Tail-latency and zero-event runs keep every sample and publish the SMI
  count, because SMIs hit both kernels and real users alike. The latency
  tracer excludes SMI-overlapped sections from the 50 µs gate and publishes
  their count ([scheduler.md](scheduler.md)).
- **Interleaved A/B boots.** The base build and the candidate build boot
  alternately on the same machine, so drift affects both equally, and each
  boot runs several repetitions of each non-tail metric (tail metrics:
  below). Boot-to-boot variation
  (physical memory layout, cache colouring) is larger than run-to-run, so the
  statistics treat the boot as a random effect: the bootstrap resamples
  whole boots, then runs within each resampled boot, and SE(diff) below is
  that hierarchical estimate. Intervals use Student's t with (boots per
  build − 1) degrees of freedom, or a BCa bootstrap; either needs at least
  20 boots per build, because a cluster bootstrap over fewer boots gives
  intervals that are too narrow.
- **Runs per metric come from measured noise.** For non-tail metrics the
  number of boots and runs per boot is computed from between-boot and
  within-boot variance over the last 20 base-build boots, as the smallest
  that gives SE(diff) ≤ 0.18%, with at least 3 runs in each of at least 20
  boots per build. The per-change tier has a fixed list and a fixed budget
  (below), sized so that a metric at the noise ceilings meets 0.18%. At SE(diff) = 0.18% and the strictest Holm level of a run with up
  to 50 metrics (two-sided 0.001, t with 19 degrees of freedom = 3.88), the
  corrected-CI branch below detects a true 2% regression with 92%
  probability (noncentral t; 84% at 0.20%, which is why the target is
  0.18%); a run with more metrics lowers the SE target to keep at least 90%.
  At the noise ceilings below (between-boot CoV 0.4%, run-to-run CoV 1%),
  20 boots of 7 runs per build give SE(diff) = 0.17%. Each metric's achieved
  SE(diff) and power at 2% are published with its result.
- **Tail metrics** (percentiles at P99 and above, variances, and the
  real-time wake-up and input-latency percentiles) have their own rule.
  Events are pooled per boot, one run per condition per boot, and the boot
  is the cluster: the statistic is computed from all events in the window
  and its interval is a BCa cluster bootstrap that resamples whole boots
  (paired across conditions of the same boot). A gated comparison needs at
  least 20 boots per kernel, and a P99.9 at least 10^5 pooled events.
  Nightly boots accumulate in rolling windows: each night adds boots and
  the gate judges the last N nights (N = 5 for the nightly block, N = 4 for
  the extended microbenchmark block, below). A window spans up to N nightly
  builds; when one fails, its builds are rerun one at a time in the rerun
  slot to find the change. Tail metrics are in no geometric mean; their
  margins are per metric (release rule).
- **Per-change metric list** (the per-change tier, run on the `isolcpus`
  boot): 20 metrics, each 7 runs of 0.4 s (2.8 s per metric, 56 s per boot,
  plus 4 s setup), means or medians only, no tails. Syscall round trip;
  vDSO `clock_gettime` (64-bit, and 32-bit through the vDSO32 from Phase
  3); kernel-thread context switch; kernel thread wake-up; pipe ping-pong
  and futex ping-pong across address spaces with full XSAVE (from 5); 4 KiB
  anonymous minor fault; 2 MiB anonymous fault (from 5); 4 KiB page-cache
  fault (from 4a); mmap plus munmap of 4 KiB; page-fault scaling on all
  isolated CPUs; TLB shootdown to 1 and to all other isolated CPUs;
  fork+exec of a static binary with 10 fds (from 5); kernel slab
  allocate/free; frame allocate/free (4 KiB and 2 MiB); user
  `malloc`/`free`; IOMMU strict map, invalidate and unmap (from 4a); timer
  interrupt latency, mean. A metric not yet available leaves its slot
  empty. Everything else in "Core kernel costs" runs nightly or per
  release.
- **Machine-hours budget.** Every machine is scheduled under 168 hours a
  week, and the plan is published. A scorecard release comes at most once
  per 4-week release cycle. The reference desktop comes as two identical
  units; every comparison runs all its builds on one unit. Steady state
  (from Phase 7; earlier phases run subsets):
  - Unit 1 (gate), per week. Per-change tier per merge batch (the changes
    merged since the last gate run; a failing batch is bisected in the
    rerun slot): 40 boots of at most 90 s (POST and netboot at most 30 s,
    measured and recorded, plus the 60 s list above), at most 10 batches a
    day, 70 h. Nightly, 7 h 55 min a day, 55 h 25 min: the nightly block,
    16 boots of 22.5 min (8 candidate, 4 matched LTS, 4 matched stable;
    each boot runs input latency idle, under full CPU load and during a
    background copy or compile, 5,000 events each, about 5 min, with frame
    time recorded in the same runs, then 4 min of real-time wake-ups at
    1 kHz, 2 min of cold and warm app launches, 1.5 min of POST and boot),
    6 h; the extended microbenchmark block, 10 boots of 11.5 min (5
    candidate, 5 last release: core-count sweeps, schbench, hackbench, the
    cyclictest equivalent, timer and interrupt latency tails, fork+exec at
    10^4 and 10^5 fds), 1 h 55 min. Latency-tracer stress boots, 1 h a
    day, 7 h. Total 132 h 25 min; rerun and bisection slot 35 h 35 min.
  - Unit 2 (long runs and release tier), per release cycle (672 h). Audio:
    128 frames idle and under load, 48 h a week; 64 and 256 frames in
    alternate weeks, 48 h a week, which in the release week runs matched
    Linux at 128 frames instead; 384 h. Release block, one boot of
    122.5 min (the extended microbenchmark block, the nightly block's
    runs, the storage set 30 min, the throughput set 20 min, browser
    10 min, network 10 min from Phase 6 against a peer on the rig VLAN,
    games 20 min from Phase 8, boot 1.5 min): candidate and last release,
    20 boots each, 81 h 40 min; matched LTS and matched stable, 20 boots
    each without the nightly block's runs (101.5 min), 67 h 40 min;
    memory-pressure boots (8 GiB usable, 15 min: idle reference, then
    pressure) for those four, 20 each, 20 h; context columns (stock LTS,
    stock stable, the C1-limited column), 5 boots each of the full block,
    published with their achieved SE and never used for a claim,
    30 h 38 min; drive preconditioning, 4 h; soak, 24 h; power runs,
    12 h. Release tier 239 h 58 min; total 623 h 58 min; rerun slot
    48 h 2 min. Until unit 2 arrives (Phase 6) the release tier uses unit
    1's rerun slot and a release waits for it.
  - The other reference machines run the release tier only, each under the
    same 168 h a week cap. If the budget caps a metric's runs before it
    reaches its SE target, its achieved power is published as below target.
- **Decision rule.** A change fails the gate on a metric when the
  confidence interval of the difference (t or BCa, as above),
  Holm–Bonferroni corrected for the
  number of metrics in the run, lies entirely beyond 1% worse; or, only for
  a metric whose SE(diff) satisfies 2% ≥ 4 × SE(diff), when the median
  difference is more than 2% worse. For any other metric only the corrected
  interval counts, because an uncorrected point estimate with a larger SE
  fires on noise. Either way an automatic rerun must reproduce it (its own
  median difference more than 1% worse), so tracking more metrics doesn't
  make false alarms more common.
- **Cumulative budget (release rule).** Small regressions that each pass
  the gate still add up, so each release is also compared with the last
  release (the release block on unit 2): for every tracked metric the
  one-sided 95% upper bound of the difference is at most its margin worse,
  and for the geometric mean of the desktop set's non-tail metrics at most
  0.5% worse. Two equal kernels pass a one-sided bound with probability
  Φ(margin / SE − 1.645), so each margin sets an SE target for 95%: a
  non-tail metric's margin is 3% (SE(diff) ≤ 0.91%); the geometric mean
  needs SE ≤ 0.15% (95.4%), and the release block's boots are budgeted for
  it, with boots added from the rerun slot when the achieved SE is higher;
  a tail metric's margin is max(3%, 3.29 × its SE(diff) over the previous
  release's runs), fixed and published before the comparison runs. Each
  metric's pass probability at a true 0% difference and power at 2% are
  published. (A point-estimate "not worse at all" rule would fail about
  half the time on two equal kernels.)
- **Which runs gate what.** Each change: the per-change metric list.
  Nightly (rolling windows): the nightly block (input latency and its
  ratios, frame time, the real-time wake-up target, app launch) and the
  extended microbenchmark block. Weekly and each release: the 24-hour
  zero-event runs (audio underruns). Each release: the release block
  (release rule and every Linux column outside the nightly block), memory
  pressure, power, battery and the long soak. A nightly, weekly or release
  failure blocks the next release until it is fixed or the change is
  reverted. Phase done-when gates run as their own comparison of at least
  20 boots per kernel, under "How a target is decided" below.
- **Noise ceiling.** Each metric must have a run-to-run coefficient of
  variation of at most 1% and a between-boot coefficient of variation (of
  per-boot means) of at most 0.4% on its machine; a tail metric's per-boot
  value at most 5% between boots, with at least 10^5 pooled events for a
  P99.9. If the harness can't get
  under that, the harness gets fixed; the gate is never widened to fit the
  noise.
- **Input latency.** The rig sends one event every 50 to 70 ms (uniformly
  random, so events don't lock to the refresh phase), about 16.7 events per
  second, so a 5,000-event run takes 5 minutes. It follows the tail rule:
  each nightly window holds 40 candidate boots (2 × 10^5 events per
  condition) and 20 boots of each matched Linux kernel (10^5 events), so
  the nightly gate judges P99 and the release gate judges P99.9 from the
  same window. Each ratio (loaded against idle, paired within boots, and
  Optimal against each matched Linux) is judged by the cluster-bootstrap
  bound of the ratio, never by single values. Memory pressure needs an
  8 GiB boot, so it runs in the release tier (20 boots per kernel).
- **Direction and normalisation.** Every metric states whether lower or
  higher is better and what it is normalised by (per operation, per second,
  per watt). Count metrics with a zero target (underruns, dropped frames,
  lock-order reports) are pass or fail and are not part of any geometric
  mean.
- **Zero-event targets** are stated as "0 observed in at least 24 hours per
  configuration (for audio, per buffer size and load condition), 95% upper
  bound on the rate at most 0.125 per hour"; the scorecard publishes the
  upper bound, not just "0 seen".

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
  in Phase 5, then `always` with `defrag=defer`, so neither kernel compacts
  on fault, [scheduler.md](scheduler.md)), the frequency settings and
  `isolcpus` use of the locked machine state above, and usable RAM
  equalised as measured where a test limits memory; in the memory-pressure
  test, zram of the same size and compressor as Optimal's compressed swap,
  `lru_gen` enabled, the same swappiness and systemd-oomd with the same
  pressure limit and duration on per-app scopes
  ([scheduler.md](scheduler.md)), all published with the configuration.
  "Beats Linux" claims are made against
  **matched**; stock is shown for context.
- **Phase 7 graphics baseline.** Until the reference GPU's driver exists
  (Phase 8), frame time and input-to-display latency are gated on the bare
  metal reference desktop with llvmpipe over the firmware's simple
  framebuffer, against Linux booted with `nomodeset` (simpledrm plus
  llvmpipe). Neither has vblank there, so both
  compositors pace from a 60 Hz timer and frame time is the interval between
  completed framebuffer updates.
- **Pinned user space.** Linux runs a pinned distribution image (Debian
  stable, from a dated snapshot archive, recorded per scorecard release), so
  the kernel is the only variable. Every kernel comparison with a desktop
  (Phase 7 on) runs the same compositor build on both kernels: Optimal's
  pinned compositor ([threat-model.md](threat-model.md)), built for Linux
  from the same source and toolchain minus the Optimal-only patches,
  with the same Mesa. Mainstream desktops are used only for the memory
  target: "the lightest mainstream Linux desktop" is whichever of GNOME,
  KDE Plasma, Xfce and LXQt uses the least total OS memory on that image
  and machine, measured and named each scorecard release.
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
- Optimal **beats Linux** on a machine when every desktop metric is at
  parity or better against both matched kernels and the geometric mean of
  the desktop set's non-tail metrics is better. Parity: the 95% confidence
  interval of the difference, Holm-corrected across the desktop set, lies
  within ± the metric's parity margin. Two equal kernels pass that with
  95% probability only if margin ≥ (c + 1.96) × SE, where c is the
  strictest Holm critical value of the set (3.17 for 10 metrics at 20
  boots), so a non-tail metric's margin is 2% and its SE target 2% / (c +
  1.96), 0.39% for 10 metrics; a tail metric's margin is max(2%, (c +
  1.96) × its SE), fixed from the previous release's runs and published
  before the comparison. Better: that corrected interval lies entirely on
  the better side of zero. Winning "most" metrics doesn't count.
- On server-style throughput the aim is that the worse end of each
  corrected interval is within 5% of matched Linux, and the scorecard says
  which workloads are which.

## Reference hardware

- **Reference desktop, two identical units** (gate and long runs, above):
  recent Intel hybrid CPU (P and E cores), 8+ cores, a native COM header
  (as every desktop reference machine, "The rig"), NVMe, and a discrete
  GPU whose exact model is fixed and recorded before the desktop set
  freezes (before Phase 5), so the locked machine state doesn't change
  afterwards; the Phase 8 GPU driver targets that model.
- **Reference AMD desktop:** dual-CCD X3D CPU (tests the cache-die policy).
- **Reference laptop:** mainstream, on battery, with a TPM and IOMMU; on
  the rig from Phase 6 (USB Ethernet results, battery-disconnect reset).
- **Dedicated KVM host** (Phase 1): an x86-64 machine with KVM that runs
  nothing else, for the clock-drift gate and the pre-hardware trend report;
  QEMU exposes CPUID level ≥ 0x15. Hosted runners are VMs whose
  `CLOCK_MONOTONIC_RAW` is itself a guest clock, so they never run it.
- **Reference old PC:** around 2012, 4 GiB, no IOMMU (tests hardware tier
  D2 and "runs on almost anything").
- **Reference multi-node machine:** a 2-socket server, or an AMD EPYC set to
  NPS2 or NPS4, so NUMA placement has real distances to measure. Until it is
  online, NUMA is tested for function only (QEMU NUMA topologies, including
  a memoryless node) and every NUMA performance entry is "not yet measured".
- **Benchmark controller host** (the rig above): one controller with a power
  relay, a serial line and a GPS-disciplined PPS output wired to the DCD pin
  per machine under test, needed with the first dedicated runner in Phase 2.
- Each is listed by exact model, CPU, microcode, RAM, storage and firmware
  version on the scorecard.
- **Input-latency rig:** a microcontroller that acts as a USB HID device and
  sends input, plus a photodiode on the screen, timestamped by the same
  microcontroller at 0.1 ms resolution or better, at the event rate and run
  sizes in "Input latency" above. Needed by Phase 7 (owner need).
- The KVM host is needed from Phase 1; the desktop from Phase 1
  (real-hardware boot) and as a dedicated runner from Phase 2, with the
  controller host; its second unit by Phase 6 (the first 24-hour audio
  runs); the laptop on the rig by Phase 6; the multi-node machine as soon
  as it can be had; the rest by Phase 5. These are owner needs.

## Provisional targets (tighten only, never loosen)

Absolute targets are committed now so a threshold can't be chosen after
seeing the results. They can only be made stricter. In every table here,
**From** is the phase whose done-when the entry gates (a target) or from
which it is measured and published (a metric); before that phase it is a
trend line at most, never a claim.

**How a target is decided.** An absolute target (a threshold on Optimal's
own value or on a ratio between its own conditions) is met when the
one-sided 95% bound on the worse side meets it. A comparative gate
(against Linux, the last release or another Optimal path) is met when the
worse end of the two-sided 95% confidence interval, Holm-corrected across
the entries of the same gate, meets it (the release rule and parity keep
their own bounds, above). Intervals are t over per-boot means for
non-tail metrics and BCa cluster bootstraps over boots for tail metrics.
Every threshold relative to Linux must hold against both matched LTS and
matched stable. "No worse than matched Linux" means the bound of
Optimal/Linux is at most 1 + max(2%, 3.29 × SE) (one-sided), because two equal
kernels can't show a bound at or below exactly 1. A count target (OOM
kills, underruns) is zero observed over every boot of the run, with the
zero-event rule for rates. Every gate uses at least 20 boots per kernel;
the sample column gives the rest.

| Metric | Target (reference desktop) | Sample per kernel | From |
|---|---|---|---|
| Wake-up latency of a real-time thread with all cores saturated (default boot, no isolated CPUs). Load in Phase 2: a spinning thread per CPU plus IPI and timer storms; at 4a also storage load (the latency gate of [storage.md](storage.md)); from Phase 5: hackbench, a pinned compile, fio on NVMe and memory pressure together | P99 < 50 µs, P99.9 < 100 µs | 20 boots, 4 min at 1 kHz each (2.4 × 10^5 wake-ups per boot) | 2 (re-gated at 4a, 5 and 7, then nightly) |
| Longest preempt-off or IRQ-off section, excluding SMI-overlapped sections (counted and published) | < 50 µs (20 µs from Phase 10): no section above it in any stress boot | every stress boot (at least 20 per gate) | 2 |
| Audio underruns at 128 frames, 48 kHz, under full CPU and I/O load | 0 observed in ≥ 24 h, 95% upper bound ≤ 0.125 per hour (also reported at 64 and 256 frames) | 24 h per buffer size and load condition | 6 |
| Input-to-display latency under full CPU load | P99 no more than 1.5 times its idle P99 (nightly gate); P99.9 no more than 1.5 times its idle P99.9 (release gate); both no worse than matched Linux | one nightly window: 40 boots Optimal, 20 per Linux kernel, 5,000 events per condition per boot | 7 |
| Frame time P99 in the frame-pacing test at 60 Hz (bare metal, llvmpipe over simple framebuffer, against Linux booted `nomodeset` with simpledrm and llvmpipe; virtio-gpu trend-only) | ≤ refresh period + 1 ms | 20 boots, 5 min (18,000 frames) per boot | 7 |
| Frame time P99 at 144 Hz on the reference discrete GPU | ≤ refresh period + 1 ms | 20 boots, 5 min (43,200 frames) per boot | 8 |
| Foreground input latency P99 while a large copy or compile runs | ≤ 1.25 times idle | one nightly window, as above | 7 |
| Memory pressure test (8 GiB usable RAM as measured, 50 browser pages and a compile, [scheduler.md](scheduler.md)) | input latency P99 ≤ 1.5 times idle; frame time P99 ≤ refresh period + 2 ms; 0 OOM kills of the foreground app or the compositor; each no worse than matched Linux | 20 memory-pressure boots | 7 |
| Boot time, firmware handoff to kernel ready | mean < 300 ms (a Phase 1 done-when item) | 20 boots | 1 |
| Total OS memory at an idle desktop (Phase 7 done-when) | ≤ 60% of the lightest mainstream Linux desktop on the same machine | 20 boots per OS | 7 |
| Idle CPU over the defined window (Phase 7 done-when) | < 0.5% and no worse than matched Linux | 20 boots, one window each | 7 |
| Latency tracer cost when off, on the syscall round-trip microbenchmark | ≤ 0.5% against a build with the tracer compiled out | 40 boots per build, 7 runs each (SE ≤ 0.15%) | 3 |
| Storage set entries compared against Linux | each within 10% of matched Linux | 20 boots, at least 3 runs per entry per boot | 4a, 5 |
| Context switch and fork+exec against Linux (Core kernel costs) | each within 10% of matched Linux | 20 boots, 7 runs per boot | 5 |
| Storage set on a per-user home volume | each entry within 10% of the same entry on the system volume path | 20 boots, 3 runs per entry per boot | 5 |
| Large TCP download over the named NIC | ≥ 95% of Linux's measured goodput on the same link | 20 boots, 3 downloads of 60 s per boot | 6 |
| Games from the game list, average and 1% low frame rate | each within 15% of Linux | 20 boots, 3 runs of a pinned 60 s scene per title per boot (at least 5 titles) | 8 |
| Battery life, each of the three workloads | no worse than matched Linux | 20 runs per workload, each the mean discharge power over 1 h at fixed brightness | 10 |
| Panic lock (F4 in [features.md](features.md)): chord to keys wiped, writeback bound 0 | ≤ 200 ms | 20 boots, 10 chords each | 5 |
| VPN-only guard on (F6): large TCP download through the tunnel | ≥ 99% of the same with the guard off | 20 boots, 3 downloads of 60 s per boot | 6 |
| Game mode overlay (F8), 144 Hz direct scanout on the reference discrete GPU | CPU cost ≤ 0.5%; input-to-display P99 and frame time P99 no worse than overlay off; per-frame agreement with the input-latency rig within 0.5 ms at P99 | 20 boots per condition, 5 min per boot | 8 |
| Focus boost ×4 (F9): 60 Hz frame-pacing app in focus while a pinned compile runs | frame time P99 ≤ refresh period + 1 ms; compile wall time ≤ 1.5 times its time with the boost off | 20 boots per condition | 10 |
| Background freezing (F9): a frozen app shown again | first new frame presented ≤ 100 ms after it becomes visible | 20 boots, 20 thaws each | 10 |
| Power meter (F10): attribution error of each of two loads run together | within 10% of its solo increment; meter cost ≤ 0.1% CPU; a 10 W cap held at ≤ 11 W over 60 s | 20 runs per load level | 10 |
| Privacy dashboard recording (F2): TCP connect rate and 64-byte UDP send rate | each ≥ 99% of recording off | 20 boots, 3 runs each | 12 |
| Browser blocking (F15), EasyList and EasyPrivacy loaded | per-request match ≤ 10 µs at P99; page load over the 50-page corpus no worse than blocking off | 20 boots, the corpus once per boot | browser engine track |

## Responsiveness (the main event)

| Metric | Reported as | What it shows | From |
|---|---|---|---|
| Input-to-display latency | P50, P99, P99.9 | how quickly a key press or mouse move reaches the screen | 7 |
| Frame time | P50, P99, variance | consistency of frame pacing, idle and under load | 7 (60 Hz, bare metal llvmpipe over simple framebuffer), 8 (144 Hz, discrete GPU) |
| Audio underruns | count and 95% upper bound per hour at 64, 128 and 256 frames (48 kHz), idle and under load | real-time scheduling quality | 6 |
| Scheduling latency under saturation | P99, P99.9 wake-up latency of a real-time thread | whether interactive work cuts through a full system | 2 |
| Foreground responsiveness during background I/O | input latency and app launch time while a large copy or compile runs | whether background work makes the desktop feel slow | 7 |
| Memory pressure | input latency P99, frame time and OOM kills with 8 GiB usable RAM, 50 pages from a pinned offline corpus and a pinned compile ([scheduler.md](scheduler.md)); target above | the biggest real cause of stutter | 7 |
| App launch time | cold and warm, P50/P99 | first impression speed | 5 |

## Core kernel costs

| Metric | From |
|---|---|
| Boot time (firmware handoff to kernel ready, and to desktop) | 1 |
| Timer and interrupt latency | 1 |
| Context switch and thread wake-up, **kernel threads, Optimal against itself only** | 2 |
| Context switch, wake-up and messaging **against Linux**, using identical static binaries on both (pipe ping-pong, futex ping-pong, hackbench, schbench, a cyclictest equivalent), with the switch including an address-space change and full XSAVE | 5 |
| Syscall round trip; vDSO `clock_gettime`, 64-bit and 32-bit (vDSO32 `__kernel_vsyscall` path) | 3 |
| fork+exec of a static binary with 10, 10^4 and 10^5 open fds (the open-file-description layer in [capabilities.md](capabilities.md)), within 10% of matched Linux at each count | 5 |
| Page fault cost (4 KiB from 3, 2 MiB from 5 when THP lands; minor and major), mmap/munmap, page-fault and mmap scaling across cores | 3, 5 |
| TLB shootdown latency against core count | 2 |
| Memory allocation (kernel and user) | 1 (kernel), 3 (user) |
| Cross-NUMA-node memory latency and bandwidth (function tests in QEMU from 2; numbers only on the multi-node reference machine) | 2 |
| Storage set: the table below | 4a, 5 |
| Driver tier cost: per-I/O latency and throughput for the same device in tier 1, 2 and 3 | 4a |
| IOMMU strict-invalidation cost | 4a |
| Worst CPU stall and worst real-time wake-up latency on surprise removal of a PCIe device ([scheduler.md](scheduler.md)) | 4a |
| GPU VRAM and GTT buffer clear before first mapping (copy/clear engine), cost per MiB and per allocation | 8 |
| Network throughput and latency (TCP and UDP, 1 and 8 streams, on a named NIC at a named link speed) | 6 |
| Overall throughput: kernel-size C compile, `cargo build` of a large crate, Zstd compression, Blender CPU render | 5 |
| Browser: Speedometer 3, JetStream 2 | 7 |
| Games: at least 5 titles from the game list, average and 1% low frame rate | 8 |
| Suspend and resume time | 10 |

## Storage set

The one list of storage benchmarks; [storage.md](storage.md) links here and
sets the conditions (aged images restored per run, drive preconditioning,
encryption on for both: LUKS2 on Optimal, dm-crypt LUKS2 with the same
cipher on Linux, ext4 default mount options). Every entry compared
against Linux runs the same binary on both (a static binary in 4a). The
Phase 4a done-when is "every 4a entry compared against Linux is within 10%
of matched Linux": the worse end of its Holm-corrected 95% confidence
interval, over at least 20 boots per kernel, against both matched LTS and
matched stable.

| Benchmark | Against Linux | From |
|---|---|---|
| fio `psync`, 4k random read, QD1 | yes | 4a |
| fio `psync`, 4k random overwrite, QD1 | yes | 4a |
| fio `psync`, 1M sequential write, QD1 | yes | 4a |
| 1 MiB sequential `O_DIRECT` write at QD32 (fio `psync`, 32 forked static processes, each its own region; the replay log's worst case, [storage.md](storage.md)) | yes | 4a |
| page-rewrite microbenchmark: the same 4 KiB page rewritten and written back in a loop (the SQLite WAL-tail pattern) | yes | 4a |
| `fsync` burst: 1,000 `fsync`s of distinct 4 KiB files within 1 s, latency P50, P99, P99.9 (full commits are capped at one per second, so the burst goes through the per-file intent log, [storage.md](storage.md)) | yes | 4a |
| `syncfs` latency (one full commit) after 1 GiB of dirty data, and the full-commit rate under a continuous `fsync` load (at most 1 per second) | yes (`syncfs` latency); the rate is Optimal only | 4a |
| fio `psync`, 4k random read and 4k random overwrite at QD32, as 32 forked static `psync` processes (fio's default process mode, `numjobs=32`) | yes | 4a |
| fsync latency P50, P99, P99.9 | yes | 4a |
| fsync latency P50, P99, P99.9 of a 4 KiB file while a background writer streams 1M sequential writes | yes | 4a |
| SQLite in WAL mode, static binary (insert and point-query mix) | yes | 4a |
| untar of a pinned source tree, static binary | yes | 4a |
| metadata: create, stat, unlink of 1 million small files | yes | 4a |
| in-kernel block benchmark at QD32 | no: Optimal against itself only (there is no identical program on Linux) | 4a |
| fio `io_uring`, 4k random read and overwrite at QD32 | yes | 5 |
| pgbench | yes | 5 |
| git checkout and `git status` on a pinned large repository | yes | 5 |
| the whole set above, rerun on a per-user home volume ([storage.md](storage.md)); each entry within 10% of the same entry on the system volume path | yes (Linux: ext4 on dm-crypt over `dm-linear` with the same extents) | 5 |

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
  connected. Target: below 0.5% and no worse than matched Linux, decided
  as in "How a target is decided", a Phase 7 done-when item.
- **Wake-ups per second** over the same window.
- **Idle power** over the same window at fixed screen brightness.
- **Battery life** on the reference laptop at fixed brightness for three
  workloads: idle desktop, a 1080p video loop, and a scripted web-browsing
  loop. Target: no worse than matched Linux on all three by Phase 10, as in
  the targets table (one target, the same for idle power and battery).

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
- Named exposures per machine (boot DMA window, remote DMA exposure, SMT on
  affected CPUs), the ATS-enabled devices and their firmware (part of the
  trusted base), the B2 anti-rollback scope: "holds against OS-level
  attackers only" (against physical attackers only with owner keys), and
  "B4: no automatic rollback, no floor" until Optimal Boot's BIOS stage
  lands ([boot.md](boot.md)).
- Disk encryption stage per machine, and "stage 2: new installs only"
  until the per-user stage 1 to stage 2 migration lands
  ([storage.md](storage.md)).
- Residual risks the threat model accepts, each with its test result: a
  compromised Bluetooth stack (input accepted only from paired HID
  devices whose link keys it doesn't hold), Rowhammer (page tables in a
  separate guarded zone; ECC and TRR status per machine, and the result of
  the published hammering PoC), and machines with fewer than 4 CPUs, where
  hot-removable device handlers can't be kept off real-time CPUs
  ([scheduler.md](scheduler.md)).
- **Prompts per typical day:** the number of permission prompts after
  first run in the scripted typical-day workload (install 5 apps, browse,
  download, plug in a known keyboard and a USB stick), at each prompt
  level ("Always ask", "Only when suspicious", "Almost never"); target at
  most 5 at the default "Only when suspicious", counted by the
  threat-model test ([threat-model.md](threat-model.md),
  [capabilities.md](capabilities.md)), from Phase 7.
- The Argon2id parameters chosen on each reference machine, and the TPM
  keyslot's PBKDF2 iteration count (exempt from the Argon2id floor: its
  input is high-entropy, [storage.md](storage.md)).
- Measured bandwidth of each covert channel between apps (covert channels
  are out of scope; the overt-channel list is tested,
  [threat-model.md](threat-model.md)).
- The tough-conditions and Secure Boot attack results.
- Fuzzing hours and executions per target, and open crash bugs found by
  fuzzing.
- Proof targets with their assurance level ([verification.md](verification.md)).
- Lines in tier-0 crates against the cap, the unsafe-block count, and the
  compositor's own lines against its 60,000-line cap (Mesa listed
  separately with its exposure).
- Driver-interrupt maskings and driver restarts seen in the soak.
- **Optional features** ([features.md](features.md)): each feature's
  default and whether it is on in the shipped image, and the measured
  security results: buffers delivered before a sensor indicator was
  presented (target 0, F1); packets outside the tunnel from power-on and
  across 1,000 VPN-client kills (target 0) and plain-DNS packets with
  encrypted DNS on (target 0, F6); the tamper alert's scope per boot tier
  (F7); the panic-lock residual (frozen apps' memory, DRAM remanence and
  cold boot, F4); the hidden vault's limits in the words of its Settings
  text (defeats one-time inspection of a powered-off disk; not two images
  over time, a compromised running system, forced unlock of the outer
  volume while the hidden one is open, TRIM-pattern analysis if the
  no-TRIM rule is broken, or the SSD's own flash), with the distinguisher
  result and the two-image result (F3); travel mode's limits and residual
  count for converted folders (F5); and **unpatched time**: a staged
  security fix takes effect only at a restart the user chooses, so the row
  gives the time from each security release to its availability to the
  update client and says that time to restart is the user's, because
  Optimal measures nothing on users' machines (F14).
- Open security bugs by severity.
