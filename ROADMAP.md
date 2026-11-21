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
  - Borrowed programs shipped in the OS until ours replace them: permissive licences preferred; weak copyleft (LGPL, MPL) allowed as separate, unmodified programs (Wine, a browser); GPL programs are not shipped.
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
- **Formal verification of the security-critical core:** machine-checked proofs (Kani, Verus, loom) for page tables, the frame allocator, the soundness of the core's safe APIs, process memory isolation, capability derivation and revocation, user-memory access, resource accounting, the scheduler, the driver boundary, the IOMMU and storage invariants. Each target states its assurance level (proved, bounded, model-checked or tested only) and its assumptions ([design/verification.md](design/verification.md)).
- **Hardware protections:** no-execute pages, write-xor-execute for all kernel memory, SMEP/SMAP, UMIP, guard pages around every stack, KASLR with a position-independent kernel and measured entropy, page-table isolation where the CPU needs it, and the IOMMU from Phase 1. Control-flow protection: user-space CET shadow stacks from Phase 5; kernel shadow stacks and indirect-branch tracking as soon as the stable Rust toolchain can build for them (tracked on the scorecard until then).
- **Least privilege through capabilities:** a process can only touch objects it holds a handle to; Linux file descriptors are capabilities, with a derivation tree and revocation ([design/capabilities.md](design/capabilities.md)).
- **Every app sandboxed by default:** its own view of the filesystem, no access to other apps' data, and access to the camera, microphone, screen, location and user files only through portals whose prompts the compositor draws. Apps holding file grants get no network access unless the user allows it.
- **IOMMU on by default from Phase 1:** devices can only reach memory handed to them, interrupt remapping on, external devices need approval. Machines without an IOMMU are told so.
- **Encryption by default:** Phase 4 encrypts the disk (AES-XTS, confidentiality only); the own filesystem then adds authenticated, per-user encryption with keys evicted on lock ([design/storage.md](design/storage.md)). Keys are sealed to measured boot with a signed TPM policy plus a PIN where there's a TPM, a passphrase where there isn't, and a recovery key is always created.
- **Verified and measured boot:** firmware checks the bootloader, the bootloader checks a signed config that pins the kernel and the system image's hash tree root, and every step is measured into the TPM. Anti-rollback, A/B slots, boot counting and a recovery image are in [design/boot.md](design/boot.md).
- **Nothing listening by default:** no remote-access services, a default-deny inbound firewall.
- **Lockdown:** no interface lets anyone, root included, load unsigned ring-0 code or read kernel memory ([design/threat-model.md](design/threat-model.md)).
- **Signed updates only,** with a TUF-style key hierarchy, an offline root, a transparency log, reproducible builds and anti-rollback.
- **CPU side-channel mitigations** chosen per CPU model after microcode is loaded, including Spectre v1 index masking in safe Rust parsers and core scheduling where SMT leaks; costs benchmarked and published.
- **Testing security like everything else:** every attack path in the threat model has a test that becomes a gate in the phase it lands; continuous syscall, network, device-side and filesystem fuzzing; outside audits of the core at the end of Phases 1, 3 and 4 and before 1.0.

## How we know it is good

