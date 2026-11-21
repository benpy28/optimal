# Optimal OS roadmap

Goal: build the best desktop OS there is, starting with our own kernel written
from scratch: fast, lean, safe, and able to run the software people already use.
The kernel comes first (Phases 0 to 11); the full OS is built on top of it (Phase 12).

This is a serious engineering project, not a toy. That means every phase ends
with a test that proves it is done, every performance claim comes from a
benchmark, and nothing merges unless CI builds it, boots it and passes the tests.

## Pillars, in priority order
1. **Performance.** The main focus. Measured against Linux on the same machine, never guessed.
2. **Lean.** Nothing runs unless something needs it. Small memory footprint, fast boot.
3. **Runs on almost anything.** Any 64-bit x86 PC first, ARM64 later.
4. **Compatibility.** Runs existing Linux programs unchanged, which is the road to Windows games through Wine/Proton.
5. **User respect.** The kernel does what the user says. No hidden telemetry, ever.
6. **Polish.** Clear error messages, clean crash reports, good docs.
7. **Customisation.** The smallest pillar: boot options, schedulers and modules the user can choose.

## Core architecture decisions

These are decided now because they are expensive to change later.

- **Language: Rust.** Memory-safety bugs cause most serious kernel security holes. Rust removes most of them at compile time with no speed cost.
- **Shape: a "framekernel".** A small core is allowed `unsafe` code: boot, paging, interrupts, context switching and the raw hardware access. Everything else (scheduler policy, filesystems, network stack, drivers) is written in safe Rust on top of that core. This gives monolithic-kernel speed (no message passing between processes for every operation) with most of a microkernel's safety. Every `unsafe` block must carry a comment saying why it is sound.
- **Compatibility through the Linux system-call interface.** Instead of inventing a new interface and waiting for apps to be written for it, Optimal speaks the same system calls as Linux. Linux programs, Mesa, Wayland and eventually Wine/Proton can then run unchanged. This is our own clean implementation of a public interface, so it involves no Linux code.
- **Boot: Limine** (BSD-2-Clause) on UEFI and BIOS. A good bootloader already exists, so we don't write one.
- **First target: x86_64.** ARM64 comes once the core is stable. Code that depends on the architecture lives in its own module from day one.
- **Licence: our own code only, never GPL.** We write clean-room code from specs (Intel/AMD manuals, ACPI, PCI, NVMe, xHCI and USB specs, the Linux man pages for the syscall interface). We never copy or translate Linux, BSD or other kernels' code. Dependencies must be MIT, Apache-2.0, BSD or similar. The repo has no licence file yet, which means all rights are reserved until you pick one.

## How we know it is good

- **CI on every change:** build, `rustfmt`, `clippy` with warnings as errors, and a boot matrix in QEMU: 64 virtual PCs (BIOS and UEFI, old and new chipsets, 128 MiB to 8 GiB of RAM, 1 to 16 cores), each booted 3 times.
- **Nightly soak:** the same matrix booted 40 times per configuration (over 2,500 boots) to catch rare, timing-dependent bugs. Soak tests grow with every phase: hour-long scheduler stress, 24-hour filesystem runs, continuous fuzzing.
- **In-kernel test suite** (from Phase 1): unit tests that run inside the kernel under QEMU, so allocators, page tables and the scheduler are tested in the real environment.
- **Benchmarks from Phase 2 on:** context switch time, syscall round trip, page fault cost, memory allocation, boot time and idle memory. Every result is compared with Linux on the same QEMU setup and, later, the same real PC. CI flags any change that makes a benchmark more than 3% slower.
- **Fuzzing** (from Phase 4): system calls are fuzzed with random inputs to find crashes before users do.
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
- In-kernel test runner that runs in CI.
- Done when: every exception type is triggered on purpose in tests and reported correctly, the allocator and page tables pass their tests, and the timer keeps accurate time over a 10-minute run.

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
- Done when: a statically linked "hello world" and a small test suite of syscall programs, built with the normal Linux toolchain, run unchanged.

### Phase 4: Storage and files
- PCI enumeration, then NVMe and AHCI (SATA) drivers.
- A virtual filesystem layer, a RAM filesystem, a page cache, and read/write support for an existing disk format (ext4 first, written from the spec).
- Syscall fuzzing starts.
- Done when: the kernel boots from a real NVMe drive, survives a 24-hour file stress test with no corruption, and file throughput is within 10% of Linux.

### Phase 5: Running real Linux programs
- Dynamic linking, threads (`clone`, futexes), `/proc` and `/sys` basics, pipes, `epoll`, pseudo-terminals.
- Run BusyBox, a shell, then real tools like Python and GCC.
- Done when: the Linux Test Project's core syscall tests pass at an agreed level (target 90%), and a shell plus common command-line tools work.

### Phase 6: Input, USB and networking
- xHCI USB with keyboards, mice and storage. PS/2 as a fallback.
- Network drivers (virtio, Intel and Realtek first) and a TCP/IP stack.
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
- Profile everything. Tune the scheduler, memory management and I/O paths until the benchmark set beats Linux.
- Power management: CPU sleep states, frequency scaling and suspend/resume for laptops.
- Done when: Optimal beats Linux on most of the benchmark set on two different real PCs, and laptop battery life is within 10% of Linux.

### Phase 11: Hardening and release
- Security: kernel address randomisation, W^X memory, stack protection, fuzzing in CI full time, and an outside review of the unsafe core.
- ARM64 port.
- Customisation: selectable schedulers and boot options.
- Done when: a month of nightly builds on test machines with no crash.

### Phase 12: The Optimal OS
- Atomic, signed system updates with one-step rollback, built in CI like the kernel.
- One settings app, one app store, one app format, and a GUI for every common task.
- Built-in tiling, a gaming mode, power profiles, and layout presets.
- No ads, no forced accounts, no telemetry by default. Optional local AI that is off until switched on.
- Done when: a stranger goes from the website to a working desktop with Steam running in under 30 minutes, without opening a terminal.

## What we deliberately don't build
- A bootloader (Limine), a compiler (Rust/LLVM), or the user-space programs themselves. We run the existing ones through the Linux interface.
