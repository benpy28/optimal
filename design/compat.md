# Linux compatibility

Optimal runs existing Linux programs unchanged by implementing the Linux
user-space ABI from the public documentation (man pages, the ELF and x86-64
psABI specs, kernel uAPI documentation). This file says exactly which ABI,
in what order, and how compatibility is measured so the numbers can't be
gamed. Security rules that shape the ABI are in
[capabilities.md](capabilities.md) and the lockdown table in
[threat-model.md](threat-model.md).

## Decisions

- **Both 64-bit and 32-bit (ia32) programs are supported.** Steam, Wine,
  Proton and many games still ship i386 binaries and libraries, and Wine
  needs 32-bit code segments and `modify_ldt` even in its WoW64 mode. The
  32-bit entry paths (`int 0x80`, `sysenter`, 32-bit `syscall`), the compat
  syscall table, compat struct layouts and compat signal frames are designed
  in **Phase 3**, alongside the 64-bit ones. Every syscall handler and every
  `ioctl` declares its 32-bit layout when it is written; an `ioctl` without
  one fails CI. x32 (the ILP32 ABI on 64-bit) is not supported; it is
  almost unused and Linux distributions have dropped it.
- **Kernel identity.** `uname` reports a fixed Linux release matching the
  feature set actually implemented, in the form `6.6.0-optimal-<build>`.
  The number moves only when the implemented features match a newer Linux.
  Programs that check features (glibc, Mesa, liburing, Steam) then behave
  correctly.
- **Unknown means ENOSYS.** Every syscall number not implemented returns
  `-ENOSYS`, never a kill or `SIGSYS` (seccomp aside). Every unsupported
  `ioctl` returns `-ENOTTY`, every unsupported flag `-EINVAL`, as Linux
  does, so libraries fall back. A CI sweep calls every syscall number from 0
  to the current Linux maximum (and the 32-bit table) and checks this.
- **The thread model is Linux's.** A task is a thread; a thread group is a
  process. Signals, credentials, the fd table and the address space are each
  shared or not according to `clone` flags. This is fixed in Phase 3 even
  though `clone` threads arrive with the rest of milestone 2, so signals are
  never designed process-wide and then retrofitted.
- **glibc is the base libc** of the system image, for binary compatibility
  with third-party programs (symbol versions, NSS). musl programs are
  supported and tested too.
- **io_uring is supported**, because high-performance programs and games'
  asset loaders increasingly use it. Every io_uring operation goes through
  the same checked handler as its system call (same capability checks), it
  is fuzzed like syscalls, and an app's manifest can turn it off. It is not
  a way around the sandbox.
- **eBPF is not available to apps.** Classic BPF is supported for seccomp and
  socket filters. See the lockdown table.
- **No loadable kernel modules** (`init_module` returns `ENOSYS`). Drivers
  are built in or run in tier 3.

## What "a static hello world" really needs (milestone 1 detail)

The ELF loader and first syscalls must provide: static, static-PIE and
dynamic (`PT_INTERP`) ELF loading; the full auxiliary vector (`AT_PHDR`,
`AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_BASE`, `AT_ENTRY`, `AT_RANDOM`,
`AT_HWCAP`, `AT_HWCAP2`, `AT_PLATFORM`, `AT_SECURE`, `AT_MINSIGSTKSZ`,
`AT_SYSINFO_EHDR`, `AT_EXECFN`, `AT_UID`/`GID` and friends); `arch_prctl`
(`ARCH_SET_FS`, `ARCH_GET_FS`, and the shadow stack calls later),
`set_tid_address`, `set_robust_list`, `rseq`, `prlimit64`, `getrandom`,
`brk`, `mmap`, `mprotect`, `munmap`, `uname`, `exit_group`; and signal frames
sized for the CPU's real XSAVE area (AVX-512 frames exceed 2 KiB).

**The vDSO** ships in Phase 3, not later: `clock_gettime`, `gettimeofday`,
`time`, `clock_getres` and `getcpu` without entering the kernel. Without it,
every frame-pacing loop pays a full syscall. "vDSO `clock_gettime` cost
against Linux" is on the benchmark list.

## Milestones and what each needs

Each milestone has a compatibility suite that runs in CI. Its pass rate goes
on the scorecard, measured as described below. The phase column says where
the work lands in the [roadmap](../ROADMAP.md).

