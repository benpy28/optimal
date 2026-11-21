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
than mass storage's transport layer, the 802.11 and Bluetooth protocol
stacks, foreign and removable file systems ([storage.md](storage.md)),
webcams, and the network configuration services. Device-facing drivers we keep in tier 1 for speed
(NVMe, AHCI, xHCI, the GPU core) are no exception: they must meet the
thresholds before their phase closes, because a malicious device that crashes
a tier-1 driver crashes the kernel. The TCP/IP stack stays in the kernel for
speed under the same rule. Wi-Fi and Bluetooth are split on every machine:
a small tier-2 transport part (rings, DMA, firmware loading, no frame
parsing), which must meet the thresholds, and a tier-3 802.11 or Bluetooth
stack.

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
| Polled xHCI boot-protocol keyboard reader (8-byte boot reports only, no HID report descriptors) | 4a | 1, thresholds required |
| xHCI host controller and hub logic | 6 | 1, thresholds required |
| USB boot-protocol keyboard and mouse | 6 | 2, thresholds required |
| Full USB HID (report descriptors), other USB classes (audio, video, serial) | 6 | 3 → 2 |
| USB mass storage transport | 6 | 2, thresholds required |
| Thunderbolt/USB4 connection manager | 6 | 2, thresholds required |
| virtio-net, Intel and Realtek Ethernet | 6 | 3 → 2 (fuzzed to the thresholds within Phase 6) |
| Wi-Fi and Bluetooth transport (rings, DMA, firmware loading; no frame parsing) | 6 | 2, thresholds required |
| 802.11 and Bluetooth protocol stacks | 6 | 3 (radios on D2 machines: see the IOMMU section) |
| HD Audio | 6 | 2 |
| USB Audio Class 2 | 6 | 3 → 2 |
| Embedded controller, battery, backlight, lid and power buttons | 6 | 2 |
| I2C-HID touchpads | 6 | 3 → 2 (HID report descriptors) |
| virtio-gpu, simple framebuffer DRM | 7 | 1 |
| Real GPU (see below) | 8 | split: 1 (core), 2 (display and power programming), 3 (management) |

## Hardware access: the only doors

Drivers in tiers 1 to 3 touch hardware only through typed handles the core
gives them. Tier-1 and tier-2 drivers never see a physical or bus
address; a tier-3 driver sees only IOVAs in its own domain.

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
- **`DmaBuffer<T>`, `DmaPool` and `DescRing<D>`**: memory the core
  allocated, pinned and mapped into the device's IOMMU domain. Buffers are
  whole pages or come from a pool whose pages hold only that device's
  buffers, so no other kernel data shares a page with a DMA buffer. Memory
  the device reads addresses from (submission queues, descriptor and TRB
  rings, virtqueues, device contexts) is a `DescRing<D>`: the driver writes
  typed descriptors `D` whose address fields are `Iova` values (the address
  the device sees), and an `Iova` can be built only from a granted
  `DmaBuffer` or streaming-map handle, with offset and length checked
  against it; the driver has no raw byte access to that memory. NVMe PRP
  lists and SGLs are built the same way, by typed builders. Registers that
  take a DMA address (NVMe ASQ/ACQ, ring base registers, xHCI DCBAAP) are
  core-only: they are left out of the driver's `MmioRegion`, and the core
  writes them only from a `DmaBuffer` handle. So a tier-1 or tier-2 driver
  can't express an arbitrary device address. Tier-3 drivers get raw ring
  memory and rely on the IOMMU alone.
- **Streaming maps** for zero-copy I/O: page-cache and user (`O_DIRECT`)
  pages can be lent to a device for one request. The frame keeps its owner
  (the memory object) and gains a recorded DMA pin; it can't be freed or
  remapped while pinned, and the pin is released only after the unmap's
  invalidation completes. This avoids a bounce copy on every disk and
  network I/O ([verification.md](verification.md) targets 2 and 10). Each
  map carries a direction: to-device maps are read-only in the IOMMU,
  from-device and bidirectional maps are writable. A buffer that doesn't
  cover whole pages is bounced when the device is behind a hotplug-capable
  port or its driver is tier 3, so the rest of the page is never exposed.
- **Config space** reads go through the core; writes are allowed only to a
  per-device-class list of registers, never to BARs, MSI capability
  registers or another function.

## The IOMMU, from Phase 1