- **CI on every change:** build, `rustfmt`, `clippy` with warnings as errors plus the core's lint rules, the unsafe inventory and core size check, `cargo-deny` and `cargo-vet`, proofs, and a boot matrix in QEMU: 64 virtual PCs (BIOS and UEFI, old and new chipsets, 128 MiB to 8 GiB of RAM, 1 to 16 cores), each booted 3 times.
- **No silent skips.** A test that can't run (missing firmware, missing tool) fails CI unless it is on a checked-in allowlist with a reason. A green run means everything listed actually ran.
- **Nightly soak:** the same matrix booted 40 times per configuration (over 2,500 boots) to catch rare, timing-dependent bugs. Soak tests grow with every phase: hour-long scheduler stress, file system crash-state runs, driver kill-and-restart loops, continuous fuzzing.
- **In-kernel test suite** (from Phase 1): unit tests that run inside the kernel under QEMU, so allocators, page tables and the scheduler are tested in the real environment.
- **Tough but realistic conditions** (from Phase 1): every change also boots on deliberately awkward machines: CPUs from Core 2 to current Xeon/EPYC with security features missing, 5-level paging, odd core topologies, NUMA (including a node with no memory), odd and split memory, no HPET, no ACPI, old and new OVMF builds, a host fighting for CPU, and Secure Boot attacks (unsigned bootloader, wrong key, tampered kernel, tampered config, old signed image), which must be refused. Fault injection joins as each piece lands: failed allocations, quirky ACPI tables, timer jitter, spurious interrupts, hostile DMA devices, then disk power cuts and I/O errors.
- **Public scorecard from the first bootable build:** performance against a pinned, matched Linux baseline, measured on dedicated bare-metal machines (shared CI VMs are too noisy and are used for functional tests only); committed provisional targets that can only tighten; and a security scorecard per hardware tier that counts only what is on in the shipped image. See [design/benchmarks.md](design/benchmarks.md).
- **Formal verification in CI:** every proof target runs on every change, with time budgets and tracked bounds ([design/verification.md](design/verification.md)).
- **Fuzzing:** syscalls from Phase 3, the driver boundary, device-side driver fuzzing and filesystems from Phase 4, the network stack and USB from Phase 6.
- **Real hardware:** from Phase 1, each milestone is also checked on at least one real PC booted from USB.

## Phases

Each phase ends with a "done when" test, and every item listed in a phase is
part of that test: if a phase builds something, its done-when checks it. Work
that depends on a phase doesn't ship until that phase's done-when passes.
Work that doesn't depend on it (the built-by-us tracks, proofs, fuzzing,
security items) runs in parallel and is never held back by a later, unrelated
phase. In particular, no security item waits for GPUs or games.

### Phase 0: It boots (done in CI)
- Rust kernel loaded by Limine, writes to the serial port, paints the screen and reports usable memory.
- CI builds the kernel, makes a bootable ISO, boots it in QEMU on BIOS and UEFI, and uploads the ISO.
- Done when: CI is green and the ISO boots on a real PC from a USB stick, showing the blue screen band.

### Phase 1: The core
- **1a, CPU setup:** GDT, TSS, IDT, and handlers for every CPU exception with a clear crash report (registers and a stack trace); a double-fault handler on its own IST stack so stack overflows are reported.
- **1b, memory:** physical frame allocator (NUMA-aware), our own page tables for the kernel, a fallible kernel heap, guard pages; the kernel built position-independent so the bootloader can randomise it (KASLR); first proofs (targets 1 and 2) and Miri on the core's host tests.
- **1c, ACPI, interrupts, IOMMU and time:** ACPI tables, an AML interpreter (safe Rust, fuzzed), local APIC and I/O APIC, the IOMMU in deny-all mode with interrupt remapping and every device's bus mastering off until assigned, an accurate clock (TSC calibrated against HPET or the ACPI timer), a timer interrupt, microcode loaded on the boot CPU before mitigations are chosen.
- **1d, console:** a framebuffer text console so the kernel can talk without serial (with a font we make or that is permissively licensed).
- The kernel becomes a workspace with tier-0 crates and enforced lint rules, the unsafe inventory and the core size cap ([design/core.md](design/core.md)).
- Security baseline: no-execute, write-xor-execute kernel mappings, SMEP, SMAP, UMIP, guard pages, KASLR; the shipped image uses the hash-pinned, enrolled boot chain.
- In-kernel test runner in CI, plus the tough-conditions run and Secure Boot attack tests.
- First scorecard entries: boot time, timer accuracy, kernel memory, and the security table.
- Done when: every exception type is triggered on purpose in tests and reported correctly; the allocator and page tables pass their tests and proofs (targets 1, 2 and 10 at level B); every tough-conditions scenario passes; the KASLR test measures at least 9 bits of entropy over 100 boots; a hostile QEMU device's DMA and interrupt writes outside its domain are blocked and logged; the timer keeps accurate time over a 10-minute run (drift under 1 part per million against the host clock); and the outside audit of the Phase 1 core is done with its findings fixed.

