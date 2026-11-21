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
- **Kernel identity.** `uname` reports the newest Linux release whose
  feature checklist Optimal fully implements, as `<x.y>.0-optimal-<build>`.
  Each Linux release we might report has a checklist in `design/kernel-versions.md`
  (created in Phase 3): the syscalls, flags and interfaces glibc, Mesa,
  liburing, Steam and systemd-style tools probe for. Items that are
  intended divergences (the list below) are excluded from every checklist,
  so a version stays reportable. CI runs the probes and fails if the
  reported version's checklist isn't fully met. Before any
  checklist above glibc's minimum is met, Optimal reports `3.2.0-optimal`
  (glibc's minimum supported kernel), and programs fall back through `ENOSYS`.
- **Unknown means ENOSYS.** Every syscall number not implemented returns
  `-ENOSYS`, never a kill or `SIGSYS` (seccomp aside). Every unsupported
  `ioctl` returns `-ENOTTY`. Unsupported flags behave exactly as on Linux
  for each call: `open` and `openat` ignore unknown flag bits, `openat2`
  rejects them with `-EINVAL`, and calls that validate their flags
  (`mmap`, `clone3`, `renameat2`, `statx` masks and so on) return the
  errno Linux returns, so libraries fall back. A CI sweep calls every
  syscall number from 0 to the current Linux maximum (and the 32-bit
  table) and checks this, and the differential tracer checks the flag
  behaviour call by call.
- **The thread model is Linux's.** A task is a thread; a thread group is a
  process. Signals, credentials, the fd table and the address space are each
  shared or not according to `clone` flags. This is fixed in Phase 3 even
  though `clone` threads arrive with the rest of milestone 2, so signals are
  never designed process-wide and then retrofitted.
- **glibc is the base libc** of the system image, for binary compatibility
  with third-party programs (symbol versions, NSS). musl programs are
  supported and tested too. glibc is LGPL; `libstdc++` and `libgcc_s` are GPL
  with the GCC Runtime Library Exception. Almost every third-party C and C++
  binary needs them, so they ship as unmodified shared libraries (allowed by
  the licence rules in the roadmap).
- **The base system's shell and tools are permissively licensed:** `/bin/sh`
  is dash (BSD), the core utilities are uutils (MIT), and other base tools
  are chosen the same way. bash and GNU tools aren't in the OS image (they
  are GPL), but users can install them from the store like any app. The
  Phase 5 suite runs the most common shell scripts (build systems, installers,
  `configure` scripts) against dash and uutils, and reports what breaks.
- **Desktop plumbing is ours or permissive:** the seat service implements the
  `org.freedesktop.login1` Session and Seat interfaces that compositors use
  (`TakeDevice`, `PauseDevice`) and the libseat protocol; device events come
  from our own udev-compatible daemon (the `libudev` ABI and the
  `/run/udev` database layout); the user service manager is ours and
  implements D-Bus activation and the `systemd --user` unit subset that
  PipeWire and the portals need; the D-Bus broker is dbus-broker
  (Apache-2.0). Each has a conformance test in milestones 9 and 12.
- **io_uring is supported** (milestone 5, Phase 5), because high-performance
  programs and game asset loaders use it. Every io_uring operation goes
  through the same checked handler as its system call (same capability
  checks, same network rule), it is fuzzed like syscalls, and an app's
  manifest can turn it off. It is not a way around the sandbox.
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
on the scorecard, measured as described below. The phase column is the one
**gating phase** whose done-when requires the suite's target in the
[roadmap](../ROADMAP.md); work may start earlier where the row says so.

