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
| 2. Restricted drivers | kernel | us, or vendored code we audited line by line; `forbid(unsafe_code)`, no `build.rs` or proc macros | as tier 1, plus: own kernel threads, no core spinlocks, every resource through revocable handles, a panic or stack overflow outside core code poisons and stops the component instead of crashing the kernel ([core.md](core.md)) | first-party drivers that are less mature or for less-tested hardware |
| 3. User-space drivers | own process | anyone, any language | hardware address-space isolation plus its own IOMMU domain; crash restarts the process | third-party drivers, anything that parses untrusted input by default (below), printers, scanners, experimental hardware |

Tier 2 and tier 3 drivers use the **same message interface** (below), so a
driver can move between them without a rewrite. That is also why tier 2,
despite being in the kernel, talks through queues instead of direct calls:
the copy-then-validate discipline and revocable handles are what make a clean
kill and restart possible.

**Placement rule.** A driver that parses input an attacker can control (a
device, the network, a file system image) goes in tier 3, unless it meets the
promotion thresholds below, which every in-kernel parser of attacker input
must meet **before the phase that ships it closes**. That puts these in tier 3
at first: HID report-descriptor parsing (the in-kernel HID path only handles
the fixed boot-protocol keyboard and mouse formats), USB class drivers other
than mass storage's transport layer, Wi-Fi and Bluetooth, foreign and
removable file systems ([storage.md](storage.md)), webcams, and the network
configuration services. Device-facing drivers we keep in tier 1 for speed
(NVMe, AHCI, xHCI, the GPU core) are no exception: they must meet the
thresholds before their phase closes, because a malicious device that crashes
a tier-1 driver crashes the kernel. The TCP/IP stack stays in the kernel for
speed under the same rule.

**Promotion on evidence, with numbers.** A driver is allowed in the kernel
(or moves towards more trust) only after: 10^9 device-side fuzz executions
with no open crash, 90% branch coverage of its code under fuzzing, 30 days in
the nightly soak with no fault, and a review. A tier-3 driver that becomes
performance critical is audited and rewritten into tier 2, not just
promoted.

**Placement of the drivers in the roadmap** ("3 → 2" means tier 3 until it
meets the thresholds):

| Driver | Phase | Tier |
|---|---|---|
| Local APIC, I/O APIC, HPET, TSC deadline, ACPI timer | 1 | 0 |
| Minimal PCI: ECAM/MCFG config access, bus walk, ACS and capability parsing, Bus Master Enable control | 1c | 0 |
| IOMMU (Intel VT-d via DMAR, AMD-Vi via IVRS) | 1c | 0 |
| ACPI table parsing (safe crate over byte slices) | 1c | 1 |
| AML interpreter | 1c | hosted by the core on the boot thread with an operation budget until Phase 2, its own budgeted thread from Phase 2, the tier-2 framework from Phase 4 (below) |
| Framebuffer, serial, PS/2 | 1, 4 | 1 |
| PCI(e) enumeration policy, hotplug, AER/DPC | 4 | 1 (config space and MSI tables stay in tier 0) |
| NVMe, AHCI | 4 | 1, thresholds required |
| virtio-blk | 4 | **2**: the first tier-2 driver, so the restart machinery is exercised from Phase 4 |
| TPM 2.0 (CRB, TIS) | 4 | 0 (MMIO) and 1 (protocol, a safe crate) |
| xHCI host controller and hub logic | 6 | 1, thresholds required |
| USB boot-protocol keyboard and mouse | 6 | 2 |
| Full USB HID (report descriptors), other USB classes (audio, video, serial) | 6 | 3 → 2 |
| USB mass storage transport | 6 | 2 |
| Thunderbolt/USB4 connection manager | 6 | 2 |
| virtio-net, Intel and Realtek Ethernet | 6 | 3 → 2 (fuzzed to the thresholds within Phase 6) |
| Wi-Fi, Bluetooth | 6 | 3 (on D2 machines, see the IOMMU section) |
| HD Audio | 6 | 2 |
| USB Audio Class 2 | 6 | 3 → 2 |
| Embedded controller, battery, backlight, lid and power buttons | 6 | 2 |
| I2C-HID touchpads | 6 | 3 → 2 (HID report descriptors) |
| virtio-gpu, simple framebuffer DRM | 7 | 1 |
| Real GPU (see below) | 8 | split |

