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
at first: HID report-descriptor parsing (the in-kernel HID paths handle only
the fixed boot-protocol keyboard and mouse formats and, from Phase 6, the
keyboard reports of internal I2C-HID laptop keyboards), USB class drivers other
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

**Input trust.** The kernel tags every input event with the device that
produced it. Each device's tier-3 HID driver runs in its own process (one
process per device), so a malicious device that takes over its process can't
read or forge another device's events. The secure-attention key and answers
to prompts are accepted only from in-kernel parsers (PS/2, the boot-protocol
USB paths and the in-kernel I2C-HID keyboard parser, each meeting the
thresholds) or from a per-device HID process whose device passed the
typed-code confirmation in [threat-model.md](threat-model.md); never from
Bluetooth unless the user opts that keyboard in by name. PIN entry on resume
uses only the in-kernel paths, because user space is frozen. Test: a
compromised Bluetooth stack or HID process injects the secure-attention key
and approves a prompt, and both fail (Phase 6; extended to the compositor's
prompts in Phase 7).

**Placement of the drivers in the roadmap** ("3 → 2" means tier 3 until it
meets the thresholds):

| Driver | Phase | Tier |
|---|---|---|
| Local APIC, I/O APIC, HPET, TSC deadline, ACPI timer | 1 | 0 |
| Minimal PCI: ECAM/MCFG config access, bus walk, ACS and capability parsing, Bus Master Enable control | 1c | 0 |
| IOMMU (Intel VT-d via DMAR, AMD-Vi via IVRS) | 1c | 0 |
| ACPI table parsing (safe crate over byte slices) | 1c | 1 |
| AML interpreter | 1c | hosted by the core on the boot thread with an operation budget until Phase 2, its own budgeted thread from Phase 2, the tier-2 framework from Phase 4a (below) |
| Framebuffer; 16550 serial (from Phase 1 the clock-drift exchange with a host-side timestamper, from Phase 2 timestamping a PPS pulse on the DCD pin, [scheduler.md](scheduler.md)) | 1 | 1 |
| PS/2 keyboard | 4a | 1, thresholds required |
| PCI(e) enumeration policy, hotplug, AER/DPC | 4a | 1 (config space and MSI tables stay in tier 0) |
| NVMe, AHCI, each with its in-driver controller reset path (faults, below) | 4a | 1, thresholds required |
| virtio-blk | 4a | **2**: the first tier-2 driver, so the restart machinery is exercised from Phase 4a |
| TPM 2.0 (CRB, TIS) | 4b | 0 (MMIO and the TPM service, which alone holds raw TPM access) and 1 (protocol, a safe crate) |
| Polled xHCI boot-protocol keyboard reader (8-byte boot reports only, no HID report descriptors); writes Intel XUSB2PR and USB3_PSSEN when it starts so the ports route to xHCI; stops when the Phase 6 xHCI driver binds | 4a | 1, thresholds required |
| xHCI host controller and hub logic | 6 | 1, thresholds required |
| EHCI, OHCI and UHCI host controllers (until then left with the firmware, IOMMU section) | 6 | 1, thresholds required |
| USB boot-protocol keyboard and mouse | 6 | 2, thresholds required |
| Full USB HID (report descriptors), other USB classes (audio, video, serial) | 6 | 3 → 2, one HID process per device |
| USB mass storage transport | 6 | 2, thresholds required |
| Thunderbolt/USB4 connection manager, and an ICM driver for controllers whose firmware runs the connection manager | 6 | 2, thresholds required |
| virtio-net, Intel and Realtek Ethernet | 6 | 3 → 2 (fuzzed to the thresholds within Phase 6) |
| Wi-Fi and Bluetooth transport (rings, DMA, firmware loading; no frame parsing) | 6 | 2, thresholds required |
| 802.11 and Bluetooth protocol stacks | 6 | 3 (PCIe radios on D2 machines: see the IOMMU section) |
| HD Audio | 6 | 2 |
| USB Audio Class 2 | 6 | 3 → 2 |
| Embedded controller, battery, backlight, lid and power buttons | 6 | 2 |
| I2C controllers (Intel LPSS and AMD DesignWare) and the I2C-HID keyboard parser for internal laptop keyboards (keyboard reports only) | 6 | 2, thresholds required |
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
  ones still work. A non-posted read to a dead device stalls the CPU until
  the completion timeout, so the core sets Completion Timeout to range A
  (50 µs to 10 ms) in Device Control 2 of every root port and device that
  supports it, and never issues an MMIO or ECAM read with interrupts off;
  devices that don't support range A are listed in the boot log.
