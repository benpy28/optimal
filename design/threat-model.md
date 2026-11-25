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
   read files, keys, the screen, the microphone, the camera, location, the
   clipboard or input outside that app's grants, and it cannot send what it
   can read to the network unless the user approved that combination (the
   data-set floor below).
2. **Nothing on the network can talk to the machine** unless the user started
   that conversation or explicitly opened a service. Exceptions, each named on
   the scorecard: Wi-Fi, Bluetooth and NIC firmware parse unsolicited frames
   and are TCB; on D2 machines with radios accepted, the remote promise does
   not hold ("remote DMA exposure", Hardware security tiers below); and one
   exposure is outside the kernel's reach: a provisioned Intel AMT (vPro) management engine
   answers on the shared NIC by itself. A tier-3 MEI/HECI client reads the
   AMT provisioning state, and a provisioned machine shows the named
   exposure "out-of-band remote management: on" at install, in Settings, on
   the scorecard and in F6's limits ([features.md](features.md)).
3. **The vendor (us) cannot read user data either.** No telemetry, no key
   escrow, no remote access, no account required. Updates can change code,
   so update signing is part of the trusted base and has its own defences
   ([boot.md](boot.md)).
4. **A stolen or lost device** that is powered off or suspended gives up no
   user data without the PIN or passphrase, except one named residual:
   frozen apps' memory stays in RAM across suspend, defeated only by memory
   encryption (AMD SME, Intel TME). At suspend the volume key, the
   service-key secret S, the home and swap keys, the class A key and class-B per-file keys are wrapped into the resume object and wiped, the agents' secret memory is AEAD-wrapped under HKDF(R5 ‖ PIN half 2), class B and C keys are wiped (suspend locks first), open vaults
   (F3) are closed and unsealed travel folders (F5) resealed, and the
   resume object needs an Argon2id PIN search even with the TPM's secrets
   extracted. Every key's fate at lock, suspend, panic, hibernate and
   shutdown is the key inventory table in [storage.md](storage.md), which
   generates every canary set. A locked one gives up no data in
   the protected classes, whose keys are evicted on lock. Until disk
   encryption stage 2, the scorecard shows the locked part as "partial".

What it does **not** promise: that an app the user granted access to a file
won't misuse that file. Grants are explicit and narrow so the damage is
bounded: apps start with no file, device or network grants.

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
vendor, and cold-boot attacks on DRAM of a running, unlocked machine. The
optional panic lock (F4 in [features.md](features.md)) wipes the keys of a
running machine at a key chord, unmaps and zeroes the page cache of
encrypted volumes and closes open vaults, but leaves frozen apps' memory
(their tmpfs, shm, memfd, pipe, socket and tty buffers included), the names
of their open files, DMA pins it can't revoke (it revokes long-term pins
of encrypted-volume page cache: GPU userptr mappings, preempting or
killing the context after 50 ms, a non-preemptible context being a named
residual, and io_uring fixed buffers, which keep their registration and
are re-pinned at unlock) and DRAM
remanence, so it narrows that case without closing it. During a panic
lock only the lock screen, the compositor and the tier-3 HID processes of
known keyboards stay unfrozen.
(Memory encryption, AMD SME and Intel TME, is enabled where present from
Phase 11 at the latest, and the scorecard says whether it is on; test in
the table below.) Rowhammer from an app or a web page (A2, A3) is in scope;
its defences are under "Side channels". Firmware TPMs (AMD fTPM, Intel PTT) are
weaker than discrete ones against physical attack (faulTPM showed AMD's can be
broken in hours), and an active bus interposer can defeat any TPM's sealing.
So the PIN is never the only secret: it is stretched with Argon2id (memory
min(1 GiB, RAM/4) but at least 64 MiB, at least 4 passes, passes raised to
reach about 2 s on the machine; the same rule for every passphrase-derived
key, [storage.md](storage.md); the LUKS2 header carries no TPM keyslot, since
Linux can never satisfy the pre-cap PCR 8/9 policy) and mixed into every sealed key: each sealed
TPM object (disk secret, service-key secret S, resume object, hibernation
key) seals only a random R, and the real key material is AEAD-encrypted
under HKDF(R ‖ stretched-PIN half 2) ([boot.md](boot.md)).
An encrypted install needs at least 256 MiB of RAM. A PIN is allowed
only with a discrete TPM; on fTPM machines install requires a passphrase
estimated at 60 bits or more instead. Every on-disk passphrase wrap (the
system volume's, the LUKS2 passphrase slot, each home's, F4's P blob)
requires that 60-bit floor on every tier, or the scorecard publishes
stolen-disk strength as that of the weakest wrap. Against those attacks
disk protection is as strong as the stretched PIN or passphrase, and the
scorecard says so.

## Trusted computing base

Everything below can break every promise if it has a bug. It is kept small,
tested hardest, and listed so nobody pretends the TCB is "just the unsafe
core".

- CPU microcode, platform firmware and device firmware the kernel loads into
  DMA-capable devices: GPU firmware (GuC, MES, PSP, SMU), DSP audio firmware
  (SOF/AVS, ACP), Wi-Fi and Bluetooth firmware (not ours; see hardware tiers). Each blob is pinned by SHA-256 in
  the firmware ledger with its version.
- Every device with ATS enabled, and its firmware: its translated requests
  bypass the IOMMU. Each is listed here per machine and on the scorecard
  ([drivers.md](drivers.md)).
- The bootloader ([boot.md](boot.md)) and the recovery image. `bootloader/`
  falls under [core.md](core.md)'s rules (its own unsafe cap, a TCB budget in the table below,
  an exact toolchain pin) and the proofs job: its floor code is in proof
  target 16's scope, and its crypto (Ed25519 config signatures, SHA-256, and the TPM session crypto:
  ECDH P-256, KDFe/KDFa, AES-128-CFB) is in target 14 ([verification.md](verification.md)).