## Hardware access: the only doors

Drivers in tiers 1 to 3 touch hardware only through typed handles the core
gives them. They never see a physical or bus address.

- **`MmioRegion`** for each BAR listed in the driver's capability set, with
  bounds-checked, width-typed reads and writes. When a read returns all ones
  (what a removed PCIe device returns), the core confirms by reading the
  device's Vendor ID; if that is all ones too, the read returns a "device
  gone" error instead of data, so a surprise removal can't make a driver loop
  or index out of bounds on garbage, and registers that legitimately read all
  ones still work.
- **The MSI-X table and pending-bit array are never writable by a driver**,
  even though they live in a device BAR. The core alone programs them;
  otherwise a driver could aim interrupts at any CPU and vector. When the
  table shares a 4 KiB page with registers the driver needs, that page is
  mapped read-only (tier 3) or accessed only through the core (tiers 1 and
  2), and writes to its non-table registers are trapped and emulated; the
  cost is benchmarked per device.
- **`IoPorts`** for the port ranges of that device only.
- **`IrqHandle`**: the core allocates MSI/MSI-X vectors (or routes INTx),
  acknowledges the interrupt, and wakes the driver's handler thread
  ([scheduler.md](scheduler.md)). Drivers never run in hard-interrupt
  context and never touch the APIC. A level-triggered or legacy line stays
  masked from the hard interrupt until the handler thread says it's done. A
  legacy line is never shared between drivers of different tiers; if the
  hardware forces it, the lower-trust device is polled instead.
- **`DmaBuffer<T>` and `DmaPool`**: memory the core allocated, pinned and
  mapped into the device's IOMMU domain. A driver puts a buffer into a device
  descriptor only through a core helper that writes the buffer's IOVA (the
  address the device sees) into the descriptor field, so the driver can't
  express an arbitrary address. Buffers are whole pages or come from a pool
  whose pages hold only that device's buffers, so no other kernel data
  shares a page with a DMA buffer.
- **Streaming maps** for zero-copy I/O: page-cache and user (`O_DIRECT`)
  pages can be lent to a device for one request. The frame keeps its owner
  (the memory object) and gains a recorded DMA pin; it can't be freed or
  remapped while pinned, and the pin is released only after the unmap's
  invalidation completes. This avoids a bounce copy on every disk and
  network I/O ([verification.md](verification.md) targets 2 and 10).
- **Config space** reads go through the core; writes are allowed only to a
  per-device-class list of registers, never to BARs, MSI capability
  registers or another function.

## The IOMMU, from Phase 1

- **Before anything else touches a device**, in Phase 1c the core walks the
  PCI buses (through ECAM from the ACPI MCFG table), hands USB controllers
  over from firmware (the xHCI/EHCI ownership handshake, so the firmware's
  legacy keyboard emulation stops cleanly), and then clears Bus Master
  Enable on every function except the IOMMU's own, so nothing can DMA until
  the core assigns it. If the firmware's DMAR table sets the DMA-protection
  opt-in flag, firmware protection stays on until the kernel's own tables
  are live, so there's no unprotected window.
- **Deny by default.** Each device gets its own domain with nothing mapped;
  only `DmaBuffer`s appear in it. Firmware-reserved regions (RMRR, IVMD)
  are mapped only for the devices that need them, logged, and shown on the
  scorecard.
- **Interrupt remapping is required** where the hardware has it, so a device
  can't forge interrupt messages. (It is also needed for more than 255
  CPUs with x2APIC.)
- **ATS (devices sending pre-translated addresses) is off by default** for
  every device. It is turned on only for a named internal device where a
  benchmark shows a need, never for a tier-3 device or one behind a
  hotplug-capable port (all of which are treated as external, whether or
  not ACPI marks them so). When it is on, unmap also waits for the device's
  own translation cache invalidation to complete.
- **Devices that can't be separated** (in the same IOMMU group, such as
  functions behind a bridge without ACS) share one domain and must all be
  driven by the same tier or higher.