- **The MSI-X table and pending-bit array are never writable by a driver**,
  even though they live in a device BAR. The core alone programs them;
  otherwise a driver could aim interrupts at any CPU and vector. When the
  table shares a 4 KiB page with registers the driver needs, that page is
  left out of the driver's mapping (tier 3) or of its `MmioRegion` (tiers 1
  and 2), and the driver reaches its other registers through an explicit
  `mmio_write(handle, offset, width, value)` call (and a matching read),
  which the core checks against the table and PBA ranges, as VFIO does. Nothing
  is trapped, so the core needs no instruction decoder; the cost is
  benchmarked per device.
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
  network I/O ([verification.md](verification.md) targets 2 and 10, both
  extended to streaming maps at 4a). Each
  map carries a direction: to-device maps are read-only in the IOMMU,
  from-device and bidirectional maps are writable. A buffer that doesn't
  cover whole pages is bounced when the device is behind a hotplug-capable
  port or its driver is tier 3, so the rest of the page is never exposed.
- **Config space** reads go through the core; writes are allowed only to a
  per-device-class list of registers, never to BARs, MSI capability
  registers or another function.

## The IOMMU, from Phase 1

- **What stops DMA.** A device's own Bus Master Enable (BME) bit is a request
  to the device, which a malicious device ignores. DMA is stopped only by
  clearing BME on the bridge or root port above the device, and by the
  IOMMU; a device's own BME is cleared too, for well-behaved devices, but no
  rule relies on it. Root-complex integrated devices (the iGPU, CNVi Wi-Fi,
  integrated Thunderbolt) have no bridge above them, so only the IOMMU stops
  them.
- **Pre-boot protection.** From `ExitBootServices` until the kernel's IOMMU
  tables are live in 1c, the kernel image, the page tables Limine built and
  everything 1a and 1b allocate are exposed unless DMA is already blocked.
  D1 therefore requires firmware pre-boot DMA protection (the DMAR
  DMA-protection opt-in flag with Protected Memory Regions enabled or
  translation on at hand-off, or on AMD the IVRS IVinfo DMA-remap-support
  bit with the IOMMU enabled at hand-off), or Optimal Boot, which just
  before `ExitBootServices` (after its last firmware I/O) installs a
  deny-all IOMMU root table whose only mappings are the firmware display's
  framebuffer, read-only, and the RMRRs, or PMRs covering all RAM when the
  IOMMU can't be programmed. A machine that meets every other D1 condition
  but has neither keeps D1's runtime rules and shows a named "boot DMA
  window" exposure in the boot log, Settings and the scorecard
  ([threat-model.md](threat-model.md)).
- **Bring-up order (1c).** The core walks the PCI buses (through ECAM from
  the ACPI MCFG table) and clears BME on every bridge and root port and then
  on every function, except the IOMMU's own function, the firmware display
  and the bridges on its path (below), and USB controllers still owned by
  the firmware (below). A bridge's BME is set again only when the first
  device below it is assigned a domain. UEFI boot-services memory and
  Limine's bootloader-reclaimable memory are not reclaimed until BME is
  cleared and the IOMMU is live, because a device the firmware left running
  may still DMA into them.