### Phase 2: Many cores and a scheduler
- Start every CPU core (SMP), with per-core data, microcode on every core, and the typed locking and per-CPU rules in [design/core.md](design/core.md), checked by a lock-order checker.
- The scheduler in [design/scheduler.md](design/scheduler.md): fully preemptible kernel, real-time, fair (EEVDF) and idle classes, threaded interrupt handlers, priority inheritance for kernel locks, topology and capacity-aware placement for hybrid CPUs, core scheduling where SMT leaks, high-resolution timers and a tickless idle.
- Kernel threads, sleeping, wake-ups and timers.
- The benchmark harness lands here, with Optimal compared against **itself** only (there is no user space yet to compare with Linux fairly), plus the first dedicated bare-metal runner.
- Proofs: targets 7, 8 and 11 (loom).
- Done when: all cores run stress tests for an hour with no crash, deadlock or lock-order report; the latency tracer finds no non-preemptible section over 50 µs; real-time wake-up latency under saturation meets its provisional target ([design/benchmarks.md](design/benchmarks.md)) on the reference desktop.

### Phase 3: User space
- Ring 3, processes and threads with the Linux task model, their own address spaces, ELF loading (static, static-PIE and dynamic), the full auxiliary vector and a vDSO, system calls, per-thread signals with XSAVE-sized frames, 64-bit **and 32-bit** entry paths ([design/compat.md](design/compat.md)).
- Virtual memory features programs need: `mmap`, copy-on-write `fork`, demand paging, PCID, page-table isolation where the CPU needs it.
- The capability system with derivation and revocation, Linux fds as capabilities, per-app file views and the lockdown table ([design/capabilities.md](design/capabilities.md)).
- Syscall entry hardened against Spectre-style attacks, including index masking. Syscall fuzzing starts.
- Proofs: targets 4, 5 and 6; Verus starts.
- Done when: static glibc and static musl programs (64 and 32 bit) and the milestone 1 suite run unchanged; the hostile-process suite and the lockdown test pass with every attempt refused; published Spectre and Meltdown proof-of-concepts fail to leak; an unknown-syscall sweep returns `ENOSYS` for every unimplemented number; proofs 4 to 6 run at their stated levels; and the outside audit of syscall entry, user copy and capabilities is done.

### Phase 4: Storage, files and the trusted boot chain
- PCI enumeration with AER/DPC and hotplug, then NVMe and AHCI drivers on the driver framework: trust tiers, the message interface, budgets, fault containment and restart, suspend/resume callbacks, device-side fuzzing ([design/drivers.md](design/drivers.md)).
- The block layer with flush/FUA and completion ordering, the durability contract, the page cache with stable pages and copy-on-write space reservation ([design/storage.md](design/storage.md)).
- A virtual filesystem layer, a RAM filesystem, and ext4 **read-only** with journal replay into memory (read-write only on a published feature list after the differential gate).
- Disk encryption stage 1 (AES-XTS), with a passphrase and recovery key; the TPM 2.0 driver, measured boot (with Optimal Boot), the signed TPM policy and PIN, which together make TPM unlocking available as soon as Optimal Boot's measured boot lands ([design/boot.md](design/boot.md)).
- The read-only, hash-tree-verified system image format with A/B slots.
- Filesystem testing: the crash-state model and oracle in [design/storage.md](design/storage.md), property tests against a reference model, image fuzzing, long randomised workloads, all running forever in the nightly soak.
- Proofs: targets 9 and 12.
- Done when: the kernel boots from a real NVMe drive with its disk encrypted; the 24-hour file stress test and at least 10^7 crash states per night pass the oracle; every durability-contract crash test passes; killing the NVMe driver under load recovers within 500 ms with no corruption (if NVMe has moved to tier 2; otherwise the same test on AHCI); the TPM tamper tests and rollback tests in [design/boot.md](design/boot.md) pass under swtpm; a flipped block in the system image is caught; device-side fuzzing of the storage drivers has run 24 hours clean; the storage benchmark set is within 10% of matched Linux on aged filesystems with encryption on; and the outside audit of the IOMMU, DMA, driver boundary and TPM code is done.