- **Free only after invalidation.** A page unmapped from a domain is freed
  only after its IOTLB invalidation (and device-TLB invalidation, if ATS is
  on) has completed. Invalidations may be batched for speed, as long as
  freeing waits for the batch; Linux's lazy mode, which frees before the
  flush, is not used. The cost is benchmarked.
- **No IOMMU** (DMA tier D2 in [threat-model.md](threat-model.md)): every
  `DmaBuffer` and streaming map is backed by bounce buffers in a dedicated
  region, so driver bugs still can't aim DMA at kernel data; Thunderbolt and
  USB4 PCIe tunnels are refused; and because a tier-3 process could aim DMA
  anywhere, DMA-capable devices whose drivers would be tier 3 (Wi-Fi first)
  get an in-kernel tier-2 build of their driver instead, marked degraded on
  the scorecard. The boot log, Settings and scorecard say "DMA protection:
  off".
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
- **CPU:** fair-class handler threads are scheduled under a fair-share
  group per driver, so an over-budget driver is slowed, not stopped
  mid-operation ([scheduler.md](scheduler.md)). Real-time handler threads
  (audio, input) are charged against a per-driver real-time bandwidth cap,
  default 5% of one CPU per 100 ms, so a runaway one can't use the whole
  real-time budget. Storage, network and GPU drivers get class-sized shares
  set from measurements at line rate and published; everything else
  defaults to 5% of one CPU sustained over 100 ms, with bursts allowed.
- **Interrupt storms:** limits are per device class, set from measurements
  of the highest legitimate rate (an NVMe or NIC MSI-X vector legitimately
  takes several hundred thousand per second), detected over 10 ms windows.
  A line over its limit is masked and reported. Timer, audio and input lines
  are rate-limited to their limit instead of masked, so a faulty touchpad
  can't storm forever and audio isn't cut off.
- **Locks:** tier-2 and tier-3 code can't hold core spinlocks at all.

Charging and releasing budgets is proof target 7.

## Faults, stopping and restarting

| Fault | Detection | Handling |
|---|---|---|
| Panic or stack overflow (tier 2) | the panic handler or the double-fault handler sees a tier-2 thread outside core code ([core.md](core.md)) | poison, stop, restart (below) |
| Panic (tier 3) | process exits | stop, restart |
| Endless loop or hang | per-component watchdog: no progress on its rings with work queued for longer than its device class's timeout (storage 30 s, matching Linux's I/O timeout, so spinning-up disks aren't killed; network 5 s; input and audio 1 s); an NMI watchdog catches a CPU stuck with interrupts off for 10 ms anywhere in the kernel | stop, restart (tier 2 and 3); crash report (tier 0 and 1) |
| Out of memory | allocation fails | the driver handles the error; repeated failure is reported |
| Interrupt storm | per-class limit above | mask or rate-limit, report, then stop and restart if it continues |
| Device error or removal | AER/DPC interrupt, confirmed all-ones reads, hotplug event | stop, then restart if the device is still there |

**Stopping a driver**, in this order:

1. Stop accepting new requests.
2. Park its threads: they are taken off the run queues and never resumed
   (a thread stuck in a loop can't be cancelled from inside safe code, so it
   is simply never run again; its stack is freed with its arena).
3. Clear the device's Bus Master Enable, then poll its Transactions Pending
   bit (bounded, 100 ms) so no DMA is in flight.
