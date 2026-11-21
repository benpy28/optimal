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
| `ptrace`, `process_vm_readv/writev`, `/proc/pid/mem` | need a **debug** capability on the target, which by default only the parent within the same app gets (like Yama scope 1), and never across apps without a user prompt |

**Revocation of Linux objects** (what an app sees when a grant is withdrawn,
for example the user revoking an app's access to a folder): the next operation
on the fd fails with `EIO` (`EBADF` once the app closes and the slot is
reused); a revoked file mapping delivers `SIGBUS` on next access; a revoked
socket returns `ECONNRESET`. Programs already handle all of these, because
Linux produces them for removed devices and network file systems.

## App identity and the file view

- **App identity** is fixed when an app is launched, from its signed manifest
  (below), and is inherited across `fork` and `execve`. A process can't change
  its own app identity. Launching another app goes through the app launcher
  portal, which starts it under its own identity.
- **Each app has its own file view**: a mount namespace built at launch from
  its manifest. It contains the system image (read-only), its own data
  directory, a private `/tmp`, the device nodes it is allowed, and nothing
  else. The user's files appear only when granted (below). Paths that aren't
  in the view don't exist (`ENOENT`), which programs handle better than
  `EACCES` for things they didn't need.
- **Unix permissions still apply, inside the view.** Every file check is
  first the normal Linux discretionary check (uid, gid, mode, ACLs) and then
  the capability check. Both must pass. This keeps multi-user permissions
  and programs that check `access()` working.
- **uids.** Each human user has a uid; each app runs under that user's uid
  inside its own view. Apps can't see other users' files because those
  aren't in their view and are under other users' keys.
- **setuid binaries.** The setuid bit has no effect on Optimal (like a
  `nosuid` mount everywhere). Tools that relied on it (`sudo`, `passwd`,
  `ping`, `bwrap`'s setuid mode) talk to the broker instead, or, for `bwrap`
  and browsers' sandboxes, use unprivileged user namespaces, which Optimal
  supports ([compat.md](compat.md)). `sudo` is replaced by an
  administrator-action portal that asks the user.
- **Denials never hang.** A call the app isn't allowed returns `EACCES`,
  `EPERM` or `ENOENT` immediately. Permission prompts never happen inside a
  system call, because a syscall that blocks on a human would hang or time
  out real programs.

## Granting access: portals

Prompts and grants go through **portals**, user-space services in the TCB that
speak the xdg-desktop-portal D-Bus interfaces, so Flatpak-aware apps work
unchanged:

- **File chooser:** the user picks files or folders in a dialog drawn by the
  compositor; the portal hands the app fds (or bind-mounts the folder into its
  view). The app never sees the rest of the tree.
- **Camera, microphone, screen capture, location, notifications, background
  running, network for apps with file grants:** the portal asks once, the
  user's answer is remembered and can be changed in Settings, and the app gets
  a capability (often an fd, such as a PipeWire stream).
- **Prompts can't be faked.** Only the compositor can draw portal prompts, in
  a layer apps can't draw over or capture, with the app's verified identity
  shown.
- Apps that don't use portals still work for everything in their own view;
  they just can't reach user files beyond what was granted at install.

## The app manifest

Every app ships a signed manifest (part of the one app format in Phase 12;
for unpackaged programs launched from a terminal, a default profile below):

- identity (name, publisher key),
- requested static grants: device classes (GPU render node, gamepad, audio),
  network (yes/no), the folders it wants by default (each shown to the user
  at install and individually removable),
- system calls or features it needs beyond the default set (for example
  `ptrace` for a debugger, `io_uring`), each shown at install.

**Programs run from a terminal** inherit the terminal's app identity and view.
The terminal itself is an app; by default it gets the user's home directory
(people use terminals to work on their files) but no other app's data
directories, and that grant is shown and can be narrowed. This is the one
place a broad home grant exists, and it is explicit.

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
  capability for the memory object that owns it. Every sharing path in the
  table above creates such a capability, so the property covers Linux
  sharing (copy-on-write, page cache, shared memory, memfd, ptrace) instead
  of silently excluding it.
- Target 5 covers derivation (never more rights than the parent) and
  revocation (the whole subtree, and the revoked-object behaviours above).
- The hostile-process suite ([threat-model.md](threat-model.md)) tests each
  row of the first table from the attacker side: a process tries to gain
  each kind of access without the capability and must fail.