### Phase 5: Running real Linux programs
- Compatibility milestones 2 to 7 in [design/compat.md](design/compat.md): dynamic linking, threads, PI futexes and `futex_waitv`, `/proc` and `/sys` per the contract, desktop IPC, namespaces, seccomp and cgroups, `ptrace` and `perf_event_open`.
- Scheduling ABI (policies, affinity, cgroup CPU control), pressure information, compressed swap, user-space shadow stacks.
- Run BusyBox, a shell, then real tools like Python, GCC, git and SQLite.
- First benchmarks against Linux with identical binaries on both.
- Done when: each milestone's suite reaches its published target pass rate with results reported by outcome (target 90% for the pinned LTP syscall list, with undocumented skips counted as failures); a Steam Runtime container runs under `bwrap`; Chromium's sandbox self-test passes; and context-switch, wake-up and messaging benchmarks are within 10% of matched Linux on the reference desktop.

### Phase 6: Input, USB, networking and audio
- xHCI USB with USB authorisation, keyboards, mice and storage. PS/2 as a fallback.
- Network drivers (virtio, Intel and Realtek first), Wi-Fi in tier 3, and a TCP/IP stack; network fuzzing starts.
- Audio: HD Audio and USB Audio Class 2 drivers, the ALSA interface, PipeWire with real-time scheduling through a portal.
- Laptop basics: embedded controller, battery, backlight, I2C-HID touchpads, lid and power buttons.
- Default-deny inbound firewall and no listening services.
- Compatibility milestones 8, 9 and 10.
- Done when: the PC can be used with USB devices only; an emulated keyboard that types immediately gets no keystroke accepted; a large download over a named NIC reaches at least 95% of the link's measured TCP goodput on Linux; a port scan of a fresh boot finds 0 open ports; the network stack has 10^9 fuzz executions and USB descriptor fuzzing 24 hours with no open crash; and audio has 0 underruns per hour at 128 frames under load.

### Phase 7: Graphics and a desktop
- Kernel modesetting interface (DRM/KMS, GEM, dma-buf, sync objects), starting with virtio-gpu and simple framebuffers.
- Run a Wayland compositor (part of the trusted base) and Mesa's software renderer on top, with portals and the seat service.
- Done when: a Wayland desktop session runs with a terminal and a browser, **with the browser's own sandbox on**; the sandbox-escape and prompt-spoofing suites pass; and input-to-display latency and frame time meet their provisional targets.

### Phase 8: Real GPU drivers
- The hardest part of any new kernel. Start with one family (AMD or Intel) and work towards accelerated 3D through Mesa, split into a small tier-1 GPU core and tier-3 management, under the written clean-room process ([design/drivers.md](design/drivers.md)).
- Done when: Mesa runs hardware-accelerated OpenGL and Vulkan on one GPU family with published Vulkan CTS and piglit pass rates; the cross-process GPU buffer isolation test passes; a hung engine recovers within 2 seconds without losing the session; and 5 benchmark games run within 15% of Linux.

### Phase 9: Games and Windows apps
- Run Steam, Wine and Proton on the Linux syscall layer (milestone 13). Fix what breaks, one game at a time, from a published test list.
- Kernel-level anti-cheat (Valorant, some Call of Duty and EA titles) is a Windows kernel driver and won't run here; the list says so per game. User-mode anti-cheat with Proton support (Easy Anti-Cheat, BattlEye) is a supported target, with named test titles.
- Done when: a published list of 20 popular games runs, with frame rates compared against Linux and Windows on the same PC.

### Phase 10: Performance and lean push
- Profile everything. Tune the scheduler, memory management and I/O paths until the desktop benchmark set beats Linux by the rule in [design/benchmarks.md](design/benchmarks.md) (server-style throughput stays within 5%).
- Power management: CPU sleep states, frequency scaling, Thread Director and preferred-core hints, the X3D policy, the deadline scheduling class, and suspend/resume for laptops (microcode reapplied on resume).
- Done when: Optimal beats matched Linux on the desktop set on two different real PCs (every desktop metric at parity or better, the geometric mean ahead, none more than 5% worse), and battery life on the three defined workloads is at least matched Linux's.

