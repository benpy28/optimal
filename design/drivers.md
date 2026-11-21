# Drivers: trust tiers, hardware access, isolation and recovery

## What safe Rust does and doesn't buy

Optimal is a **memory-safe in-kernel component architecture**. Drivers and
subsystems written in safe Rust can't corrupt memory through use-after-free,
buffer overflows or data races, which removes the biggest class of kernel
security bugs. It is **not** a hardware isolation boundary like a microkernel's
separate address spaces. A safe-Rust component in the kernel still runs with
full privilege, and can still:

- exhaust memory, CPU time or interrupt capacity,
- deadlock, or hold a lock too long,
- get state transitions wrong (a logic bug is still a bug),
- race with hardware or with other CPUs in ways the type system doesn't see,
- program a device to DMA into memory it shouldn't touch,
- inherit bugs from `unsafe` code in the core or in a dependency.

And safe Rust is **not a defence against a hostile author**: the compiler has
known open soundness bugs that safe code can exploit, and `build.rs` scripts
and procedural macros run arbitrary code at build time. So in-kernel tiers are
for code we wrote or fully audited, and anything else runs in its own process.

## Trust tiers

| Tier | Runs where | Who may write it | Isolation | Use for |
|---|---|---|---|---|
| 0. Core | kernel | us only; `unsafe` allowed, audited, proof targets in [verification.md](verification.md) | none (it is the TCB) | paging, interrupt controllers (local APIC, I/O APIC), the timer hardware, context switch, the IOMMU, the TPM, the driver interface itself ([core.md](core.md)) |
| 1. Trusted drivers | kernel | us only; `#![forbid(unsafe_code)]` | type system, own IOMMU domain, budgets | hot-path drivers we write and test hardest: NVMe, AHCI, the framebuffer, virtio |
| 2. Restricted drivers | kernel | us, or vendored code we audited line by line; `forbid(unsafe_code)`, no `build.rs` or proc macros | as tier 1, plus: own kernel threads, no core spinlocks, every resource through revocable handles, unwinding to a poison-and-restart boundary | first-party drivers that are less mature or for less-tested hardware |
| 3. User-space drivers | own process | anyone, any language | hardware address-space isolation plus its own IOMMU domain; crash restarts the process | third-party drivers, anything that parses untrusted input by default (below), printers, scanners, experimental hardware |

Tier 2 and tier 3 drivers use the **same message interface** (below), so a
driver can move between them without a rewrite. That is also why tier 2,
despite being in the kernel, talks through queues instead of direct calls:
the copy-then-validate discipline and revocable handles are what make a clean
kill and restart possible.

**Placement rule.** A driver that parses input an attacker can control goes
in tier 3 by default, or in tier 1 or 2 only with device-side fuzzing at the
promotion thresholds below. That puts these in tier 3 at first: USB class
drivers other than HID boot keyboards and mass storage's transport layer,
Wi-Fi and Bluetooth, foreign and removable file systems
([storage.md](storage.md)), and webcams. The TCP/IP stack stays in the kernel
for speed and is continuously fuzzed from Phase 6.

**Promotion on evidence, with numbers.** A driver moves towards more trust
only after: 10^9 device-side fuzz executions with no open crash, 90% branch
coverage of its code under fuzzing, 30 days in the nightly soak with no
fault, and a review. A tier-3 driver that becomes performance critical is
audited and rewritten into tier 2, not just promoted.

**Placement of the drivers in the roadmap:**

