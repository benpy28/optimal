# Drivers: trust tiers, resource budgets and the restricted-driver interface

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

So memory safety is the floor, not the whole answer. The tiers below add real
isolation where it's worth its cost, and every in-kernel component gets
resource budgets so one bad driver can't take the system down.

## Trust tiers

| Tier | Runs where | Written in | Can touch | Use for |
|---|---|---|---|---|
| 0. Core | kernel, full privilege | Rust, `unsafe` allowed (audited, every block justified, proof targets in [verification.md](verification.md)) | everything | paging, interrupts, context switch, IOMMU, the driver interface itself |
| 1. Trusted drivers | kernel | safe Rust only (`#![forbid(unsafe_code)]`), hardware access only through core-provided typed APIs | only the devices and memory the core hands it; DMA only through its IOMMU domain | hot-path drivers we write and test ourselves: NVMe, the timer, interrupt controllers, framebuffer |
| 2. Restricted drivers | kernel | safe Rust only | a capability set fixed at load time (named device BARs, IRQ lines, DMA buffers), under per-driver resource budgets; a violation stops the driver, not the kernel | drivers we didn't write, less-tested hardware, anything third party |
| 3. User-space drivers | separate process | any language | only what its capabilities and IOMMU domain allow; a crash restarts the process | USB gadgets, printers, scanners, experimental or untrusted hardware support |

Every tier from 1 up gets its **own IOMMU domain**: a device can only DMA into
buffers its driver was given. That is what stops a buggy or malicious device
(or Thunderbolt peripheral) from reading the rest of RAM, whatever tier its
driver is in.

Drivers move down the table (towards more trust) only on evidence: fuzzing
hours, test coverage, proofs, review. A tier-3 driver that becomes performance
critical is rewritten or audited into tier 2, not just promoted.

## Resource budgets

Every component in tiers 1 to 3 is charged against budgets set when it loads:

- **Memory:** a cap on kernel heap and pinned pages. Allocations past the cap
  fail (the driver must handle the error), they don't eat into everyone else.
- **CPU:** a cap on time in its interrupt handlers and deferred work per
  period. Over budget, its interrupts are throttled and it's reported.
- **Interrupts:** a cap on interrupt rate per line. An interrupt storm from one
  device masks that line, not the whole CPU.
- **Locks:** core locks a driver may take are documented, with a maximum hold
  time checked in debug and test builds.

Charging and releasing budgets is proof target 7 in
[verification.md](verification.md).

## The restricted-driver interface (tiers 2 and 3)

The isolation layer is itself attack surface: if the boundary between kernel
and driver is complicated, the boundary is where the bugs go. The design rule
is **small, one-directional, fuzzed and proved**.

**Communication: shared-memory rings.** Each driver gets a pair of
single-producer/single-consumer rings (requests in, completions out) in
memory shared between it and the kernel, in the same spirit as virtio queues
and io_uring. There's no general IPC and no shared kernel objects.

- Fixed-size descriptors, a small fixed set of message types per device class.
- The kernel copies each descriptor out of shared memory once, then validates
  the copy (no double fetch, so the driver can't change a field between check
  and use).
- Indices from the driver are always bounds-checked; a bad index stops the
  driver.
- Data buffers are referred to by handle (an index into buffers the kernel
  granted), never by raw address.

**Interrupts.** The kernel owns the interrupt controller. A driver's IRQ is
acknowledged by the core and delivered as an event on its ring (tier 2) or a
notification to its process (tier 3). Drivers never touch the APIC.

**DMA.** Only through the driver's IOMMU domain, only into buffers the kernel
mapped for it. Devices with no IOMMU coverage can't be given to tier 2 or 3
drivers at all; the kernel says so in the log instead of running them
unprotected.

**Device registers (MMIO).** Mapped into the driver only for the BARs listed
in its capabilities, never config space for other devices.

**Hardening the boundary.**

- Every ring entry point is fuzzed continuously with hostile drivers (random,
  replayed and mutated descriptors) from Phase 4.
- The ring validation code is proof target 9.
- The interface is versioned and kept small: adding a message type needs a
  written reason.