### Phase 11: Hardening and release
- Full-time fuzzing in CI, kernel CET if not already done, the final outside audit of the core before 1.0.
- ARM64 port.
- Customisation: selectable schedulers and boot options.
- Done when: each item above has passed its own check (audit findings fixed, fuzzing running with no open crash older than 30 days, ARM64 passing the same boot matrix and test suite on QEMU and one real board), and a month of nightly builds on test machines has no crash.

### Phase 12: The Optimal OS
- Atomic, signed system updates using the A/B system image, with anti-rollback, boot counting, automatic fallback and a recovery image ([design/boot.md](design/boot.md)).
- One settings app, one app store, one app format (with manifests per [design/capabilities.md](design/capabilities.md)), and a GUI for every common task. Flatpak, AppImage and plain tarballs keep working.
- Built-in tiling, a gaming mode, power profiles, and layout presets.
- No ads, no forced accounts, no telemetry by default. Optional local AI that is off until switched on.
- Done when: a stranger goes from the website to a working desktop with Steam running in under 30 minutes, without opening a terminal; a corrupted update and a hanging kernel both fall back to the old slot automatically; an image below the anti-rollback floor is refused; and a second user can't read the first user's files.

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
| Bootloader (Optimal Boot) | Phase 1 | Limine | Boots Optimal on UEFI across the full CI boot matrix and on 3 real PCs; signed config, measured boot and the tamper tests pass; Secure Boot paths per [design/boot.md](design/boot.md) (owner keys working on 3 PCs, MOK on 1, shim submitted for review); parsers fuzzed. BIOS support is a later, separate item |
| Filesystem | Phase 4 | ext4 as the default | Copy-on-write with authenticated encryption, checksums and snapshots (a user feature; OS rollback uses the A/B image); at least 10^7 crash states per night pass the oracle in [design/storage.md](design/storage.md), plus 1,000 real power cuts on 3 drive models; fsck, scrub and rescue tools ship with format v1; beats matched ext4 on the aged storage benchmark set |
| Compositor | Phase 7 | Borrowed Wayland compositor | Speaks the Wayland protocol so existing apps run; tear-free, variable refresh rate, and input-to-display latency P99 and P99.9 lower than the best of KWin, Mutter and Sway on the reference desktop in the benchmark harness |
| Desktop | Compositor | Borrowed desktop shell | Panel, launcher, notifications, built-in tiling and gaming mode, all within the total OS memory target in [design/benchmarks.md](design/benchmarks.md) |
| Settings app | Desktop | Separate config tools | One app controls everything, with no terminal needed for any common task (a published list of tasks, each tested) |
| Store | Desktop + networking | Borrowed store | One store and one package format, with signed packages, atomic installs and rollback |
| Everyday apps | Desktop | Borrowed apps | Files, terminal, text editor, image viewer, media player, calculator and system monitor |
| Web browser engine | Phase 7 | Borrowed browser | Passes at least 90% of the Web Platform Tests subsets for HTML, CSS and DOM that the major engines all pass, runs a JavaScript engine that passes test262 at 95%, and renders the 50 most-visited sites with no blocking bug |

**Minimum product.** The kernel through Phase 7 with every security gate
passing, the bootloader, and borrowed userland is a complete, usable,
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

- **Real hardware:** booting each milestone from USB on a real PC; the reference machines in [design/benchmarks.md](design/benchmarks.md), set up as dedicated runners for the performance gate (the desktop by Phase 2, the rest by Phase 5); a relay power-cut rig and 3 drive models for the filesystem track.
- **Signing keys:** hardware tokens for the offline root key (2 of 3) before the first public release.
- **Outside audits** at the end of Phases 1, 3 and 4 and before 1.0 (volunteers or funding).
- **Accounts:** shim review submission and Microsoft signing, and repository settings.

Until a dependency arrives, the work that needs it is reported as "not yet
measured" or "not yet done", never claimed.

## What we deliberately don't build
- A compiler (Rust/LLVM), and for now the GPU graphics stack (Mesa) and Windows compatibility (Wine/Proton). Those run through the Linux interface and can become "built by us" tracks later.