- The unsafe kernel core (tier 0), every in-kernel safe-Rust component (tiers
  1 and 2 in [drivers.md](drivers.md)), and every dependency of theirs. This
  includes the TPM service, which lives in the kernel: the kernel alone holds
  raw TPM access, and user space reaches the TPM only through the TPM broker
  API, a capability-gated kernel interface that exposes named operations
  (unwrap a user key, derive the caller's F2 day key as HKDF(ratchet, user
  ID) from the one system NV index holding F2's daily ratchet key, which the
  kernel writes once a day and reads under `PolicySecret` on the service
  key), never raw TPM commands. Sealing the hibernation key and bumping the replay counter are
  kernel-internal and not in the API.
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
| Lock screen (on the prompt virtual terminal from Phase 5, in the compositor's prompt layer from Phase 7) | ours | 4,000 lines |
| SSH agent, GPG agent, secret service (permissively licensed, our own; OpenPGP through a pinned MIT/Apache crate counted in the ledger) | ours | 4,000, 8,000 and 6,000 lines |
| Store service and Flatpak launcher | ours | 15,000 and 5,000 lines |
| Document portal file system (tier 3, FUSE protocol) | ours | 6,000 lines |
| Storage service, log service | ours | 8,000 and 3,000 lines |
| Update client (TUF metadata, transparency proofs) | ours | 8,000 lines |
| DHCP client, resolver, time service | ours | 4,000, 10,000 and 3,000 lines |
| Wi-Fi supplicant | ours | 20,000 lines |
| Compositor, with XWayland servers' host side | upstream, pinned, patched | 60,000 lines of compositor code |
| Mesa driver loaded into the compositor | upstream, pinned | measured size; only the present GPU's driver is installed. Exposure: it imports app buffers (dma-buf, shm) for compositing; it never compiles app shaders or runs app command streams, which go to the app's own Mesa and the kernel GPU driver |
| Audio server permission layer | PipeWire, pinned | measured size |
| Per-app accessibility (AT-SPI) proxy | ours | 3,000 lines |
| Input method service (IBus/Fcitx-style, never an app; sees every keystroke of the focused app; Wayland input-method, IBus and Fcitx D-Bus and XIM front ends, candidate window). Conversion engines (Mozc, libpinyin, Rime, Hangul) are not in it: they run as sandboxed non-TCB helpers (keys in, candidates out, no network, own dictionary per identity, nothing learned from password fields) | ours | 25,000 lines (front ends and candidate window only) |
| Bootloader (Optimal Boot, all stages) | ours | 15,000 lines, of which at most 3,000 are `unsafe` tier-0 code ([core.md](core.md)); the `core-crypto` crate it shares with the kernel (cap 7,500 lines, including the TPM session crypto the loader uses: ECDH P-256, KDFa and KDFe, AES-128-CFB) is counted once, in the kernel's ledger |
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
| **B4** | Legacy BIOS | as B3. Until Optimal Boot's BIOS stage lands (bootloader track), B4 boots through static A/B Limine entries with a manual fallback, and the scorecard says "B4: no automatic rollback, no floor" |

Until Optimal Boot path 1 or 2 works on a given PC, that PC is B3, and the
scorecard says "verified boot: only with owner keys enrolled".

**DMA axis**

| Tier | Needs | What holds |
|---|---|---|
| **D1** | an IOMMU with every PCI segment covered, every IOAPIC and HPET in scope, interrupt remapping on, compatibility format blocked, and x2APIC where present; and firmware pre-boot DMA protection that maps per buffer (`EDKII_IOMMU_PROTOCOL` backed by the IOMMU, such as `IntelVTdDxe`, or the AMD equivalent). The loader detects it by locating the protocol and records GSTS.TES/PMEN or IommuEn and the DMAR/IVRS `DMA_CTRL_PLATFORM_OPT_IN` flag at its first instruction, passed in the hand-off. The loader keeps the firmware's tables for all firmware I/O (disk reads, the PIN through `ConIn`, the TPM); after the last firmware I/O and before `ExitBootServices` it enables Intel PMRs over the pool, loader and kernel and clears Bus Master Enable on every bridge and root port (both vendors) except those on the firmware display's path when the GOP framebuffer lies in system RAM ("firmware display DMA", naming every function below a kept bridge); after `ExitBootServices` it loads its own deny-all tables; bridge BME stays clear until the kernel re-enables it per device ([boot.md](boot.md) "DMA before the kernel", [drivers.md](drivers.md) "Pre-boot protection"). Root-complex integrated devices (no bridge) show the named "EBS-gap window (integrated devices)" unless translation stays on through `ExitBootServices`. On S3 resume the waking vector re-arms the IOMMU first, then clears and restores bridge BME, before any device or interrupt is re-enabled | DMA attacks blocked; per-device isolation. A machine meeting every other D1 condition without per-buffer firmware protection is **D1 with pre-loader window** (its own scorecard column), not clean D1: the scorecard names "pre-loader DMA window (firmware code)" (a device can patch firmware code that later runs with the PIN live). One booted with neither firmware protection nor Optimal Boot's tables shows a named "boot DMA window": from power-on until the kernel's IOMMU setup, a device can DMA into memory, including the kernel image after the loader checked it; a Limine boot (before 4b) is clean D1 only on firmware that keeps translation or covering PMRs on through `ExitBootServices`, and shows that window otherwise. Firmware without S3 boot-script protection shows a named "S3 resume DMA window". User mappings of hot-removable devices' BARs are the named exposure "removable-device BAR stalls", outside the real-time gate under device stalls ([scheduler.md](scheduler.md)) |
| **D2** | no IOMMU, or an IOMMU short of D1's conditions; the reason is named. A machine with no usable IOMMU and no firmware pre-boot protection is D2 | A named "pre-loader secret DMA window" exposure on the scorecard: the PIN and the secrets unsealed before hand-over are DMA-readable. Devices the IOMMU does cover keep their domains. Other DMA attacks from a malicious device, or from compromised device firmware, are **not** blocked. External PCIe tunnels are refused: from Phase 1c Bus Master Enable stays clear on every external port's bridge or root port (a device's own BME is never relied on), where "external" means the ACPI `ExternalFacingPort` property (`_DSD`) or Thunderbolt/USB4 topology, decided in the kernel only, never the hotplug-capable bit alone; below a non-external hotplug-capable port, devices present at boot may master and devices added after boot are refused; on firmware connection-manager controllers the ICM driver refuses the tunnel. A tier-1 or tier-2 driver bug can't aim DMA at kernel data: `DmaBuffer`s and rings come from a dedicated DMA region and only streaming maps are bounced; descriptor memory the device reads is a `DescRing<D>` whose `Iova` fields are built only from granted handles (no raw byte access); PRP and SGL lists come from typed builders; GPU page-table entries are built only from `DmaBuffer`, `Iova` or `VramBuffer` handles (proof target 13 covers D1 and D2, resting on a per-vendor GPU page-table axiom), and a GPU without per-process GPU page tables (pre-PPGTT) gets no acceleration; DMA-address registers (NVMe ASQ/ACQ, ring bases, DCBAAP, display surface, cursor and LUT addresses, SMU DRAM-address messages) are written only by the core from a `DmaBuffer` handle. Tier 3 relies on the IOMMU alone, so on D2 no tier-3 process is given a DMA-capable device. Wi-Fi and Bluetooth radios that do PCIe DMA stay off until the user accepts a warning named "remote DMA exposure", and from then the scorecard says the remote promise does not hold on that machine (USB-attached radios can't DMA and are not affected). Big warning at install, with how to turn the IOMMU on if the firmware can |

## Rules every component follows

### Default deny

Apps start with no file, device or network grants: the default profile has
no network. The network is closed inbound. Devices start unauthorised.
There is no "root that can do anything".

### Data and network together are one explicit grant

The most common real-world "remote read" is an app that can both read data
and phone home. A component's **data set** is its persistent file grants
plus its camera, microphone, screen-capture and screenshot, location,
background clipboard-read and screen-reader (AT-SPI) grants, and every raw
USB-portal, `hidraw` and serial device grant (`data_set = true` in
`design/channels.toml`). So:

- **Network access** is declared in the app's manifest and shown and
  approved at install, or, for a program without a manifest, decided at its
  first outbound use, once per identity and remembered. The first-use
  event is the first non-loopback `connect` or send, or the first name
  lookup through the resolver (below), from an identity with no recorded
  answer; it queues one request for that identity to the broker. Silent
  decisions are made synchronously from the level policy the broker pushes
  into the kernel and the resolver, so no app sees a failure when no human
  is needed. When a human is needed, the lookup, `connect` or send waits
  up to 30 s for the decision (socket and poll behaviour and the full
  trigger list in [capabilities.md](capabilities.md) "First use"); on
  refusal or timeout the lookup answers
  SERVFAIL (`EAI_AGAIN`) and `connect` returns `ENETUNREACH`, never
  `ECONNREFUSED`. It can also be requested through the network portal
  ([capabilities.md](capabilities.md)). An identity whose component holds
  any of the data set is always prompted, at every level (the data-set
  floor). Inbound
  and listening ports and remote-control access (remote desktop, screen
  sharing, input injection) are separate explicit grants, never implied by
  outbound network, and always prompted. Network access is enforced by a
  per-identity egress filter checked on every connect and send, not by
  swapping network namespaces, so a grant or revocation reaches sockets that
  are already open. A network grant is **outbound only**. An identity whose
  request was refused gets `ENETUNREACH`, immediately, for any non-loopback
  destination. Loopback
  (a loopback-only TCP/UDP stack from Phase 5, the full stack from Phase 6)
  is per identity: an app reaches loopback ports bound by its own identity,
  and reaching another identity's port (a dev server started from the
  terminal, opened in the browser) needs a per-port grant naming both; that
  connection is a direct channel (below). **Binding always succeeds.** A
  socket bound to a wildcard address receives only loopback connections, and
  inbound unicast traffic to a non-loopback address is dropped, unless the
  identity holds an inbound grant for that port.
- **DNS** goes through the system resolver at `127.0.0.53` (named in
  `resolv.conf`), a TCB service, not an identity's loopback port, that
  resolves only for identities holding outbound network; a lookup from an
  identity without it is the first-use event above, and a refused identity
  gets SERVFAIL, so lookups can't carry data out. The kernel stamps the
  sender identity on loopback datagrams and TCP connections to the
  resolver (an Optimal control message), which is how it knows who asks.
  `resolv.conf` ships `options timeout:30 attempts:1`, so stub resolvers
  wait out the 30 s hold; the resolver dedupes retransmits per identity
  and answers SERVFAIL within 5 s on upstream failure. Upstream it uses
  encrypted DNS to a default provider pinned by IP address (no port-53
  bootstrap, no DDR by default); until NTS has set the clock, certificate
  checks use the signed time floor with a not-before check only (F6 in
  [features.md](features.md)).
- **The data set** (persistent file grants, that is folders or files
  granted for longer than one use, and the sensor, screen, location and
  clipboard grants above) and network access can be held together only if
  the user approved that combination explicitly, in a prompt naming each
  source in plain words ("This app will be able to read *Documents*, use
  the microphone and send data to the internet"). That prompt is part of
  the floor: it appears at every prompt level. The USB portal refuses raw
  access to audio (class 0x01) and video (class 0x0E) interfaces; those go
  through PipeWire, where F1's indicators and kill switches apply.
- **A file the user picks** in the file chooser for one use (an upload, an
  attachment, "open this photo") is handed to the app as a one-use document-portal entry even if the
  app has network access, because the user chose to give that file to that
  app. It doesn't widen the app's grants.
- **Secrets are used, not read.** SSH keys, GPG keys, browser and app
  passwords, and tokens live in agent services (the SSH agent, the GPG
  agent, the secret service), which are TCB services with budgets in the
  table above. Programs use them through the agent's socket; the agent
  confirms the first use per session with a prompt through the broker
  naming the requesting app (see below). The files themselves (`~/.ssh`,
  `~/.gnupg`, keyrings, browser profiles, and every app's private home,
  keyed by its identity ID and mounted as that app's `$HOME`; for a Flatpak
  app `~/.var/app/<app id>` at its own path, as on Flatpak,
  [capabilities.md](capabilities.md)) are **protected paths**, never part
  of any folder grant.
  So are the places that decide what runs with which identity:
  `~/.config/autostart`, `~/.local/share/applications`, `~/Applications`,
  `~/.local/share/dbus-1/services`, `~/.config/systemd/user`,
  `~/.config/environment.d`, `~/.config/mimeapps.list`, the whole install
  directory of every desktop-launched unpackaged program (its executable,
  libraries and data files such as `app.asar`), and for an interpreted app
  its script's own directory tree. These are written only through the
  launcher portal, except that the install directories and script trees
  stay visible and writable to the terminal identity and are protected
  only against other app identities (lineage and provenance reset grants
  when another writer changes them); a directory whose files all have
  terminal provenance is never protected, and only its closure hashes are
  recorded. The shared settings projected into every view
  (`~/.config/gtk-3.0`, `~/.config/fontconfig`, `~/.local/share/fonts`) are
  protected paths written only by the settings portal. `~/.local/bin` is protected from
  every identity except the terminal's, which can write it so
  `pip install --user` and `cargo install` work; what is written there
  carries the kernel provenance attribute (below).
- **Writable grants name their readers.** Data an app writes into a folder
  that another app may read is reachable by that app, so a folder one
  identity can write and another can read is a direct channel from the
  writer to the reader (below), and the prompt for a writable folder grant
  names every app holding network together with a grant to that folder.
  The terminal's reads of the home don't count as a channel: executing a
  file another identity wrote is refused by provenance until approved
  (below), and the rest is data. Files the terminal's tools run without
  exec (`Makefile`, `.envrc`, a sourced script) are covered by the grant
  instead: a writable grant to a folder inside the terminal's home says
  "*App* can run code as Terminal" in its prompt, and that prompt is in the
  floor whenever the terminal holds network.

### The terminal

People use terminals to work on their files, so the terminal app gets the
user's home directory **minus the protected paths**, and that is the one
broad home grant in the system. It is not given network access by default;
the first time a program run from it needs the network, the call waits
under the first-use hold ([capabilities.md](capabilities.md) "First use")
while the terminal shows the floor prompt for the "terminal: home +
network" combination, with the warning above; approved within 30 s, the
call succeeds (test: `curl` from the terminal approved at 25 s succeeds on
its first run, 6). Programs run from the terminal inherit the terminal's identity
(that's what makes `git`, `make` and `cargo` work unchanged), except a
registered app's executable: when the terminal execs one (`firefox` run
from a shell), it is an identity exec through the launcher under that
app's own identity, and the exec is the `launch-relay` direct channel
terminal → app, checked by the combined approval like any other channel;
the environment is an allowlist, argv tokens beyond existing paths and
URLs are a floor prompt naming the app, and only fds 0 to 2 that are ttys
get a fresh pty ([capabilities.md](capabilities.md) "Registered apps run
from a terminal"). A user who grants the terminal network is
explicitly trusting what they run there; protected paths stay out of reach
either way, which is what keeps `curl | sh` from reaching SSH keys. A
terminal-identity process that execs a file whose provenance is another
identity, or "removable", is decided by the prompt level (suspicious-list
entry 1) and gets `EACCES` until the user approves that file in a prompt
naming its provenance. A file with terminal provenance runs as the
terminal's own code: no prompt and no `EACCES`, whatever its "downloaded" tag,
so `git clone`, `cargo build` and `./target/x` just work. Updates the terminal itself
writes to an app installed from the terminal (`pip install --upgrade`) count
as the same owner after one prompt, a floor prompt when the app holds
network or a non-empty private home. When the launcher spawns a different identity from a
terminal (`flatpak run`, a desktop file), that process gets a fresh pty, so
it can't inject input into the terminal. The launcher relays that pty's
output to the terminal with OSC 52 (clipboard write), title-set and
report-request sequences removed, so the terminal ignores them from every
foreign identity (lockdown table).

### App identity can't be forged

Grants belong to an identity, so the identity has to be unforgeable:

- A packaged app's identity is its store or Flatpak ID **plus its
  publisher's or remote's signing key**; a change of key is a new identity,
  which resets its grants and its private home, so an administrator's
  system-wide app with the same ID inherits nothing. An unpackaged
  program's identity is its **path plus the hash of its launch closure**:
  the executable, its `PT_INTERP` and every library loaded from a path
  outside the system image; for an interpreter, the interpreter, the
  script, every file the interpreter opens from the script's own directory
  tree, and the site-packages and classpath roots listed for the app (plus
  the argument string for `-c` style calls, so `python3 a.py`, `python3
  b.py` and `sh -c '<x>'` are three identities); for an AppImage, the
  AppImage file. When the hash changes, its grants reset, except that
  grants carry across an update whose writer's provenance is the app
  itself (a self-updater such as Steam or a Firefox tarball), or, for a
  child identity, its declared parent writing paths the parent's manifest
  shares with it (Steam updating a game or Proton).
- Grants and the private home are keyed by the **identity ID** = (path,
  lineage ID, argument-string hash). The lineage ID is created at the
  identity's first launch and carried only across the provenance-approved updates
  above, so a binary replaced at the same path by another writer gets an
  empty home, and two `-c` identities get disjoint homes
  ([capabilities.md](capabilities.md)).
- An executable written by an app identity carries that identity as its
  provenance, in a kernel-set extended attribute no app can write or
  remove. Files copied from removable media carry provenance "removable".
  The "downloaded" tag has one definition, in [capabilities.md](capabilities.md)
  "Provenance": set by the kernel (never on the writer's say-so) on a file
  written into a download folder, received through a portal from a
  networked identity, or written by a tainted process: for every
  identity, one that has read from a non-loopback socket or read a
  downloaded file (a low-water mark like "removable", across `fork` and
  `execve`), or read a tainted process's data through a pipe or Unix
  socket, or mapped shared memory it writes (applied at map time); for a
  non-terminal app identity it is identity-wide. Files an identity writes inside
  its own install directory or closure keep its provenance. A desktop
  launch (file manager, launcher, portal open) of a downloaded executable
  is in the floor: it runs as a new identity with no grants and no network
  until a prompt naming it and its provenance is approved. Exec of an app-
  or removable-provenance file by another identity follows the prompt
  levels; from the terminal it gets `EACCES` until approved (above). The service manager and D-Bus
  activation apply the same provenance rule to what they start.
- Only the launcher and the service manager hold the kernel capability
  "spawn with identity X". Every other process's children get the parent's
  identity, with grants only within the parent's.
- Identities that share a direct channel (parent and child, D-Bus peers,
  shared IPC objects, a loopback connection between them, a folder one can
  write and the other can read, and traffic one relays to the other through
  PipeWire or D-Bus) are checked **as one principal**. Channels are
  directional, writer to reader; two-way channels (sockets, D-Bus, shared
  memory) point both ways. Each session keeps a graph of identities joined
  by any direct channel, live or past, until logout; edges created by
  grants (a folder one identity writes and another reads) are rebuilt at
  every login. The data a networked identity can reach is the data set of
  every identity with a path to it, and it must be covered by a **combined
  approval**: one prompt that names each source and the networked members,
  asked again whenever the component grows. The check runs on every new
  channel and on every new grant (network, folder, sensor, inbound port,
  persistent document) to any member, against the whole component; its
  invariant is proof target 18 (B/T at 5, P/T at 7,
  [verification.md](verification.md)). A channel open that needs a combined
  approval not yet given is refused at once (`ECONNREFUSED`, or
  `AccessDenied` on D-Bus) and queues one combined-approval request per
  identity pair; after approval a retry succeeds. A grant that needs one
  prompts, and refused it is refused at the moment it is asked for. Either
  refusal carries a reason the app and Settings both show ("*App* shares files with
  *Other app*, which has the internet"). Each app has its own IPC namespace
  (System V IPC, POSIX message queues, abstract Unix sockets). The overt
  channels are a closed list checked in as `design/channels.toml`, listing
  every portal and channel, each with a test; covert channels (CPU and
  cache timing, `statfs` free space, lock contention) are out of scope, and
  their measured bandwidth is published on the scorecard.

### Prompts can't be spoofed

From Phase 5, portal and agent prompts appear on a dedicated virtual
terminal that only the kernel switches to, reached by a secure-attention key
(Ctrl+Alt+Delete) that the kernel handles before any app sees it and that
injected input can't produce. From Phase 7 the compositor draws them in a
layer apps can't draw over, capture or cover
([capabilities.md](capabilities.md)). Each prompt shows the requesting app's
attested identity. The camera, microphone and screen-capture indicators and
their kill switches are drawn in the same layer (F1 in
[features.md](features.md)).

Input is trusted by its source, not its content. The kernel tags every input
event with the device that produced it. The secure-attention key and every
prompt answer are accepted only from an in-kernel parser (PS/2, USB boot
protocol, and the in-kernel I2C-HID parser for internal laptop keyboards,
promoted with the in-kernel fuzz thresholds) or from a per-device HID
process (one process per device) whose keyboard interface passed the
typed-code confirmation; never from a mouse, gamepad, consumer-control-only
device (headset buttons) or FIDO key, and never from a Bluetooth keyboard
or a 2.4 GHz receiver unless the user opts that keyboard in by name (rules
in [drivers.md](drivers.md) "Device authorization"). Before Phase 6 the
prompt terminal accepts USB input only from the device that typed the
unlock secret this boot: the loader records the EFI device path behind
`ConIn` and hands it over, and if no path is known the kernel's reader asks
for a typed code after hand-over; an unencrypted install uses the keyboard
that types the login password. The kernel also limits what any HID device
can send: an in-kernel descriptor classifier (a target 15 parser) stamps
each device's allowed event set, keyboard-range keycodes from a device
outside the keyboard floor are dropped, and consumer-page devices are
limited to a media and volume allow-list. User-action transfers (a
file-chooser pick, a cross-identity drop, a paste) count only from
confirmed devices; a new pointer is confirmed by a click on a target the
trusted layer shows. So a compromised Bluetooth stack or HID
process can't approve a prompt or fake the secure-attention key, and one
hostile USB device can't read another device's keys. The polled xHCI
boot-keyboard reader stops when the Phase 6 xHCI driver binds; in safe mode
it accepts only known keyboards or the one that typed the unlock
passphrase. The PIN on resume from suspend is typed through the in-kernel
paths, which work while user space is frozen.

Bluetooth is off until the user pairs a device, and pairing is confirmed on
the trusted path. Link keys are held in the tier-2 Bluetooth transport, never
in the tier-3 stack, and input events from Bluetooth reach any app only from
a paired HID device's link, so an over-the-air exploit of the stack can't
type into the focused app ([drivers.md](drivers.md)). The residual risk,
compromised radio firmware injecting input on a paired keyboard's link, is
listed on the scorecard.

The lock screen fails closed: if it crashes or is killed, the session stays
locked and the kernel switches to the prompt virtual terminal, which asks
for the login secret. The lock-screen PIN is checked through a
dictionary-attack-protected TPM object (`PolicySecret` plus
`PolicyAuthValue`, no `noDA`), and after exactly 10 failures only the
passphrase unlocks. Without a TPM the lock screen takes the passphrase,
with a persistent exponential back-off stored on disk.

### How often Optimal asks

Prompts are only a defence if people read them, so their number is a
measured budget ([capabilities.md](capabilities.md) has the mechanism and
the closed "suspicious" list). Grants are remembered, defaults are safe, and
wording is plain. The user picks one prompt level, a TCB setting changed
only on the trusted path (no app can change it through any portal or
file): **"Always ask"**, **"Only when suspicious"** (the default) or
**"Almost never"**. Below the chosen level a request is granted silently and
logged, visible and revocable in Settings (before Phase 12, the
trusted-path settings console: on the prompt terminal from Phase 5, in the
compositor from Phase 7). "Signed" in the list means a store signature, a
Flatpak remote signature or a publisher key the user approved; an
executable's age comes from its provenance timestamp under the signed time
floor. A network-only first use is suspicious only by entries 1 to 5;
entry 6 is "requested before any user input to the identity's own
surfaces" and doesn't apply to it. Whatever the level, the **floor**
always prompts, and Settings explains it in plain words:

- inbound remote access, remote control, screen sharing and screenshots
  (the screenshot portal);
- a new HID interface with keyboard/keypad (usage page 0x07) or
  system-control usages, which also types the shown code (a remembered
  keyboard re-plugged into the same port excepted, "USB and external
  devices");
- any PCIe device or tunnel added after boot on any hotplug port,
  including its refusal while locked;
- a desktop launch of a downloaded executable;
- AT-SPI input injection (`Action`, `EditableText`, `Value.Set`,
  `GenerateKeyboardEvent`, global key listeners);
- a terminal exec of a registered app with argv tokens beyond existing
  paths and URLs, and a terminal update's lineage carry-over to an app
  holding network or a non-empty private home;
- the data set and network for the same identity or component (the
  remote-read promise).

Tests: each entry of the suspicious list prompts at "Only when suspicious"
(5 for the executable, first-network-request and locked-or-before-input entries,
with entry 1 also run with the clock set back; 6 for the USB device
entries and inbound listeners, 7 for remote control); each floor item
prompts at "Almost never" (5 for files + network; 6 for a new keyboard
interface, a PCIe device or tunnel added after boot, inbound access, and
USB storage and a serial GPS each with network;
7 for remote control, screen sharing and a double-clicked downloaded
executable; 7 for microphone, camera, location, clipboard, screenshot and
screen reader each with network, one prompt each); an app that tries to change the
level, through every portal and by writing the settings file, fails (5).
The scripted "typical day", with the expected prompt count of each step,
is defined only in [benchmarks.md](benchmarks.md) ("Prompts per typical
day"). The target is at most 5 prompts per typical day at the default
level, counting every prompt the user did not start; install
confirmations (the user clicked Install) are counted and published
separately; both are scorecard rows (7).

### No confused deputies

The launcher portal, the open-URI portal, file and URL handlers and D-Bus
activation never pass a command line, arguments, a `file://` URI or a file
to another app on an app's behalf without a confirmation prompt naming
both apps. A terminal exec of a registered app is the `launch-relay`
direct channel, checked by the combined approval (above). The terminal can't be started with a command by another app at
all.

Every TCB service that sends traffic for an app (the resolver, open-URI,
notifications, the email portal, the print portal) checks the caller's
grants first, because a networked deputy is a way out for an app without
network. For a caller without network, notifications drop remote content
(image URLs) and the email portal shows a prompt naming the recipients,
subject and attachments before the mail client gets them, at every prompt
level. Printing from an app without network needs a prompt naming the
destination printer and its address, so a printer an attacker advertises
over mDNS can't receive a document silently. Open-URI with
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
- **Accessibility and input methods:** each app reaches the AT-SPI bus
  only through its own a11y proxy, which owns only that app's name (as
  Flatpak's `--a11y-own-name`); reading other apps' text is a screen-reader
  grant, in the data set, and reads only; AT-SPI control and key
  listening are input injection, in the floor and never toward the
  terminal or the prompt layer. The input method is a TCB service whose
  conversion engines are sandboxed non-TCB helpers (table above).
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
the TCB, and network configuration and naming (DHCP, router advertisements,
and DNS in the resolver) run in tier 3 or in sandboxed user-space services
([drivers.md](drivers.md)). Wi-Fi and Bluetooth are split on every machine:
a small tier-2 transport part (rings and DMA, no frame parsing) and the
tier-3 stack. Proof target 15, the network-path parsers, lists each parser
with its component: in the kernel IP, TCP, UDP, ICMP and ND; in sandboxed
user-space services DHCP, RA and DNS ([verification.md](verification.md)).
What stays in the kernel (those packet parsers, the system file system, the
boot-protocol keyboard paths, the internal I2C-HID keyboard parser, the HID
descriptor classifier, the radio transport parts, and the
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
| GDS | microcode mitigation; without it, AVX is disabled for apps through XCR0 (gather alone can't be turned off) |
| Rowhammer | page tables come from a separate physical zone with guard rows around it; ECC and TRR errors are read and reported where the platform exposes them; the refresh policy is reported (the memory controller's refresh interval where it is documented, and whether the firmware's double-refresh setting is on, which Settings recommends), and the scorecard says which of these hold and that Rowhammer stays a residual risk |

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

The mechanism and the full rules are in [drivers.md](drivers.md), "Device
authorization" (Phase 6); the rules are:

- **Held until approved.** Every new USB device, Thunderbolt tunnel and
  Bluetooth device is held until approved; nothing it presents reaches a
  driver first. Authorization is **per USB interface**, so a "storage"
  stick with a hidden HID interface gets its storage approved and its HID
  interface held.
- **Identity.** An interface's identity is the device's vendor ID, product
  ID and serial-number string (if any), the interface number, and its
  class, subclass and protocol, the class being that of the driver that
  would bind; a HID interface's identity also includes its report
  descriptor hash and its port path from the root hub, so a moved HID
  device asks again. A known
  identity needs no new prompt. A device whose vendor, product and serial
  match a known device but whose interface classes differ is new, and is
  "suspicious". A remembered identity never relaxes the IOMMU or any other
  check.
- **Who is asked**, by the prompt level (above):
  - A new HID interface with keyboard/keypad (usage page 0x07) or
    system-control usages always prompts, at every level (the floor), and
    its key events are accepted only after it also types the code shown on
    screen (stops BadUSB). A remembered keyboard identity re-plugged into
    the **same port** is accepted without a code; on a different port it
    needs the confirmation again. USB can't authenticate devices, so a
    clone of a known keyboard's VID/PID, serial and descriptor on its port
    is a residual listed on the scorecard. Mice, gamepads,
    consumer-control-only devices (headset buttons) and FIDO keys are
    never trusted for prompts or the secure-attention key; consumer-control
    devices and FIDO keys are on the suspicious list, pointer-only and
    gamepad HID are not at the default level. A 2.4 GHz receiver is treated like Bluetooth: a
    typed code for each newly paired sub-device, never trusted for prompts
    unless opted in by name, never handed to apps as `hidraw`.
  - New storage and network interfaces, and any device presenting different
    classes than last time, prompt at "Only when suspicious" and "Always
    ask"; at "Almost never" they bind silently. Binding a storage device
    never makes its contents trusted: every file on it carries provenance
    "removable" (above).
  - Every other new interface (audio, video, printer, serial and the rest)
    prompts only at "Always ask"; otherwise it binds silently. The USB
    portal never hands an app raw access to an audio or video class
    interface (above).
  - Every silent binding is logged and shown in Settings, where it can be
    revoked.
- **Before unlock and on resume.** Until the volume is unlocked, the
  in-kernel keyboard paths accept any keyboard, but only into the
  passphrase or PIN prompt, where guessing is limited by the KDF and the
  TPM's lockout. After unlock every device that isn't known goes through the
  rules above, including one that typed the passphrase. Keyboards used
  during installation are recorded as known; PS/2 and internal I2C-HID
  keyboards need no approval. On resume only keyboards present before
  suspend type the PIN; one counts as present if its port path, VID/PID,
  serial and descriptor hash match after re-enumeration (an xHCI reset
  across S3 included).
- **While the screen is locked**, all USB and Thunderbolt ports refuse new
  devices; devices attached before locking keep working. A device attached
  while locked binds nothing until unlock, except a known HID interface,
  whose events go only to the lock screen, and unlock then shows "a
  keyboard was reconnected while locked". No new Thunderbolt tunnel or
  other PCIe device added after boot is admitted while locked. Guessing
  the lock-screen PIN is limited by the TPM's dictionary-attack lockout,
  or the back-off without a TPM, and ends at the passphrase after 10
  failures ("Prompts can't be spoofed").
- **Thunderbolt/USB4 PCIe tunnels**, and any PCIe device added after boot
  below any hotplug port, need approval per device, in the floor. Where the
  controller supports the USB4 security challenge, it is used. On firmware
  connection-manager (ICM) controllers the security level is read, never
  set: approved tunnels need level "user" or "secure", and otherwise the
  deny-all IOMMU (D1) or BME clear (D2) refuses the tunnel and the
  scorecard notes it.

### Administrators

An administrator (granted through the administrator-action portal, which
always asks) can install and remove system-wide apps, change system
settings, create and delete accounts, and update the OS. An administrator
**cannot** read another user's files, reset another user's login secret in a
way that unlocks their data (resetting creates a fresh, empty key; the old
data needs the old secret or that user's recovery key; each home's TPM
object is `PolicySecret(S)` AND `PolicyAuthValue(one half of the stretched user secret)`
and its key is HKDF(R_u ‖ the other half), so the system recovery key and S
alone can't open it), install software into
another user's session, or read another user's crash dumps. A system-wide
app the administrator installs with the same ID as one a user granted is a
new identity unless its signing key matches (above), so it inherits no
grants.

### Crash reports stay local

App dumps go to the crashing user's encrypted storage, are redacted of app
memory by default, and are never uploaded unless the user chooses to send
one, each time, after seeing what is in it. Kernel dumps contain **kernel
memory only** by default (no user pages, no page cache, and no slab cache
tagged as carrying user data: keyring payloads, pipe and socket buffers,
tty buffers, dentry names), are encrypted to the administrators' keys, and
are stored in an administrator-only location. Including a user's pages or
tagged caches needs that user's consent, asked of each user whose data
would be included. Every dump excludes key pages. Test (Phase 5): with
canary bytes in a key page, a user's process memory, a user's page cache,
a keyring, a pipe, a tty, a socket buffer and a file name, a forced kernel
crash produces a dump that holds none of them unless that user consented,
and no key bytes in any case.

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
| hibernation | entered only from a running unlocked system, refused before any wipe if the image-size bound doesn't fit; open vaults (F3) are closed, and class-C keys and pages evicted, before the image is written, so the image holds none ([storage.md](storage.md)). The image lives in a dedicated hibernation extent, encrypted and authenticated under a fresh image key, and records the release, its security version, the commit generation and the replay value. On B1 the image key is AEAD-encrypted under HKDF(R ‖ stretched-PIN half 2), where R is sealed at hibernate to the signed pre-cap policy (PCR 4, 8 and 9 signed, PCR 7, the rollback floor, `TPM2_PolicyAuthValue`), and that blob goes in the header block, never into a persistent TPM object; the complete next header-block copy (version, CRC, tags) is built before the wipe and written after the snapshot. Otherwise the key comes from the passphrase. Resume unseals before capping, so it needs the PIN; the boot kernel hands the volume key and S to the restored kernel in a reserved page the restored kernel wipes, and the resumed file system draws a new nonce prefix. Any normal mount first erases the image blob. Resume refuses an image it can't authenticate, whose version is below the rollback floor, whose release differs from the booting one (the loader keeps the old slot for the resume boot), whose generation doesn't match, or whose recorded (mount count, journal sequence) differs for any volume mounted at hibernate, homes included; a hibernation resume bumps nothing |
| `modify_ldt` | allowed (Wine needs it); a return to a 16-bit stack segment goes through a per-CPU read-only espfix64-style alias stack, so `IRET` leaks no kernel stack bits |
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
| A1 → remote control | remote desktop, screen sharing and input injection are separate explicit grants, never implied by network or inbound grants, and in the prompt floor | at "Almost never", an app holding network and a folder grant starts a VNC and an RDP listener and asks for screen capture and virtual input; every request prompts, and without the grants a remote host reaches nothing and captures nothing (7) |
| A1 → network stack | IP, TCP, UDP, ICMP and ND in safe Rust in the kernel, fuzzed, index masking; proof target 15, network-path parsers (in-kernel part: these parsers never panic or read out of range) | 10^9 fuzz executions, no open crash; IPv6 RA/NDP/ICMPv6 included; target 15 at B/T (6) |
| A1 → DHCP, DNS, NTP, Wi-Fi supplicant | each a sandboxed user-space service in the TCB list, fuzzed; their DHCP, RA and DNS parsers are target 15's user-space part; encrypted DNS to a default provider pinned by IP address, certificate checks against the signed time floor (not-before only) until NTS sets the clock; time from NTS, never below a floor: a stored, signed time floor shipped in the image until Phase 7, then the last-seen update metadata time | each service fuzzed 10^8 executions; spoofed-NTP test can't roll the clock back below the floor; with the RTC reset, encrypted DNS and NTS still come up and no packet goes to port 53 (6) |
| A1 → Wi-Fi, Bluetooth drivers | split on every machine: tier-2 transport (rings and DMA, no frame parsing) and tier-3 802.11 and Bluetooth stacks in their own IOMMU domain | frame fuzzing of the tier-3 stacks 24 h with no open crash; the transport parts fuzzed to the thresholds in [drivers.md](drivers.md) (6) |
| A1 → compromised radio firmware → DMA | on D1, the radio's IOMMU domain holds only its rings and buffers; on D2, radios that do PCIe DMA stay off until the user accepts the "remote DMA exposure" warning | our vfio-user hostile device server on the pinned QEMU, bound to the Wi-Fi transport, DMAs and sends MSIs outside its domain; every attempt faults and is logged; on a D2 machine the radio stays off until the warning is accepted, and the scorecard then shows the remote promise as not holding (6) |
| A1 → out-of-band management | a tier-3 MEI/HECI client reads the AMT provisioning state; a provisioned machine shows "out-of-band remote management: on" at install, in Settings, on the scorecard and in F6's limits | on the vPro reference machine with AMT provisioned, the exposure is shown in all four places; unprovisioned, it is not (6) |
| A1 → compromised Bluetooth stack or HID process → input | Bluetooth off until a device is paired; link keys held in the tier-2 transport; Bluetooth input reaches apps only from a paired HID device's link; the secure-attention key and prompt answers are accepted only from in-kernel parsers or a typed-code-confirmed per-device HID process, never from Bluetooth unless opted in by name; residual risk (compromised radio firmware on a paired link) on the scorecard | a compromised Bluetooth stack injects keystrokes into a focused terminal and emits Ctrl+Alt+Delete; nothing reaches the terminal and no prompt opens; a compromised HID process emits Ctrl+Alt+Delete and keypresses on the prompt terminal; no prompt opens or is answered (6); extended to compositor-drawn prompts (7) |
| A2 → app parser bug | the app is sandboxed | sandbox escape suite (7) |
| A2 → app control-flow hijack | user-space CET shadow stacks (`arch_prctl` as on Linux) where the CPU has them | on a CET-capable runner, a test program that overwrites its own return address faults with `#CP` and is killed (5) |
| A3 → other apps' data | per-app file views, capabilities, per-app `/proc`, PID namespaces, `ptrace` policy | hostile-process suite: read another app's files, protected paths, another process's memory (`/proc/pid/mem`, `process_vm_readv`, `ptrace`), forge or guess a capability, map another app's frames. All fail, each with its defence present, not by `ENOSYS` (5, grows every phase) |
| A3 → kernel | memory safety, proofs, fuzzing, SMEP/SMAP, KASLR; kernel CET shadow stacks and indirect-branch tracking from the phase they land (Phase 11 at the latest, [ROADMAP](../ROADMAP.md)) | syscall fuzzing (3), proof targets 4 to 6 (3), lockdown test (3); a kernel ROP and a forward-edge hijack test fault (11, or earlier in the phase kernel CET lands) |
| A3 → forged identity | launch-closure hash (interpreted apps include every file opened from the script's tree), identity ID with lineage, publisher key in packaged identities, provenance, protected install directories and launch paths, spawn-with-identity capability held only by the launcher and service manager | **identity-forge suite**: an app replaces or edits a granted executable, a library beside a granted unpackaged app, a granted app's script, and a `helper.py` it imports; writes into each protected launch path (a `systemd --user` unit running `python3 -c '<payload>'`, a D-Bus service file, `environment.d`, `mimeapps.list`); spawns a child claiming another identity; execs a copy of a granted binary from another path; two Python apps get different identities; a binary replaced at the same path sees an empty home; two `sh -c` identities see disjoint homes; `firefox` run from the terminal runs under Firefox's identity, and a terminal exec of a registered app with `ELECTRON_RUN_AS_NODE=1` gets no such variable and one with `--load-extension` gets the floor prompt; no grant transfers and the new code gets no network before its prompt; a desktop launch of `./target/debug/x` then 10 `cargo build`s give 0 `EACCES`, `rm -rf ~/opt/app` from the terminal works, and `pip install --upgrade` of a terminal-installed app gives exactly 1 prompt (5); an executable the app wrote, launched from the desktop, gets no network before its prompt (7) |
| A3 → removable or downloaded executable | provenance "removable" and the "downloaded" tag set by the kernel and spread on read for every identity, across pipes, Unix sockets, shared memory (at map time), `fork` and `execve` ([capabilities.md](capabilities.md) "Provenance"); a desktop launch of such an executable runs a new identity with no grants and no network until approved (floor for downloaded); a removable one gets `EACCES` from the terminal until approved; a file with terminal provenance runs from the terminal unprompted | an executable copied from a USB stick, started through the launcher, has no grants and no network, and from the terminal gets `EACCES`, until approved (6); `git clone` from a local fake remote over loopback, `cargo build` and `./target/x`, 10 rebuilds, give 0 prompts and 0 `EACCES` (5; with a real remote, 6); `curl -O`, Chromium "Save as" to `~/Documents`, `curl \| tar x`, `wget` then `tar xf`, an archive app extracting a browser download outside Downloads, and a cloned repository's binary, each then double-clicked in the file manager, give exactly 1 prompt each (7) |
| A3 → terminal | exec of a file with another identity's or removable provenance refused until approved (a file with terminal provenance runs unprompted, whatever its "downloaded" tag), writable grants in the terminal's home carry the "can run code as Terminal" prompt, fresh pty per identity, `TIOCSTI` refused, escape sequences filtered | an app with a writable grant to `~/Projects` plants a git hook and a `Makefile`, and the grant needed the "can run code as Terminal" prompt; the terminal execs the hook and gets `EACCES` until approved; a process of another identity started from the terminal tries `TIOCSTI`, `TIOCLINUX`, OSC 52 and report sequences; all fail (5) |
| A3 → shared channels, D-Bus, AT-SPI and keyrings | D-Bus proxy, per-app a11y proxy, per-app keyrings, per-app IPC namespace | for each, an app without the grant tries to use it and fails (5; the a11y proxy at 7, when it exists) |
| A3 → shared channels, PipeWire | PipeWire permissions | an app without the grant tries to record or capture another app's stream and fails (6) |
| A3 → shared channels, Wayland and X11 | Wayland security contexts, per-app XWayland | an app without the grant tries each privileged protocol and another app's X11 server and fails (7) |
| A3 → confused deputy | portals pass nothing without confirmation; networked TCB services check the caller's grants; printing without network names the destination; for a caller without network notifications drop remote content and the email portal prompts | an app tries the launcher, open-URI, handlers and D-Bus activation to run a command or open a file through another app; all need confirmation; an app with files and no network calls open-URI with an `https` URL carrying data and gets a prompt showing the full URL (5); a notification it sends carries no remote content and an email-portal message carrying data gets a prompt naming recipients and attachments (6); it prints to an IPP printer advertised over mDNS and gets a prompt naming that printer (6); it opens a loopback page in the browser and gets no auto-grant (7) |
| A3 → permission broker and portals | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions, no open crash (5) |
| A3 → D-Bus proxy and broker | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions against the proxy and dbus-broker, no open crash (5) |
| A3 → Flatpak launcher | fuzzed TCB service; `org.freedesktop.Flatpak` talk-name refused | 10^8 fuzz executions over manifests and launch requests, no open crash; `flatpak-spawn --host` fails (5) |
| A3 → Flatpak portal | `Spawn` starts only child identities with a subset of the caller's grants; `SpawnSignal` reaches only children the caller spawned; `latest-version` and `CreateUpdateMonitor` refused ([capabilities.md](capabilities.md)) | 10^8 protocol fuzz executions over `Spawn` and `SpawnSignal`, no open crash; `Spawn` with a `sandbox-expose` path the caller doesn't hold, with network from a caller without network, and with a `usr-fd` or `app-fd` of a writable tree gives the child none of them; `SpawnSignal` to a process the caller didn't spawn fails; `latest-version` is refused (5) |
| A3 → launch relay | the exec'ing process becomes a system-image stub for an identity exec through the launcher (new process under the app's identity, `AT_SECURE=1`, only fds 0 to 2 plus manifest-declared fds; the stub's `waitpid` returns the app's status); the exec is the `launch-relay` direct channel, checked by the combined approval; environment allowlist; argv tokens beyond existing paths and URLs a floor prompt; a pty only for tty fds 0 to 2, with the escape filter on relayed output; tracing only through the debugger portal ([capabilities.md](capabilities.md)) | 10^8 fuzz executions over the relay's launcher protocol and relayed stdio, no open crash; a terminal-identity process that execs a registered test app's path gets that app under its own identity with exit status and forwarded signals passed through; `head -c 1G /dev/urandom \| app \| sha256sum` matches; OSC 52, title-set and report sequences the app writes never reach the terminal; `gdb -p`, `strace -p` through the debugger portal and `taskset` act on the app (5); `code .` and `EDITOR="code --wait" git commit` work with the combined approval asked once per app (7) |
| A3 → Background portal | the only writer of `~/.config/autostart`, asking per the prompt level; entries an app writes inside its private home become requests, and an approved entry's `Exec` line launches under the requesting identity | 10^8 protocol fuzz executions, no open crash; a test app's direct write to `~/.config/autostart` fails; its entry written inside its private home appears only after approval; an approved entry whose `Exec` names another app's executable runs under the requesting app's identity with its grants only (5) |
| A3 → settings portal | the only writer of the projected shared settings; accepts only the published keys and value types of each file, never an entry that names code or a file outside the shared paths | 10^8 protocol fuzz executions, no open crash; an app's direct writes to `~/.config/gtk-3.0`, `~/.config/fontconfig` and `~/.local/share/fonts` fail; through the portal, a `gtk-modules` setting, a fontconfig `<include>` or `<dir>` naming a file in the app's home, and a change to the prompt level are refused (5) |
| A3 → user service manager | fuzzed, size-capped TCB service; control interface unreachable from app proxies | 10^8 protocol fuzz executions over D-Bus activation and unit requests, no open crash (5) |
| A3 → store service | fuzzed, size-capped TCB service | 10^8 fuzz executions over install requests, manifests and repository metadata, no open crash (5) |
| A3 → storage service | fuzzed, size-capped TCB service; passthrough and firmware download only with signed firmware | 10^8 protocol fuzz executions, no open crash (5) |
| A3 → log service | fuzzed, size-capped TCB service | 10^8 fuzz executions over log submissions and queries, no open crash (5) |
| A3 → SSH agent, GPG agent, secret service | TCB services, prompts through the broker naming the requesting app | 10^8 protocol fuzz executions against each, no open crash; an app without a confirmed use gets no signature and no secret (5) |
| A3 → lock screen | fuzzed, size-capped TCB service; fails closed | 10^8 fuzz executions over its input and protocol, no open crash; killing it and crashing it each leave the session locked, and the prompt virtual terminal asks for the login secret (5); repeated with the compositor's lock (7) |
| A3 → document portal | tier-3 file system, fuzzed FUSE protocol | 10^8 protocol fuzz executions, no open crash; an app reaches only its own entries, and a revoked entry is gone (5) |
| A3 → resolver | TCB service at `127.0.0.53` resolving only for identities with outbound network, told the asker by a kernel-stamped sender identity; a lookup from an identity with no answer is the first-use event; refused, it answers SERVFAIL | 10^8 protocol fuzz executions, no open crash; a refused identity's lookup gets SERVFAIL (`EAI_AGAIN`), never `ECONNREFUSED`, and carries no data upstream (5) |
| A3 → PipeWire | pinned upstream with our permission layer, fuzzed | 10^8 protocol fuzz executions, no open crash (6) |
| A3 → udev-compatible daemon | fuzzed, size-capped TCB service; uevent records only for granted devices | 10^8 fuzz executions over uevents and libudev queries, no open crash (6) |
| A3 → seat service | fuzzed, size-capped TCB service | 10^8 protocol fuzz executions, no open crash (7) |
| A3 → compositor | 60,000-line cap, security contexts, Mesa exposure limited to buffer import | 10^8 Wayland protocol fuzz executions, no open crash, with hostile dma-buf and shm buffers included (7) |
| A3 → input method | fuzzed, size-capped TCB service; reached only through the input-method protocols; conversion engines are sandboxed non-TCB helpers with no network and a dictionary per identity | 10^8 protocol fuzz executions, no open crash; each conversion engine fuzzed 10^8 executions through its keys-in, candidates-out interface, and a password field's keys never reach its dictionary (7) |
| A3 → accessibility proxy | fuzzed, size-capped TCB service; each app exposes only its own accessible tree; other apps' text only with the screen-reader grant, which reads only; control and key listening are floor input injection, never toward the terminal or the prompt layer | 10^8 AT-SPI protocol fuzz executions, no open crash; hostile tests: a screen-reader grant holder's `Action`, `EditableText`, `Value.Set`, `GenerateKeyboardEvent` and global key listener calls fail without the input-injection grant, and with it never reach the terminal or the prompt layer (7) |
| A3 → update client | fuzzed, size-capped TCB service | 10^8 fuzz executions over TUF metadata, transparency proofs and downloads, no open crash (7) |
| A3 → prompt spoofing, console | prompts on a dedicated virtual terminal reached by the secure-attention key | an app draws a fake prompt on its own terminal, injects keystrokes, and tries to switch virtual terminals; no grant is made without the secure-attention key and a keypress on the prompt terminal (5) |
| A3 → screen, input, mic, camera, location, clipboard | grants only through portals; prompts drawn by the compositor; keyboard and pointer emulation only to the focused surface behind a grant; these grants are in the data set, so with network they are in the floor; raw USB audio and video interfaces refused | prompt-spoofing test against the compositor; an app can't read another window's pixels or keys, and an app holding the virtual-input grant reaches only the focused surface; at "Almost never", microphone, camera, location, background clipboard read and screenshot, each with network, prompt once each; a USB-portal request for a UVC camera or USB microphone interface is refused (7) |
| A3 → exfiltration | the data set (file, sensor, screen, location and clipboard grants) plus network is one explicit grant; first network use decided per identity; per-identity egress filter; secrets used through agents; networked deputies check grants; directional direct channels checked as one principal on every channel and every grant; the overt channels are the closed list in `design/channels.toml` | **exfiltration suite**: a canary file in a granted folder and a canary SSH key; an app (and a script run from the terminal) without the combined grant tries every overt channel (every portal and open-URI (5); sockets of every family, loopback to another identity's port, io_uring socket ops, DNS lookups through the resolver, notifications, printing, launching a networked app (6); the clipboard, and a script exec'ing a registered browser with a crafted HTML file (7)); the canary must not reach an external sink. Grant after join: app A (files) opens a channel to B (nothing), then B asks for network; the grant prompts naming A's files, and refused, it is refused at grant time with the reason; an app holding Downloads and network does not join the terminal's component (6); a writer-to-reader folder grant made in one session still joins the two after logout and login, because grant-created edges are rebuilt at login; a socket channel open needing a combined approval is refused at once, approved, and the retry succeeds (5). In full it adds **colluding apps**: two, one holding the folder and one holding network, and a chain of three (files, nothing, network), which try every channel in `design/channels.toml` between them, including relays through PipeWire and D-Bus, a granted loopback port, a shared writable folder, drag and drop, and the grants given to the networked one at run time; the combined approval is asked again when a fourth app joins; a BadUSB mouse drags `~/Documents` into a networked chat app and pastes the clipboard into it, and nothing transfers; covert-channel bandwidth is measured and published (7) |
| A3 → network without approval | default profile has no network; the first non-loopback `connect` or send, or the first resolver lookup, queues one request per identity, decided synchronously when the level needs no human and otherwise waited on for up to 30 s; refused or timed out, `ENETUNREACH` or SERVFAIL, never `ECONNREFUSED`; remembered | at "Always ask", `curl https://<name>` from a fresh identity raises exactly 1 request and, approved within 30 s, succeeds on that first run, with glibc, musl, Go and Chromium's resolver each approved at 2 s and at 25 s; an epoll UDP client sees 0 spurious wakeups during a 25 s hold; refused, it stays unreachable across restarts; at "Only when suspicious" a program with no data set in its component is granted with 0 failures and 0 prompts and logged; with a folder grant it prompts at "Almost never" (5, on the loopback-only stack with a fake upstream behind the resolver); the same over real interfaces and a real upstream (6) |
| A3 → prompt level | the level is a TCB setting changed only on the trusted path | an app tries to change the level through every portal and by writing the settings file; all fail (5) |
| A3 → side channels | the mitigation matrix, SMT off on affected CPUs | the PoC suite recovers zero secret bytes above chance on the shipped default for each CPU model, and app to app with opt-in SMT on (3) |
| A3 → Rowhammer | page tables in a separate physical zone with guard rows; ECC/TRR reporting; refresh policy reported; residual risk on the scorecard | on each reference machine, a published hammering PoC run from an app for 24 h flips no page-table bit and gains no access outside the app; flips elsewhere are counted and published (3) |
| A3 → 16-bit stack leak | espfix64-style per-CPU read-only alias stack for `modify_ldt` 16-bit SS | a 32-bit program with a 16-bit SS from `modify_ldt` reads `RSP[31:16]` after `IRET` 10^6 times and recovers 0 bits of the kernel stack address (5) |
| A4 → signed malicious update | offline root, threshold signing, transparency log with checkpoints cosigned by at least 2 of at least 4 witnesses, an emergency witness-set change signed by the offline root (2-of-3) and cosigned by at least 1 old witness, or else applied only after 30 days in the public log with a trusted-path notice, carrying a monotonic version kept with rollback protection under the floor; before resealing, the update client checks that the loader hash and the disk policies are logged; reproducible builds | an unlogged but validly signed release is refused; a root-signed witness change while 2 or more old witnesses cosign within 7 days is refused (4b); a witness change with no old cosigner is applied only after 30 days in the log with the trusted-path notice (7); a stale-metadata mirror is detected within 24 h; with one witness removed a release still boots; the witness set rotated with one old witness dead still verifies (4b for the signing and witness tests, and from 7 the stale-mirror test) |
| A4 → poisoned dependency | pinned hashes, cargo-vet, cargo-deny | CI fails on an unpinned fetch or unvetted crate (1) |
| A5 → DMA attack | D1's pre-boot rule (hardware tiers above): the firmware's per-buffer tables for all firmware I/O, then PMRs over the pool and bridge BME cleared before `ExitBootServices`, then Optimal Boot's deny-all tables; the S3 waking vector re-arms the IOMMU first, then restores bridge BME, before any device is re-enabled; IOMMU deny-all (every unused AMD-Vi device table entry and Intel context entry blocking), per-device domains, ATS off by default, PASID and PRI off in devices and refused in the IOMMU, interrupt remapping with every IRTE checked against its exact requester ID, a global fault-rate cap; DMA is stopped by bridge or root-port BME plus the IOMMU, never by a device's own BME | our vfio-user hostile device server on the pinned QEMU sends DMA and MSI writes outside its domain and from an unassigned requester ID; all fault and are logged (1c); a PCILeech-class FPGA card on the D1 reference machine reads and writes nothing outside its domain, across the firmware-to-kernel hand-off too, and its spoofed requester IDs (including a storm cycling through many IDs), ACS-bypass attempts, PASID-prefixed requests, page requests and MSIs using another device's handle fail (4a); on OVMF with `IntelVTdDxe` the vfio-user device DMAs continuously across `ExitBootServices` and 0 reads or writes of the pool land; a hostile boot-disk device patching `ConIn`'s code in a boot without firmware protection is reported as the "pre-loader DMA window (firmware code)"; the FPGA card on the D1 reference machine scans memory during Optimal Boot and finds 0 copies of the PIN canary and of the TOTP and M secrets (4b); the FPGA card attached during suspend writes nothing after the waking vector starts, nor before it on firmware that protects its S3 boot script, and otherwise the S3 resume DMA window is shown (10) |
| A5 → BadUSB | no driver binding before approval; per-interface authorization; typed confirmation for keyboard/keypad and system-control HID interfaces except a remembered keyboard on the same port (identity includes the descriptor hash and port path; clone residual on the scorecard); mice, gamepads, consumer-control-only devices and FIDO keys never trusted for prompts; 2.4 GHz receivers treated like Bluetooth; before Phase 6 prompts accept USB input only from the device that typed the unlock secret (the loader hands over the `ConIn` device path, else a typed code); an in-kernel descriptor classifier stamps each HID device's allowed events (keyboard-range keycodes from non-floor devices dropped, consumer page limited to media and volume); user-action transfers only from confirmed devices; safe mode accepts only known keyboards; new devices refused while locked ([drivers.md](drivers.md) "Device authorization") | a `usb-kbd` hot-added after unlock can't open or answer a prompt (5; the B1 swtpm boot rerun at max(5, 4b)); an emulated keyboard that types at once gets no keystroke accepted; a composite device's hidden HID is held; a rogue USB network adapter gets no driver binding until approved and prompts at the default level; a known storage identity re-presented with an added HID interface is treated as new; a mass-storage device plugged in while the screen is locked is not bound or mounted until unlock and approval, a known keyboard replugged while locked types only into the lock screen and unlock shows "a keyboard was reconnected while locked", and a remembered keyboard moved to a different port needs the typed code again; a receiver's newly paired keyboard needs its own code and answers no prompt; a scripted USB HID guesser's 11th lock-screen PIN guess is refused, only the passphrase unlocks, and failedTries has risen by exactly 10; a compromised mouse process emits `KEY_A` and Enter into a focused terminal and nothing arrives (6); a keyboard present before suspend types the resume PIN after an xHCI reset across S3 (10); a BadUSB keyboard in safe mode types nothing (12) |
| A5 → Thunderbolt/USB4 tunnel, hotplugged PCIe | per-device approval in the floor for every tunnel and every PCIe device added after boot below any hotplug port; refused while locked; ICM level read, never set; IOMMU domain per device | on the reference laptop, an unapproved Thunderbolt NVMe enclosure gets no PCIe tunnel; approved, its DMA outside its domain faults; plugged in while locked, it gets no tunnel; with the ICM security level set below "user" in firmware the tunnel is refused and the scorecard notes it; at "Almost never" a tunnel and a PCIe device hot-added below a non-external hotplug port each prompt, and while locked each is refused (6) |
| A5 → malformed descriptors | HID and class drivers in tier 3, one HID process per device, fuzzed | descriptor and report-descriptor fuzzing 24 h clean (6) |
| A6 → read the disk | encryption ([storage.md](storage.md)); core-crypto proof target 14 | an offline disk image shows only ciphertext and the documented plaintext metadata; target 14 at T/T (4a) |
| A6 → tamper with the boot chain | Secure Boot, signed config, measured boot, signed TPM policy over PCR 4, 7, 8, 9; PCR-4-per-machine objects under the separate policy reference `optimal-disk-v1-pm/f`, the only one PCR 8/9-only digests are signed under; floor state machine proof target 16; policy composition target 17, each object checked (B/T) against its own row of the per-object table in [boot.md](boot.md) ([verification.md](verification.md)); residual on the scorecard: without a firmware setup password a correct TOTP code doesn't prove the firmware is unchanged, so object 3 binds the Secure Boot state through a `PolicyOR` over PCR 7 values (resealed on OS-applied changes) and a boot with Secure Boot off shows no code, and the loader warns when PK/KEK/db changed; a shim revoked from outside (a dbx or SbatLevel update from Windows or firmware) has a documented way back: Secure Boot off (a B3 boot), reinstall the loader, Secure Boot on, a recovery-key boot that reseals ([boot.md](boot.md) "Shim revoked from outside") | tamper tests in [boot.md](boot.md), including an older signed loader, a signed tool replaying the event log and a config signed by a revoked key; on a machine with per-machine PCR 4, a signed tool replaying the PCR 8 and 9 events can neither unseal the TOTP key nor increment the floor counter; a test dbx entry for the shim applied from outside Optimal stops path 1 and the documented steps bring the machine back; with Secure Boot turned off, no code is shown; targets 16 and 17 at B/T (4b; target 17's objects 4 and 5 at 10) |
| A6 → substituted volume | before capping only the fixed-format Optimal header block is parsed (the passphrase- and recovery-wrapped volume key live there from 4a, so no Optimal boot path reads a LUKS2 key slot or parses LUKS2 JSON before capping); each LUKS2 copy (primary header and JSON, secondary header and JSON) has its own volume tag, keyed from the volume key, and the kernel accepts a copy whose own tag verifies and whose JSON passes the allow-list, never choosing by seqid; each header-block copy holds a current and a pending tag set (write pending, flush, write both LUKS2 copies, flush, promote; either is accepted); a recovery-key boot that finds no verifying tag shows the JSON diff, checks the allow-list, asks on the trusted path and re-tags after capping, with S and every sealed secret never unsealed, then reboots; the LUKS2 header has no TPM keyslot (Linux rescue uses the passphrase and recovery slots); the service-key secret is released only after a tag matches | a volume with a hostile header and JSON area, swapped in for the real one, gets no secret and no counter bump; a secondary header copy with a higher seqid and a valid checksum is refused (fuzz case); one flipped bit in the secondary copy still boots; a power cut at every write of first-boot sealing and of a passphrase change still boots; `cryptsetup luksAddKey` from Linux, then a recovery-key re-tag, works; a recovery-key boot parses no JSON before capping; the pre-cap parser fuzzed to the in-kernel thresholds (4b) |
| A6 → clear the TPM to reset the floor | the SRK, counter and base names are recorded in the header block, the record the check uses ([boot.md](boot.md)); whenever the anti-rollback counter and base exist with their recorded names, the floor is max(the minimum config SVN built into the loader, counter − base); the TPM counts as cleared only when the SRK, the counter or the base is missing or its name differs from that record (so a root rewrite of the recorded names takes this branch: an older image may boot but opens nothing, and the counter keeps the floor), and then the floor is max(the built-in minimum, the highest SVN among ESP configs whose slot's kernel and modules are present and hash-match), never above the highest SVN that can boot, no code is shown and the loader warns; a recovery boot never redefines a counter that exists; the replay value is counter minus a write-once base, covered by every reseal and clear recovery, and after a clear the user sees the last authenticated commit time and generation before accepting; older loaders are retired through SBAT and dbx, which survive a clear; the loader disables an open platform hierarchy before `ExitBootServices` | clear the TPM, redefine the indices, delete the newer slots and recovery copies, then try to boot an older slot; it is refused with a warning; root writes a wrong SRK name into loader state, corrupts object 3's slot and deletes the newer configs; the floor stays counter − base, the older slot is refused, and a recovery-key boot leaves the counter as it is; a security update whose kernel hangs falls back to the old slot (4b) |
| A6 → tamper with ciphertext | stage 1: metadata checksums plus hardened parsing; stage 2: authenticated encryption | random and replayed sector writes to an encrypted ext4 image never crash or corrupt the kernel (4a); bit flips detected in stage 2 (file system track) |
| A6 → TPM bus | every command carrying an authValue or sensitive data (`Create`, `PolicySecret`, `DictionaryAttackLockReset`, `HierarchyChangeAuth`, `NV_DefineSpace`, `Unseal`) uses a salted HMAC session bound to the SRK recorded at first sealing, encrypted both ways, never a password session; PIN stretched and mixed into the key; the service-key secret also needs the PIN (`TPM2_PolicyAuthValue`) | swtpm bus traffic contains none of R, S, either PIN half, M, the TOTP key, the owner secret or the lockout secret, each checked by name; a swtpm man-in-the-middle proxy fails to unseal the disk secret or the service-key secret without the PIN (4b) |
| A6 → TPM secrets extracted (fTPM break) | every sealed object seals only a random R; the key material is AEAD-encrypted under HKDF(R ‖ stretched-PIN half 2) | with the TPM seeds extracted (swtpm state dump), the disk-secret and S blobs still need an Argon2id PIN search (4b), and so do the resume and hibernation blobs (10) |
| A6 → TPM reset or lost state on resume | a PCR reset while running is treated as tamper; the resume object's policy is `TPM2_PolicyCounterTimer` resetCount equal to its value at suspend, plus PCR 4, 7 and capped 8 and 9, plus `TPM2_PolicyAuthValue`, DA-protected; at capping a fresh random 256-bit boot nonce is extended into PCR 9, so capped values are unique per boot; it seals only R and its blob is in RAM only | resume after a forced TPM reset can't use the resume object, falls to recovery (passphrase or recovery key) and logs tamper; a reboot into another OS that replays the PCR 8 and 9 events fails to unseal it; suspend, hard reset, then a genuine boot: object 5 refuses (10) |
| A6 → guess the PIN | TPM dictionary-attack lockout (maxTries 32, recoveryTime 7,200 s) with owner and lockout authorisation set at install (on a TPM already provisioned, as by Windows in a dual boot, Optimal works without lockoutAuth, so with no lockout reset and the budget stated in [boot.md](boot.md), unless the user accepts an explained clear); `TPM2_Shutdown` on every reboot and panic path so orderly boots add no failed tries, with the non-orderly increment budget stated in [boot.md](boot.md); after each good pre-cap unseal of object 1 the kernel resets the lockout with the lockout secret wrapped under S, so failedTries counts only wrong PINs and unclean shutdowns since the last good boot, and a locked-out TPM is reset only from a recovery-key boot; a PIN-epoch NV index in objects 1 and 2 (`PolicyNV` EQ), raised on each PIN change, so an old header copy and the old PIN unseal nothing; the lock-screen PIN goes through a DA-protected TPM object ("Prompts can't be spoofed") | lockout engages after 32 tries and then allows one try per 7,200 s; root can't reset it; across at least 64 power cuts failedTries stays within the stated budget and every boot unseals without the recovery key; after a PIN change, the pre-change header copy restored with the old PIN fails; on a swtpm with Windows-style provisioning, install and boot work without a clear (4b) |
| A6 → locked or suspended laptop | protected-class keys and page cache evicted on lock; suspend wraps the volume key, the service-key secret, the home and swap keys (stage 2: also every per-user key, the class A key and class-B per-file keys) into the resume object, then wipes them, and AEAD-wraps the agents' secret memory under HKDF(R5 ‖ PIN half 2); frozen app memory is promise 4's named residual; class B and C keys are wiped (suspend locks first); open vaults are closed and unsealed travel folders resealed; each key's fate is the key inventory table in [storage.md](storage.md) | after suspend, a stage-1 memory scan for the canary set generated from the key inventory finds no volume key, home key, service-key secret, vault or travel key, no agent canary and no canary from a stage-1 home, and resume asks for the PIN (10); the stage-2 scan finds no per-user, class or per-file key (10, once stage 2 lands); after lock, a memory scan finds no protected-class key and no plaintext canary from protected files (file system track; until stage 2 the scorecard shows promise 4's locked part as "partial") |
| A6 → hibernation image | class-C keys evicted before hibernating; image key sealed as R plus a PIN-keyed blob in the header block; image tied to release, generation and replay value (lockdown table) | the image holds no class-C key or canary; resume without the PIN, with tampered PCRs, with an image below the rollback floor, with a different release, or after a normal boot or Linux mounted the system volume or a home is refused; the keys handed to the restored kernel are gone from the reserved page after resume (10) |
| A6 → DRAM read (cold boot, bus probe) | memory encryption (AMD SME, Intel TME) where present, from Phase 11 at the latest; scorecard shows whether it is on | on a reference machine with SME, a canary page written through an encrypted mapping reads as ciphertext through a C-bit-clear alias; on one with TME, `IA32_TME_ACTIVATE` reads enabled and locked (11) |
| A7 → other user's data | per-user keys (stage 1: per-user encryption of home directories; stage 2: in the file system), per-user file trees, limited administrators, packaged identities include the signing key | a second user, including an administrator, can't read the first user's files or reset their way in; an administrator who adds a Flatpak remote and installs an app with the first user's granted app ID gets none of its grants or its home (5); on B1 a second user logs in after PCR capping, and the recovery key plus S alone cannot open a user's home (5, rerun at max(5, 4b)) |
| A7 → kernel crash dumps | kernel memory only by default, encrypted to administrators, user pages only with each user's consent | the crash-dump canary test under "Crash reports stay local" (5) |
| Promise 3 → telemetry | nothing phones home | a fresh install left idle 24 h contacts only the update and time hosts, all logged (7) |