- **USB controllers stay with the firmware until their driver starts.** A
  USB host controller is taken over from the firmware (the xHCI/EHCI/OHCI
  ownership handshake, which ends the firmware's legacy keyboard emulation)
  only when the Optimal driver that runs it starts: the polled xHCI reader in
  4a, the full host-controller drivers in Phase 6. Until then it keeps its
  BME and gets its own domain that maps only its RMRRs, so firmware legacy
  emulation keeps feeding the in-kernel PS/2 path on machines whose only
  controllers are EHCI, OHCI or UHCI. On Intel 7- and 8-series chipsets the
  ports route to EHCI until XUSB2PR and USB3_PSSEN are written, so the xHCI
  reader writes both when it starts on such a chipset. Tests: QEMU
  `usb-ehci` and `piix-uhci` controllers are left with the firmware, BME
  on, in a domain holding only their RMRRs (1c gate); a USB keyboard on a
  USB 3 port of a 7- or 8-series Intel machine types the passphrase through
  the reader (4a gate).
- **Firmware protection carries over.** If the DMAR table sets the
  DMA-protection opt-in flag, or Intel Protected Memory Regions are enabled,
  they stay on until the kernel's tables are live; the PMRs are turned off
  (PMEN cleared, PRS checked) only after translation runs on the kernel's
  root table. If translation is already on at hand-off (GSTS.TES set by the
  firmware, Optimal Boot or a previous kernel), the core never turns it off:
  it loads its own root table with SRTP while translation stays on, then
  does a global context-cache and IOTLB invalidation. On AMD-Vi, if the
  IOMMU is already enabled, the core switches to its own device table, with
  the firmware display's scanout range and the RMRRs already mapped in it,
  and then invalidates every device table entry.
- **Firmware display.** The device scanning out the firmware framebuffer,
  and the bridges on its path, keep BME so the screen stays on. Once the
  IOMMU is live, its domain maps only the framebuffer range, read-only, plus
  its RMRRs. Until then (and always on D2) this is a named exposure,
  "firmware display DMA", in the boot log and on the scorecard.
- **Deny by default, for every requester ID.** Each device gets its own
  domain with nothing mapped; only `DmaBuffer`s and streaming maps appear in
  it. Requester IDs with no device assigned are blocked too, not left open:
  on AMD-Vi all 65,536 device table entries of every segment are written
  with V=1, TV=1, Mode=0 with IR=IW=0 (every access refused) and IV=1 with interrupts
  blocked (IntCtl=00b), because an entry with V=0 passes DMA untranslated
  and IV=0 passes interrupts unremapped; on Intel, where a not-present root
  or context entry blocks and faults every request, unused root and context
  entries stay not present (none is ever set to pass-through) and every
  unused interrupt-remapping entry is not present. Fault storms from an
  unassigned requester ID are rate-limited per requester ID like a
  device's. Firmware-reserved regions
  (RMRR, IVMD) are mapped only for the devices that need them, logged, and
  shown on the scorecard.
- **What counts as D1.** A machine is DMA tier D1 only if every PCI segment
  is covered by an IOMMU unit (DMAR INCLUDE_PCI_ALL or scopes listing every
  device; IVRS entries for every device), every I/O APIC and HPET is in a
  unit's device scope, interrupt remapping is on, compatibility-format
  interrupts are blocked (on Intel, GCMD.CFI left 0 and GSTS.CFIS checked to
  read 0), x2APIC is used where the CPU has it, and DMA is blocked before
  the kernel takes over (pre-boot protection, above). Otherwise it is D2,
  and the boot log, Settings and scorecard name the missing condition.
  Devices an IOMMU unit does cover keep their domains; every other D2 rule
  below applies.
- **ACS.** On every root and downstream port with the ACS capability the
  core enables Source Validation, P2P Request Redirect, P2P Completion
  Redirect, Upstream Forwarding and Translation Blocking, so a device can't
  send a requester ID from outside its port's bus range or reach a peer
  without passing the IOMMU.
- **IOMMU groups.** Devices that still can't be separated (one IOMMU group,
  such as functions behind a port without ACS or functions of one
  multi-function device without ACS) share one domain. The domain is
  reference-counted, one reference per member with a live driver, and the
  whole group runs under the rules of its lowest-trust member (the highest
  tier number in it): if any member's driver is tier 3, partial-page buffers
  of every member are bounced and ATS stays off for all of them. Stopping
  one member unmaps only that member's buffers, waits for the invalidation
  and drops its reference; the domain is torn down only when the count
  reaches zero, so a tier-1 sibling keeps running. External devices behind
  a port without ACS share one domain; the approval prompt names every
  device in the group, and approval is refused if the group holds an
  internal device.