| Driver | Phase | Tier |
|---|---|---|
| Local APIC, I/O APIC, HPET, TSC deadline, ACPI timer | 1 | 0 |
| IOMMU (Intel VT-d via DMAR, AMD-Vi via IVRS) | 1 | 0 |
| ACPI table parsing; AML interpreter | 1 (tables), 1 (AML, below) | 0 (tables), 2 (AML) |
| Framebuffer, serial, PS/2 | 1, 4 | 1 |
| PCI(e) enumeration, MSI/MSI-X, AER/DPC | 4 | 0 (config space and MSI tables), 1 (policy) |
| NVMe, AHCI, virtio-blk | 4 | 1 |
| TPM 2.0 (CRB, TIS) | 4 | 0 |
| xHCI host controller | 6 | 1 |
| USB HID (keyboard, mouse) | 6 | 2 |
| Other USB classes (storage, audio, video, serial) | 6 | 3 at first |
| virtio-net, Intel and Realtek Ethernet | 6 | 2 |
| Wi-Fi, Bluetooth | 6 | 3 |
| HD Audio, USB Audio Class 2 | 6 | 2 (HDA), 3 (USB audio) |
| Laptop basics: embedded controller, battery, backlight, I2C-HID touchpads, lid and power buttons | 6 | 2 |
| virtio-gpu, simple framebuffer DRM | 7 | 1 |
| Real GPU (see below) | 8 | split |

## Hardware access: the only doors

Drivers in tiers 1 to 3 touch hardware only through typed handles the core
gives them. They never see a physical or bus address.

- **`MmioRegion`** for each BAR listed in the driver's capability set, with
  bounds-checked, width-typed reads and writes. Every read checks for the
  "device gone" pattern (all ones from a removed PCIe device) and returns an
  error instead of data, so a surprise removal can't make a driver loop or
  index out of bounds on garbage.
- **The MSI-X table and pending-bit array are never mapped into a driver**,
  even though they live in a device BAR. The core alone programs them;
  otherwise a driver could aim interrupts at any CPU and vector.
- **`IoPorts`** for the port ranges of that device only.
- **`IrqHandle`**: the core allocates MSI/MSI-X vectors (or routes INTx),
  acknowledges the interrupt, and wakes the driver's handler thread
  ([scheduler.md](scheduler.md)). Drivers never run in hard-interrupt
  context and never touch the APIC.
- **`DmaBuffer<T>` and `DmaPool`**: memory the core allocated, pinned and
  mapped into the device's IOMMU domain. A driver puts a buffer into a device
  descriptor only through a core helper that writes the buffer's IOVA (the
  address the device sees) into the descriptor field, so the driver can't
  express an arbitrary address. Buffers are whole pages or come from a pool
  whose pages hold only that device's buffers, so no other kernel data
  shares a page with a DMA buffer.
- **Config space** reads go through the core; writes are allowed only to a
  per-device-class list of registers, never to BARs, MSI capability
  registers or another function.

## The IOMMU, from Phase 1

- **Before anything else touches a device**, the kernel clears Bus Master
  Enable on every PCI function it finds, so nothing can DMA until the core
  assigns it. If the firmware's DMAR table sets the DMA-protection opt-in
  flag, the kernel keeps firmware protection on until its own tables are
  live, so there's no unprotected window.
- **Deny by default.** Each device gets its own domain with nothing mapped;
  only `DmaBuffer`s appear in it. Firmware-reserved regions (RMRR, IVMD)
  are mapped only for the devices that need them, logged, and shown on the
  scorecard.
- **Interrupt remapping is required** where the hardware has it, so a device
  can't forge interrupt messages. (It is also needed for more than 255
  CPUs with x2APIC.)
- **ATS (devices sending pre-translated addresses) is off** for any device
  behind a port marked external by ACPI, and for any tier-3 device.
- **Devices that can't be separated** (in the same IOMMU group, such as
  functions behind a bridge without ACS) share one domain and must all be
  driven by the same tier or higher.
- **Strict invalidation.** When a buffer is unmapped, the IOTLB is
  invalidated and the invalidation has completed before the page can be
  freed. Lazy batching is not used, because it allows use-after-free by DMA.
  The cost is benchmarked.
- **No IOMMU** (hardware tier S3 in [threat-model.md](threat-model.md)):
  internal devices get `DmaBuffer`s backed by bounce buffers in a dedicated
  region, so driver bugs still can't aim DMA at kernel data; Thunderbolt and
  USB4 PCIe tunnels are refused; tier-3 drivers can't be given DMA-capable
  devices; the boot log, Settings and scorecard say "DMA protection: off".
