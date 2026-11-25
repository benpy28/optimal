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
  `clone`, `execve`, launcher and portal path (Phase 5) and by launching
  from the desktop shell (Phase 7).
- **Where identities come from:** a signed manifest (identity = manifest
  ID plus publisher key; the format and the launcher's manifest reader land
  in Phase 5); a Flatpak app (Flatpak ID plus the signing key of the remote
  it came from); for an unpackaged program launched from the desktop, an
  identity of **path + SHA-256 of its launch closure**; for a program run
  from a terminal, the terminal's identity, except registered apps (below).
  A changed publisher or remote key is a new identity: grants reset and the
  private home is retired (below), so an administrator who installs a
  system-wide app with the same ID under another key inherits nothing
  (A7 test, Phase 5). The launch closure is everything that decides what
  the program does:
  - a script started through an interpreter (`python3 app.py`, `java -jar
    app.jar`, `wine foo.exe`, a `#!` script): the interpreter plus the
    script's (or jar's or `.exe`'s) hash, plus the full argv for `-c` style
    calls (`sh -c`, `python3 -c`), so two Python apps are two identities;
    plus the code the interpreter loads: every file it opens from the
    script's own directory tree and from the roots in the published
    per-interpreter list (Python's `site-packages` and venv directories on
    `sys.path` outside the system image, Java's `-cp` and `Class-Path`
    entries, Node's `node_modules` directories above the script) joins the
    closure record below on first launch;
  - an ELF program: the executable and its `PT_INTERP`, plus every file
    outside the system image that it maps with `PROT_EXEC` (`$ORIGIN`
    libraries, `LD_LIBRARY_PATH` entries, `LD_PRELOAD`, plugins);
  - an AppImage: the AppImage file itself.

  The **closure record**, stored with the identity's grants, holds the
  SHA-256 of every such file, collected on first launch. Later, an
  executable mapping of a non-image file, or an interpreter's open of a
  file in those roots, whose hash isn't in the record succeeds only if the
  update rules below accept the file's provenance (it is then added to the
  record); otherwise it fails with `EACCES`.
- **Identity ID and lineage.** Grants, the closure record and the private
  home are keyed by an **identity ID**, never by a path. For a manifest or
  Flatpak app it is (ID, signing key). For an unpackaged program it is
  (launch path, lineage ID, argv hash): the lineage ID is a random 128-bit
  value created at the identity's first launch and carried to a new
  closure hash only across an update the rules below accept. A binary replaced at the same path by another
  writer gets a new lineage, no grants and an empty home; `sh -c '<a>'` and
  `sh -c '<b>'` differ in argv hash and get disjoint homes. Hostile tests
  (Phase 5): a replaced binary sees an empty home; two `-c` identities see
  disjoint homes; a replaced `helper.py` beside a granted Python app fails
  `EACCES` and inherits nothing. For a vault identity (F3 in
  [features.md](features.md)) every per-identity record (lineage, closure
  record, grants, first-use answers, silent-grant log entries, provenance
  timestamps, known-device entries added while the vault is open,
  dashboard logs) lives inside the open hidden volume and is never written
  to the system settings store.
- **Updates.** The install directory of every program launched from the
  desktop is protected against other app identities: the directory holding
  its executable (for example `~/opt/firefox/`), or for a script the
  script's directory tree; when that directory is the home root, an XDG
  user directory (Downloads, Desktop, Documents and so on) or a removable
  volume, only the closure files themselves. No other app identity's grant
  includes it, but it stays visible and writable to the terminal identity
  (a terminal write gives the file terminal provenance, so the rules below
  decide the lineage). A directory whose files all have terminal
  provenance is never protected; only its closure hashes are recorded.
  Tests (Phase 5): desktop-launch `./target/debug/x`, then 10 `cargo
  build` runs give 0 `EACCES`; `rm -rf ~/opt/app` from the terminal works;
  `pip install --upgrade` of a terminal-installed app gives exactly 1
  prompt. When the closure hash changes, the lineage carries over
  only if every changed file's provenance is (a) the identity itself
  (Firefox, Discord and Steam updating themselves), (b) for a child
  identity, its declared parent, for paths the parent's manifest shares
  with it (Steam updating a game or Proton), or (c) for an app the terminal
  installed (every closure file had terminal provenance at first launch),
  the terminal, after a prompt listing the changed files (`pip install
  --upgrade`), a floor prompt when the app holds network or a non-empty
  private home. Any other writer makes a new lineage with the default
  profile and an empty home. The old home is **retired**: moved into
  `~/.var/app/.retired/`, a protected path listed in Settings, where the
  user can delete it or hand it to the new identity on the trusted path.
  Milestone 13 test (Phase 9): Steam updates a game and Proton, the game
  relaunches with no `EACCES` and keeps its grants and home.
