# Optimal OS roadmap

Goal: build the best desktop OS there is, starting with our own kernel written
from scratch: fast, lean, safe, and able to run the software people already use.
The kernel comes first (Phases 0 to 11); the full OS is built on top of it (Phase 12).

This is a serious engineering project, not a toy. That means every phase ends
with a test that proves it is done, every performance claim comes from a
benchmark, and nothing merges unless CI builds it, boots it and passes the tests.

## Pillars, in priority order
1. **Performance.** The main focus. Measured against Linux on the same machine, never guessed.
2. **Security.** An absolute focus. Nobody can read a user's data remotely, and a compromised app can't reach anything it wasn't given. Every security feature ships on by default unless it would cause a major performance or compatibility cost, and that cost has to be measured, not assumed.
3. **Lean.** Nothing runs unless something needs it. Small memory footprint, fast boot.
4. **Runs on almost anything.** Any 64-bit x86 PC first, ARM64 later.
5. **Compatibility.** Runs existing Linux programs unchanged, which is the road to Windows games through Wine/Proton.
6. **User respect.** The kernel does what the user says. No hidden telemetry, ever.
7. **Polish.** Clear error messages, clean crash reports, good docs.
8. **Customisation.** The smallest pillar: boot options, schedulers and modules the user can choose.

## Core architecture decisions

These are decided now because they are expensive to change later.

- **Language: Rust.** Memory-safety bugs cause most serious kernel security holes. Rust removes most of them at compile time with no speed cost.
- **Shape: a "framekernel".** A small core is allowed `unsafe` code: boot, paging, interrupts, context switching and the raw hardware access. Everything else (scheduler policy, filesystems, network stack, drivers) is written in safe Rust on top of that core. This is a memory-safe in-kernel component architecture: it keeps monolithic-kernel speed (no message passing between processes for every operation) and removes most memory-corruption bugs. It is not a hardware isolation boundary the way a microkernel's separate address spaces are: a safe-Rust component still runs with kernel privilege and can still exhaust resources, deadlock, get logic wrong or misprogram a device. Real isolation comes from the driver trust tiers, per-component resource budgets and per-device IOMMU domains in [docs/drivers.md](docs/drivers.md). Every `unsafe` block must carry a comment saying why it is sound.
- **Compatibility through the Linux system-call interface.** Instead of inventing a new interface and waiting for apps to be written for it, Optimal speaks the same system calls as Linux. Linux programs, Mesa, Wayland and eventually Wine/Proton can then run unchanged. This is our own clean implementation of a public interface, so it involves no Linux code.
- **Boot: Limine** (BSD-2-Clause) on UEFI and BIOS. A good bootloader already exists, so we don't write one.
- **First target: x86_64.** ARM64 comes once the core is stable. Code that depends on the architecture lives in its own module from day one.
- **Licence: our own code only, never GPL.** We write clean-room code from specs (Intel/AMD manuals, ACPI, PCI, NVMe, xHCI and USB specs, the Linux man pages for the syscall interface). We never copy or translate Linux, BSD or other kernels' code. Dependencies must be MIT, Apache-2.0, BSD or similar. The repo has no licence file yet, which means all rights are reserved until you pick one.

## Security model

Security is built in from Phase 1, not bolted on at the end.

