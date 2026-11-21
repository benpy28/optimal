# Optimal OS roadmap

Goal: build the best desktop OS there is, starting with our own kernel written
from scratch: fast, lean, safe, and able to run the software people already use.
The kernel comes first (Phases 0 to 11); the full OS is built on top of it (Phase 12).

This is a serious engineering project, not a toy. That means every phase ends
with a test that proves it is done, every performance claim comes from a
benchmark, and nothing merges unless CI builds it, boots it and passes the tests.

This file is the plan. The detailed design, which every phase below must
meet, is in [design/](design/README.md).

## Pillars, in priority order
1. **Performance.** The main focus. Measured against Linux on the same machine, never guessed ([design/benchmarks.md](design/benchmarks.md)).
2. **Security.** An absolute focus. Nobody can read a user's data remotely, and a compromised app can't reach anything it wasn't given. Exactly what that means, against whom, and how each promise is tested is in [design/threat-model.md](design/threat-model.md).
3. **Lean.** Nothing runs unless something needs it. Small memory footprint, fast boot.
4. **Runs on almost anything.** Any 64-bit x86 PC first, ARM64 later. On hardware that lacks a security feature (no TPM, no IOMMU, BIOS only), Optimal still runs and says plainly which protections that machine can't have.
5. **Compatibility.** Runs existing Linux programs unchanged, 64-bit and 32-bit, which is the road to Windows games through Wine/Proton.
6. **User respect.** The kernel does what the user says. No hidden telemetry, ever.
7. **Polish.** Clear error messages, clean crash reports, good docs.
8. **Customisation.** The smallest pillar: boot options, schedulers and modules the user can choose.

**When pillars conflict.** Security features are on by default. The only
reason to make one optional is a measured cost of more than 2% on the
geometric mean of the desktop benchmark set, or more than 5% on any single
desktop metric, recorded on the scorecard with the owner's sign-off.
Mitigations that stop data leaking between processes, or from the kernel to a
process, are never off by default whatever they cost.

## Core architecture decisions

These are decided now because they are expensive to change later.

- **Language: Rust.** Memory-safety bugs cause most serious kernel security holes. Rust removes most of them at compile time with no speed cost.
- **Shape: a "framekernel".** A small core is allowed `unsafe` code: boot, paging, interrupts, context switching and the raw hardware access. Everything else (scheduler policy, filesystems, network stack, drivers) is written in safe Rust on top of that core. This is a memory-safe in-kernel component architecture: it keeps monolithic-kernel speed (no message passing between processes for every operation) and removes most memory-corruption bugs. It is not a hardware isolation boundary the way a microkernel's separate address spaces are: a safe-Rust component still runs with kernel privilege and can still exhaust resources, deadlock, get logic wrong or misprogram a device, and safe Rust is no defence against a hostile author. So in-kernel code is ours or fully audited, third-party drivers run in their own processes, and isolation comes from the driver trust tiers, resource budgets and per-device IOMMU domains in [design/drivers.md](design/drivers.md). The boundary between the core and the rest is enforced by tools, not convention ([design/core.md](design/core.md)).
- **Compatibility through the Linux system-call interface.** Instead of inventing a new interface and waiting for apps to be written for it, Optimal speaks the same system calls as Linux, 64-bit and 32-bit. Linux programs, Mesa, Wayland and eventually Wine/Proton can then run unchanged. This is our own clean implementation of a public interface, so it involves no Linux code. How Linux semantics sit on top of capabilities is in [design/capabilities.md](design/capabilities.md), and the ABI plan in [design/compat.md](design/compat.md).
- **Boot: our own bootloader, "Optimal Boot"**, a UEFI loader in Rust that speaks the Limine boot protocol, checks a signed config and measures everything into the TPM ([design/boot.md](design/boot.md), `bootloader/DESIGN.md`). Until it ships, Limine (BSD-2-Clause) is used, with every file hash-pinned and its config enrolled, in the shipped image as well as the tests.
- **First target: x86_64.** ARM64 comes once the core is stable. Code that depends on the architecture lives in its own module from day one.
- **Licence: our own code only, never GPL.**
  - Kernel, bootloader and every component we write: clean-room code from specs (Intel/AMD manuals, ACPI, PCI, NVMe, xHCI and USB specs, the Linux man pages and uAPI docs for the syscall interface). We never copy, translate or read for reference the code of Linux, BSD or other kernels. Dependencies must be MIT, Apache-2.0, BSD, Zlib or similar, checked by `cargo-deny` in CI.
  - Borrowed programs shipped in the OS until ours replace them: permissive licences preferred; weak copyleft (LGPL, MPL) allowed as separate, unmodified programs (Wine, a browser); GPL programs are not shipped, with one exception: shared libraries under the GCC Runtime Library Exception (glibc is LGPL; libstdc++ and libgcc_s are GPL with that exception), which every Linux program links against ([design/compat.md](design/compat.md)). The base system uses dash as `/bin/sh` and uutils for coreutils; bash and other GPL programs are third-party apps the user can install from the store, never part of the image.
  - Firmware: redistributable vendor binaries (CPU microcode, GPU and Wi-Fi firmware) are allowed as data, each listed with its licence and hash ([design/drivers.md](design/drivers.md)).
  - Test tools: GPL test suites and checkers (LTP, xfstests, kselftest, `e2fsck`) may be run as external test oracles, never linked, vendored or shipped.
  - The repo has no licence file yet, which means all rights are reserved until the owner picks one.

