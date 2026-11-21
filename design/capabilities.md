# Capabilities, and how Linux semantics sit on top of them

Optimal's security rests on one rule: **a process can only act on a kernel
object through a handle it holds, and the handle says what it may do.**
Linux programs don't know about handles; they open paths, check uids and
inherit file descriptors. This file defines the capability system and then,
precisely, how every Linux way of getting authority maps onto it, so "runs
unchanged" and "sandboxed by default" are both true and both testable.

## Objects and capabilities

- **Kernel objects:** memory objects (anonymous memory, a file's page cache,
  shared memory, memfd), address spaces, threads, files and directories,
  open file descriptions, mounts, sockets, pipes, devices, IRQ and DMA grants for drivers, and portals
  (connections to brokers).
- **A capability** is an entry in a process's capability table: (object,
  rights). Rights are a bit set specific to the object type (read, write,
  execute, map, grant, signal, debug, and so on).
- **Derivation.** A new capability is always derived from one the process
  holds, with equal or fewer rights. Every derivation is recorded in a
  derivation tree (each capability knows its parent and children), as in
  seL4's capability derivation tree.
- **Revocation.** Revoking a capability revokes everything derived from it,
  by walking its subtree. What a revoked handle does next is defined per
  object below.
- **No ambient authority.** There is no "root that can do anything". The
  system service that needs a power holds a capability for exactly that.

## Linux file descriptors are capabilities