- **Memory safety:** Rust everywhere, with `unsafe` kept to the small audited core. Memory safety is the floor, not the whole answer: logic bugs, races, hardware misuse and DMA mistakes are handled by the measures below.
- **Driver trust tiers:** core, trusted safe-Rust drivers, capability-restricted drivers, and user-space drivers, each tier with its own IOMMU domain and resource budgets (memory, CPU time, interrupt rate). Restricted drivers talk to the kernel only through small, fuzzed and proved shared-memory rings. See [docs/drivers.md](docs/drivers.md).
- **Formal verification of the security-critical core:** machine-checked proofs (Kani, later Verus) for page tables, the frame allocator, unsafe abstractions, process memory isolation, capability enforcement, syscall entry points, resource accounting and the driver boundary, starting with page tables in Phase 1b. Proofs of individual pieces are not a proof that the whole kernel is secure, and each states its assumptions. See [docs/verification.md](docs/verification.md).
- **Hardware protections on from day one:** no-execute pages, write-xor-execute for all kernel memory, SMEP/SMAP (the kernel can't run or casually read user memory), UMIP, guard pages around every stack, and kernel address randomisation (KASLR). Later: control-flow protection (CET shadow stacks and indirect-branch tracking) where the CPU supports it.
- **Least privilege through capabilities:** a process can only touch objects it holds a handle to. The Linux-compatible interface is built on top of that, so even Linux programs run inside a capability sandbox.
- **Every app sandboxed by default:** its own view of the filesystem, no access to other apps' data, and access to the camera, microphone, screen, location and files only through permission prompts.
- **IOMMU on by default:** devices (Thunderbolt, PCIe, USB4) can only reach memory that was handed to them, which blocks "plug in a device and read RAM" attacks.
- **Full-disk encryption by default:** the key is sealed in the TPM and also needs the user's PIN or passphrase. AES-XTS on the CPU's AES instructions, which costs almost nothing in speed (measured in our benchmarks, not assumed).
- **Verified boot:** firmware checks the bootloader, the bootloader checks the kernel, the kernel checks the system image. Every step is measured into the TPM, so a tampered system can't unlock the disk.
- **Nothing listening by default:** no remote-access services, a default-deny inbound firewall, and an optional per-app outbound network permission.
- **Signed updates only,** with instant rollback.
- **CPU side-channel mitigations** (Spectre and friends) are on by default, picked per CPU model, with their speed cost benchmarked and published.
- **Testing security like everything else:** continuous syscall and driver fuzzing, sanitiser builds, and outside audits of the unsafe core before 1.0.

## How we know it is good

- **CI on every change:** build, `rustfmt`, `clippy` with warnings as errors, and a boot matrix in QEMU: 64 virtual PCs (BIOS and UEFI, old and new chipsets, 128 MiB to 8 GiB of RAM, 1 to 16 cores), each booted 3 times.
- **Nightly soak:** the same matrix booted 40 times per configuration (over 2,500 boots) to catch rare, timing-dependent bugs. Soak tests grow with every phase: hour-long scheduler stress, 24-hour filesystem runs, continuous fuzzing.
- **In-kernel test suite** (from Phase 1): unit tests that run inside the kernel under QEMU, so allocators, page tables and the scheduler are tested in the real environment.
- **Tough but realistic conditions** (from Phase 1): every change also boots on deliberately awkward machines: CPUs from Core 2 to current Xeon/EPYC with security features missing, 5-level paging, odd core topologies, NUMA (including a node with no memory), odd and split memory, no HPET, no ACPI, old and new OVMF builds, a host fighting for CPU, and Secure Boot attacks (unsigned bootloader, wrong key, tampered kernel, tampered config), which must be refused. Fault injection joins as each piece lands: failed allocations, quirky ACPI tables, timer jitter, spurious interrupts, then disk power cuts and IO errors.
- **Public scorecard from the first bootable build:** benchmarks and security features tracked against Linux on identical hardware and workloads, published with every run. Responsiveness is measured as P50/P99/P99.9 latency, frame-time variance and audio dropouts, never as absolute promises. Memory is split into kernel, background services, total OS and app-available, and idle CPU and power have a defined measurement window and reference hardware. The aim is to win on desktop workloads, not every workload. CI flags any change that makes a benchmark more than 3% worse. See [docs/benchmarks.md](docs/benchmarks.md).
- **Formal verification in CI:** every proof target in [docs/verification.md](docs/verification.md) runs on every change once written.
- **Fuzzing** (from Phase 4): system calls, the driver boundary and filesystems are fuzzed continuously to find crashes before users do.
- **Real hardware:** from Phase 1, each milestone is also checked on at least one real PC booted from USB.

## Phases

Each phase ends with a "done when" test. The next phase doesn't start until it passes.

### Phase 0: It boots (now)
- Rust kernel loaded by Limine, writes to the serial port, paints the screen and reports usable memory.
- CI builds the kernel, makes a bootable ISO, boots it in QEMU on BIOS and UEFI, and uploads the ISO.
- Done when: CI is green and the ISO boots on a real PC from a USB stick, showing the blue screen band.

### Phase 1: The core
- CPU setup: GDT, TSS, IDT, and handlers for every CPU exception with a clear crash report (registers and a stack trace).
- Memory: physical frame allocator, our own page tables for the kernel, a kernel heap, and guard pages to catch stack overflows.
- Interrupts and time: local APIC and I/O APIC, an accurate clock (TSC calibrated against HPET or the ACPI timer), a timer interrupt.
- ACPI tables parsed so we can find CPUs, interrupt routing and power controls.
- A framebuffer text console so the kernel can talk without serial.
- Security baseline: no-execute, write-xor-execute kernel mappings, SMEP, SMAP, UMIP, guard pages, and a relocatable kernel so the bootloader can randomise its address (KASLR).
- In-kernel test runner that runs in CI, plus the tough-conditions run and Secure Boot attack tests.
- First proofs: page-table manipulation and the frame allocator (Kani, in CI).
- First scorecard entries: boot time, timer accuracy, kernel memory, and the security features table.
- Done when: every exception type is triggered on purpose in tests and reported correctly, the allocator and page tables pass their tests and proofs, every tough-conditions scenario passes, and the timer keeps accurate time over a 10-minute run.

### Phase 2: Many cores and a scheduler
- Start every CPU core (SMP), with per-core data and safe locking.
- A preemptive scheduler built for desktops: low latency for interactive work first, throughput second. Aware of hybrid CPUs (Intel P/E cores, AMD X3D cache dies) from the start.
- Kernel threads, sleeping, wake-ups and timers.
- Benchmark harness lands here: context switch, wake-up latency and scheduling fairness against Linux.
- Done when: all cores run stress tests for an hour with no crashes or deadlocks, and context-switch and wake-up latency are within 10% of Linux on the same machine (beating it is the later goal).

### Phase 3: User space
- Ring 3, processes with their own address spaces, ELF loading, system calls, signals.
- Virtual memory features programs need: `mmap`, copy-on-write `fork`, demand paging.
- The first part of the Linux system-call interface, enough for statically linked programs.
- The capability system: every process starts with only the handles it is given. Syscall entry hardened against Spectre-style attacks.
- Done when: a statically linked "hello world" and a small test suite of syscall programs, built with the normal Linux toolchain, run unchanged.

### Phase 4: Storage and files
- PCI enumeration, then NVMe and AHCI (SATA) drivers, with the IOMMU locking each device to its own memory.
- Full-disk encryption (AES-XTS) with the key sealed in the TPM plus a PIN or passphrase.
- A virtual filesystem layer, a RAM filesystem, a page cache, and read/write support for an existing disk format (ext4 first, written from the spec).
- Syscall fuzzing starts.
- Filesystem testing: crash-consistency testing (cut power at every write point and check recovery), property-based tests against a reference model, filesystem image fuzzing, recovery verification, and long randomized workloads. A clean test campaign is evidence, not proof, so this runs forever in the nightly soak.
- Done when: the kernel boots from a real NVMe drive, survives a 24-hour file stress test and the crash-consistency suite with no corruption, and file throughput is within 10% of Linux.

### Phase 5: Running real Linux programs
- Dynamic linking, threads (`clone`, futexes), `/proc` and `/sys` basics, pipes, `epoll`, pseudo-terminals.
- Run BusyBox, a shell, then real tools like Python and GCC.
- Done when: the Linux Test Project's core syscall tests pass at an agreed level (target 90%), and a shell plus common command-line tools work.

### Phase 6: Input, USB and networking
- xHCI USB with keyboards, mice and storage. PS/2 as a fallback.
- Network drivers (virtio, Intel and Realtek first) and a TCP/IP stack.
- Default-deny inbound firewall and no listening services.
- Done when: the PC can be used with USB devices only, and can download a large file at full line speed.

### Phase 7: Graphics and a desktop
- Kernel modesetting interface (the DRM/KMS interface programs expect), starting with virtio-gpu and simple framebuffers.
- Run a Wayland compositor and Mesa's software renderer on top.
- Done when: a Wayland desktop session runs with a terminal and a browser.

### Phase 8: Real GPU drivers
- The hardest part of any new kernel. Start with one family (AMD or Intel, whose hardware documentation is public) and work towards accelerated 3D through Mesa.
- Done when: Mesa runs hardware-accelerated OpenGL and Vulkan on one GPU family, and a benchmark game runs within 15% of Linux.

### Phase 9: Games and Windows apps
- Run Steam, Wine and Proton on the Linux syscall layer. Fix what breaks, one game at a time, from a published test list.
- Kernel-level anti-cheat (Valorant, some Call of Duty and EA titles) is written for Windows and won't run here. We say so clearly instead of failing silently.
- Done when: a published list of 20 popular games runs, with frame rates compared against Linux and Windows on the same PC.

### Phase 10: Performance and lean push
- Profile everything. Tune the scheduler, memory management and I/O paths until the desktop benchmark set beats Linux (server-style throughput only has to stay close).
- Power management: CPU sleep states, frequency scaling and suspend/resume for laptops.
- Done when: Optimal beats Linux on most of the desktop benchmark set (input latency, frame pacing, audio, app launch, responsiveness under background load, idle power) on two different real PCs, and laptop battery life is within 10% of Linux.

### Phase 11: Hardening and release
- Security: verified boot end to end, control-flow protection (CET), full-time fuzzing in CI, and an outside audit of the unsafe core.
- ARM64 port.
- Customisation: selectable schedulers and boot options.
- Done when: a month of nightly builds on test machines with no crash.

### Phase 12: The Optimal OS
- Atomic, signed system updates with one-step rollback, built in CI like the kernel.
- One settings app, one app store, one app format, and a GUI for every common task.
- Built-in tiling, a gaming mode, power profiles, and layout presets.
- No ads, no forced accounts, no telemetry by default. Optional local AI that is off until switched on.
- Done when: a stranger goes from the website to a working desktop with Steam running in under 30 minutes, without opening a terminal.

## Linux compatibility milestones

Linux programs depend on far more than syscall numbers: exact semantics,
signals, threading, memory behaviour, `/proc` and `/sys`, device interfaces,
filesystem behaviour and graphics. This is likely the biggest single job in
the kernel, and it takes years of testing against real apps. It is split into
milestones, each with its own app compatibility suite that runs in CI and
whose pass rate goes on the scorecard.

| # | Milestone | Phase | Compatibility suite |
|---|---|---|---|
| 1 | Basic ELF execution (static binaries) | 3 | hello world, static coreutils, our syscall tests |
| 2 | Processes and threads (`fork`, `clone`, futexes, `wait`) | 3 to 5 | LTP process and thread tests, pthread stress tests |
| 3 | Memory and signals (`mmap` semantics, COW, signal delivery and masks) | 3 to 5 | LTP mm and signal tests, glibc and musl test suites |
| 4 | Filesystem semantics, `/proc` and `/sys` | 4 to 5 | LTP fs tests, xfstests subset, BusyBox, Python and GCC test suites |
| 5 | Networking (sockets, `epoll`, netlink basics) | 6 | LTP net tests, curl, OpenSSH, a browser's network tests |
| 6 | Mesa and graphics (DRM/KMS, render nodes, dma-buf) | 7 to 8 | Mesa's own tests, piglit, Vulkan CTS subset |
| 7 | Wayland | 7 | a compositor plus GTK and Qt test apps, a browser |
| 8 | Wine and Proton | 9 | Wine's test suite, the published 20-game list |

## Built by us: our own stack

These pieces will be written by us, from specs, instead of borrowed. Each one
runs as its own track alongside the kernel phases, and starts once the kernel
can support it. Until ours is ready, the borrowed version (where one exists)
keeps things working.

| Piece | Starts after | Replaces | Done when |
|---|---|---|---|
| Bootloader | Phase 1 | Limine | Boots Optimal on UEFI (and BIOS) across the full CI boot matrix and on 3 real PCs, with Secure Boot signing |
| Filesystem | Phase 4 | ext4 as the default | Copy-on-write with checksums and snapshots (instant rollback for updates); survives a 7-day power-cut fuzz test with zero corruption; beats ext4 on our file benchmarks |
| Compositor | Phase 7 | Borrowed Wayland compositor | Speaks the Wayland protocol so existing apps run; tear-free, variable refresh rate, and lower input latency than KWin/Mutter in our tests |
| Desktop | Compositor | Borrowed desktop shell | Panel, launcher, notifications, built-in tiling and gaming mode, all under the lean budget |
| Settings app | Desktop | Separate config tools | One app controls everything, with no terminal needed for any common task |
| Store | Desktop + networking | Borrowed store | One store and one package format, with signed packages, atomic installs and rollback |
| Everyday apps | Desktop | Borrowed apps | Files, terminal, text editor, image viewer, media player, calculator and system monitor |
| Web browser engine | Phase 7 | Borrowed browser | Passes a set share of the Web Platform Tests and Acid3, then runs the 50 most-visited sites correctly |

Honest notes:
- **Bootloader** and **everyday apps** are the most achievable. The bootloader can start soon after Phase 1.
- **Filesystem** is very hard to get right, because a bug loses people's files. It gets the heaviest testing of anything here.
- **Web browser engine** is the biggest single project on this list, bigger than the kernel. HTML, CSS and JavaScript each take years. It is a long-running track, and a borrowed browser ships until ours passes its tests.

## What we deliberately don't build
- A compiler (Rust/LLVM), and for now the GPU graphics stack (Mesa) and Windows compatibility (Wine/Proton). Those run through the Linux interface and can become "built by us" tracks later.