| # | Milestone | Phase | Main pieces | Suite |
|---|---|---|---|---|
| 1 | ELF execution | 3 | loader, auxv, vDSO, 64- and 32-bit entry, signal frames | static glibc and static musl binaries (64 and 32 bit), static coreutils, the single-threaded subset of gVisor's syscall tests for the implemented calls (multi-threaded tests join milestone 2) |
| 2 | Processes and threads | 5 (starts in 3) | `fork`, `clone`, `wait*`, futexes including `FUTEX_LOCK_PI` and `futex_waitv`, `membarrier`, scheduling policies and affinity ([scheduler.md](scheduler.md)), `pidfd`, `prctl` basics | LTP process and thread tests, kselftest futex, glibc nptl tests, pthread stress |
| 3 | Memory and signals | 5 (starts in 3) | `mmap` semantics, COW, `mremap`, `madvise`, `userfaultfd` (later), per-thread signal masks, `tgkill`, `sigaltstack`, `SA_RESTART`, `signalfd` | LTP mm and signal tests, glibc and musl test suites, kselftest mm |
| 4 | Files, `/proc` and `/sys` | 5 (starts in 4a) | VFS semantics, durability contract ([storage.md](storage.md)), `inotify`, `fanotify` (read-only uses), `/proc` and `/sys` per the contract below, per-app private homes ([capabilities.md](capabilities.md)) | LTP fs tests, xfstests subset, SQLite crash tests, BusyBox, Python, GCC and git test suites |
| 5 | Desktop IPC and async I/O | 5 | io_uring, `AF_UNIX` with `SCM_RIGHTS` and `SCM_CREDENTIALS`, abstract sockets scoped per app, `memfd_create` with seals, `eventfd`, `timerfd`, `epoll`, pipes, pseudo-terminals | dbus-daemon tests, Wayland's test suite (protocol layer), LTP ipc tests |
| 6 | Containers and sandboxes | 5 | user, mount, PID, network, UTS and IPC namespaces (unprivileged user namespaces included), `pivot_root`, bind mounts, `seccomp` with classic BPF and `RET_ERRNO`/`RET_TRAP`/`RET_USER_NOTIF`, `no_new_privs`, cgroup v2 subset (cpu, memory, pids, io), the FUSE protocol (`/dev/fuse`, the broker's `fusermount3` replacement, the document portal), `/dev/net/tun` per user namespace, the Flatpak launcher ([capabilities.md](capabilities.md)) | `bwrap` running a Steam Runtime container, Chromium's and Firefox's sandbox self-tests, kselftest seccomp and namespaces, libfuse's tests and the FUSE protocol fuzzed with 10^8 executions, rootless podman (`podman run` with pasta and with slirp4netns networking), the Flatpak launcher's own conformance tests plus upstream Flatpak's `flatpak run` tests |
| 7 | Debugging and profiling | 5 | `ptrace` (policy in [capabilities.md](capabilities.md)), `perf_event_open` (own process or one held with a debug capability, user-space counters only), core dumps (local only) | gdb test suite subset, strace's tests, rr's tests where the CPU supports it |
| 8 | Networking | 6 | sockets (TCP, UDP, ICMP for unprivileged ping), `netlink` route and uevent basics, `getaddrinfo` paths | LTP net tests, curl and OpenSSH test suites, a browser's network tests |
| 9 | Input and devices | 6 | evdev (`/dev/input/event*`), uevent netlink and the `/sys/class` and `/sys/dev/char` layout libudev expects, `hidraw` and `usbfs` for approved devices, `uinput` (input service and compositor; gamepad codes only through the virtual-gamepad portal; keyboard and pointer emulation through the compositor's virtual keyboard and pointer) | libinput test suite, SDL gamepad tests, libusb tests |
| 10 | Audio | 6 | ALSA PCM, control and timer interfaces including mmap mode; real-time scheduling for PipeWire through the rtkit portal | alsa-lib tests, PipeWire tests, underrun counts from the benchmark |
| 11 | Graphics | 8 (starts in 7 with virtio-gpu) | DRM/KMS, GEM, render nodes, dma-buf, `sync_file`, timeline syncobj, the per-driver uAPI of the chosen GPU family, DRM master handed out by the seat service | Mesa's tests, piglit, the Vulkan Conformance Test Suite subset (pass rate published), IGT subset |
| 12 | Wayland desktop | 7 | milestones 1 to 10, milestone 11's Phase 7 part (KMS, GEM, dma-buf and sync objects on virtio-gpu and simple framebuffers) and the seat service, so a compositor, GTK and Qt apps and a browser run | compositor plus GTK and Qt test apps, a browser's test suite |
| 13 | Wine and Proton | 9 | everything above, 32-bit in full, Steam's child identities with their declared shared channels and the virtual-gamepad portal ([capabilities.md](capabilities.md)), `modify_ldt`, `/dev/ntsync` (the NT synchronisation driver current Proton uses), `RLIMIT_NOFILE` soft 1024 and hard 524,288 by default (soft stays at 1024 so `select()` users keep working; Proton raises its own soft limit for esync), the futex and fd-heavy patterns Proton uses | Wine's conformance tests, the published game list |

Anti-cheat, precisely: **kernel-level** anti-cheat (Valorant's Vanguard, some
Call of Duty and EA titles) is a Windows kernel driver and won't run; the game
list says so per title. **User-mode** anti-cheat with official Proton support
(Easy Anti-Cheat, BattlEye) is a supported target. Each named test title on
the game list has a **probe list**: every `/proc`, `/sys`, `ptrace`-state,
`uname` and other call its anti-cheat makes, recorded by the differential
tracer on Linux and pinned. On Optimal each probe must return the same
normalised result, or be an intended divergence the title is shown to
tolerate; the probe lists are part of milestone 13's suite (Phase 9).

Phase 12's app story also keeps existing formats working: Flatpak (needs
milestone 6, started by the Flatpak launcher built in Phase 5), AppImage
(mounted read-only by the broker) and plain tarballs are compatibility
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
- **Privacy.** Each app runs in its own PID namespace, whose PID 1 is a
  minimal init (it reaps orphans and forwards termination signals, like
  bwrap's), and `/proc` shows only processes in the viewer's PID namespace
  and the namespaces below it. So an app can't see or read other apps'
  command lines, environments or memory maps. System monitors and the
  terminal's `ps`, `top` and `kill` reach other apps' processes only through
  the **process portal**, which lists the user's processes with their
  attested identities and signals one, with the user's grant. Kernel
  pointers are hidden as in the lockdown table.

## Measuring compatibility honestly

- **Pinned suites.** Each suite is pinned to a version and an explicit test
  list (for LTP: a named `runtest` file at a tagged release). The list for
  a phase is frozen when that phase starts; removing a test needs the
  owner's sign-off, recorded in the change, and a version change resets
  the trend line.
- **The LTP harness.** LTP runs under a dedicated `ltp` test identity with
  the terminal's profile (home minus protected paths, outbound network for
  the net tests), as uid 0 inside an unprivileged user namespace that maps
  0 to the test user's uid and the rest to the user's allocated subuid and
  subgid range, so tests that check for root run; it holds the `CAP_*`
  that namespace gives and no grant beyond that profile. The harness spec
  is checked in beside the pinned list.
- **Results are reported by outcome**, never as one blended percentage:
  pass, fail, broken, and "not configured" (LTP's `TCONF`, a test that skips
  because a feature is missing). Pass rates are computed over the **full
  pinned list**. **The skip rule**, the same in the roadmap: every skipped
  test (`TCONF` or any test that doesn't run) counts as a failure; a test on
  the phase's excused list is shown separately and still counts against
  the rate.
- **Targets are fixed now.** Every suite must reach a 90% pass rate, and
  100% of its tests that touch security (capabilities, identity, namespaces,
  mounts, seccomp, `ptrace`, `perf_event_open`), by the end of its gating
  phase. Every security test has a checked-in **expectation file** giving
  its exact expected output on Optimal: Linux's output, or, where an
  intended divergence applies (setuid ignored, no `CAP_*` outside a user
  namespace, file capabilities ignored, loop mounts only through the
  tier-3 service, the `/proc/<pid>/fd` reopen cap), the divergent output.
  Matching it exactly counts as a pass; any other output is a fail. Security tests can never be on an excused list. The
  excused list for a phase is frozen when that phase starts and can only
  shrink; adding to it needs the owner's sign-off, recorded in the change.
- **Differential testing.** A corpus of programs runs on Linux and Optimal
  under a syscall tracer; normalised traces (calls, arguments, results,
  errno, signal delivery) are compared. A difference is a bug or an intended
  divergence (for example a sandbox denial); each intended divergence is
  listed with the owner's sign-off, and their count is on the scorecard.
  Every `ENOENT` on Optimal where Linux succeeded is flagged for the file
  view list ([capabilities.md](capabilities.md)). The intended-divergence
  list includes the
  `/proc/<pid>/fd` reopen cap, the `EXDEV` confinement of directory fds
  from another identity, `TIOCSTI` returning `EPERM` and `TIOCLINUX`
  absent, wildcard sockets receiving only loopback without an inbound
  grant, the per-app private home, and the toolkits whose file dialogs
  can't be forced onto the portal (Wine, Java's Swing and JavaFX, Blender),
  each with the user-facing outcome stated in capabilities.md.
- **Status tables.** CI publishes a per-syscall and per-`ioctl` table:
  implemented, partial (which flags), or not yet, for 64 and 32 bit.
- **App suites by phase.** Phase 5: coreutils, Python, GCC, git, SQLite.
  Phase 7: Firefox, Chromium, GTK and Qt apps. Phase 9: the game list, with
  ProtonDB ratings shown for comparison.
- GPL test suites (LTP, xfstests, kselftest) are run as external programs
  only, never linked into or shipped with Optimal.