- **Provenance.** Every file an app identity creates or writes carries a
  kernel-maintained provenance tag naming that identity and the time it was
  written (a clock never earlier than the signed time floor in
  [boot.md](boot.md)), stored in an xattr namespace no app can set or remove. Two
  origin flags go with it. **Downloaded** is defined here once
  ([threat-model.md](threat-model.md) and the roadmap refer to this
  definition): a file is tagged downloaded when (a) any identity writes it
  into a download folder (the XDG Downloads folder, a browser's download
  directory), (b) it arrives through a portal from a networked identity, or
  (c) it is written by a tainted process. For every identity, a process is
  tainted once it has read from a non-loopback socket or read a downloaded
  file (a low-water mark like "removable" below, inherited across `fork`
  and `execve`), and the taint spreads to every process that reads from a
  tainted one through a pipe or Unix socket, and to every process that maps
  shared memory a tainted one can write (applied at map time); for a
  non-terminal app identity it is identity-wide (Chromium's network
  process taints its browser process). Tests (Phase 7): Chromium "Save
  as" to `~/Documents`, `curl … | tar x`, `wget` then `tar xf`, an archive
  app extracting a browser download outside Downloads, and a cloned
  repository's binary each prompt once on a double-click.
  Files an identity writes inside its own install directory or closure keep
  its provenance and are not tagged (update rule (a) below). **Removable**
  is set on files read from removable or foreign volumes. Copies keep the
  source's tag and flags when the kernel does the copy (`copy_file_range`,
  `FICLONE`, `sendfile` and `splice` from a file) or the file manager
  portal does; and a terminal-identity process that has opened for reading
  a file with an app tag or the removable flag gives that provenance to
  every file it writes afterwards (the lowest-trust tag it has read,
  inherited across `fork` and `execve`), so `cp` and `tar x` from a USB
  stick keep it. Bytes piped into an interpreter carry no tag; they run
  as the terminal.

  What the tags change:
  - **Desktop launch** (file manager, launcher, portal open) of a
    downloaded executable prompts at every level (the floor), naming where
    it came from ("downloaded by Firefox", "downloaded in Terminal"); an
    executable with another app's provenance or the removable flag
    ("written by *App*", "from USB stick *label*") follows the levels
    (suspicious-list entry 1). Approved, it runs under its own identity in
    the default profile. The user service manager and D-Bus activation
    apply the same rule to every unit and service they start.
  - **The terminal executing a file with terminal provenance** runs its
    own code: no prompt and no `EACCES`, whatever its flags.
    So `git clone`, `cargo build` and `./target/debug/x` just work.
  - **Exec by any other identity than the file's provenance identity**
    (the terminal running a file Firefox downloaded or an editor wrote, or
    one from a USB stick) follows the levels; where the level asks, the
    terminal process gets `EACCES` and the terminal shows a one-click
    request naming the provenance. Approving it covers files from that
    provenance identity under that folder from then on (listed in Settings
    and revocable), so an edit-build-run loop with a GUI editor asks once.

  "Executes" means `execve` (a `#!` script included), an executable
  mapping, and `execveat` with `AT_EXECVE_CHECK`, the Linux 6.14 call
  interpreters use before running a script. Files that tools run as data
  (a `Makefile`, `.envrc`, a `.git/hooks` entry run by git, `.git/config`,
  npm scripts) are covered by a published list of code-bearing names: a
  terminal-identity process that opens a file on that list whose
  provenance is another identity or that carries the removable flag is
  treated as executing it. Tests (Phase 5, with a hot-added virtio-blk
  volume standing in for removable media and a local fake git remote over
  loopback; a real USB stick and a real remote in Phase 6): `git clone` +
  `cargo build` + `./target/debug/x`, rebuilt and run 10 times, gives 0
  prompts and 0 `EACCES`; an app-written binary and a removable-media
  binary fail `EACCES` from the terminal at "Only when suspicious" until
  approved; `cp` and `tar x` copies keep the tag; a planted `Makefile` and
  git hook written by another identity fail `EACCES`. Phase 7: `curl -O`
  then a double-click in the file manager gives exactly 1 prompt.
- **Protected launch paths.** `~/.config/autostart`,
  `~/.local/share/applications`, `~/.local/share/dbus-1/services`,
  `~/.config/systemd/user`, `~/.config/environment.d`,
  `~/.config/mimeapps.list`, `~/.local/bin`, `~/Applications`, every
  desktop-launched program's install directory (against other app
  identities only, above), every private home and the
  projected shared settings (below) are protected paths
  ([threat-model.md](threat-model.md)): no folder grant includes them.
  Autostart entries are written only through the **Background portal**
  (`RequestBackground` with `autostart`); desktop, D-Bus service, unit,
  environment and `mimeapps.list` entries only through the launcher
  portal; the shared settings only through the settings portal. Each asks
  per the prompt level (below). When an app writes
  `~/.config/autostart/*.desktop` or `~/.local/share/applications/*.desktop`
  inside its own private home, as Steam and Discord do, the launcher (which
  watches those two paths in every private home) turns the file into the
  matching portal request on the app's behalf; an approved entry is
  installed with its `Exec` line rewritten to launch under that identity.
  Test (Phase 5): a test app writes both and each appears after approval,
  and only then. `~/.local/bin` is writable by the terminal identity alone
  (so `pip install --user` and `cargo install` work), because what runs
  from it runs as the terminal anyway.
- **Terminals.** `TIOCSTI` returns `EPERM` (as on Linux 6.2 and later with
  `dev.tty.legacy_tiocsti=0`) and `TIOCLINUX` is absent. The launcher gives
  a process a fresh pty whenever it starts a different identity from a
  terminal (`flatpak run`, `xdg-open`), and terminals ignore OSC 52
  clipboard writes and title and report sequences written by a foreign
  identity. These are lockdown rows with tests in Phase 5
  ([threat-model.md](threat-model.md)).