## Security model

Security is built in from Phase 1, not bolted on at the end. The threat model,
the trusted computing base, the hardware security tiers and a table of every
attack path with its defence and its test are in
[design/threat-model.md](design/threat-model.md). In short:

- **Memory safety:** Rust everywhere, with `unsafe` kept to a small, size-capped, inventoried core ([design/core.md](design/core.md)). Memory safety is the floor, not the whole answer: logic bugs, races, hardware misuse and DMA mistakes are handled by the measures below.
- **Driver trust tiers:** core, trusted drivers, restricted drivers (all ours, in the kernel), and user-space drivers (anything third party, and anything parsing untrusted input by default), each with its own IOMMU domain and resource budgets, with defined fault containment and restart ([design/drivers.md](design/drivers.md)).
- **Formal verification of the security-critical core:** proofs and bounded checks at stated levels (Kani, Verus, loom) for page tables, the frame allocator, the soundness of the core's safe APIs, process memory isolation, capability derivation and revocation, user-memory copies, resource accounting, the scheduler, the driver boundary, the IOMMU and storage invariants. The `unsafe` hardware shims are a separate, tested-only target with a size cap. Each target states its assurance level (proved, bounded, model-checked or tested only) and its assumptions, and the level reported is the weakest of its own and every target its assumptions rest on ([design/verification.md](design/verification.md)).
- **Hardware protections:** no-execute pages, write-xor-execute for all kernel memory, SMEP/SMAP, UMIP, guard pages around every stack, KASLR with a position-independent kernel randomised over at least 2^16 slots and measured entropy, page-table isolation where the CPU needs it, and the IOMMU from Phase 1. Control-flow protection: user-space CET shadow stacks from Phase 5; kernel shadow stacks and indirect-branch tracking as soon as the stable Rust toolchain can build for them (tracked on the scorecard until then).
- **Least privilege through capabilities:** a process can only touch objects it holds a handle to; Linux file descriptors are capabilities, with a derivation tree and revocation ([design/capabilities.md](design/capabilities.md)).
- **Every app sandboxed by default:** its own view of the filesystem, no access to other apps' data, and access to the camera, microphone, screen, location and user files only through portals whose prompts are drawn by the system (on a dedicated prompt VT reached by Ctrl+Alt+Delete from Phase 5, by the compositor from Phase 7). Network access is a per-identity egress filter that reaches live sockets: a network grant is outbound only, a listening port needs its own grant, and apps holding file grants get no network access unless the user allows that combination.
- **IOMMU on by default from Phase 1:** devices can only reach memory handed to them, interrupt remapping on, external devices need approval. Machines without an IOMMU are told so; there, device-read descriptors hold only typed addresses of granted buffers and DMA-address registers are written only by the core, so driver bugs can't aim DMA at kernel data, but a malicious device or radio firmware can, and radios stay off until the user accepts that named exposure.
- **Encryption by default:** Phase 4 encrypts the disk (AES-XTS, confidentiality only); the own filesystem then adds authenticated, per-user encryption with keys evicted on lock ([design/storage.md](design/storage.md)). Keys are sealed to measured boot with a signed TPM policy plus a PIN where there's a TPM, a passphrase where there isn't, and a recovery key is always created.
- **Verified and measured boot:** firmware checks the bootloader, the bootloader checks a signed config that pins the kernel and the system image's hash tree root, and every step is measured into the TPM. Anti-rollback, A/B slots, boot counting and a recovery image are in [design/boot.md](design/boot.md).
- **Nothing listening by default:** no remote-access services, a default-deny inbound firewall.
- **Lockdown:** no interface lets anyone, root included, load unsigned ring-0 code or read kernel memory ([design/threat-model.md](design/threat-model.md)).
- **Signed updates only,** with a TUF-style key hierarchy, an offline root, a transparency log, reproducible builds and anti-rollback; the update client ships in Phase 7, so the minimum product can be patched.
- **CPU side-channel mitigations** chosen per CPU model after microcode is loaded, including Spectre v1 index masking in safe Rust parsers; SMT is off by default on CPUs affected by MDS, L1TF, TAA or RFDS, and core scheduling is an opt-in for app-to-app isolation only (on by default only where siblings can't leak kernel data); costs benchmarked and published.
- **Testing security like everything else:** every attack path in the threat model has a test that becomes a gate in the phase it lands; continuous syscall, network, device-side and filesystem fuzzing; outside audits of the core at the end of Phases 1, 3 and 4 and before 1.0.

## How we know it is good

- **CI on every change:** build, `rustfmt`, `clippy` with warnings as errors, the in-kernel tests and a boot matrix in QEMU: 64 virtual PCs (BIOS and UEFI, old and new chipsets, 128 MiB to 8 GiB of RAM, 1 to 16 cores), each booted 3 times. Lands in Phase 1: the Rust toolchain pinned to an exact version (not a channel name), Limine and every other download pinned by SHA-256 and GitHub Actions by commit hash, the core's lint rules, the unsafe inventory and core size check, `cargo-deny` and `cargo-vet`, and the gate-table check below. Lands in Phase 1b: the proofs job.
- **Gate tables match the plan** (lands in Phase 1): a CI script checks that every proof target and level change in [design/verification.md](design/verification.md), and every attack row in [design/threat-model.md](design/threat-model.md), is named in the done-when of the phase its table gives, and that no done-when below names a gate those tables put in another phase.
- **No silent skips.** A test that can't run (missing firmware, missing tool) fails CI unless it is on a checked-in allowlist with a reason. A green run means everything listed actually ran.
- **Nightly soak:** the same matrix booted 40 times per configuration (over 2,500 boots) to catch rare, timing-dependent bugs. Soak tests grow with every phase: hour-long scheduler stress, file system crash-state runs, driver kill-and-restart loops, continuous fuzzing.
- **In-kernel test suite** (from Phase 1): unit tests that run inside the kernel under QEMU, so allocators, page tables and the scheduler are tested in the real environment.
- **Tough but realistic conditions** (from Phase 1): every change also boots on deliberately awkward machines: CPUs from Core 2 to current Xeon/EPYC with security features missing, 5-level paging, odd core topologies, NUMA (including a node with no memory), odd and split memory, no HPET, no ACPI, old and new OVMF builds, a host fighting for CPU, and Secure Boot attacks (unsigned bootloader, wrong key, tampered kernel, tampered config, old signed image), which must be refused. Fault injection joins as each piece lands: failed allocations, quirky ACPI tables, timer jitter, spurious interrupts, hostile DMA devices, then disk power cuts and I/O errors.
- **Public scorecard from the first bootable build:** performance against a pinned, matched Linux baseline (booted with the same SMT setting, `nosmt` on affected CPUs), measured on dedicated bare-metal machines driven by a separate controller host (shared CI VMs are too noisy and are used for functional tests only); committed provisional targets that can only tighten; and a security scorecard per hardware tier that counts only what is on in the shipped image. See [design/benchmarks.md](design/benchmarks.md).
- **Formal verification in CI:** every proof target runs on every change, with time budgets and tracked bounds ([design/verification.md](design/verification.md)).
- **Fuzzing:** syscalls from Phase 3, the driver boundary, device-side driver fuzzing, the boot keyboard reader and filesystems from Phase 4a, the network stack, Wi-Fi, Bluetooth and USB from Phase 6.
- **What runs today and what lands later.** Each item above is marked on the scorecard as running now or "lands in Phase N"; nothing is described as running before its job exists.
- **Real hardware:** from Phase 1, each milestone is also checked on at least one real PC booted from USB.

## Phases

Each phase ends with a "done when" test, and every item listed in a phase is
part of that test: if a phase builds something, its done-when checks it. Work
that depends on a phase doesn't ship until that phase's done-when passes.
Work that doesn't depend on it (the built-by-us tracks, proofs, fuzzing,
security items) runs in parallel and is never held back by a later, unrelated
phase. In particular, no security item waits for GPUs or games.
Phases 1 and 4 have lettered parts; "Phase 4" on its own means both 4a and
4b, so an item due "at the end of Phase 4" is due when 4b closes.
Each done-when ends by naming the proof targets and levels
([design/verification.md](design/verification.md)) and the attack rows
([design/threat-model.md](design/threat-model.md)) that gate in that phase.