4. Function Level Reset (or secondary bus reset if FLR isn't supported).
5. Tear down its IOMMU domain and wait for invalidation to complete.
6. Unmap its MMIO with a TLB shootdown.
7. Hand its in-flight requests back to the subsystem above (for storage, the
   block layer keeps and resubmits them to the new instance, see
   [storage.md](storage.md); they fail with `EIO` only if three restarts in a
   row fail).
8. Revoke all of its handles, poison its sleeping locks, free its arena.

Only then can a new instance start. Because the core holds every resource a
driver uses in a per-component handle table, nothing is left dangling.

Tier-1 drivers can't be restarted this way (a panic there is a kernel crash),
which is why tier 1 is small and gets the hardest testing.

**Tests:** CI injects each fault (panic, stack overflow, endless loop,
interrupt storm, allocation failure, surprise removal with `device_del` under
load) into every tier-2 and tier-3 driver. Each must be contained. For
storage (virtio-blk from Phase 4), I/O must resume within 500 ms of restart,
and the crash oracle in [storage.md](storage.md) must hold: every fsync'd byte
present, no error reported to applications for requests that were resubmitted
successfully. The
"kill a driver mid-I/O" test runs 10,000 times per night with no leak or hang.

## Hotplug, errors and external devices

- PCIe native hotplug and ACPI hotplug: devices can appear and disappear at
  runtime; domains and handles are created and destroyed dynamically.
- PCIe Advanced Error Reporting and Downstream Port Containment are handled:
  a containment event stops the affected drivers as above.
- **External PCIe (Thunderbolt, USB4) needs user approval** per device
  before its tunnel is opened ([threat-model.md](threat-model.md)), enforced
  by our own connection manager (Phase 6). On machines where the firmware's
  connection manager opens tunnels by itself, the IOMMU's deny-all default
  is what protects memory: the new device gets no DMA mapping and no driver
  until approved, and is reset if refused.
- **USB devices need authorisation** before their class driver binds, per
  the USB rules in the threat model.

## Power management in the interface from day one

Every driver implements `suspend`, `resume` and `reset` from the first
version of the interface (Phase 4), even before system suspend exists, and
they are exercised in CI (driver-level suspend/resume cycles in QEMU). Device
power states (D0 to D3cold) and runtime power management use these. Phase 10
then adds system suspend without rewriting drivers.

## ACPI's bytecode (AML)

Interrupt routing for legacy interrupts (`_PRT`), the `_PIC` and `_OSC`
handshakes, hotplug, power buttons, the embedded controller and sleep all need
AML evaluation, not just table parsing. The AML interpreter is written in safe
Rust, because firmware bytecode is untrusted input.

- **Hosting.** In Phase 1 it runs on the boot thread, called by the core,
  with an operation budget per evaluation (a looping method is stopped and
  logged). From Phase 2 it has its own budgeted thread; from Phase 4 it runs
  under the tier-2 framework.
- **Confinement.** AML declares its own operation regions at run time, so
  "only declared regions" would restrict nothing. Instead every access goes
  through the core, which refuses: system memory regions overlapping RAM in
  the firmware memory map or any core-owned range (IOMMU, APIC, HPET, TPM,
  MSI-X tables, device BARs assigned to drivers); I/O ports owned by drivers
  and the PCI config ports `0xCF8` and `0xCFC`; and PCI config writes to BAR,
  command (Bus Master Enable) and MSI registers of assigned devices. Every
  refusal is logged; a test runs a hostile DSDT that tries each.
- **Faults.** AML state can't safely be rebuilt (re-running `_INI` methods
  changes hardware), so a fault doesn't restart the interpreter: the kernel
  enters a reported "ACPI events disabled" mode (no hotplug, power button or
  lid events; MSI-routed devices keep working). Before Phase 4, an AML panic
  is a kernel crash.
- **Testing.** Fuzzed against a corpus of DSDT and SSDT tables dumped from at
  least 50 real machines. MSI and MSI-X are always preferred, so legacy
  interrupt routing is a fallback.

## Device-side fuzzing

The threat model includes malicious devices, so drivers are fuzzed from the
device's side: an emulated device (in QEMU or a host test harness) returns
fuzzed register values, DMA contents, descriptors, completion entries and USB
descriptors. Coverage is tracked per driver. Every in-kernel driver that
faces a device must reach the promotion thresholds before its phase closes;
tier-3 drivers must run 24 hours with no open crash.

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
  sync objects, engine reset, and firmware loading (each firmware file
  hash-checked by the core before the GPU's microcontrollers get it). This
  is the part that decides which process can read which buffer, so it is
  kept small, must meet the promotion thresholds (it parses untrusted command
  streams), and its reset paths never panic. It is proof target 13.
- **GPU management (tier 3):** modesetting policy, power and clock policy,
  display detection. It gets **no register mapping**: GPU registers aren't
  split at page granularity, so access to them would let it rewrite the GPU
  page-table base. Instead it calls typed operations on the GPU core ("set
  this mode", "set this clock level"), which validates and performs them.
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