- **Registered apps run from a terminal keep their own identity.** The
  launcher marks the launch path of every registered identity (a manifest
  app, a Flatpak export, an unpackaged program with a record) with a
  kernel-set attribute. When a terminal-identity process `execve`s such a
  path, the exec is an **identity exec**: the launcher starts the app as a
  new process, a child of the launcher service, under the app's own
  identity with `AT_SECURE=1`; only fds 0 to 2 plus fds its manifest
  declares pass; and the `execve`ing process becomes the system image's
  **launch relay**, a stub holding a pidfd on the app, so a `waitpid` on it
  returns the app's exit status. The launcher passes argv, an environment
  allowlist (locale variables, `LANG*`, `TERM`, `TZ`, `DISPLAY`,
  `WAYLAND_DISPLAY`, the `XDG_*` runtime directories, `HOME`, `PATH`;
  nothing else), the cwd and the caller's rlimits, nice value, CPU
  affinity and cgroup. Only whole argv tokens that `stat` to an existing
  file or directory become document-portal entries (argv rewritten to
  their paths, Flatpak's `@@` forwarding), and the cwd becomes a directory
  entry; entries last until the identity's last process exits, and other
  forms (`--file=x`, a new output path outside the cwd) are listed
  divergences ([compat.md](compat.md)). Any other argv token beyond those
  paths and URLs (`--load-extension`, `-e`, `--remote-debugging-*`) needs a
  floor prompt naming the app. Of fds 0 to 2, those that are ttys get a
  fresh pty that the relay connects to the caller's tty (with the escape
  filter above); the others pass as their open file descriptions, so pipes
  and redirections are untouched. The relay forwards every catchable signal to its app, exempt from the
  terminal's signal limit toward other identities ([compat.md](compat.md));
  if the stub dies (`SIGKILL` included) the launcher kills the app through
  its pidfd, and the stub re-raises the app's fatal signal so
  `WIFSIGNALED` holds.
  The exec is a direct channel terminal → app (`launch-relay` in
  channels.toml): it needs the combined approval like any other channel,
  asked once per app ("your home, through Terminal → *App*"), a floor
  prompt when the app holds network. Tracing a registered app is an
  intended divergence: `gdb -p` or `strace -p` through the debugger portal.
  So `firefox` run from a shell and Firefox started from the desktop use
  one profile. Tests: (Phase 5) a registered test app run from the shell
  and from the launcher sees the same home, and its exit status and the
  forwarded signals pass through; `head -c 1G /dev/urandom | app |
  sha256sum` matches; `gdb -p` and `strace -p` through the debugger portal
  and `taskset` act on the app; (Phase 7) `code .` and `EDITOR="code
  --wait" git commit` work, asking the combined approval once per app, and
  a script exec'ing a registered browser with a crafted HTML file is
  caught by the exfiltration suite.