- **ATS (devices sending pre-translated addresses) is off by default** for
  every device. It is turned on only for a named internal device where a
  benchmark shows a need, never for a tier-3 device or one behind a
  hotplug-capable port (all of which are treated as external, whether or
  not ACPI marks them so). A device with ATS on can send translated
  requests the IOMMU doesn't check, and root-complex integrated devices have
  no ACS Translation Blocking above them, so every ATS-enabled device and its
  firmware are listed in the TCB in [threat-model.md](threat-model.md) and on
  the scorecard. When ATS is on, unmap also waits for the device's own
  translation cache invalidation to complete.
- **Free only after invalidation.** A page unmapped from a domain is freed
  only after its IOTLB invalidation (and device-TLB invalidation, if ATS is
  on) has completed. Invalidations may be batched for speed, as long as
  freeing waits for the batch; Linux's lazy mode, which frees before the
  flush, is not used. The cost is benchmarked.
- **Fault storms.** IOMMU faults are logged and rate-limited per device:
  past 100 in one second, the core suppresses further fault reporting for
  that device (Fault Processing Disable in its context and interrupt
  remapping entries on Intel, SA in its device table entry on AMD), and
  stops its driver (below), which clears BME and empties its mappings.
- **D2** (no IOMMU, or one short of D1; [threat-model.md](threat-model.md)):
  for every device no IOMMU unit covers, `DmaBuffer`s, `DmaPool`s and
  `DescRing`s (including xHCI device contexts and an NVMe Host Memory
  Buffer) are allocated from a dedicated DMA region that never holds other
  kernel data, and only streaming maps are bounced into it, because a ring
  or context the device reads continuously can't be bounced. With the typed
  descriptor rules above, tier-1 and tier-2 driver bugs still can't aim DMA
  at kernel data; a malicious device, or its firmware, can.
  GPUs follow the same rule: every GPU page-table entry (GPUVM, GTT,
  PPGTT) is built by the GPU core only from a `DmaBuffer` or `Iova` handle,
  never from a raw address (target 13, which covers D2 machines too), and a
  GPU without per-process hardware page tables (for example Intel before
  Gen8) gets no acceleration: it is driven as a
  simple framebuffer with llvmpipe.
  Thunderbolt and USB4 PCIe tunnels are refused: from 1c the core keeps BME
  clear on every Thunderbolt downstream port and every hotplug-capable port,
  so a tunnelled or hot-plugged device can't master even if a firmware
  connection manager opens the tunnel. Because tier 3 relies on the IOMMU
  alone, DMA-capable devices whose drivers would be tier 3 get an in-kernel
  tier-2 build of their driver instead, marked degraded on the scorecard.
  Wi-Fi and Bluetooth radios that do PCIe DMA stay off until the user
  accepts a named "remote DMA exposure" warning, because radio firmware
  compromised over the air could DMA anywhere; the scorecard then says the
  promise that nobody can read a user's data remotely does not hold on that
  machine. USB-attached radios can't master the bus and need no warning. The
  boot log, Settings and scorecard say "DMA protection: off".
- **Test (Phase 1c gate):** QEMU machines with an emulated Intel IOMMU and
  an emulated AMD IOMMU. The hostile device is our own vfio-user device
  server (permissively licensed, built on libvfio-user), attached to an
  unmodified QEMU at a pinned version used as an external test tool, once
  on the root bus and once behind a root port. It tries DMA reads and writes
  outside its domain, DMA and MSIs while it has no domain at all (an
  unassigned requester ID), forged MSI writes, compatibility-format MSIs and
  a fault storm, and DMAs continuously into boot-services memory from
  firmware through `ExitBootServices` and IOMMU bring-up. Every attempt
  must fault or be dropped and be logged, no write may land after the IOMMU
  is live or the bridge above it has BME clear, none may land in a
  reclaimed page, the boot DMA window must be reported on OVMF without
  pre-boot protection, and the kernel must keep running. QEMU can't model
  a device that ignores its own BME, forge another device's requester ID or
  enforce ACS Source Validation, so those tests are in the 4a hardware gate.