Optimal keeps Linux's three layers. An fd is a slot in the process's fd
table; the slot holds a reference to an **open file description** (OFD); the
OFD is a kernel object holding the capability (object, rights) for the file,
socket, pipe or device, the file offset, the status flags (`O_APPEND`,
`O_NONBLOCK`, `O_DIRECT`, `O_ASYNC`) and OFD locks (`F_OFD_SETLK`).
Close-on-exec is per slot, as in Linux. Each OFD is one node in the
derivation tree, a child of the capability it was opened through (a mount in
the file view, a folder grant, a portal's fd). Sharing an fd shares the OFD,
so offsets and flags behave as on Linux: `dup`ed fds and a forked child move
one offset together, and `cmd >>log 2>&1` works.

| Linux operation | Capability meaning |
|---|---|
| `open`, `openat`, `openat2` | look up a path inside the process's file view (below), check permissions, create a new OFD whose capability is derived with rights from the open flags, and put it in a new slot |
| `dup`, `dup2`, `dup3`, `fcntl(F_DUPFD)` | new slot, same OFD |
| `fork`, `clone` without `CLONE_FILES` | the child's table is a copy of the parent's slots, each referencing the same OFD, as Linux copies the fd table |
| `execve` | close-on-exec slots are dropped; the new image keeps the app identity (below) |
| `SCM_RIGHTS` over a Unix socket | the receiver gets a new slot on the sender's OFD, so it can never get more rights than the sender had |
| `pidfd_getfd` | needs a debug capability on the target process; new slot on the target's OFD |
| reopen through `/proc/self/fd/N` or `/proc/<pid>/fd/N` | **intended divergence:** the new OFD's rights are at most those of fd N's OFD (Linux re-checks the file's mode instead, so a read-only fd passed to an app could be reopened for writing). An `O_PATH` OFD records the rights the view allowed at lookup and caps its reopen the same way |
| lookup relative to a directory fd whose OFD was opened by another identity (received by `SCM_RIGHTS`, `pidfd_getfd` or a portal) | **intended divergence:** confined beneath that directory as with `RESOLVE_BENEATH`: `..` above it, absolute paths and symlinks that leave it fail with `EXDEV` |
| `mmap` of a file or memfd | derives a mapping capability on the file's memory object, rights capped by the OFD's rights |
| `fork`'s copy-on-write memory | both processes hold a read capability on the shared, copy-on-write memory object until one writes; a write gives that process its own private object |
| SysV and POSIX shared memory | named shared memory objects, reached through the app's own IPC namespace and the same permission check as files |
| `ptrace`, `process_vm_readv/writev`, `/proc/pid/mem` | need a **debug** capability on the target. Within one app this follows Yama scope 1 exactly: a process may debug its descendants, and any process that named it with `prctl(PR_SET_PTRACER)` (how Crashpad and Breakpad crash reporters work). An app's manifest can ask for "debug own app", which lets any of its processes debug any other process of the same app identity (Wine's `wineserver` needs this). Across apps, only through the debugger portal, which asks the user *before* the attach and hands the debugger a capability; the `ptrace` call itself never prompts |
| `perf_event_open` | only on the calling process or one it holds a debug capability for, and only user-space counters (`exclude_kernel` and `exclude_hv` forced on); anything else is `EACCES` |

Both intended divergences are in the divergence list ([compat.md](compat.md))
and have hostile tests in Phase 5: reopen a passed read-only fd and a passed
`O_PATH` fd for writing (`EACCES`), and escape a passed directory fd with
`..`, an absolute symlink and a relative symlink chain (`EXDEV`).

**Cost.** `fork` copies the slot array and takes one reference per OFD, as
Linux does; nothing is added to the derivation tree per fd. Benchmark:
`fork` + `execve` with 10, 10^4 and 10^5 open fds, within 10% of matched
Linux on the reference desktop by Phase 5.

**Revocation of Linux objects** (what an app sees when a grant is withdrawn,
for example the user revoking an app's access to a folder) acts on OFDs: every
OFD derived from the grant is revoked, so every fd sharing one, across `dup`,
`fork` and `SCM_RIGHTS`, fails together. The next operation on such an fd
fails with `EIO` (`EBADF` once the app closes and the slot is reused); a
revoked file mapping delivers `SIGBUS` on next access; a revoked socket
returns `ECONNRESET`. Programs already handle all of these, because Linux
produces them for removed devices and network file systems.

## App identity

- **App identity** is fixed when an app is launched and inherited across
  `fork` and `execve`. A process can't change its own identity. The kernel
  attests it: any process can learn a peer's app identity from a pidfd
  (`SO_PEERPIDFD` on a Unix socket, then an Optimal `pidfd` query), which is
  race-free, unlike reading `/proc` by pid. Portals, the compositor and the
  D-Bus proxies use this; it's what the "verified identity" on every prompt
  is.
- **Who can set an identity.** Starting a process under an identity other
  than the caller's needs the kernel capability "spawn with identity X",
  held only by the launcher (which includes the Flatpak launcher) and the
  user service manager. Any other process starts children under its own
  identity, or under a child identity (below) whose grants are a subset of
  its own. The hostile-process suite tries to forge an identity by every
  `clone`, `execve` and portal path (Phase 5).
- **Where identities come from:** a signed manifest (the Optimal app format,
  Phase 12); a Flatpak app (below); for an unpackaged program launched from
  the desktop, an identity of **path + SHA-256 of the executable's
  contents**, with the default profile below; for a program run from a
  terminal, the terminal's identity. When an unpackaged program's hash
  changes, its grants reset to the default profile, so replacing a granted
  binary in place inherits nothing.
- **Provenance.** Every file an app identity creates or writes carries a
  kernel-maintained provenance tag naming that identity (stored in an xattr
  namespace no app can set or remove, kept across rename and copy by the
  file manager portal; files on foreign or removable volumes count as
  unknown provenance). An executable whose provenance is an app, not the
  store or the system image, gets **no network on its first desktop launch**
  until the user approves it in a prompt naming the app that wrote it.
- **Protected launch paths.** `~/.config/autostart`,
  `~/.local/share/applications`, `~/.local/bin`, `~/Applications` and every
  executable that holds grants are protected paths
  ([threat-model.md](threat-model.md)): no folder grant includes them.
  Desktop and autostart entries are written only through the launcher
  portal, which asks. `~/.local/bin` is writable by the terminal identity
  alone (so `pip install --user` and `cargo install` work), because what
  runs from it runs as the terminal anyway.
- **Direct channels make one principal.** Identities joined by a direct
  channel (parent and child, D-Bus peers allowed by the proxy policy, a
  shared Unix socket, pipe, or shared memory object) are checked as one
  principal: the files + network rule applies to the union of their grants,
  and a channel that would join a persistent file grant to network access
  without the combined approval is refused (`ECONNREFUSED` on connect,
  `EACCES` on passing the fd or mapping the object). TCB services (portals,
  compositor, PipeWire, the D-Bus proxies) are not principals: talking to
  one joins nobody, but app-to-app messages they relay do. The two-app collusion case is in the exfiltration suite
  (Phase 7).
- **Apps that launch apps.** A launcher like Steam can start children under a
  **child identity** (`steam/<game id>`): the child gets its own capability
  set and file view (by default its own data directory and the folders the
  parent declares for it, never the parent's tokens or data), but stays in a
  PID namespace below the parent's, so the parent can `wait` for it, track it
  with a pidfd and signal it. The parent's manifest declares the **shared
  channels** each child gets, all mediated: read-only paths (the Steam
  library folders), named shared memory objects (the overlay) and named
  socket endpoints (the Steam client IPC socket); nothing else crosses.
  Steam Input's virtual controllers go through the **virtual-gamepad
  portal**, which hands out a `uinput` fd that accepts only gamepad event
  codes (`BTN_GAMEPAD` to `BTN_THUMBR`, `BTN_TRIGGER_HAPPY*`, the `ABS_*`
  axes, `EV_FF`); keyboard and pointer codes fail `UI_SET_*BIT` with
  `EINVAL`. Both are tested in milestone 13. Flatpak's
  `flatpak-spawn --sandbox` and browsers' renderer sandboxes map onto the
  same mechanism or onto ordinary namespaces and seccomp.
- **Flatpak apps** are started by the **Flatpak launcher**, a TCB service
  that reads each app's `metadata` and permissions from store-owned storage
  no app can write, never from the app's own files at run time. User
  (`--user`) installs are installed through the store service, which shows
  the permissions and records the user's approval at install. Each Flatpak
  app gets a per-runtime view: `/usr` is its runtime, `/app` is the app,
  and the rest of the view is as below.
- **The default profile** for unpackaged desktop programs: own data
  directory, no folder grants, outbound network (it holds no user files)
  unless provenance withholds it, GPU render node, audio playback, and
  portals for everything else. It is shown in Settings and can be changed
  per program.

## The file view

Each app has its own mount namespace, built at launch. Its contents are fixed
by a published list, so apps find everything they expect:

- the system image read-only (`/usr`, and `/bin`, `/lib`, `/lib64`, `/sbin` as
  links into it), or for a Flatpak app its runtime as `/usr` and the app as
  `/app`;
- `/etc` as a generated, read-only view: `passwd` and `group` (this user and
  system accounts only), `hosts`, `resolv.conf`, `nsswitch.conf`,
  `machine-id` (a per-app value, so apps can't use it to track the user
  across apps), `localtime`, `os-release`, `ssl/certs`, `fonts`,
  `ld.so.cache`;
- `$HOME` containing only the app's own data directories
  (`~/.config/<app>`, `~/.local/share/<app>`, `~/.cache/<app>` and the XDG
  equivalents) plus any granted folders;
- a private `/tmp` and `/var/tmp`;
- `$XDG_RUNTIME_DIR` with the Wayland socket (through the app's security
  context), the PipeWire socket (with the app's permissions), the session
  bus (through the app's D-Bus proxy) and the portal sockets;
- `/proc` per the contract in [compat.md](compat.md), showing only the app's
  own PID namespace and those below it;
- a `/sys` subset: `devices/system/cpu` (online, topology, caches),
  `class/drm` and `dev/char` entries for devices the app was granted, and
  what libdrm, the Vulkan loader and libudev need for those, nothing else;
- the device nodes below.

The list grows with the compatibility milestone that needs each entry, and
each addition lands in that milestone's gating phase:

| Milestone | Added to the view |
|---|---|
| 3 (Phase 5) | `/sys/kernel/mm/transparent_hugepage` (read-only) |
| 4 (Phase 5) | `/var` with `lib/dbus/machine-id` (the per-app value), `cache` and `lib` private to the app |
| 6 (Phase 5) | the app's own cgroup: `/proc/self/cgroup`, its subtree of `/sys/fs/cgroup`; `/etc/subuid` and `/etc/subgid` with the user's allocated range |
| 8 (Phase 6) | `/etc/services`, `/etc/protocols`, `/etc/gai.conf` |
| 9 (Phase 6) | `/sys/class/input` and `/sys/class/hidraw` entries and `/run/udev/data` records for granted devices only |
| 10 (Phase 6) | `/sys/class/sound` and `/proc/asound` for granted audio devices |

Paths that aren't in the view don't exist (`ENOENT`). The differential
tracer ([compat.md](compat.md)) flags every call that gets `ENOENT` on
Optimal and succeeds on Linux; each is either added to the list or entered
as an intended divergence.

**Mount namespace rules.** These keep a view from being widened from inside:

- Hidden paths are **absent**: the view is built from the listed entries
  only, never by mounting over what should be hidden. Where a mount must
  cover something (a granted folder bind-mounted inside a read-only tree),
  it is locked, as Linux locks mounts inherited into an unprivileged user
  namespace: it can't be unmounted, moved or have its flags relaxed.
- **Mounts are capabilities.** Each mount in a view is a capability on its
  root directory, a child of the grant that put it there. `CLONE_NEWNS`,
  `unshare` and bind mounts derive copies of it, so revoking a folder grant
  revokes the mount in every namespace copied from the view.
- **`..` stops at a bind mount's root**, so a granted `~/Documents` never
  leads to `~`.

Hostile tests (Phase 5): unmount or move a locked mount, reach a hidden
path through `/proc/<pid>/root`, `..` out of a granted folder, and use a
`CLONE_NEWNS` copy after the grant is revoked. All fail.

**Networking** is a per-identity egress filter in the network stack, not a
separate namespace, so a grant or revocation reaches live sockets:

- The network grant is **outbound only**. Every non-loopback `connect`,
  `sendto` and `sendmsg` checks the sender's identity; an identity without
  the grant gets `ENETUNREACH` immediately, and revoking the grant resets
  its live sockets (`ECONNRESET`).
- **Inbound** on any non-loopback address (`listen`, or receiving on an
  unconnected UDP socket) needs a separate **per-port inbound grant**,
  declared in the manifest or requested through the network portal; the
  default-deny firewall opens only that port, for that identity.
- **Loopback** is shared, but a connection to a loopback port whose listener
  belongs to another identity needs a per-port grant too (`ECONNREFUSED`
  without it). A browser reaching a development server started from the
  terminal needs that grant; the terminal shows a one-click request for it.
  OAuth loopback callbacks work unchanged: when an app asks the open-URI
  portal to open `http://127.0.0.1:<port>/…` or `http://localhost:<port>/…`
  and that port's listener is the requesting app, the portal grants the
  browser that port for as long as the listener lives.
- Each app has its own IPC namespace (SysV IPC, POSIX message queues) and its
  own `/dev/shm`; **abstract Unix sockets are scoped per app**.

Tests (Phase 6): revoke the network grant under a live TCP connection; an
app without an inbound grant listens on every port and is unreachable from
another machine; a cross-identity loopback connection without the grant is
refused; the OAuth callback flow of a named app works.

## Unix permissions, uids and Linux capabilities

- **Unix permissions still apply, inside the view.** Every file check is the
  normal Linux discretionary check (uid, gid, mode, ACLs) and then the
  capability check. Both must pass.
- **uids.** Each human user has a uid; each app runs under that user's uid
  inside its own view. There is a uid 0 for the system's own services, but
  it carries no special power over the kernel: being uid 0 only matters for
  Unix permission checks on files in a view, and no app runs as uid 0.
- **Linux capabilities (`CAP_*`).** Programs that check `capget` (bwrap,
  Chromium, Flatpak) see the capabilities Linux would give them, and those
  capabilities work, but **only over objects owned by the user namespace
  they're in**: `CAP_SYS_ADMIN` inside an unprivileged user namespace lets a
  process mount inside that namespace's mount namespace, set its hostname,
  and so on, exactly as Linux scopes them, and never grants anything outside
  it. Outside any user namespace an app holds no `CAP_*` at all. File
  capabilities (`security.capability` xattrs) are ignored, like setuid.
- **setuid binaries.** The setuid bit has no effect (like `nosuid`
  everywhere). Each tool that depended on it has a named replacement, each
  with a test:

  | Tool | Replacement |
  |---|---|
  | `sudo`, `pkexec` | the administrator-action portal, which asks the user |
  | `passwd`, `chsh` | the accounts portal |
  | `ping` | unprivileged ICMP sockets |
  | `bwrap` setuid mode | unprivileged user namespaces |
  | `newuidmap`, `newgidmap` (rootless containers) | the broker writes the uid map for ranges allocated to the user |
  | `fusermount3` (FUSE) | the broker mounts FUSE inside the calling app's view if it holds the FUSE grant |
  | AppImage's `fusermount3` mount | the broker mounts the AppImage's embedded image itself, read-only, `nosuid` and `nodev`, inside the launching app's view, parsing it in a tier-3 service |
  | `flatpak-system-helper` (via polkit) | the store service, which installs Flatpak apps system-wide after an administrator prompt |

- **Denials never hang.** A call the app isn't allowed returns `EACCES`,
  `EPERM`, `ENETUNREACH` or `ENOENT` immediately. Prompts never happen inside
  a system call, because a syscall that blocks on a human would hang or time
  out real programs. Apps ask through portals, which are asynchronous.

## Granting access: portals

Prompts and grants go through **portals**: user-space services in the TCB that
speak the xdg-desktop-portal D-Bus interfaces, so Flatpak-aware apps work
unchanged. The broker and portals land in Phase 5. In Phase 5 prompts are
shown on a **dedicated virtual terminal** owned by the broker, reached with
the **secure-attention key** (Ctrl+Alt+Delete, consumed by the kernel's
input path in every phase and never delivered to any app); no app can
write to, switch to or read input from that terminal. The compositor draws
prompts from Phase 7; the secure-attention key still opens the broker's
terminal.

- **File chooser:** the user picks files or folders in a dialog drawn by the
  compositor; the portal hands the app fds (one use) or adds the folder to
  its view (persistent). The network rule in
  [threat-model.md](threat-model.md) applies to persistent grants.
- **Network:** outbound access, and each inbound port, is requested through
  the network portal or declared at install.
- **Camera, microphone, screen capture, location, notifications, background
  running, real-time scheduling (the rtkit interface):** the portal asks
  once, the answer is remembered and can be changed in Settings, and the app
  gets a capability (often an fd, such as a PipeWire stream).
- **Prompts can't be faked.** Only the broker's terminal (Phase 5) and the
  compositor (from Phase 7) show portal prompts, the compositor in a layer
  apps can't draw over, capture or cover, both showing the app's attested
  identity. The borrowed compositor used from Phase 7 is patched to do this
  (the patches are kept in the tree and the prompt-spoofing tests run
  against it); the own compositor does it by design. Phase 5 spoofing test:
  a hostile app draws a copy of a prompt on its own terminal and tries to
  reach the broker's; the secure-attention key always lands on the real
  prompt, and nothing typed into the copy grants anything.

## The app manifest and Flatpak permissions

A manifest lists: identity (name, publisher key); network (outbound yes or
no, and inbound ports); device
classes (GPU render node, gamepad, audio, camera); default folders (each shown
at install and removable); D-Bus names it owns and talks to; and extra
features (`debug own app`, `io_uring` off, FUSE, child identities and their
shared channels for launchers). Each is shown at install.

Flatpak permissions map onto the same grants:

| Flatpak permission | Optimal grant |
|---|---|
| `--share=network` | outbound network |
| `--filesystem=home` or `host` | home minus protected paths, persistent, so it needs the combined files + network approval if the app also has network |
| `--filesystem=xdg-documents` and similar | that folder, persistent |
| `--device=dri` | GPU render node |
| `--device=all` | GPU, gamepads, approved `hidraw`; never keyboards, `uinput` or block devices |
| `--socket=wayland`, `pulseaudio`, `x11` | Wayland security context, PipeWire playback (recording needs the portal), own XWayland server |
| `--talk-name`, `--own-name` | D-Bus proxy policy |
| `--socket=session-bus`, `system-bus` (unfiltered) | refused; the app runs with the filtered proxy and the user is told |

## Device nodes

`/dev` inside an app's view contains only what the manifest allows: the basics
every program needs (`null`, `zero`, `full`, `random`, `urandom`, `tty`,
`ptmx`, `pts/`, `shm/`, `fd/`), plus granted classes such as `dri/renderD*`,
`snd/`, gamepads under `input/` (only gamepads, never keyboards unless the app
is the compositor or input service), `hidraw*` for specific devices the user
approved, and `fuse` for apps allowed to mount FUSE file systems inside their
view.

## Proof and test obligations

- [verification.md](verification.md) target 4 is stated over memory
  objects: a user mapping in process A reaches a frame only if A holds a
  capability for the memory object that owns it. The sharing paths it covers
  are a **closed list** in `design/proofs.toml`: copy-on-write after `fork`,
  shared file mappings, SysV and POSIX shared memory, memfd, `SCM_RIGHTS`
  passing, `ptrace` and `process_vm_*`, io_uring registered buffers,
  dma-buf, and GPU buffer objects. Each phase that adds a path extends the
  proof in its own done-when; a path that isn't on the list doesn't exist.
- Target 5 covers derivation (never more rights than the parent),
  revocation (the whole subtree, and the revoked-object behaviours above),
  and OFDs and mounts: an fd or mount never carries more rights than the
  capability it was derived from, and revoking that capability reaches every
  fd sharing the OFD and every copied mount.
- The hostile-process and exfiltration suites
  ([threat-model.md](threat-model.md)) test every row of the first table, the
  file view list, the mount namespace rules, the identity rules, the
  shared-channel mediation and the setuid replacements from the attacker's
  side. Each shared channel is gated in the phase it lands: D-Bus and kernel
  keyrings in Phase 5, PipeWire in Phase 6, Wayland, XWayland and the
  clipboard in Phase 7.