| # | Milestone | Phase | Main pieces | Suite |
|---|---|---|---|---|
| 1 | ELF execution | 3 | loader, auxv, vDSO, 64- and 32-bit entry, signal frames | static glibc and static musl binaries (64 and 32 bit), static coreutils, gVisor syscall tests for the implemented calls |
| 2 | Processes and threads | 3 to 5 | `fork`, `clone`, `wait*`, futexes including `FUTEX_LOCK_PI` and `futex_waitv`, `membarrier`, scheduling policies and affinity ([scheduler.md](scheduler.md)), `pidfd`, `prctl` basics | LTP process and thread tests, kselftest futex, glibc nptl tests, pthread stress |
| 3 | Memory and signals | 3 to 5 | `mmap` semantics, COW, `mremap`, `madvise`, `userfaultfd` (later), per-thread signal masks, `tgkill`, `sigaltstack`, `SA_RESTART`, `signalfd` | LTP mm and signal tests, glibc and musl test suites, kselftest mm |
| 4 | Files, `/proc` and `/sys` | 4 to 5 | VFS semantics, durability contract ([storage.md](storage.md)), `inotify`, `fanotify` (read-only uses), `/proc` and `/sys` per the contract below | LTP fs tests, xfstests subset, SQLite crash tests, BusyBox, Python, GCC and git test suites |
| 5 | Desktop IPC | 5 | `AF_UNIX` with `SCM_RIGHTS` and `SCM_CREDENTIALS`, abstract sockets inside the app's network namespace, `memfd_create` with seals, `eventfd`, `timerfd`, `epoll`, pipes, pseudo-terminals | dbus-daemon tests, Wayland's test suite (protocol layer), LTP ipc tests |
| 6 | Containers and sandboxes | 5 | user, mount, PID, network, UTS and IPC namespaces (unprivileged user namespaces included), `pivot_root`, bind mounts, `seccomp` with classic BPF and `RET_ERRNO`/`RET_TRAP`/`RET_USER_NOTIF`, `no_new_privs`, cgroup v2 subset (cpu, memory, pids, io) | `bwrap` running a Steam Runtime container, Chromium's and Firefox's sandbox self-tests, kselftest seccomp and namespaces, Flatpak test suite |
| 7 | Debugging and profiling | 5 | `ptrace` (policy in [capabilities.md](capabilities.md)), `perf_event_open` (own process, user counters), core dumps (local only) | gdb test suite subset, strace's tests, rr's tests where the CPU supports it |
| 8 | Networking | 6 | sockets (TCP, UDP, ICMP for unprivileged ping), `netlink` route and uevent basics, `getaddrinfo` paths | LTP net tests, curl and OpenSSH test suites, a browser's network tests |
| 9 | Input and devices | 6 | evdev (`/dev/input/event*`), uevent netlink and the `/sys/class` and `/sys/dev/char` layout libudev expects, `hidraw`, `uinput`, `usbfs` for approved devices | libinput test suite, SDL gamepad tests, libusb tests |
| 10 | Audio | 6 | ALSA PCM, control and timer interfaces including mmap mode; real-time scheduling for PipeWire through the rtkit portal | alsa-lib tests, PipeWire tests, underrun counts from the benchmark |
| 11 | Graphics | 7 to 8 | DRM/KMS, GEM, render nodes, dma-buf, `sync_file`, timeline syncobj, the per-driver uAPI of the chosen GPU family, DRM master handed out by the seat service | Mesa's tests, piglit, the Vulkan Conformance Test Suite subset (pass rate published), IGT subset |
| 12 | Wayland desktop | 7 | everything above plus the seat service, so a compositor, GTK and Qt apps and a browser run | compositor plus GTK and Qt test apps, a browser's test suite |
| 13 | Wine and Proton | 9 | everything above, 32-bit in full, `modify_ldt`, the futex and fd-heavy patterns Proton uses | Wine's conformance tests, the published game list |

Anti-cheat, precisely: **kernel-level** anti-cheat (Valorant's Vanguard, some
Call of Duty and EA titles) is a Windows kernel driver and won't run; the game
list says so per title. **User-mode** anti-cheat with official Proton support
(Easy Anti-Cheat, BattlEye) is a supported target; it probes `/proc`, `ptrace`
state and kernel identity, so those must look exactly like Linux. Named test
titles are on the game list.

Phase 12's app story also keeps existing formats working: Flatpak (needs
milestone 6), AppImage (needs FUSE) and plain tarballs are compatibility
targets in its done-when.

## `/proc` and `/sys`: a contract, not "basics"

Programs parse these files in exact formats (JVMs read `/proc/self/maps`,
sanitizers read `/proc/self/status`, glibc reads
`/sys/devices/system/cpu/online`). So:

- Every `/proc` and `/sys` file Optimal provides is listed in
  `design/procfs.md` (created with milestone 4) with the Linux release whose
  format it copies, and each has a **golden-output test**: the same program
  on Linux and Optimal, outputs compared after normalising values like pids
  and addresses.
- A file that isn't implemented doesn't exist (`ENOENT`), never a file with
  a made-up format.
- **Privacy.** `/proc` is per app: an app sees only its own processes (the
  equivalent of `hidepid=invisible` plus a per-app PID namespace by default),
  so it can't read other apps' command lines, environments or memory maps.
  Kernel pointers are hidden as in the lockdown table.

## Measuring compatibility honestly

- **Pinned suites.** Each suite is pinned to a version and an explicit test
  list (for LTP: a named `runtest` file at a tagged release). Changing the
  version or list is a reviewed change and resets the trend line.
- **Results are reported by outcome**, never as one blended percentage:
  pass, fail, broken, and "not configured" (LTP's `TCONF`, a test that skips
  because a feature is missing). A `TCONF` counts as a **fail** unless the
  missing feature is in the documented list of features Optimal doesn't
  implement yet, with the milestone that will add it.
- **Differential testing.** A corpus of programs runs on Linux and Optimal
  under a syscall tracer; normalised traces (calls, arguments, results,
  errno, signal delivery) are compared. A difference is a bug or a
  documented, intended divergence (for example a sandbox denial).
- **Status tables.** CI publishes a per-syscall and per-`ioctl` table:
  implemented, partial (which flags), or not yet, for 64 and 32 bit.
- **App suites by phase.** Phase 5: coreutils, Python, GCC, git, SQLite.
  Phase 7: Firefox, Chromium, GTK and Qt apps. Phase 9: the game list, with
  ProtonDB ratings shown for comparison.
- GPL test suites (LTP, xfstests, kselftest) are run as external programs
  only, never linked into or shipped with Optimal.