### Phase 0: It boots (done in CI)
- Rust kernel loaded by Limine, writes to the serial port, paints the screen and reports usable memory.
- CI builds the kernel, makes a bootable ISO, boots it in QEMU on BIOS and UEFI, and uploads the ISO.
- Done when: CI is green and the ISO boots on a real PC from a USB stick, showing the blue screen band.

### Phase 1: The core
- **1a, CPU setup:** GDT, TSS, IDT, and handlers for every CPU exception with a clear crash report (registers and a stack trace); a double-fault handler on its own IST stack so stack overflows are reported.
- **1b, memory:** physical frame allocator (NUMA-aware), our own page tables for the kernel, a fallible kernel heap, guard pages; the kernel built position-independent so the bootloader can randomise its virtual base over at least 2^16 slots at 2 MiB granularity (KASLR), with physical placement randomised separately; first proofs and Miri on the core's host tests.
- **1c, ACPI, interrupts, IOMMU and time:** ACPI tables, AML hosting staged as in [design/drivers.md](design/drivers.md) (safe Rust, fuzzed, confined), minimal PCI enumeration (enough to find and program the IOMMU), local APIC and I/O APIC, the IOMMU in deny-all mode with interrupt remapping and every device's bus mastering cleared in the defined order (USB handover first, the IOMMU's own function exempt) and kept off until assigned; boot-services and bootloader-reclaimable memory is not reused until bus mastering is cleared and the IOMMU is live. Time: a TSC invariance check, calibration from CPUID leaves 15h/16h, else HPET or the ACPI PM timer, else the PIT, and a clocksource watchdog; timer events from the LAPIC TSC-deadline mode, falling back to LAPIC one-shot, HPET, then PIT. Microcode loaded on the boot CPU before mitigations are chosen.
- **1d, console:** a framebuffer text console so the kernel can talk without serial (with a font we make or that is permissively licensed).
- The kernel becomes a workspace with tier-0 crates and enforced lint rules, the unsafe inventory and the core size cap ([design/core.md](design/core.md)).
- Security baseline: no-execute, write-xor-execute kernel mappings, SMEP, SMAP, UMIP, guard pages, KASLR; the shipped image uses the hash-pinned, enrolled boot chain. Supply chain: the exact toolchain version, Limine's SHA-256 and every other build input pinned.
- In-kernel test runner in CI, plus the tough-conditions run and Secure Boot attack tests.
- First scorecard entries: boot time, timer accuracy, kernel memory, and the security table.
- Where it runs today: CI covers QEMU boots, the in-kernel tests and proofs as each lands; real-hardware and bare-metal items are reported "not yet measured" until the owner's machines are online.
- Done when: every exception type is triggered on purpose in tests and reported correctly; the allocator and page tables pass their tests; every tough-conditions scenario passes; the KASLR tests in [design/boot.md](design/boot.md) pass (an SP 800-90B estimate of at least 9 bits for the picker's output over 10^6 host draws, a 90B estimate for each raw source: EFI_RNG, RDRAND/RDSEED and the TSC fallback; and at least 95 distinct bases in 100 QEMU boots); boot time from firmware handoff to kernel ready is under 300 ms on the reference desktop; the timer drifts under 1 part per million over a 10-minute run on KVM against the host's `CLOCK_MONOTONIC_RAW` read over virtio-serial, and on bare metal its drift against a PPS or NTP reference is published and at most 50 ppm (never measured under emulation or against the calibration source); the gate-table check runs; and the outside audit of the Phase 1 core is done with its findings fixed. Proof gates: target 3a at B for each tier-0 API and target 3b at T for each shim, from 1a; targets 1 and 2 at B, at 1b; target 10 at B, at 1c. Attack rows: A4 → poisoned dependency; A5 → DMA attack, on both QEMU `intel-iommu` and `amd-iommu` (including a device that DMAs across the firmware handoff and compatibility-format MSIs).

### Phase 2: Many cores and a scheduler
- Start every CPU core (SMP), with per-core data, microcode on every core, and the typed locking and per-CPU rules in [design/core.md](design/core.md), checked by a lock-order checker.
- The scheduler in [design/scheduler.md](design/scheduler.md): fully preemptible kernel, real-time, fair (EEVDF) and idle classes, threaded interrupt handlers, priority inheritance for kernel locks, topology and capacity-aware placement for hybrid CPUs, high-resolution timers and a tickless idle.
- Kernel threads, sleeping, wake-ups and timers.
- The benchmark harness lands here, with Optimal compared against **itself** only (there is no user space yet to compare with Linux fairly), plus the first dedicated bare-metal runner and its rig ([design/benchmarks.md](design/benchmarks.md)): a controller host, not the machine under test, boots the runner over UEFI HTTP or PXE, power-cycles it through a relay and on a watchdog, and captures checksummed results over serial; only maintainer-approved commits run, on an isolated network with no secrets; benchmark cores are isolated from housekeeping work (`isolcpus`-style) and runs during which `MSR_SMI_COUNT` moved are discarded.
- Done when: all cores run stress tests for an hour with no crash, deadlock or lock-order report; the latency tracer finds no non-preemptible section over 50 µs on the bare-metal runner (report-only in VMs); the priority-inheritance chain, capacity-aware placement, tickless idle and timer accuracy tests in [design/scheduler.md](design/scheduler.md) pass; real-time wake-up latency under saturation meets its provisional target ([design/benchmarks.md](design/benchmarks.md)) on the reference desktop, run from the controller host. Proof gates: target 7 at B for scheduler CPU accounting; target 8 at M for run queues and priority inheritance (loom) and at B for real-time throttling (Kani over an abstract clock); targets 11 and 1s (TLB shootdown) at M; target 3a at M for interrupt re-entry; target 10 at M for the invalidation-before-free order. Attack rows: none.

### Phase 3: User space
- Ring 3, processes and threads with the Linux task model, their own address spaces, ELF loading (static, static-PIE and dynamic), the full auxiliary vector and a vDSO, system calls, per-thread signals with XSAVE-sized frames, 64-bit **and 32-bit** entry paths ([design/compat.md](design/compat.md)).
- Virtual memory features programs need: `mmap`, copy-on-write `fork`, demand paging, PCID (6 dynamic slots per CPU, [design/scheduler.md](design/scheduler.md)), page-table isolation where the CPU needs it.
- A minimal virtual filesystem layer, an initramfs unpacked into a RAM filesystem, and mount namespaces, so per-app file views exist from the start.
- The capability system with derivation and revocation, Linux fds as capabilities to shared open file descriptions (offset, status flags and locks shared by `dup`, `fork` and `SCM_RIGHTS`; revocation acts on the description), kernel-attested app identity, per-app file views, the default app profile and the lockdown table ([design/capabilities.md](design/capabilities.md)).
- Syscall entry hardened against Spectre-style attacks, including index masking; SMT off by default on CPUs affected by MDS, L1TF, TAA or RFDS (listed in the mitigation matrix), with core scheduling as an app-to-app opt-in there and the default only on CPUs whose siblings can't leak kernel data. Syscall fuzzing starts. Verus starts.
- Done when: static glibc and static musl programs (64 and 32 bit) and the milestone 1 suite (with the single-threaded gVisor subset) run unchanged; the lockdown test passes with every attempt refused; published Spectre and Meltdown proof-of-concepts, and the cross-hyperthread leak test on the shipped SMT default of each CPU model in the matrix and separately with SMT on and core scheduling opted in, recover zero bytes over 10^6 attempts; an unknown-syscall sweep returns `ENOSYS` for every unimplemented number; the latency tracer costs at most 0.5% on the syscall round-trip microbenchmark when off; and the outside audit of syscall entry, user copy and capabilities is done. Proof gates: target 1 at P; target 1s re-gated at M (PCIDs and user address spaces); target 4 at P for the sharing paths that exist in Phase 3; target 5 at P, including open file descriptions, with its per-syscall test at T; target 6 at B. Attack rows: A3 → other apps' data (the hostile-process suite); A3 → kernel; A3 → side channels.

### Phase 4a: Storage and files
- PCI enumeration with AER/DPC and hotplug, then NVMe and AHCI drivers on the driver framework: trust tiers, the message interface, budgets, fault containment and restart, suspend/resume callbacks, device-side fuzzing ([design/drivers.md](design/drivers.md)). virtio-blk runs in tier 2; NVMe and AHCI start in tier 1 and must meet the promotion thresholds for in-kernel parsers before this phase closes. On machines without an IOMMU (D2), descriptor memory a device reads is built only from typed addresses of granted buffers, and DMA-address registers (NVMe admin queues, ring bases, DCBAAP) are written only by the core from a `DmaBuffer`; tier-3 drivers rely on the IOMMU alone.
- Unlock input before Phase 6: PS/2 and serial keyboards, and a polled xHCI boot-protocol keyboard reader in tier 1 that parses only 8-byte boot reports and must meet the promotion thresholds for its parser.
- The block layer with flush/FUA and completion ordering, the durability contract, the page cache with stable pages and copy-on-write space reservation ([design/storage.md](design/storage.md)).
- The full virtual filesystem layer, ext4 read-only on any volume (volumes Optimal didn't create, such as a dual-boot partition, are parsed by the tier-3 file system server), and ext4 **read-write on volumes Optimal created** with its own mkfs and the fixed feature list in [design/storage.md](design/storage.md), after the differential `e2fsck` gate passes.
- Disk encryption stage 1 (LUKS2, AES-XTS) with a passphrase (Argon2id, at least 1 GiB and 4 passes) and a 256-bit recovery key.
- The read-only, hash-tree-verified system image format with A/B slots.
- Filesystem testing: the crash-state model and oracle in [design/storage.md](design/storage.md), property tests against a reference model, image fuzzing (including LUKS2 headers), long randomised workloads, all running forever in the nightly soak.
- Done when: the kernel boots from a real NVMe drive with its disk encrypted, and the passphrase can be typed on a PS/2, a USB and a serial keyboard; the 24-hour file stress test and at least 10^7 crash states per night pass the oracle; every durability-contract crash test passes, including overlapping writes re-sent after a driver restart; the differential `e2fsck` gate is clean; the LUKS2 KDF floor test refuses a keyslot below the Argon2id floor; 1,000 real power cuts on ext4 with LUKS2 on the relay rig pass the oracle before read-write ext4 is released; killing the virtio-blk driver (tier 2) under load recovers within 500 ms with in-flight requests resubmitted and no corruption; NVMe, AHCI and the boot keyboard reader meet the promotion thresholds; a flipped block in the system image is caught; device-side fuzzing of the storage drivers has run 24 hours clean; a PCILeech-class FPGA board fails to read a canary, spoof a requester ID, use a translated request or forge an MSI ([design/drivers.md](design/drivers.md)); the Phase 4 storage benchmark set in [design/benchmarks.md](design/benchmarks.md) (QD1 `psync`, QD32 as 32 forked static `psync` processes, SQLite in WAL mode and an untar, static binaries on both) is within 10% of matched Linux on aged ext4 with encryption on; and the outside audit of the IOMMU, DMA and driver boundary code is done. Proof gates: target 2 at P; target 7 at B for memory and interrupt budgets; target 9 at B; target 10 at P; target 12 at B; target 1's write-during-writeback QEMU test. Attack rows: A5 → DMA attack (the FPGA board on a D1 reference machine, across the firmware-to-kernel hand-off too); A6 → read the disk; A6 → tamper with ciphertext (stage 1).

### Phase 4b: The trusted boot chain
- Entry condition: Optimal Boot's measured boot and boot counting have landed (bootloader track).
- The TPM 2.0 driver, the signed TPM policy, PIN and recovery, TPM ownership, with parameter encryption ([design/boot.md](design/boot.md)). The PIN is stretched with Argon2id and mixed into the key; on firmware-TPM machines a passphrase of at least 60 estimated bits is required instead. Once Optimal Boot lands, the PIN can be typed in the loader through UEFI ConIn before ExitBootServices.
- PCRs 8 and 9 are capped unconditionally, whatever the unseal result (passphrase fallback included), before any byte of the data volume beyond its LUKS2 header is parsed; the sealed secret binds the volume's identity. Before capping, the kernel's TPM service unseals the secret of a persistent service key and loads it; objects used after capping (per-user keys, the replay counter, the hibernation key) carry `TPM2_PolicySecret` against it, so only a kernel that passed the pre-cap check can use them.
- Anti-rollback: the floor is the NV counter's value minus a base kept in a write-once NV index locked at install. Raising it takes two boots: after the health check, user space writes a "commit SVN n" request into loader state, and on the next boot the kernel (B1, before capping) or the loader (B2) raises the floor, resealing under the new floor before the counter is incremented; before any raise the other slot and both copies of the recovery image are written at the new SVN and verified. On B2 machines (no TPM) the floor is a boot-services-only UEFI variable written only by the loader.
- Done when: the TPM tamper tests and rollback tests in [design/boot.md](design/boot.md) pass under swtpm and on the reference laptop, including a pre-incremented swtpm counter, a floor raise across a reboot, no increment possible after capping, a power cut at every step of a raise, the OS unable to write the B2 floor variable, booting the recovery image after a raise, a substituted data volume refused, and a flipped block in the active slot making the loader fall back to the other; and the outside audit of the TPM code and of the core as a whole is done. Proof gates: none. Attack rows: A6 → tamper with the boot chain; A6 → TPM bus; A6 → guess the PIN.

### Phase 5: Running real Linux programs
- Compatibility milestones 2 to 7 in [design/compat.md](design/compat.md) (2 and 3 start in Phase 3 and 4 in Phase 4a; all gate here): dynamic linking, threads, PI futexes and `futex_waitv`, `/proc` and `/sys` per the contract, desktop IPC, namespaces, seccomp and cgroups, `ptrace` and `perf_event_open`.
- Scheduling ABI (policies, affinity, cgroup CPU control, the `sched_attr.sched_runtime` slice), pressure information, compressed swap, transparent huge pages and INVLPGB broadcast shootdowns, user-space shadow stacks, io_uring (milestone 5).
- The permission broker and portals, with prompts on a dedicated prompt VT reached by the secure-attention key (Ctrl+Alt+Delete) until the compositor can draw them in Phase 7 ([design/capabilities.md](design/capabilities.md)); setuid replaced by brokers. D-Bus proxies and per-app keyrings are mediated from here.
- App identity: an unpackaged program's identity is its path plus content hash, and its grants reset when the hash changes; executables written by an app carry that provenance and get no network on first desktop launch until the user is asked; only the launcher and the service manager hold the capability to spawn a process with an identity, children get at most their parent's grants, and processes joined by a direct channel are checked as one principal.
- Per-user home encryption: each user's home lives in per-user extents of an in-kernel volume map on the outer encrypted device, encrypted only by that user's key, which the TPM service unwraps after the PCRs are capped ([design/storage.md](design/storage.md)).
- Run BusyBox, a shell, then real tools like Python, GCC, git and SQLite.
- First benchmarks against Linux with identical binaries on both. Precondition: frequency control with hardware P-states (Intel HWP, AMD CPPC) at a fixed energy-performance preference, set the same on Linux.
- Done when: each milestone's suite reaches its published target pass rate with results reported by outcome (target 90% for the pinned LTP syscall list, with undocumented skips counted as failures, and 100% for security-related tests); a Steam Runtime container runs under `bwrap`; Chromium's sandbox self-test passes; the hostile tests in [design/capabilities.md](design/capabilities.md) pass: widening an fd through its open file description, `/proc/self/fd` reopen or a cross-identity `..` (`EXDEV`), escaping a mount namespace, and forging an app identity; a second user logs in after the PCRs are capped on a B1 machine and gets their key; per-user home encryption is benchmarked; and context-switch, wake-up, messaging and `fork`+`exec` with 10, 10^4 and 10^5 open fds are within 10% of matched Linux on the reference desktop. Proof gates: target 1s re-gated at M (INVLPGB and transparent huge pages); target 3a at P for the capability and memory APIs; target 4 extended to shared memory, memfd, `SCM_RIGHTS`, `ptrace`, `process_vm_*` and io_uring buffers. Attack rows, A3 (malicious app): A3 → forged identity; A3 → shared channels, D-Bus and keyrings; A3 → confused deputy; A3 → prompt spoofing, console. A7 (other local user): A7 → other user's data; A7 → kernel crash dumps.

### Phase 6: Input, USB, networking and audio
- The full xHCI USB driver with USB authorisation, keyboards, mice and storage (USB mass storage meeting the promotion thresholds); PS/2 mice as a fallback.
- Network drivers (virtio, Intel and Realtek first) and a TCP/IP stack. Wi-Fi and Bluetooth are split on every machine into a small tier-2 transport (rings and DMA, no frame parsing, meeting the promotion thresholds) and a tier-3 802.11 or Bluetooth stack; on D2 machines the radios stay off until the user accepts a named "remote DMA exposure" warning, and the scorecard says the remote promise doesn't hold on that machine. Network, Wi-Fi and Bluetooth fuzzing start. Network-facing services (DHCP, DNS, time) run sandboxed; until the update client lands in Phase 7, the time floor is a stored, signed time floor.
- Network policy: a per-identity egress filter, so grants reach live sockets. A network grant is outbound only; listening on a non-loopback port needs a separate per-port grant (including a terminal dev server reached from the browser); an app with no network gets `ENETUNREACH` for anything but loopback.
- The Thunderbolt/USB4 connection manager in tier 2 (meeting the promotion thresholds), with external devices needing approval.
- Audio: HD Audio and USB Audio Class 2 drivers, the ALSA interface, PipeWire with per-app permissions and real-time scheduling through a portal.
- Laptop basics: embedded controller, battery, backlight, I2C-HID touchpads, lid and power buttons.
- Default-deny inbound firewall and no listening services.
- Compatibility milestones 8, 9 and 10.
- Done when: the PC can be used with USB devices only; a large download over a named NIC reaches at least 95% of the link's measured TCP goodput on Linux; an app with no network grant gets `ENETUNREACH` for anything but loopback, and revoking a grant cuts its live sockets; Wi-Fi and Bluetooth frame fuzzing has run 24 hours with no open crash; and audio has 0 underruns at 128 frames under load over a 24-hour run per buffer size (run weekly and per release, not nightly), with the 95% upper bound on the rate published. Proof gates: none. Attack rows: A1 → open port; A1 → listening app; A1 → network stack; A1 → DHCP, DNS, NTP, Wi-Fi supplicant (against the stored, signed time floor); A1 → Wi-Fi, Bluetooth drivers; A1 → compromised radio firmware → DMA; A3 → shared channels, PipeWire; A3 → exfiltration (sockets); A5 → BadUSB; A5 → malformed descriptors.

### Phase 7: Graphics and a desktop
- Kernel modesetting interface (DRM/KMS, GEM, dma-buf, sync objects), starting with virtio-gpu and simple framebuffers.
- Run a borrowed Wayland compositor, patched to draw portal prompts and enforce `wp_security_context_v1` (part of the trusted base), and Mesa's software renderer (llvmpipe) on top, with portals, the seat service, per-app XWayland and clipboard mediation. Compatibility milestone 12, and milestone 11's virtio-gpu part.
- The signed update client: TUF metadata, transparency-log inclusion proofs with witness cosignatures, install into the inactive A/B slot, boot counting and automatic fallback ([design/boot.md](design/boot.md)), so the minimum product can be patched.
- Done when: a Wayland desktop session runs with a terminal and a browser, **with the browser's own sandbox on**; a corrupted update and a hanging kernel both fall back to the old slot automatically, and an image below the anti-rollback floor is refused; input-to-display latency (gated on P99 per run of at least 10^4 events, and P99.9 from at least 10^5 events pooled across runs with a bootstrap confidence interval), 60 Hz frame time (virtio-gpu and llvmpipe, against Linux on the same stack), foreground responsiveness during I/O, the memory-pressure test (`mem=8G`), total OS memory at an idle desktop and idle CPU meet their provisional targets ([design/benchmarks.md](design/benchmarks.md)). Proof gates: target 4 extended to dma-buf. Attack rows: A2 → app parser bug; A3 → shared channels, Wayland and X11; A3 → screen, input, mic, camera; A3 → exfiltration (full suite, including two colluding apps); A4 → signed malicious update; Promise 3 → telemetry.

### Phase 8: Real GPU drivers
- The hardest part of any new kernel. Start with one family (AMD or Intel) and work towards accelerated 3D through Mesa, split into a small, size-capped tier-1 GPU core, display and power code in the tier-2 framework, and tier-3 management, under the written clean-room process ([design/drivers.md](design/drivers.md)). The GPU's own firmware (GuC, MES, PSP, SMU) is listed in the trusted base and the firmware ledger. Compatibility milestone 11.
- Done when: Mesa runs hardware-accelerated OpenGL and Vulkan on one GPU family with published Vulkan CTS and piglit pass rates; the cross-process GPU buffer isolation test passes; 144 Hz frame time meets its target on the reference discrete GPU; a hung engine is reset within 2 seconds without losing the session, and a full adapter reset is measured and published separately; and 5 benchmark games run within 15% of Linux. Proof gates: target 4 extended to GPU buffers; target 13 at B. Attack rows: none.

### Phase 9: Games and Windows apps
- Run Steam, Wine and Proton on the Linux syscall layer (milestone 13), with Steam's manifest-declared shared channels to the games it starts and a virtual-gamepad portal whose `uinput` accepts only gamepad codes. Fix what breaks, one game at a time, from a published test list.
- Kernel-level anti-cheat (Valorant, some Call of Duty and EA titles) is a Windows kernel driver and won't run here; the list says so per game. User-mode anti-cheat with Proton support (Easy Anti-Cheat, BattlEye) is a supported target, with named test titles whose anti-cheat probes are run and published.
- Done when: the milestone 13 suite reaches its target, including the shared-channel and virtual-gamepad tests; and a published list of 20 popular games runs, with frame rates compared against Linux and Windows on the same PC. Proof gates: none. Attack rows: none.

### Phase 10: Performance and lean push
- Profile everything. Tune the scheduler, memory management and I/O paths until the desktop benchmark set beats Linux by the rule in [design/benchmarks.md](design/benchmarks.md) (server-style throughput stays within 5%).
- Power management: CPU sleep states, frequency scaling, Thread Director and preferred-core hints, the X3D policy, the deadline scheduling class, and suspend/resume for laptops (microcode reapplied on resume), with the LUKS volume key and home keys wiped on suspend and asked for again on resume, and hibernation ([design/storage.md](design/storage.md)).
- Done when: Optimal beats matched Linux on the desktop set on two different real PCs (every desktop metric at parity or better, the geometric mean ahead, none more than 5% worse); battery life on the three defined workloads is at least matched Linux's; and hibernation evicts protected-class keys before writing the image, whose key needs the pre-cap PCRs plus the PIN. Proof gates: none. Attack rows: A6 → locked or suspended laptop (suspend: no volume or home key in memory, the secret asked again on resume); A6 → TPM reset or lost state on resume; A6 → hibernation image.

### Phase 11: Hardening and release
- Full-time fuzzing in CI, kernel CET if not already done, the final outside audit of the core before 1.0.
- ARM64 port.
- Customisation: selectable schedulers and boot options.
- Done when: each item above has passed its own check (audit findings fixed, fuzzing running with no open crash older than 30 days, ARM64 passing the same boot matrix and test suite on QEMU and one real board), and a month of nightly builds on test machines has no crash. Proof gates: none. Attack rows: none.

### Phase 12: The Optimal OS
- Updates driven from the settings app and store, on the Phase 7 update client.
- One settings app, one app store, one app format (with manifests per [design/capabilities.md](design/capabilities.md)), and a GUI for every common task. Flatpak, AppImage and plain tarballs keep working.
- Built-in tiling, a gaming mode, power profiles, and layout presets.
- No ads, no forced accounts, no telemetry by default. Optional local AI that is off until switched on.
- Done when: a stranger goes from the website to a working desktop with Steam running in under 30 minutes, without opening a terminal, and installs and updates an app and the OS from the GUI. Proof gates: none. Attack rows: none.

## Linux compatibility milestones

Linux programs depend on far more than syscall numbers. Compatibility is
split into 13 milestones, from ELF execution to Wine and Proton, each with a
pinned test suite that runs in CI and whose results (by outcome, never one
blended number) go on the scorecard. The full table, the ABI decisions
(32-bit support, kernel identity, `ENOSYS` for anything missing, io_uring and
eBPF) and the measurement rules are in [design/compat.md](design/compat.md).

## Built by us: our own stack

These pieces will be written by us, from specs, instead of borrowed. Each one
runs as its own track alongside the kernel phases, and starts once the kernel
can support it. Until ours is ready, the borrowed version (where one exists)
keeps things working.

| Piece | Starts after | Replaces | Done when |
|---|---|---|---|
| Bootloader (Optimal Boot) | Phase 1 | Limine | Boots Optimal on UEFI across the full CI boot matrix and on 3 real PCs; signed config, measured boot and the tamper tests pass; Secure Boot paths per [design/boot.md](design/boot.md) (owner keys working on 3 PCs, shim submitted for review); parsers fuzzed. BIOS support is a later, separate item |
| Filesystem | Phase 4 | ext4 as the default | Copy-on-write with authenticated encryption, checksums and snapshots (a user feature; OS rollback uses the A/B image); at least 10^7 crash states per night pass the oracle in [design/storage.md](design/storage.md), plus 1,000 real power cuts on 3 drive models; fsck, scrub and rescue tools ship with format v1; beats matched ext4 on the aged storage benchmark set |
| Compositor | Phase 7 | Borrowed Wayland compositor | Speaks the Wayland protocol so existing apps run; tear-free, variable refresh rate, and input-to-display latency P99 and P99.9 lower than the best of KWin, Mutter and Sway on the reference desktop in the benchmark harness |
| Desktop | Compositor | Borrowed desktop shell | Panel, launcher, notifications, built-in tiling and gaming mode, all within the total OS memory target in [design/benchmarks.md](design/benchmarks.md) |
| Settings app | Desktop | Separate config tools | One app controls everything, with no terminal needed for any common task (a published list of tasks, each tested) |
| Store | Desktop + networking | Borrowed store | One store and one package format, with signed packages, atomic installs and rollback |
| Everyday apps | Desktop | Borrowed apps | Files, terminal, text editor, image viewer, media player, calculator and system monitor |
| Web browser engine | Phase 7 | Borrowed browser | Passes at least 90% of the Web Platform Tests subsets for HTML, CSS and DOM that the major engines all pass, runs a JavaScript engine that passes test262 at 95%, and renders the 50 most-visited sites with no blocking bug |

**Minimum product.** The kernel through Phase 7 with every security gate
passing (signed updates included), the bootloader, and borrowed userland is a complete, usable,
secure OS. Everything after that improves it. If time runs short, the order
of priority is: kernel phases and their security gates, then the bootloader
and filesystem tracks, then GPU and games, then the other tracks.

**Defer rules.** Every track reports one progress metric (for the browser
engine, its WPT pass rate; for the compositor, its latency against the best
borrowed one). A track whose metric hasn't moved in 12 months is paused and
its borrowed replacement stays, until there's a plan to unblock it. No track
is ever shipped before it meets its done-when, whatever that costs in time.

Honest notes:
- **Bootloader** and **everyday apps** are the most achievable.
- **Filesystem** is very hard to get right, because a bug loses people's files. It gets the heaviest testing of anything here.
- **Web browser engine** is the biggest single project on this list, bigger than the kernel. HTML, CSS and JavaScript each take years. It is a long-running track, and a borrowed browser ships until ours passes its tests.

## What only the owner can provide

Some things can't be done from CI and need Ben:

- **Real hardware:** booting each milestone from USB on a real PC; the reference machines in [design/benchmarks.md](design/benchmarks.md), set up as dedicated runners for the performance gate (the desktop by Phase 2, the rest by Phase 5); the benchmark controller host (a Raspberry Pi or similar, with a power relay, serial capture and an isolated network) by Phase 2; a multi-node NUMA reference machine (2-socket, or one CPU in NPS2/NPS4), until which NUMA is tested for function only; a PCILeech-class FPGA DMA board by Phase 4a; a relay power-cut rig by Phase 4a (ext4) and 3 drive models for the filesystem track; an input-latency rig (USB HID microcontroller and photodiode) by Phase 7.
- **Signing keys:** hardware tokens for the offline root key (2 of 3) before the first public release.
- **Outside audits** at the end of Phases 1, 3 and 4 and before 1.0 (volunteers or funding).
- **Accounts:** shim review submission and Microsoft signing, and repository settings.

Until a dependency arrives, the work that needs it is reported as "not yet
measured" or "not yet done", never claimed.

## What we deliberately don't build
- A compiler (Rust/LLVM), and for now the GPU graphics stack (Mesa) and Windows compatibility (Wine/Proton). Those run through the Linux interface and can become "built by us" tracks later.