- **Before anything else touches a device**, in Phase 1c the core walks the
  PCI buses (through ECAM from the ACPI MCFG table), hands USB controllers
  over from firmware (the xHCI/EHCI ownership handshake, so the firmware's
  legacy keyboard emulation stops cleanly), and then clears Bus Master
  Enable on every function except the IOMMU's own and the firmware display
  (below). Bridges lose BME too; a bridge's BME is set again only when the
  first device below it is assigned a domain. UEFI boot-services memory and
  Limine's bootloader-reclaimable memory are not reclaimed until BME is
  cleared and the IOMMU is live, because a device the firmware left running
  may still DMA into them.
- **Firmware protection carries over.** If the DMAR table sets the
  DMA-protection opt-in flag, or Intel Protected Memory Regions are enabled,
  they stay on until the kernel's tables are live; the PMRs are turned off
  (PMEN cleared, PRS checked) only after translation runs on the kernel's
  root table. If translation is already on at handoff (GSTS.TES set by the
  firmware or a previous kernel), the core never turns it off: it loads its
  own root table with SRTP while translation stays on, then does a global
  context-cache and IOTLB invalidation. On AMD-Vi, if the IOMMU is already
  enabled, the core switches to its own device table only while every
  function except the firmware display has BME clear, with the display's
  scanout range already mapped in the new table.
- **Firmware display.** The device scanning out the firmware framebuffer,
  and the bridges on its path, keep BME so the screen stays on. Once the
  IOMMU is live, its domain maps only the framebuffer range, read-only, plus
  its RMRRs. Until then (and always on D2) this is a named exposure,
  "firmware display DMA", in the boot log and on the scorecard.
- **Deny by default.** Each device gets its own domain with nothing mapped;
  only `DmaBuffer`s and streaming maps appear in it. Firmware-reserved
  regions (RMRR, IVMD) are mapped only for the devices that need them,
  logged, and shown on the scorecard.
- **What counts as D1.** A machine is DMA tier D1 only if every PCI segment
  is covered by an IOMMU unit (DMAR INCLUDE_PCI_ALL or scopes listing every
  device; IVRS entries for every device), every I/O APIC and HPET is in a
  unit's device scope, interrupt remapping is on, compatibility-format
  interrupts are blocked (on Intel, GCMD.CFI left 0 and GSTS.CFIS checked to
  read 0), and x2APIC is used where the CPU has it. Otherwise it is D2, and
  the boot log, Settings and scorecard name the missing condition. Devices
  an IOMMU unit does cover keep their domains instead of bounce buffers;
  every other D2 rule below applies.
- **ACS.** On every root and downstream port with the ACS capability the
  core enables Source Validation, P2P Request Redirect, P2P Completion
  Redirect, Upstream Forwarding and Translation Blocking, so a device can't
  send a requester ID from outside its port's bus range or reach a peer
  without passing the IOMMU.
  Devices that still can't be separated (one IOMMU group, such as functions
  behind a port without ACS) share one domain and must all be driven by the
  same tier or higher. External devices behind a port without ACS share one
  domain; the approval prompt names every device in the group, and approval
  is refused if the group holds an internal device.
- **ATS (devices sending pre-translated addresses) is off by default** for
  every device. It is turned on only for a named internal device where a
  benchmark shows a need, never for a tier-3 device or one behind a
  hotplug-capable port (all of which are treated as external, whether or
  not ACPI marks them so). When it is on, unmap also waits for the device's
  own translation cache invalidation to complete.
- **Free only after invalidation.** A page unmapped from a domain is freed
  only after its IOTLB invalidation (and device-TLB invalidation, if ATS is
  on) has completed. Invalidations may be batched for speed, as long as
  freeing waits for the batch; Linux's lazy mode, which frees before the
  flush, is not used. The cost is benchmarked.
- **Fault storms.** IOMMU faults are logged and rate-limited per device:
  past 100 in one second, the core suppresses further fault reporting for
  that device (Fault Processing Disable in its context and interrupt
  remapping entries on Intel, SA in its device table entry on AMD), clears
  its BME and stops its driver (below).
- **D2** (no IOMMU, or one short of D1; [threat-model.md](threat-model.md)):
  every `DmaBuffer` and streaming map of an uncovered device is backed by
  bounce buffers in a dedicated region, and with the typed descriptor rules above, tier-1 and tier-2
  driver bugs still can't aim DMA at kernel data; a malicious device, or its
  firmware, can. Thunderbolt and USB4 PCIe tunnels are refused. Because
  tier 3 relies on the IOMMU alone, DMA-capable devices whose drivers would
  be tier 3 get an in-kernel tier-2 build of their driver instead, marked
  degraded on the scorecard. Wi-Fi and Bluetooth radios stay off until the
  user accepts a named "remote DMA exposure" warning, because radio
  firmware compromised over the air could DMA anywhere; the scorecard then
  says the promise that nobody can read a user's data remotely does not
  hold on that machine. The boot log, Settings and scorecard say "DMA
  protection: off".