- **Direct channels make one principal.** Data moves between identities
  through **direct channels**, a closed list checked in as
  [`design/channels.toml`](channels.toml), each row with its phase and test.
  A channel is **directional**, from the identity that can write into it to
  the one that can read from it; a two-way channel (a socket, writable
  shared memory, a D-Bus peer pair) is two edges. The kernel keeps, per
  session, a directed graph of identities with an edge for every channel
  opened in the session, live or past; edges are dropped only at logout,
  and edges created by grants (`created = "grant"` in channels.toml) are
  rebuilt from the stored grants at every login. The **component** of an
  identity is itself plus every identity with a path of edges to it, and
  the component's **data set** is the union of its members' persistent
  file grants and their camera, microphone, screen capture and screenshot,
  location, background clipboard-read, USB-portal raw device, `hidraw`,
  serial and screen-reader (AT-SPI) grants. The data + network rule
  applies to components: an identity holding network may have a non-empty
  component data set only under a **combined approval**, a prompt that
  names each source in that data set and every networked identity it would
  reach, and that is asked again whenever either grows. It is checked at
  two points:
  - **opening a channel** A → B checks every networked identity reachable
    from B, B included; without a covering approval the channel is refused
    at once (`ECONNREFUSED` on connect, `EACCES` on passing the fd or
    mapping the object, `org.freedesktop.DBus.Error.AccessDenied` from the
    D-Bus proxy), one combined-approval request per identity pair is
    queued, and a retry after approval succeeds (the rule is stated in
    [threat-model.md](threat-model.md) "App identity can't be forged"; Phase 5 test: a socket channel
    refused, approved, then retried succeeds);
  - **every new grant** (network, folder, inbound port, persistent
    document, and each data-set grant above) is checked against the holder's component and every
    networked identity downstream of it before it is made. A writable
    folder grant creates its folder channels then. A grant whose combined
    approval the user refuses is refused at grant time, and the portal's
    denial carries the reason ("*App* shares data with *Other*, which has
    network").

  So A (files) → B (nothing) → C (network) is refused at the second join,
  and A (files) → B (nothing) followed by a network grant to B asks the
  combined approval at that grant. An existing edge costs one hash lookup
  on connect; a new edge or grant runs a breadth-first search over the
  session's graph (Phase 5 benchmark: under 100 µs with 1,000 identities).
  **The terminal** is a special node, because it can read the whole home:
  its reads make no edge (what it runs is covered by the provenance rule
  above), so a writable grant inside its home doesn't join an app to it,
  and that grant's prompt says that running anything the app writes there
  from the terminal follows the prompt level (provenance rule above). Its writes do make edges, created on its
  first actual write of a file inside a folder another identity's grant
  covers, not when the grant is made; when that edge is refused the write
  fails `EACCES` and the terminal shows a one-click request for the
  combined approval, which names "your home, through Terminal". The
  projected shared settings are written only by the settings portal, so
  they join nobody. TCB services (portals, the launcher,
  compositor, PipeWire, the D-Bus proxies, the resolver, the document
  portal, the SSH agent, GPG agent and secret service, the per-app
  accessibility proxy and the input method) are not principals:
  talking to one joins nobody, and each enforces its own rule
  (channels.toml lists every one). Covert channels (CPU and cache timing,
  `statfs` free space, `flock` on shared read-only files) are out of scope;
  their measured bandwidth is published on the scorecard. The exfiltration
  suite runs every row of `channels.toml` in that row's phase, a
  writer-to-reader folder pair split across a logout (Phase 5), and in
  Phase 7 two colluding apps, a three-app chain and a grant after a join,
  each as its own row. The graph crate is proof target 18 in
  [verification.md](verification.md) (B/T at Phase 5, P/T at Phase 7): no
  networked identity's upstream data set is outside a combined approval
  after any sequence of channel opens, grants and approvals; two seeded
  bugs (dropping the downstream search, treating edges as undirected)
  must each fail the proof. F9's background freezing
  ([features.md](features.md)) reads the same graph: an identity with a
  live direct channel from an unfrozen app identity is never frozen.
- **Apps that launch apps.** A launcher like Steam can start children under a
  **child identity** (`steam/<game id>`): the child gets its own capability
  set and file view (by default its own private home and the folders the
  parent declares for it, never the parent's tokens or data), and the parent can `wait` for it
  (through the identity exec's stub, above), track it with a pidfd and
  signal it. The parent's manifest declares the **shared
  channels** each child gets, all mediated: read-only paths (the Steam
  library folders), writable per-child paths (`compatdata/<appid>` and
  `shadercache/<appid>`), named shared memory objects (the overlay), named
  socket endpoints (the Steam client IPC socket) and a read-write edge
  between Steam and each child's save paths (Steam Cloud), a declared
  graph edge; nothing else crosses.
  Steam ships unpackaged, so the system image carries a **signed Steam
  manifest** for the official Steam path and closure (and a mapping of
  Flatpak Steam's metadata onto the same grants), written as a Phase 9
  input. When a process of the Steam identity itself `execve`s a program
  with `SteamAppId` in its environment or under an appmanifest install
  directory, the exec is an identity exec (above), which the launcher maps
  through that manifest to the child identity (so
  Proton's shared `proton` and `wine64` binaries run as the game's child)
  and builds its view; a child identity's execs keep its own identity
  whatever `SteamAppId` says. Phase 9 tests: a Proton title and a
  Cloud-synced native title each round-trip their saves; Steam's reaper's
  `waitpid` sees the game's exit; an fd Steam holds open is absent in the
  game; an exec with `SteamAppId=B` from game A is refused B's identity. Steam Input's virtual controllers go through the
  **virtual-gamepad portal**, which hands out a `uinput` fd that accepts
  only gamepad event codes (`BTN_GAMEPAD` to `BTN_THUMBR`,
  `BTN_TRIGGER_HAPPY*`, the `ABS_*` axes, `EV_FF`); keyboard and pointer
  codes fail `UI_SET_*BIT` with `EINVAL`. Its keyboard and mouse emulation
  (desktop mode, mapped keys) goes through a **virtual keyboard and pointer**
  the compositor provides behind its own grant: events reach only the
  surface that has focus, never the prompt layer, and never count as the
  secure-attention key or a prompt answer. All three are tested in
  milestone 13 (Phase 9).
- **The Flatpak portal.** Every identity that sees `/.flatpak-info`
  (below) gets `org.freedesktop.portal.Flatpak` from the launcher, because
  Chromium, Electron and Steam's pressure-vessel switch to it when that file
  exists. `Spawn` starts a child identity of the caller whose grants are a
  subset of the caller's: `sandbox` gives the child only the
  `sandbox-expose` and `sandbox-expose-ro` paths and the `sandbox-flags`
  sockets (Wayland, PipeWire, session bus, GPU) the caller holds;
  `no-network` withholds network; `clear-env`, `watch-bus`, `expose-pids`,
  `share-pids` and `notify-start` behave as on Flatpak; `usr-fd` and
  `app-fd` must be fds of read-only trees the caller can read, mounted as
  the child's `/usr` and `/app`; `latest-version` and
  `CreateUpdateMonitor` are refused (no access to installs). `SpawnSignal`
  signals only children the caller spawned. Browsers' renderer sandboxes
  without that file use ordinary namespaces and seccomp. Tests (Phase 5):
  headless Chromium in Flatpak mode starts its renderers through `Spawn`
  and passes its sandbox self-test; pressure-vessel under `/.flatpak-info`
  starts a Steam Runtime container through `Spawn`.
- **Flatpak apps** are started by the **Flatpak launcher** (Phase 5), a TCB
  service that reads each app's `metadata` and permissions from
  store-owned storage no app can write, never from the app's own files at
  run time. User (`--user`) installs are installed through the store
  service, which shows the permissions and records the user's approval at
  install (before the store, on the settings console's install review
  page, below). Each Flatpak app's view follows Flatpak's layout exactly: `/usr`
  is its runtime, `/app` is the app, `$HOME` is the user's real home path,
  which shows only `~/.var/app/<app id>` (persistent, at its own path)
  unless a home or host grant shows more, and writes elsewhere under
  `$HOME` don't persist, as on Flatpak; `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
  `XDG_CACHE_HOME` and `XDG_STATE_HOME` are that directory's `config`,
  `data`, `cache` and `.local/state`; `--persist=<dir>` mounts
  `~/.var/app/<app id>/<dir>` at `~/<dir>`. An existing `~/.var/app` tree
  copied from Linux is used as it is (migration test, Phase 5: two apps
  start with their Linux data). The launcher has its own conformance tests
  in milestone 6, alongside upstream `flatpak run` tests.
- **The default profile** for unpackaged desktop programs, and for manifest
  apps that don't declare network, has **no network**, no file or device
  grants, and portals for everything else. **First use.** The first
  non-loopback `connect` or send, or the first name lookup at the resolver
  (below), from an identity with no recorded network answer queues one
  request per identity to the network portal, which applies the prompt
  level (below); the answer is remembered per identity. Silent decisions
  are made synchronously from the level policy the broker pushes into the
  kernel and the resolver, so the call itself succeeds and no app sees a
  failure when no human is needed (granted silently and logged). When a
  human is needed, the lookup, `connect` or send waits up to 30 s for
  the decision (an `O_NONBLOCK` connect returns `EINPROGRESS` and
  completes or fails later; an `O_NONBLOCK` send returns `EAGAIN`). A
  pending socket reports no `POLLOUT` until the decision; on approval the
  call proceeds normally (TCP: `EPOLLOUT` at connection completion), on
  denial `EPOLLERR` with `ENETUNREACH`. The triggers are TCP
  `connect` and every send path: `sendto`, `sendmsg`, `sendmmsg`, `write`
  on a connected socket, io_uring sends and `MSG_FASTOPEN`; a UDP
  `connect` sends nothing and never triggers.
  The generated `resolv.conf` carries `options timeout:30 attempts:1`, so
  stub resolvers wait out the hold, and the resolver dedupes retransmits
  per identity and answers SERVFAIL within 5 s on upstream failure; on refusal or timeout the lookup answers SERVFAIL
  (`EAI_AGAIN`) and `connect` returns `ENETUNREACH`, never
  `ECONNREFUSED`. A manifest app's network declaration is shown
  at install, and the install approval is the grant. Inbound ports and
  remote-control access (screen casting, remote desktop, input capture, the
  virtual keyboard and pointer) are never implied by network: each is its
  own grant, always prompted. Every graphical app's base view, which is not
  a grant, also holds its private home, the Wayland socket through its
  security context, PipeWire playback and the GPU render node (for a
  Flatpak app the render node only with `--device=dri`, as on Flatpak). The
  profile is shown in Settings and can be changed per program. Tests
  (Phase 5 on the loopback-only stack with a fake upstream; Phase 6 against
  the real network): `curl https://<name>` from a new identity raises
  exactly 1 request, prompts at "Always ask" and succeeds on its first run
  when approved at 2 s and at 25 s, through glibc, musl, Go and Chromium's
  built-in resolver; an epoll UDP client sees 0 spurious wakeups during a
  25 s hold; at the default level a network-only first use
  gives 0 failures and 0 prompts; a refused identity's lookup gets
  `EAI_AGAIN` and its `connect` `ENETUNREACH`.

## The file view

Each app has its own mount namespace, built at launch. Its contents are fixed
by a published list, so apps find everything they expect:

- the system image read-only (`/usr`, and `/bin`, `/lib`, `/lib64`, `/sbin` as
  links into it, with the i386 multiarch set of [compat.md](compat.md) from
  Phase 7), or for a Flatpak app its runtime as `/usr` and the app as
  `/app`;
- `/etc` as a generated, read-only view: `passwd` and `group` (this user and
  system accounts only), `hosts`, `resolv.conf` (naming the system
  resolver, with `options timeout:30 attempts:1`, below), `nsswitch.conf`,
  `machine-id` (a per-app value, so apps can't use it to track the user
  across apps), `localtime`, `os-release`, `ssl/certs`, `fonts`,
  `ld.so.cache`;
- `$HOME`: the app's **private home**, persistent and writable, keyed by
  its identity ID (above) and stored on disk at `~/.var/app/<app id>` for
  a Flatpak app (Flatpak's layout, above) and at
  `~/.var/app/<escaped launch path>@<lineage ID>+<argv hash>` for an
  unpackaged program (all protected paths). For every identity except a
  Flatpak app it is mounted at the user's real home path, so `$HOME`,
  `getpwuid` and absolute paths saved in configs agree, and
  `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and
  `XDG_CACHE_HOME` point inside it. Apps that pick their own dotfile paths
  (`~/.wine`, `~/.steam`, `~/.mozilla`, `~/.gitconfig`, `~/.npm`) create
  them there as on Linux. Granted folders are bind-mounted at their real
  paths inside it (`~/Documents`). A **home grant** overlays the private
  home: the real home minus the protected paths is mounted at `$HOME`, and
  the private home's existing top-level entries are bind-mounted over the
  same names, so the app keeps the settings it made before the grant (a
  Flatpak app keeps `~/.var/app/<app id>` at its own path). A published
  read-only list of shared settings is projected in: `~/.config/gtk-3.0`,
  `~/.config/gtk-4.0`, `~/.config/fontconfig`, `~/.local/share/fonts`,
  `~/.icons`, `~/.local/share/icons` and `~/.config/mimeapps.list`; these
  are protected paths in the real home, written only by the settings
  portal (the launcher portal for `mimeapps.list`). The terminal identity
  instead gets the real home minus the protected paths, install
  directories included in its view (above). Golden first-run
  test: git, Python and new-WoW64 Wine (`wineboot --init` with no display) in
  Phase 5, Firefox and Steam (whose first run downloads its client and
  opens an X11 window) in Phase 7 run for the first time on Linux and on
  Optimal, and the paths each creates under `$HOME` match;
- a private `/tmp` and `/var/tmp`;
- `$XDG_RUNTIME_DIR` with the Wayland socket (through the app's security
  context), the PipeWire socket (with the app's permissions), the session
  bus (through the app's D-Bus proxy), the portal sockets and the
  **document portal** at `$XDG_RUNTIME_DIR/doc` (see the file chooser
  below);
- a generated `/.flatpak-info` holding the app's ID (for an unpackaged
  identity `org.optimal.Unpackaged.<base32 identity ID>`) and permissions, and
  `GTK_USE_PORTAL=1`, `GDK_DEBUG=portals` and
  `QT_QPA_PLATFORMTHEME=xdgdesktopportal` in its environment, so GTK, Qt,
  Electron and GLib use the portals for file dialogs, URI opening and
  app-ID lookup as they do under Flatpak (every identity except the
  terminal's, whose programs see the host as on Linux); the file's
  presence also gives the identity the Flatpak portal (above);
- `/proc` per the contract in [compat.md](compat.md), showing only the app's
  own PID namespace and those below it (the terminal identity's view is
  wider, as compat.md defines);
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

- The network grant is **outbound only**, and the default profile has
  none (above). Every non-loopback `connect`, `sendto` and `sendmsg`
  checks the sender's identity; an identity with no recorded answer goes
  through the first-use request above, one whose answer is a refusal gets
  `ENETUNREACH` immediately, and revoking the grant resets its live
  sockets (`ECONNRESET`).
- **Inbound.** `bind` and `listen` always succeed, as on Linux. A socket
  bound to the wildcard address receives only loopback traffic unless its
  identity holds a **per-port inbound grant** for that port (declared in
  the manifest and shown at install, or requested through the network
  portal, always with a prompt at every prompt level); inbound packets
  to a unicast non-loopback address without that grant are dropped by the
  default-deny firewall, so the app sees silence, not an error.
- **Loopback is per identity.** A loopback-only TCP and UDP stack
  (`AF_INET` and `AF_INET6` on `127.0.0.0/8` and `::1`) lands in Phase 5
  with this rule; network drivers and the full stack follow in Phase 6.
  An app reaches loopback ports its own
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
  resolver, a TCB service and not an identity's loopback port. It sends
  queries upstream only for identities holding outbound network. A lookup
  from an identity with no recorded answer is the first-use event above
  (decided before any query leaves the machine); an identity whose answer
  is a refusal gets SERVFAIL (`EAI_AGAIN`), never `ECONNREFUSED`, so DNS
  can't tunnel data out of an app without network. The kernel stamps the
  sender's identity on every loopback datagram and TCP connection to the
  resolver (an Optimal control message), which is how the resolver tells
  who asks. It does its own upstream retries and answers SERVFAIL within
  5 s when the upstream fails. The resolver lands in
  Phase 5, forwarding to a fake upstream on the loopback-only stack until
  the full stack arrives in Phase 6.
- **Services that send for an app.** Every TCB service that sends traffic
  on an app's behalf checks the caller's attested grants first: the
  resolver as above; the open-URI portal, which for a caller without
  outbound network shows any non-`file` URI in a prompt naming both apps
  with the full URL before handing it to the browser; the notification
  portal, which for such a caller drops remote content (image URLs) and
  sends any URI an action opens through the open-URI rule; and the email
  portal, which for such a caller shows the recipients, subject and
  attachments in the same kind of prompt before the mail client gets them;
  and the print portal, which for such a caller shows the destination
  (printer name, its address, and whether it was found over the network)
  in the same kind of prompt before any job is sent, because a printer an
  attacker advertises over mDNS is a network sink. These confirmations
  show the content of one send, so they appear at every prompt level.
- Each app has its own IPC namespace (SysV IPC, POSIX message queues) and its
  own `/dev/shm`; **abstract Unix sockets are scoped per app**.
- The optional VPN-only guard and encrypted DNS upstream of the resolver
  are rules in this egress filter and resolver (F6 in
  [features.md](features.md)).

Tests: in Phase 5, a cross-identity loopback connection without the
grant is refused on the loopback-only stack. In Phase 6: revoke the
network grant under a live TCP connection; an app without an inbound
grant that listens on the wildcard and on a non-loopback address is
unreachable from another machine, and with a per-port grant exactly that
port is reachable; a refused identity gets SERVFAIL from the resolver
and no query for it leaves the machine; the OAuth callback flow of a
named app works.
Exfiltration-suite rows, each its own row: an app holding Documents and no
network sends a canary through open-URI with an `https` URL (Phase 5,
with a networked test app as the URI handler), through a notification,
the email portal, a `<canary>.example` lookup, a cross-identity loopback
relay and an mDNS-advertised IPP printer through the print portal
(Phase 6), and through its own loopback page opened in
the browser (Phase 7); each is refused or shown in the prompt.

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
  out real programs; apps ask through portals, which are asynchronous. The
  one exception is the first-use network request (above): a lookup,
  `connect` or send waits at most 30 s, and only when a human must
  decide.

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
(one process per device) passed typed-code confirmation. Before Phase 6
(no device authorization yet) the prompt terminal accepts USB input only
from the device that typed the unlock secret this boot: the loader records
the EFI device path behind `ConIn` and hands it over, and the kernel's
reader trusts that device; if no path is known, the reader asks for a
typed code after hand-over. Unencrypted installs use the keyboard that
types the login password. Phase 5 test (the B1 swtpm boot rerun at
max(5, 4b)): a `usb-kbd` hot-added after unlock can't open or answer a
prompt. Bluetooth keyboards and 2.4 GHz receivers never count unless the
user opts that keyboard in by name;
`uinput` devices and the compositor's virtual keyboard and pointer never
count, and neither do events produced by the user's key remapping, which
applies only to app surfaces (F17 in [features.md](features.md)). The
polled xHCI boot-keyboard reader stops when the Phase 6 xHCI driver
binds, and resume PIN entry uses the in-kernel paths. Ordinary
input is gated too, with the mechanisms and the one HID definition in
[drivers.md](drivers.md) "Device authorization": every new USB device
(identity VID/PID, serial if any, descriptor hash and port path) gets no
driver binding until approved, an interface with keyboard/keypad or
system-control usages additionally needs the typed code, known devices
are remembered, and every USB, Thunderbolt and other PCIe hotplug port
refuses new devices while the screen is locked (devices already present
keep working); Bluetooth stays off until a device is paired, and input from the
Bluetooth stack is accepted only for paired HID devices whose link keys
the tier-2 transport holds. Attack rows: a compromised Bluetooth stack or
HID process injects the secure-attention key and approves a prompt, and a
compromised Bluetooth stack injects keys into a focused terminal; all fail
(Phase 6, the first extended in Phase 7 to the compositor path).
The user's own transfers (a file-chooser pick, a cross-identity drop, a
paste) count only when driven from a confirmed device: a keyboard that
may answer prompts, or a pointer the user confirmed by clicking a target
the prompt layer shows when that pointer is first used for one; from any
other device the transfer is refused. Exfiltration row (Phase 7): a
BadUSB mouse drags `~/Documents` into a networked app and pastes the
clipboard into it; nothing arrives.

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
  in Settings or drags the file onto the window. Files such an app saves
  into its private home (`~/Documents/x.blend` there) are found through
  Files, which shows each private home's XDG folders in a trusted-path
  browser with an export action. Test (Phase 7): Blender saves
  `~/Documents/x.blend` and the user finds and exports it in Files.
- **Network:** outbound access, and each inbound port, is requested through
  the network portal or declared at install; the first-use request is
  raised for the app (above).
- **Camera, microphone, screen capture, location, background clipboard
  read, notifications, background running, real-time scheduling (the rtkit
  interface), USB devices:** the portal decides once per the prompt level,
  the answer is remembered and can be changed in Settings, and the app gets
  a capability (often an fd, such as a PipeWire stream). Camera,
  microphone, screen capture and screenshot, location, background
  clipboard-read, USB-portal raw device, `hidraw`, serial and
  screen-reader grants are in the component's data set (above), so with
  network in the component they need the combined approval at every
  level. A screen reader reaches other apps' text only through the
  per-app accessibility proxy (Flatpak's `--a11y-own-name` model), and the
  screen-reader grant reads only: AT-SPI `Action`, `EditableText`,
  `Value.Set`, `GenerateKeyboardEvent` and global key listeners are input
  injection, in the floor and never toward the terminal or the prompt
  layer. The input method, which sees every keystroke, is a TCB service,
  never an app; its conversion engines (Mozc, libpinyin, Rime, Hangul) run
  as sandboxed non-TCB helpers (keys in, candidates out, no network, their
  own dictionary per identity, nothing learned from password fields)
  ([`channels.toml`](channels.toml) rows; hostile and fuzz tests at
  Phase 7). Screenshots, screen casting, remote desktop and input capture are
  in the floor below. The USB portal refuses raw access to audio (class
  0x01) and video (class 0x0E) interfaces; those devices are reached only
  through PipeWire, where F1's indicators and kill switches apply. Trusted indicators for the camera,
  microphone and screen capture, per-sensor kill switches and the privacy
  dashboard over these grants are F1 and F2 in [features.md](features.md).
- **Every portal** is a row of [`design/channels.toml`](channels.toml)
  with its landing phase, what it may send off the machine and its test.

**Prompt levels.** The user picks one level in Settings: **Always ask**,
**Only when suspicious** (the default) or **Almost never**. The level is a
TCB setting stored by the broker and changed only on the trusted path (the
prompt terminal, later the compositor's prompt layer); no app can change
it. A **prompt** here is a dialog the user
didn't open: an install review page, a file chooser pick and drag and
drop are the user's own actions and aren't prompts (install
confirmations are counted and published separately, below).
- *Always ask:* every new grant prompts.
- *Only when suspicious:* a request prompts if it is on the closed
  **suspicious list**, or in the floor; any other grant is given silently.
- *Almost never:* only the floor prompts.

The suspicious list (each entry with its test phase). An executable is
**signed** when it carries a store signature, a Flatpak remote signature,
or a signature by a publisher key the user approved; its **age** is the
age of its provenance timestamp (above), never when its hash was first
seen.
1. executing a file with app, downloaded or removable provenance by an
   identity other than its provenance identity, or an unsigned executable
   less than 7 days old; the terminal running a file with terminal
   provenance never matches (5; tested with the clock set back);
2. any identity's first network request while its component's data set is
   non-empty (the data + network combination) (5);
3. any inbound listener grant, and any remote-control access: screen
   casting, remote desktop, input capture, the virtual keyboard and pointer
   (inbound in 6, the rest in 7);
4. a new USB device of network or storage class, or a new HID interface
   without keyboard/keypad or system-control usages other than a
   pointer-only or gamepad interface (consumer-control-only headset
   buttons, FIDO keys; [drivers.md](drivers.md) "Device authorization")
   (6);
5. a device presenting a different class than it did last time (6);
6. any grant requested while the screen is locked, or requested before any
   user input to the identity's own surfaces (5).

A network-only first use (an empty component data set) is suspicious only
by entries 1 to 5, never by entry 6.

The **floor** prompts at every level: inbound access, remote control,
screen sharing and screenshots (every grant in entry 3, and the
screenshot portal); a new HID interface with keyboard/keypad or
system-control usages (the typed code, [drivers.md](drivers.md) "Device
authorization"); any
PCIe device or tunnel added after boot on any hotplug port, refused while
locked; a desktop launch of a downloaded executable; AT-SPI input
injection; a terminal exec of a registered app with argv beyond paths and
URLs; a terminal update's lineage carry-over for an app holding network or
a non-empty private home; and data + network
for one identity or component (every combined approval). It is the
remote-read promise, and Settings explains it in those words. A grant
given silently is logged with its time, app and the reason it didn't
prompt, listed in Settings and revocable there. Tests: at "Only when
suspicious" each entry prompts and a request outside the list doesn't; at
"Almost never" each floor case still prompts, with microphone + network,
camera + network, location + network, background clipboard + network and
screenshot + network and screen reader + network each prompting exactly
once (Phase 7), USB storage + network, serial GPS + network, a hot-added
PCIe device and a Thunderbolt tunnel each prompting (Phase 6); an
app's attempt to change the level (D-Bus, settings files, input
injection) fails (5, with the entries in their phases above). Prompt
budget (Phase 7): the one typical-day script, its expected count per step
and the target (at most 5 prompts per typical day at the default level,
counting every prompt the user did not start; install confirmations
counted and published separately) are in [benchmarks.md](benchmarks.md),
row "prompts per typical day".

**Settings before Phase 12.** Until the Settings app lands, every
"Settings" screen named in this file is a trusted-path **settings
console**: on the prompt terminal from Phase 5 and in the compositor's
prompt layer from Phase 7. Screens by phase: prompt level, silent-grant
log and revocation, document and folder grants (5); Bluetooth pairing and
VPN designation (6); kill switches, the private-home browser and the
Flatpak install review page, which shows a Flatpak app's permissions and
records the approval before the store exists (7).
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
no, shown at install; inbound ports, each prompted); device
classes (GPU render node, gamepad, audio, camera); default folders (each shown
at install and removable); D-Bus names it owns and talks to; and extra
features (`debug own app`, `io_uring` off, FUSE, child identities and their
shared channels for launchers, virtual keyboard and pointer). Each is
shown at install. The format and the launcher's reader land in Phase 5.

Flatpak permissions map onto the same grants:

| Flatpak permission | Optimal grant |
|---|---|
| `--share=network` | outbound network, shown at install |
| `--filesystem=home` or `host` | home minus protected paths, persistent, overlaid as in the file view with `~/.var/app/<app id>` at its own path, so it needs the combined data + network approval if the app also has network |
| `--persist=<dir>` | `~/.var/app/<app id>/<dir>` mounted at `~/<dir>` |
| `--filesystem=xdg-documents` and similar | that folder, persistent |
| `--device=dri` | GPU render node |
| `--device=all` | GPU, gamepads, approved `hidraw`; never keyboards, `uinput` or block devices |
| `--socket=wayland`, `pulseaudio`, `x11` | Wayland security context, PipeWire playback (recording needs the portal), own XWayland server |
| `--talk-name`, `--own-name` | D-Bus proxy policy |
| `--talk-name=org.freedesktop.Flatpak` (`flatpak-spawn --host`) | never passed to the host's Flatpak service; the launcher answers `HostCommand` itself with a prompt naming the app and showing the full command line, and an approved command runs under the terminal identity over a pty and pipes; its output to the app is a direct channel from the terminal, checked as above; every other method is refused with `org.freedesktop.DBus.Error.AccessDenied` |
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
devices only inside the caller's own network namespace (milestone 8,
Phase 6, because it needs the full network stack). The one exception is
the VPN app the user designates on the trusted path for the VPN-only guard,
whose TUN interface is in the system network namespace (F6 in
[features.md](features.md)). It holds no `CAP_NET_ADMIN`; the **VPN
portal** (Phase 6) sets addresses, routes and DNS on that designated TUN
only, on the VPN identity's request, and logs every endpoint update it
makes, so roaming and mesh VPNs work. The VPN identity's netlink surface
is rtnetlink address, route and rule requests on its own TUN, filtered by
the VPN portal's policy; nfnetlink is absent. Tests (Phase 6): an
unchanged WireGuard config through `wg-quick` with
`WG_QUICK_USERSPACE_IMPLEMENTATION=wireguard-go` (bash in the test image)
and an unchanged OpenVPN config connect; Tailscale with
`--netfilter-mode=off` (a listed divergence) keeps its tunnel while
roaming between two networks.

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
  side, including the identity-forge cases: replace a library or a
  `helper.py` beside a granted app, replace a binary at the same path, run
  two Python apps and two `-c` identities and check they get two
  identities and disjoint homes, install a same-ID app under another key
  as an administrator, plant a foreign-provenance hook or unit and try to
  run it as the terminal or through the service manager, and exec a
  registered app from the terminal with `ELECTRON_RUN_AS_NODE=1` or
  `--load-extension`. Each channel and
  portal is gated in the phase its `channels.toml` row names: D-Bus, kernel
  keyrings, the document portal, the Flatpak launcher and portal and the
  loopback-only stack and the resolver in Phase 5, PipeWire, the
  notification, email, print and VPN portals and full sockets in Phase 6,
  Wayland, XWayland, the clipboard, DnD, the camera, location and
  screenshot portals, the remote-control portals, the accessibility proxy
  and the input method in Phase 7. Each TCB service named here (the
  launcher and launch relay, broker and portals, the SSH agent, GPG agent,
  secret service, resolver and input method included) has an A3 row in
  [threat-model.md](threat-model.md) with 10^8 protocol fuzz executions
  gated in its landing phase.