- **Hardware test and audit (Phase 4a gate):** from a PCILeech-class FPGA
  board in a D1 reference machine with firmware pre-boot protection: the
  1c attacks, a device that ignores its own BME, requester-ID spoofing
  through an ACS port, translated requests from a device with ATS off, and
  the same DMA across the firmware-to-kernel hand-off. The outside audit of
  the IOMMU, DMA and driver boundary code is also in Phase 4a's done-when.
  Optimal Boot's pre-`ExitBootServices` deny-all set-up is tested with the
  same board when the loader lands (Phase 4b).

## The message interface (tiers 2 and 3)

The isolation layer is itself attack surface: if the boundary is complicated,
the boundary is where the bugs go. The design rule is **small,
one-directional, fuzzed and bounded-checked** (target 9, B at 4a).

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
  Phase 4a; the ring validation is target 9, checked at B (bounded) from 4a.
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

Charging and releasing budgets is target 7, at B from 4a for these driver
budgets, the real-time cap included.

## Faults, stopping and restarting

| Fault | Detection | Handling |
|---|---|---|
| Panic or stack overflow (tier 2) | the panic handler or the double-fault handler sees a tier-2 thread outside core code ([core.md](core.md)) | poison, stop, restart (below) |
| Panic (tier 3) | process exits | stop, restart |
| Device hang | a command the driver handed to the device has no completion within its device class's timeout (storage 30 s, matching Linux's I/O timeout, so spinning-up disks aren't killed; network 5 s; input and audio 1 s) | the driver's own controller reset, in every tier (below); stop, restart (tier 2 and 3) or device stopped and reported (tier 1) if three resets in a row fail |
| Driver loop or hang | per-component watchdog: requests waiting on its ring that the driver hasn't taken for longer than the class timeout, or a driver thread running for 1 s without taking or completing a request | stop, restart (tier 2 and 3); crash report (tier 1) |
| CPU stuck | NMI watchdog: a CPU with interrupts off for 10 ms anywhere in the kernel, or a tier-2 thread with preemption off for 10 ms | a stop event for the tier-2 component (parked on core exit, below); otherwise a crash report, as for a tier-2 core call that still hasn't returned 1 s later |
| Out of memory | allocation fails | the driver handles the error; repeated failure is reported |
| Interrupt storm | per-class limit above | mask or rate-limit, report, then stop and restart if it continues |
| IOMMU fault storm | over 100 faults in one second (IOMMU section) | suppress fault reporting, clear BME, stop, restart |
| Device error or removal | AER/DPC interrupt, confirmed all-ones reads, hotplug event | stop, then restart if the device is still there |

**Stopping a driver**, in this order:

1. Stop accepting new requests.
2. Park its threads. A thread that isn't running is taken off the run
   queues. A running thread gets a forced-preemption IPI: if its core-call
   depth count ([core.md](core.md)) is 0 it is parked at the interrupt
   return; otherwise a park-on-core-exit flag is set and it is parked when
   its outermost core call returns. Core calls a tier-2 thread can make are
   bounded: none waits on driver-controlled state without a timeout. A
   parked thread is never resumed (a thread stuck in a loop can't be
   cancelled from inside safe code); its stack is freed with its arena, and
   no core lock is held by it, because it never parks inside the core.
3. Clear Bus Master Enable on the device and on the bridge or root port
   above it when every other device below that bridge is stopped (a device
   can ignore its own bit; on a root-complex integrated device the IOMMU
   domain is what stops it, step 5), then poll the device's Transactions
   Pending bit (bounded, 100 ms) so no DMA is in flight.