- **Test (Phase 1c gate):** a QEMU machine with an emulated IOMMU and a
  deliberately hostile test device that tries DMA reads and writes outside
  its domain and forged MSI writes; every attempt must fault and be logged,
  and the kernel must keep running.

## The message interface (tiers 2 and 3)

The isolation layer is itself attack surface: if the boundary is complicated,
the boundary is where the bugs go. The design rule is **small,
one-directional, fuzzed and proved**.

- Each driver gets a pair of single-producer, single-consumer rings
  (requests in, completions out) in memory shared with the kernel, in the
  same spirit as virtio queues and io_uring. There is no general IPC and no
  shared kernel objects.
- Fixed-size descriptors, a small fixed set of message types per device
  class (block, network, input, audio, display, and so on).
- The kernel copies each descriptor out once, then validates the copy (no
  double fetch).
- Indices from the driver are bounds-checked; a bad index stops the driver.
- Data buffers are referred to by handle (an index into buffers the kernel
  granted), never by address.
- Interrupts arrive as a ring event (tier 2) or an eventfd-style notification
  (tier 3).
- **Cost is measured:** per-I/O latency and throughput for the same device in
  tier 1, 2 and 3, against Linux, are on the scorecard.
- Every ring entry point is fuzzed continuously with hostile drivers from
  Phase 4; the ring validation is proof target 9.
- Linux's VFIO and UIO interfaces are **not** offered to apps (they would
  bypass this layer). Tier-3 drivers use Optimal's own interface.

## Resource budgets

Every component in tiers 1 to 3 is charged against budgets set when it loads:

- **Memory:** a cap on heap and pinned pages, allocated from a per-component
  arena. Allocations past the cap fail; the kernel's allocation APIs are
  fallible, so this is an error the driver handles, not an abort.
- **CPU:** handler threads are scheduled under a fair-share group per driver,
  so an over-budget driver is slowed, not stopped mid-operation
  ([scheduler.md](scheduler.md)). Default: 5% of one CPU sustained per
  driver over a 100 ms window, with bursts allowed; tier-1 storage and
  network drivers get a larger share sized by measurement.
- **Interrupt storms:** a line firing far above what the device class can
  legitimately produce (default 100,000 per second for 1 second) is masked
  and the event is reported; timer, audio and input lines are never masked
  automatically, they are only reported.
- **Locks:** tier-2 and tier-3 code can't hold core spinlocks at all.

Charging and releasing budgets is proof target 7.

## Faults, stopping and restarting

| Fault | Detection | Handling |
|---|---|---|
| Panic (tier 2) | unwinding reaches the component boundary | poison, stop, restart (below) |
| Panic (tier 3) | process exits | stop, restart |
| Endless loop or hang | per-component watchdog: no progress on its rings for 2 s with work queued; an NMI watchdog catches a CPU stuck with interrupts off for 10 ms anywhere in the kernel | stop, restart (tier 2 and 3); crash report (tier 0 and 1) |
| Out of memory | allocation fails | the driver handles the error; repeated failure is reported |
| Interrupt storm | rate limit above | mask, report, then stop and restart if it continues |
| Device error or removal | AER/DPC interrupt, all-ones reads, hotplug event | stop, then restart if the device is still there |

**Stopping a driver**, in this order: stop accepting new requests; quiesce the
device with a Function Level Reset (or secondary bus reset if FLR isn't
supported); tear down its IOMMU domain and complete the IOTLB invalidation;
unmap its MMIO with a TLB shootdown; fail its in-flight requests with `EIO`;
revoke all of its handles; free its memory. Only then can a new instance
start. Because the core holds every resource a driver uses in a
per-component handle table, nothing is left dangling.

Tier-1 drivers can't be restarted this way (a panic there is a kernel crash),
which is why tier 1 is small and gets the hardest testing.