- **Test (Phase 1c gate):** QEMU machines with an emulated Intel IOMMU and
  an emulated AMD IOMMU, each with a deliberately hostile test device that
  tries DMA reads and writes outside its domain, forged MSI writes,
  compatibility-format MSIs, requests behind an ACS port carrying another
  device's requester ID, and a fault storm; and a device that DMAs continuously into
  boot-services memory from firmware through ExitBootServices and IOMMU
  bring-up. Every attempt must fault or be dropped and be logged, no write
  may land after BME is cleared or in a reclaimed page, and the kernel must
  keep running.
- **Hardware test and audit (Phase 4a gate):** the same attacks from a
  PCILeech-class FPGA board in a reference machine, and the outside audit
  of the IOMMU, DMA and driver boundary code, are in Phase 4a's done-when.

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
  A line over its limit is masked and reported: by its per-vector mask bit
  (MSI-X, or MSI with per-vector masking); for MSI without it, by marking
  its interrupt-remapping entry not present with Fault Processing Disable
  set, or where there is no remapping by clearing MSI Enable and setting
  Interrupt Disable in the command register. Timer, audio and input lines
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
| IOMMU fault storm | over 100 faults in one second (IOMMU section) | suppress fault reporting, clear BME, stop, restart |
| Device error or removal | AER/DPC interrupt, confirmed all-ones reads, hotplug event | stop, then restart if the device is still there |

**Stopping a driver**, in this order:

1. Stop accepting new requests.
2. Park its threads: they are taken off the run queues and never resumed
   (a thread stuck in a loop can't be cancelled from inside safe code, so it
   is simply never run again; its stack is freed with its arena).
3. Clear the device's Bus Master Enable, then poll its Transactions Pending
   bit (bounded, 100 ms) so no DMA is in flight.
4. Reset, taking the first method the device supports: a device-specific
   reset from the quirk table, FLR, Advanced Features FLR, D3hot to D0 if
   the device's No_Soft_Reset bit is clear, and a slot or secondary bus
   reset only when every function below that bridge is stopped. Then wait
   100 ms and poll the Vendor ID with CRS Software Visibility until the
   device answers, up to 60 s (Linux's limit). A device with no working
   method stays off, BME clear and no domain, until reboot, and is reported.
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
load) into every tier-2 and tier-3 driver. Each must be contained, including
on an emulated device with no FLR and one whose reset never completes. For
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
  kept small (at most 30,000 lines per vendor, generated register headers
  excluded, reported on the scorecard), must meet the promotion thresholds
  (it parses untrusted command streams), and its reset paths never panic.
  It is proof target 13.
- **Display and power programming (tier 2):** display engine programming
  and power and clock sequencing run under the tier-2 framework. Their
  register access goes through an accessor that checks every offset against
  a per-ASIC list of display and power register ranges, because GPU
  registers aren't split at page granularity and the page-table base and
  command-submission registers must stay with the GPU core.
- **GPU management (tier 3):** modesetting policy, power and clock policy,
  display detection. It gets **no register access**; it calls typed
  operations ("set this mode", "set this clock level"), which the tier-2
  part validates and performs.
- **GPU firmware is in the TCB.** The GPU's microcontrollers (Intel GuC,
  AMD MES, PSP and SMU) schedule contexts and handle GPU security, so a
  firmware bug can break buffer isolation within the GPU. They are in the
  TCB list in [threat-model.md](threat-model.md), each blob pinned by
  SHA-256 with its version in the firmware ledger; the IOMMU still confines
  them to the GPU's domain.
- **Isolation test:** a process tries to read another process's GPU buffers
  through every path (submitting commands that reference foreign addresses,
  guessing buffer handles, dma-buf imports it wasn't given); all must fail.
- **Hang recovery:** a hung engine is reset within 2 seconds without losing
  the desktop session; only the guilty context is killed. When engine reset
  fails, the whole adapter is reset: the session must survive (the
  compositor rebuilds its GPU state, other apps get a device-lost error),
  and adapter reset time is measured and published separately.
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