4. Reset, taking the first method the device supports: a device-specific
   reset from the quirk table, FLR, Advanced Features FLR, D3hot to D0 if
   the device's No_Soft_Reset bit is clear, and a slot or secondary bus
   reset only when every function below that bridge is stopped. Then wait
   100 ms and poll the Vendor ID with CRS Software Visibility until the
   device answers, up to 60 s (Linux's limit). A device with no working
   method stays off, BME clear and no domain, until reboot, and is reported.
5. Unmap all of its buffers from its IOMMU domain and wait for the
   invalidation to complete; drop its reference on the domain, which is
   torn down at zero (IOMMU groups, above).
6. Unmap its MMIO with a TLB shootdown.
7. Hand its in-flight requests back to the subsystem above (for storage, the
   block layer keeps and resubmits them to the new instance, see
   [storage.md](storage.md); they fail with `EIO` only if three restarts in a
   row fail).
8. Revoke all of its handles, poison its sleeping locks, free its arena.

Only then can a new instance start. Because the core holds every resource a
driver uses in a per-component handle table, nothing is left dangling.

Tier-1 drivers can't be restarted this way (a panic there is a kernel crash),
which is why tier 1 is small and gets the hardest testing. A hung device is
not a driver bug, though, so it never crashes the kernel: every storage
driver has its own controller reset path. NVMe first sends Abort for the
timed-out command; if the Abort times out too, it sets CC.EN=0, waits for
CSTS.RDY=0 (bounded by CAP.TO), rebuilds its queues and re-enables. AHCI
issues a COMRESET on the port (PxSCTL.DET=1), and resets the HBA (GHC.HR)
if the port doesn't recover. The block layer then re-sends the in-flight
requests and the replay log exactly as after a restart
([storage.md](storage.md)), because a controller reset can drop the volatile
cache. If three resets in a row fail, the device is stopped as above with its
driver left unrestarted, its requests fail with `EIO`, and it is reported.
Test (Phase 4a gate): an emulated NVMe and AHCI device (the vfio-user device
server) stops completing commands under load; the kernel must not crash,
I/O must resume after the reset, and the crash oracle in
[storage.md](storage.md) must hold.

**Tests:** CI injects each fault (panic, stack overflow, endless loop at
core depth 0, a loop that repeatedly calls into the core, preemption held off
for 10 ms inside a core call, interrupt storm, allocation failure, surprise removal with `device_del` under
load) into every tier-2 and tier-3 driver. Each must be contained, including
on an emulated device with no FLR and one whose reset never completes. For
storage (virtio-blk from Phase 4a), I/O must resume within 500 ms of restart,
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
  from Phase 6 by our own connection manager or, on controllers whose
  firmware runs the connection manager (ICM, such as Thunderbolt 3 before
  Ice Lake), by our ICM driver, which sets the controller's security level
  to user approval and sends the approve message only after the user
  approves. Where the firmware opens tunnels by itself anyway (security
  level fixed at "none", or before Phase 6), the IOMMU's deny-all default is
  what protects memory on D1: the new device gets no DMA mapping and no
  driver until approved, and is reset if refused. On D2, BME stays clear on
  the Thunderbolt and hotplug-capable ports from 1c (IOMMU section), so
  tunnels are refused.
- **USB devices need authorisation** before their class driver binds, per
  the USB rules in the threat model.

## Power management in the interface from day one

Every driver implements `suspend`, `resume` and `reset` from the first
version of the interface (Phase 4a), even before system suspend exists, and
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
  logged). From Phase 2 it has its own budgeted thread; from Phase 4a it runs
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
  lid events; MSI-routed devices keep working). Before Phase 4a, an AML panic
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
  It is target 13 (B at 8), on D1 and D2 machines alike: every GPU
  page-table entry is built from a `DmaBuffer` or `Iova` handle (D2 bullet
  in the IOMMU section).
- **Display and power programming (tier 2):** display engine programming
  and power and clock sequencing run under the tier-2 framework. Their
  register access goes through an accessor that checks every offset against
  a per-ASIC list of display and power register ranges, because GPU
  registers aren't split at page granularity and the page-table base and
  command-submission registers must stay with the GPU core. Every register
  or firmware message that carries a memory address is left out of those
  ranges and is a core-only operation taking a buffer handle: plane surface
  addresses, cursor addresses, gamma and degamma LUT addresses, and on AMD
  the SMU messages that pass a DRAM address (driver table, tool table and
  similar), each listed per ASIC with the other ranges. Otherwise the tier-2
  part could scan out, or have the SMU read or write, any memory. Test
  (Phase 8 gate): a hostile tier-2 display build writes every address
  register and SMU address message directly, and every attempt is refused
  and logged.
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
