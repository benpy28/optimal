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
- **Where identities come from:** a signed manifest (the Optimal app
  format; the format and the launcher's manifest reader land in Phase 5);
  a Flatpak app (below); for an unpackaged program launched from the
  desktop, an identity of **path + SHA-256 of its launch closure**; for a
  program run from a terminal, the terminal's identity. The launch closure
  is everything that decides what the program does:
  - a script started through an interpreter (`python3 app.py`, `java -jar
    app.jar`, `wine foo.exe`, a `#!` script): the interpreter plus the
    script's (or jar's or `.exe`'s) hash, plus the full argv for `-c` style
    calls (`sh -c`, `python3 -c`), so two Python apps are two identities;
  - an ELF program: the executable and its `PT_INTERP`, plus a **closure
    record** stored with its grants: the SHA-256 of every file outside the
    system image that it maps with `PROT_EXEC` (`$ORIGIN` libraries,
    `LD_LIBRARY_PATH` entries, `LD_PRELOAD`, plugins), collected on its
    first launch. Later, an executable mapping of a non-image file whose
    hash isn't in the record succeeds only if the file's provenance is the
    app's own identity (it is then added to the record); otherwise it fails
    with `EACCES`;
  - an AppImage: the AppImage file itself.
- **Updates.** The whole install directory of a granted unpackaged app (the
  directory holding its executable, for example `~/opt/firefox/`) is a
  protected path. When the closure's hash changes, grants and the private
  home (below) carry over only if every changed file's provenance is the
  app's own identity (Firefox, Discord and Steam updating themselves);
  any other writer makes it a new identity with the default profile and an
  empty home, so replacing a granted binary or a library beside it
  inherits nothing.
- **Provenance.** Every file an app identity creates or writes carries a
  kernel-maintained provenance tag naming that identity (stored in an xattr
  namespace no app can set or remove, kept across rename and copy by the
  file manager portal; files on foreign or removable volumes count as
  unknown provenance). An executable whose provenance is an app, not the
  store or the system image, gets **no network on its first desktop launch**
  until the user approves it in a prompt naming the app that wrote it. The
  user service manager and D-Bus activation apply the same rule to every
  unit and service they start. A terminal-identity process that executes a
  file whose provenance is another app identity gets `EACCES` until the
  user approves that file in a prompt naming the writer. "Executes" means
  `execve` (a `#!` script included), an executable mapping, and
  `execveat` with `AT_EXECVE_CHECK`, the Linux 6.14 call interpreters use
  before running a script; files read only as data (a Makefile, `.envrc`,
  npm scripts) are covered instead by the writable-grant rule under
  direct channels below.
- **Protected launch paths.** `~/.config/autostart`,
  `~/.local/share/applications`, `~/.local/share/dbus-1/services`,
  `~/.config/systemd/user`, `~/.config/environment.d`,
  `~/.config/mimeapps.list`, `~/.local/bin`, `~/Applications`, every
  granted app's install directory and every private home are protected
  paths ([threat-model.md](threat-model.md)): no folder grant includes
  them. Desktop, autostart, D-Bus service, unit, environment and
  `mimeapps.list` entries are written only through the launcher portal,
  which asks. `~/.local/bin` is writable by the terminal identity alone (so
  `pip install --user` and `cargo install` work), because what runs from
  it runs as the terminal anyway.
- **Terminals.** `TIOCSTI` returns `EPERM` (as on Linux 6.2 and later with
  `dev.tty.legacy_tiocsti=0`) and `TIOCLINUX` is absent. The launcher gives
  a process a fresh pty whenever it starts a different identity from a
  terminal (`flatpak run`, `xdg-open`), and terminals ignore OSC 52
  clipboard writes and title and report sequences written by a foreign
  identity. These are lockdown rows with tests in Phase 5
  ([threat-model.md](threat-model.md)).
- **Direct channels make one principal.** Identities joined by a direct
  channel are checked as one principal: the files + network rule applies
  to the union of the grants of every identity in the same **component**,
  and a channel that would give a component a persistent file grant
  together with network access without the combined approval is refused
  (`ECONNREFUSED` on connect, `EACCES` on passing the fd or mapping the
  object). The kernel keeps a union-find of identities per session: every
  direct channel ever opened in the session joins its two components, a
  new channel is checked against the union of both components, and
  components split only at logout, so A (files) to B (nothing) to C
  (network) is refused at the second join. The direct channels are a
  closed list, checked in as `design/channels.toml` and tested row by row:
  parent and child, D-Bus peers allowed by the proxy policy, a shared Unix
  socket, pipe, pty or shared memory object, a cross-identity loopback
  connection (the OAuth grant below included), app-to-app messages a TCB
  service relays, a folder one identity can write and another can read,
  and a writable grant inside the terminal's home, which joins the app to
  the terminal (its prompt says "can run code as Terminal"). TCB services
  (portals, compositor, PipeWire, the D-Bus proxies, the resolver, the
  document portal) are not principals: talking to one joins nobody.
  Covert channels (CPU and cache timing, `statfs` free space, `flock` on
  shared read-only files) are out of scope; their measured bandwidth is
  published on the scorecard. The exfiltration suite (Phase 7) covers two
  colluding apps, a three-app chain, a loopback-grant relay and every row
  of `channels.toml`, each as its own row.
- **Apps that launch apps.** A launcher like Steam can start children under a
  **child identity** (`steam/<game id>`): the child gets its own capability
  set and file view (by default its own private home and the folders the
  parent declares for it, never the parent's tokens or data), but stays in a
  PID namespace below the parent's, so the parent can `wait` for it, track it
  with a pidfd and signal it. The parent's manifest declares the **shared
  channels** each child gets, all mediated: read-only paths (the Steam
  library folders), named shared memory objects (the overlay) and named
  socket endpoints (the Steam client IPC socket); nothing else crosses.
  Steam ships unpackaged, so the system image carries a **signed Steam
  manifest** for the official Steam path and closure (and a mapping of
  Flatpak Steam's metadata onto the same grants), written as a Phase 9
  input. Steam Input's virtual controllers go through the
  **virtual-gamepad portal**, which hands out a `uinput` fd that accepts
  only gamepad event codes (`BTN_GAMEPAD` to `BTN_THUMBR`,
  `BTN_TRIGGER_HAPPY*`, the `ABS_*` axes, `EV_FF`); keyboard and pointer
  codes fail `UI_SET_*BIT` with `EINVAL`. Its keyboard and mouse emulation
  (desktop mode, mapped keys) goes through a **virtual keyboard and pointer**
  the compositor provides behind its own grant: events reach only the
  surface that has focus, never the prompt layer, and never count as the
  secure-attention key or a prompt answer. All three are tested in
  milestone 13 (Phase 9). Flatpak's `flatpak-spawn --sandbox` and browsers'
  renderer sandboxes map onto the same mechanism or onto ordinary
  namespaces and seccomp.
- **Flatpak apps** are started by the **Flatpak launcher** (Phase 5), a TCB
  service that reads each app's `metadata` and permissions from
  store-owned storage no app can write, never from the app's own files at
  run time. User (`--user`) installs are installed through the store
  service, which shows the permissions and records the user's approval at
  install. Each Flatpak app gets a per-runtime view: `/usr` is its
  runtime, `/app` is the app, its private home is `~/.var/app/<app id>` as
  on Flatpak, and the rest of the view is as below. The launcher has its
  own conformance tests in milestone 6, alongside upstream `flatpak run`
  tests.
- **The default profile** for unpackaged desktop programs is outbound
  network only (unless provenance withholds it): no file or device grants,
  and portals for everything else. Every graphical app's base view, which
  is not a grant, also holds its private home, the Wayland socket through
  its security context, PipeWire playback and the GPU render node (for a
  Flatpak app the render node only with `--device=dri`, as on Flatpak). The
  profile is shown in Settings and can be changed per program.

## The file view

Each app has its own mount namespace, built at launch. Its contents are fixed
by a published list, so apps find everything they expect:

- the system image read-only (`/usr`, and `/bin`, `/lib`, `/lib64`, `/sbin` as
  links into it), or for a Flatpak app its runtime as `/usr` and the app as
  `/app`;
- `/etc` as a generated, read-only view: `passwd` and `group` (this user and
  system accounts only), `hosts`, `resolv.conf` (naming the system
  resolver, below), `nsswitch.conf`,
  `machine-id` (a per-app value, so apps can't use it to track the user
  across apps), `localtime`, `os-release`, `ssl/certs`, `fonts`,
  `ld.so.cache`;
- `$HOME`: the app's **private home**, persistent and writable, stored at
  `~/.var/app/<id>` on disk (a protected path; `<id>` is the manifest or
  Flatpak ID, or for an unpackaged program its launch path (executable,
  script or AppImage), escaped)
  and mounted at the user's real home path, so `$HOME`, `getpwuid` and
  absolute paths saved in configs agree. `XDG_CONFIG_HOME`,
  `XDG_DATA_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` point inside it.
  Apps that pick their own dotfile paths (`~/.wine`, `~/.steam`,
  `~/.mozilla`, `~/.gitconfig`, `~/.npm`) create them there as on Linux.
  Granted folders are bind-mounted at their real paths inside it
  (`~/Documents`). A published read-only list of shared settings is
  projected in: `~/.config/gtk-3.0`, `~/.config/gtk-4.0`,
  `~/.config/fontconfig`, `~/.local/share/fonts`, `~/.icons`,
  `~/.local/share/icons` and `~/.config/mimeapps.list`. The terminal
  identity instead gets the real home minus the protected paths. Golden
  first-run test (Phase 5): Wine, Steam, Firefox and git run for the first
  time on Linux and on Optimal, and the paths each creates under `$HOME`
  match;
- a private `/tmp` and `/var/tmp`;
- `$XDG_RUNTIME_DIR` with the Wayland socket (through the app's security
  context), the PipeWire socket (with the app's permissions), the session
  bus (through the app's D-Bus proxy), the portal sockets and the
  **document portal** at `$XDG_RUNTIME_DIR/doc` (see the file chooser
  below);
- a generated `/.flatpak-info` holding the app's ID and permissions, and
  `GTK_USE_PORTAL=1`, `GDK_DEBUG=portals` and
  `QT_QPA_PLATFORMTHEME=xdgdesktopportal` in its environment, so GTK, Qt,
  Electron and GLib use the portals for file dialogs, URI opening and
  app-ID lookup as they do under Flatpak (every identity except the
  terminal's, whose programs see the host as on Linux);
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
- **Inbound.** `bind` and `listen` always succeed, as on Linux. A socket
  bound to the wildcard address receives only loopback traffic unless its
  identity holds a **per-port inbound grant** for that port (declared in
  the manifest or requested through the network portal); inbound packets
  to a unicast non-loopback address without that grant are dropped by the
  default-deny firewall, so the app sees silence, not an error.
- **Loopback is per identity.** An app reaches loopback ports its own
  identity listens on. Connecting to a loopback port whose listener
  belongs to another identity needs a per-port grant (`ECONNREFUSED`
  without it), and the connection is a direct channel, so the one-principal
  check above applies to it. A browser reaching a development server
  started from the terminal needs that grant; the terminal shows a
  one-click request for it. OAuth loopback callbacks work unchanged for
  apps that already hold outbound network: when such an app asks the
  open-URI portal to open `http://127.0.0.1:<port>/…` or
  `http://localhost:<port>/…` and that port's listener is the requesting
  app, the portal grants the browser that port for as long as the listener
  lives. For an app without outbound network the request is an ordinary
  open-URI request (below).
- **DNS.** `resolv.conf` names `127.0.0.53`, served by the system
  resolver, a TCB service and not an identity's loopback port. It answers
  only identities holding outbound network; any other identity gets
  `ECONNREFUSED`, so DNS can't tunnel data out of an app without network.
- **Services that send for an app.** Every TCB service that sends traffic
  on an app's behalf checks the caller's attested grants first: the
  resolver as above; the open-URI portal, which for a caller without
  outbound network shows any non-`file` URI in a prompt naming both apps
  with the full URL before handing it to the browser; the notification
  portal, which for such a caller drops remote content (image URLs) and
  sends any URI an action opens through the open-URI rule; and the email
  portal, which for such a caller shows the recipients, subject and
  attachments in the same kind of prompt before the mail client gets them.
- Each app has its own IPC namespace (SysV IPC, POSIX message queues) and its
  own `/dev/shm`; **abstract Unix sockets are scoped per app**.

Tests (Phase 6): revoke the network grant under a live TCP connection; an
app without an inbound grant that listens on the wildcard and on a
non-loopback address is unreachable from another machine, and with a
per-port grant exactly that port is reachable; a cross-identity loopback
connection without the grant is refused; an identity without network gets
`ECONNREFUSED` from the resolver; the OAuth callback flow of a named app
works. Exfiltration-suite rows, each its own row: an app holding
Documents and no network sends a canary through open-URI with an `https`
URL, through a notification, and through the email portal (Phase 5, with
a networked test app as the URI handler), through a `<canary>.example`
lookup (Phase 6), and through its own loopback page opened in the browser
(Phase 7); each is refused or shown in the prompt.

## Unix permissions, uids and Linux capabilities

- **Unix permissions still apply, inside the view.** Every file check is the
  normal Linux discretionary check (uid, gid, mode, ACLs) and then the
  capability check. Both must pass.
- **uids.** Each human user has a uid; each app runs under that user's uid
  inside its own view. There is a uid 0 for the system's own services, but
  it carries no special power over the kernel: being uid 0 only matters for
  Unix permission checks on files in a view, and no app runs as uid 0
  (uid 0 inside an app's own user namespace maps to the user's uid, as on
  Linux).
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

**Trusted input.** The kernel tags every input event with the device that
produced it. The secure-attention key and prompt answers (on the broker's
terminal, and in the compositor's prompt layer from Phase 7) are accepted
only from keyboards parsed in the kernel (PS/2, boot-protocol USB, and
in-kernel I2C-HID for internal laptop keyboards once it meets the parser
promotion thresholds) or from a keyboard whose own per-device HID process
(one process per device) passed typed-code confirmation. Bluetooth
keyboards never count unless the user opts that keyboard in by name;
`uinput` devices and the compositor's virtual keyboard and pointer never
count. The polled xHCI boot-keyboard reader stops when the Phase 6 xHCI
driver binds, and resume PIN entry uses the in-kernel paths. Attack rows:
a compromised Bluetooth stack or HID process injects the secure-attention
key and approves a prompt; both fail (Phase 6, extended in Phase 7 to the
compositor path).

- **File chooser:** the user picks files or folders in the portal's dialog
  (drawn on the broker's terminal in Phase 5, by the compositor from
  Phase 7). As with xdg-desktop-portal, the portal returns URIs that point
  into the **document portal**, a FUSE-protocol file system served by a
  tier-3 process and mounted at `$XDG_RUNTIME_DIR/doc` in every view. Each
  app sees only its own entries there (`doc/<entry id>/<name>`): a one-use
  pick lasts until the app exits, a persistent pick is recorded in
  store-owned storage and survives reboots, and revoking it in Settings
  revokes every OFD opened through it (`EIO`, as above). A folder picked
  persistently is instead added to the app's view at its real path. The
  network rule in [threat-model.md](threat-model.md) applies to persistent
  grants. Drag and drop of `file://` URIs between apps goes through the
  compositor, which turns each dropped file into a one-use document-portal
  entry for the receiving app (Flatpak's file-transfer portal); DnD is a
  row of the Phase 7 exfiltration suite.
- **Toolkits that can't be forced** onto the portal draw their own dialogs
  over the app's view. Wine, Java (Swing and JavaFX) and Blender are
  intended divergences in [compat.md](compat.md) with this user-facing
  outcome: the dialog shows the app's private home, its granted folders
  and `$XDG_RUNTIME_DIR/doc`; to open another file the user grants a folder
  in Settings or drags the file onto the window.
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
shared channels for launchers, virtual keyboard and pointer). Each is
shown at install. The format and the launcher's reader land in Phase 5.

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
| `--talk-name=org.freedesktop.Flatpak` (`flatpak-spawn --host`) | never passed to the host's Flatpak service; the launcher answers `HostCommand` itself with a prompt naming the app and showing the full command line, and an approved command runs under the terminal identity over a pty and pipes, which are a direct channel between the two; every other method is refused with `org.freedesktop.DBus.Error.AccessDenied` |
| `--socket=session-bus`, `system-bus` (unfiltered) | refused; the app runs with the filtered proxy and the user is told |

## Device nodes

`/dev` inside an app's view contains only what the manifest allows: the basics
every program needs (`null`, `zero`, `full`, `random`, `urandom`, `tty`,
`ptmx`, `pts/`, `shm/`, `fd/`), plus granted classes such as `dri/renderD*`,
`snd/`, gamepads under `input/` (only gamepads, never keyboards unless the app
is the compositor or input service), `hidraw*` for specific devices the user
approved, `uinput` only as the virtual-gamepad portal's gamepad-only fd,
`fuse` for apps allowed to mount FUSE file systems inside their view, and
`net/tun` for apps running rootless containers, which creates TUN and TAP
devices only inside the caller's own network namespace (milestone 6).

## Proof and test obligations

- [verification.md](verification.md) target 4 is stated over memory
  objects: a user mapping in process A reaches a frame only if A holds a
  capability for the memory object that owns it. Every user page-table
  entry is installed through one core-mm API that takes a memory-object
  capability witness, and target 4 is proved over that API and the
  capability table; a CI check fails if any other code reaches the
  PTE-writing shim. The sharing paths that call the API are a **closed
  list** in `design/proofs.toml`: copy-on-write after `fork`, shared file
  mappings, SysV and POSIX shared memory, memfd, `SCM_RIGHTS` passing,
  `ptrace` and `process_vm_*`, io_uring registered buffers, dma-buf, and
  GPU buffer objects. Each phase that adds a path extends the proof in its
  own done-when.
- Target 5 covers derivation (never more rights than the parent),
  revocation (the whole subtree, and the revoked-object behaviours above),
  and OFDs and mounts: an fd or mount never carries more rights than the
  capability it was derived from, and revoking that capability reaches every
  fd sharing the OFD and every copied mount.
- The hostile-process and exfiltration suites
  ([threat-model.md](threat-model.md)) test every row of the first table, the
  file view list, the mount namespace rules, the identity rules, the
  shared-channel mediation and the setuid replacements from the attacker's
  side, including the identity-forge cases: replace a library beside a
  granted app, run two Python apps and check they get two identities,
  plant a foreign-provenance hook or unit and try to run it as the
  terminal or through the service manager. Each shared channel is gated in
  the phase it lands: D-Bus, kernel keyrings, the document portal and the
  Flatpak launcher in Phase 5, PipeWire and the resolver in Phase 6,
  Wayland, XWayland, the clipboard and DnD in Phase 7. Each TCB service
  named here has an A3 row in [threat-model.md](threat-model.md) with
  10^8 protocol fuzz executions gated in its landing phase.
