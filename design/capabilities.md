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
  sockets, pipes, devices, IRQ and DMA grants for drivers, and portals
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

A Linux fd is a slot in the capability table. Linux operations map onto
derivation as follows:

| Linux operation | Capability meaning |
|---|---|
| `open`, `openat` | look up a path inside the process's file view (below), check permissions, derive a capability for the file with rights from the open flags |
| `dup`, `dup2`, `dup3`, `fcntl(F_DUPFD)` | copy of the same capability (same rights) |
| `fork`, `clone` without `CLONE_FILES` | child gets a copy of every capability in the parent's table, as Linux copies the fd table |
| `execve` | close-on-exec capabilities are dropped; the new image keeps the app identity (below) |
| `SCM_RIGHTS` over a Unix socket | sender derives a copy for the receiver; the receiver can never get more rights than the sender had |
| `pidfd_getfd` | needs a debug capability on the target process |
| `mmap` of a file or memfd | derives a mapping capability on the file's memory object, rights capped by the fd's rights |
| `fork`'s copy-on-write memory | both processes hold a read capability on the shared, copy-on-write memory object until one writes; a write gives that process its own private object |
| SysV and POSIX shared memory | named shared memory objects, reached through the IPC namespace and the same permission check as files |
| `ptrace`, `process_vm_readv/writev`, `/proc/pid/mem` | need a **debug** capability on the target. Within one app this follows Yama scope 1 exactly: a process may debug its descendants, and any process that named it with `prctl(PR_SET_PTRACER)` (how Crashpad and Breakpad crash reporters work). An app's manifest can ask for "debug own app", which lets any of its processes debug any other process of the same app identity (Wine's `wineserver` needs this). Across apps, only through the debugger portal, which asks the user *before* the attach and hands the debugger a capability; the `ptrace` call itself never prompts |

**Revocation of Linux objects** (what an app sees when a grant is withdrawn,
for example the user revoking an app's access to a folder): the next operation
on the fd fails with `EIO` (`EBADF` once the app closes and the slot is
reused); a revoked file mapping delivers `SIGBUS` on next access; a revoked
socket returns `ECONNRESET`. Programs already handle all of these, because
Linux produces them for removed devices and network file systems.

## App identity

- **App identity** is fixed when an app is launched and inherited across
  `fork` and `execve`. A process can't change its own identity. The kernel
  attests it: any process can learn a peer's app identity from a pidfd
  (`SO_PEERPIDFD` on a Unix socket, then an Optimal `pidfd` query), which is
  race-free, unlike reading `/proc` by pid. Portals, the compositor and the
  D-Bus proxies use this; it's what the "verified identity" on every prompt
  is.
- **Where identities come from:** a signed manifest (the Optimal app format,
  Phase 12); a Flatpak app's ID and its `metadata` permissions (mapped
  below); for an unpackaged program launched from the desktop, an identity
  derived from its install location, with the default profile below; for a
  program run from a terminal, the terminal's identity.
- **Apps that launch apps.** A launcher like Steam can start children under a
  **child identity** (`steam/<game id>`): the child gets its own capability
  set and file view (by default its own data directory and the folders the
  parent declares for it, never the parent's tokens or data), but stays in the
  parent's PID namespace subtree, so the parent can `wait` for it, track it
  with a pidfd and signal it. Flatpak's `flatpak-spawn --sandbox` and
  browsers' renderer sandboxes map onto the same mechanism or onto ordinary
  namespaces and seccomp.
- **The default profile** for unpackaged desktop programs: own data
  directory, no folder grants, network allowed (it holds no user files),
  GPU render node, audio playback, and portals for everything else. It is
  shown in Settings and can be changed per program.

## The file view

Each app has its own mount namespace, built at launch. Its contents are fixed
by a published list (tested against the Phase 5 and Phase 7 app suites), so
apps find everything they expect:

- the system image read-only (`/usr`, and `/bin`, `/lib`, `/lib64`, `/sbin` as
  links into it);
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
  own processes;
- a `/sys` subset: `devices/system/cpu` (online, topology, caches),
  `class/drm` and `dev/char` entries for devices the app was granted, and
  what libdrm, the Vulkan loader and libudev need for those, nothing else;
- the device nodes below.

Paths that aren't in the view don't exist (`ENOENT`).

**Networking.** Apps with network access share the host's network stack,
including loopback, so a browser reaches a development server on
`localhost` and OAuth loopback callbacks work. Apps without network access
get their own network namespace with only a loopback of their own.
Abstract Unix sockets are scoped per app.

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
  | `fusermount3` (AppImage, FUSE) | the broker mounts FUSE inside the calling app's view if it holds the FUSE grant |
  | `flatpak-system-helper` (via polkit) | the store service, which installs Flatpak apps system-wide after an administrator prompt |

- **Denials never hang.** A call the app isn't allowed returns `EACCES`,
  `EPERM`, `ENETUNREACH` or `ENOENT` immediately. Prompts never happen inside
  a system call, because a syscall that blocks on a human would hang or time
  out real programs. Apps ask through portals, which are asynchronous.

## Granting access: portals

Prompts and grants go through **portals**: user-space services in the TCB that
speak the xdg-desktop-portal D-Bus interfaces, so Flatpak-aware apps work
unchanged. The broker and portals land in Phase 5 (with prompts on the text
console until the compositor exists); the compositor draws them from Phase 7.

- **File chooser:** the user picks files or folders in a dialog drawn by the
  compositor; the portal hands the app fds (one use) or adds the folder to
  its view (persistent). The network rule in
  [threat-model.md](threat-model.md) applies to persistent grants.
- **Network:** requested through the network portal or declared at install.
- **Camera, microphone, screen capture, location, notifications, background
  running, real-time scheduling (the rtkit interface):** the portal asks
  once, the answer is remembered and can be changed in Settings, and the app
  gets a capability (often an fd, such as a PipeWire stream).
- **Prompts can't be faked.** Only the compositor draws portal prompts, in a
  layer apps can't draw over, capture or cover, showing the app's attested
  identity. The borrowed compositor used from Phase 7 is patched to do this
  (the patches are kept in the tree and the prompt-spoofing tests run
  against it); the own compositor does it by design.

## The app manifest and Flatpak permissions

A manifest lists: identity (name, publisher key); network (yes or no); device
classes (GPU render node, gamepad, audio, camera); default folders (each shown
at install and removable); D-Bus names it owns and talks to; and extra
features (`debug own app`, `io_uring` off, FUSE, child identities for
launchers). Each is shown at install.

Flatpak permissions map onto the same grants:

| Flatpak permission | Optimal grant |
|---|---|
| `--share=network` | network |
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
- Target 5 covers derivation (never more rights than the parent) and
  revocation (the whole subtree, and the revoked-object behaviours above).
- The hostile-process and exfiltration suites
  ([threat-model.md](threat-model.md)) test every row of the first table, the
  file view list, the shared-channel mediation and the setuid replacements
  from the attacker's side.