**Tests:** CI injects each fault (panic, endless loop, interrupt storm,
allocation failure, surprise removal with `device_del` under load) into every
tier-2 and tier-3 driver. Each must be contained, and for storage the I/O
must recover within 500 ms of restart with zero file system corruption. The
"kill a driver mid-I/O" test runs 10,000 times per night with no leak or hang.

## Hotplug, errors and external devices

- PCIe native hotplug and ACPI hotplug: devices can appear and disappear at
  runtime; domains and handles are created and destroyed dynamically.
- PCIe Advanced Error Reporting and Downstream Port Containment are handled:
  a containment event stops the affected drivers as above.
- **External PCIe (Thunderbolt, USB4) needs user approval** per device
  before its tunnel is opened ([threat-model.md](threat-model.md)).
- **USB devices need authorisation** before their class driver binds, per
  the USB rules in the threat model.

## Power management in the interface from day one

Every driver implements `suspend`, `resume` and `reset` from the first
version of the interface (Phase 4), even before system suspend exists, and
they are exercised in CI (driver-level suspend/resume cycles in QEMU). Device
power states (D0 to D3cold) and runtime power management use these. Phase 10
then adds system suspend without rewriting drivers.

## ACPI's bytecode (AML)

Interrupt routing for legacy interrupts (`_PRT`), hotplug, power buttons, the
embedded controller and sleep all need AML evaluation, not just table
parsing. The AML interpreter is written in safe Rust in tier 2: firmware
bytecode is untrusted input. It runs with an operation budget per evaluation
(a looping method is stopped and logged), can only touch the address ranges
the ACPI tables declare as operation regions, and is fuzzed against a corpus
of DSDT and SSDT tables dumped from at least 50 real machines. MSI and MSI-X
are always preferred, so legacy interrupt routing is a fallback.

## Device-side fuzzing

The threat model includes malicious devices, so drivers are fuzzed from the
device's side: an emulated device (in QEMU or a host test harness) returns
fuzzed register values, DMA contents, descriptors, completion entries and USB
descriptors. Coverage is tracked per driver. Phase 4 (storage) and Phase 6
(USB, network) can't close until their drivers have run 24 hours of this with
no open crash, and the promotion thresholds above apply.

## Firmware for devices

Many devices need vendor firmware (GPUs, Wi-Fi, some NICs, CPU microcode).
These are redistributable vendor binaries, treated as data: each is listed
with its licence and hash in the tree, loaded only by the core or the driver
that needs it, checked against its hash, and where the device supports it,
signature-checked by the device. They are allowed under the firmware policy
in the roadmap's licence section; no firmware runs on the CPU except
microcode.

## GPUs

A full GPU driver is huge (Linux's AMD driver is millions of lines), needs
hundreds of driver-specific ioctls for Mesa, and hangs routinely. It is split:

- **GPU core (tier 1):** command submission, the GPU's own page tables
  (GPUVM) with one GPU address space per process, buffer objects, dma-buf and
  sync objects, engine reset. This is the part that decides which process can
  read which buffer, so it is kept small and tested hardest.
- **GPU management (tier 3):** modesetting policy, power and clock
  management, firmware loading, display detection.
- **Isolation test:** a process tries to read another process's GPU buffers
  through every path (submitting commands that reference foreign addresses,
  guessing buffer handles, dma-buf imports it wasn't given); all must fail.
- **Hang recovery:** a hung engine is reset within 2 seconds without losing
  the desktop session; only the guilty context is killed.
- **Clean-room process.** Public documentation for AMD and Intel GPUs leaves
  out some initialisation sequences that only exist in the GPL Linux
  drivers. We never read those drivers. Missing information comes from the
  public register documentation, from observing the hardware (traces of the
  device's behaviour under other OSes, captured at the bus or MMIO level by
  a tracing tool, never from source code), or from a written specification
  produced by a separate person who studied the behaviour, so the
  implementer never sees GPL code. The process is written down and
  followed for every GPU driver.
- **Done-when (Phase 8)** includes a published pass rate on the Vulkan
  Conformance Test Suite and piglit, not just a game benchmark.
