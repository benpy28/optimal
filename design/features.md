# Optional features

These are owner-requested features, each a user option. Every one is built
on a mechanism that another design file already specifies, and names it.
Each entry gives its default and why, what the user sees, the mechanism,
what it does **not** protect against, the phase it lands in with its
dependencies, and its test with a pass number. A feature is gated in the
done-when of the phase it lands in, like any other item there
([ROADMAP](../ROADMAP.md), "Optional features" in each phase). Measured
claims have rows on the scorecard ([benchmarks.md](benchmarks.md)).

Three rules hold for all of them:

- **Options are TCB settings where they weaken or strengthen security.**
  Turning a security or privacy option off, or loosening one, happens only
  on the trusted path (the prompt terminal in Phase 5, the compositor's
  prompt layer from Phase 7), like the prompt level
  ([capabilities.md](capabilities.md)). No app can change one through a
  portal, a settings file or injected input. Turning a protection on is
  allowed from any input.
- **Nothing leaves the machine.** Every log, record and snapshot here is
  local and per user, encrypted at rest under that user's key (the home
  volume in stage 1, the user's class keys in stage 2,
  [storage.md](storage.md)), and never uploaded.
- **Limits are shown where the option is.** The "does not protect against"
  text of each security feature is also its Settings text, in the same
  words, and its scorecard row.

| # | Feature | Default | Lands in | Builds on |
|---|---|---|---|---|
| F1 | Trusted sensor indicators and kill switches | indicators on; kill switches off | 7 | compositor prompt layer, device authorization, PipeWire permissions |
| F2 | Privacy dashboard | on, 30 days | 12 | silent-grant log, egress filter, device authorization, F1 |
| F3 | Hidden encrypted vault | off | 12 (stage 2: file system track) | volume map, KDF rule, provenance and private homes |
| F4 | Panic lock | off | 5 (freeze, power-off), 10 (hibernate) | suspend key wipe, lock screen, in-kernel input |
| F5 | Travel mode | off | 12 (stage 2: file system track) | volume map, class C eviction |
| F6 | Network leak guard | VPN-only off; encrypted DNS on | 6 | egress filter, system resolver |
| F7 | Tamper alert | on | 4b | measured boot, event log replay, TOTP |
| F8 | Game mode overlay | off | 8 | compositor, presentation timestamps, scheduler accounting |
| F9 | Focus boost and background freezing | off | 10 | desktop awareness, cgroup groups |
| F10 | Power meter and per-app caps | meter on; caps off | 10 | RAPL, battery driver, cgroup `cpu.max` |
| F11 | System undo from the boot menu | on | 12 | A/B slots, loader state, anti-rollback floor |
| F12 | File time-travel | on (own file system only) | file system track | own file system snapshots |
| F13 | Plain-English safe mode | on | 12 | per-entry signed configs, boot counting, crash reports |
| F14 | No-surprise updates | on | 7 | update client, A/B slots |
| F15 | Ad and tracker blocking in the browser | on | 7 (interim), browser engine track | signed update client |
| F16 | Export everything | available, encrypted by default | 12 | settings store, store service, document portal |
| F17 | Themes, tiling, key remapping, per-app rules | off (stock theme, floating windows, no remaps) | 12 | compositor, trusted input, settings portal |

## Security and privacy

### F1. Trusted sensor indicators and kill switches

**Default:** indicators on, turned off only on the trusted path; kill
switches off (sensors enabled), because the per-app portal grant already
decides who may use a sensor and a kill is a deliberate user action.

**What the user sees.** A camera, a microphone and a screen icon at a fixed
corner of every output whenever that sensor is delivering data to any app,
with the app's attested name beside it. Each icon is also a one-tap kill
switch: tapped, the sensor is off for every app, the icon shows a slash,
and the state survives reboots. Where the machine has a hardware switch or
shutter the platform reports, the icon shows "off (hardware)" and the
software switch can't turn it back on.

**Mechanism** (builds on the prompt layer in
[capabilities.md](capabilities.md), "Prompts can't be faked"; device
authorization and "Stopping a driver" in [drivers.md](drivers.md);
PipeWire's per-app permissions in [threat-model.md](threat-model.md)):

- *Drawn where apps can't reach.* The compositor draws the indicators in
  the prompt layer, which no app can draw over, capture or cover, including
  a fullscreen app on direct scanout (the indicator then takes a hardware
  plane, or the compositor composites that frame). Indicators are left out
  of every screen cast.
- *Lit from the trusted source, before the data flows.* Apps never hold a
  camera or microphone device; PipeWire does, and hands streams to apps
  through the portal. PipeWire's permission layer tells the compositor when
  it is about to deliver a stream's first buffer to an identity, and holds
  that buffer until the compositor reports that a frame showing the
  indicator has been presented (presentation feedback, at most one refresh
  period). Screen casting and screenshots are done by the compositor
  itself, which lights the indicator before its first captured frame
  leaves it. Each indicator stays lit for 3 s after the last buffer, so a
  100 ms capture is visible.
- *Software kill means detached, not muted.* Camera: every video interface
  of that class is deauthorized in the USB hub logic (endpoints
  deconfigured, so no data moves) and its tier-3 driver is stopped, its
  identity kept so re-enabling needs no new approval; a camera on another
  bus has its driver stopped. Microphone: the HD Audio driver stops and
  powers down every input converter, refuses capture opens with `ENODEV`
  and refuses retasking an output pin as an input; USB audio input
  interfaces are deauthorized; PipeWire refuses Bluetooth headset capture
  and sends the stack no capture request. Screen: the compositor ends every
  screen cast and refuses new ones. The kill state is in the TCB settings
  store and is applied at driver bind from boot, so a killed camera never
  starts.
- *Hardware switches* are read where the platform reports them (an
  embedded-controller or HID privacy-switch event, or the camera dropping
  off the bus) and shown as such.
- *Only trusted input re-enables.* Killing a sensor is accepted from any
  input; un-killing one is accepted only from trusted input sources (the
  same rule as prompt answers), never from the compositor's virtual pointer
  or an app.

**What it does not protect against.** A compromised kernel, compositor or
PipeWire can deliver data without lighting an icon. Compromised camera or
codec firmware can capture into its own memory, but its data still has to
pass the driver to reach anything. An app that holds the grant while the
sensor is on records what it is allowed to; the indicator says who is
receiving data, not what it does with it. Software kill relies on the
kernel; only a hardware switch survives a kernel compromise, and Optimal
can only show a switch the platform reports. A compromised Bluetooth stack
can still read the audio on its own links (the existing residual,
[threat-model.md](threat-model.md)). Before Phase 7 there are no
indicators.

**Phase.** Phase 7. Needs the compositor's prompt layer (7), PipeWire with
per-app permissions and the audio and USB video drivers (6), and device
authorization (6).

**Test (Phase 7).** (a) On QEMU with `ich9-intel-hda` and `hda-duplex` fed a
tone, 1,000 microphone stream starts, and on the reference laptop 100
internal-camera stream starts and 1,000 screen casts: 0 buffers reach the
app before the presentation timestamp of the first frame showing the
indicator, and the indicator stays lit at least 3 s after the last buffer
in every case. (b) A hostile app goes fullscreen, places a subsurface over
the indicator and asks for layer-shell and overlay planes: in the
compositor's own capture of 60 s of output the indicator pixels are
present in 100% of frames. (c) With each kill switch on, an app holding the
grant gets 0 buffers in 60 s; the HD Audio input converters read back as
stopped and a pin retask is refused; the camera's streaming interface has
no configured endpoint. (d) Re-enabling through D-Bus, the settings file
and the virtual pointer fails in every attempt.

### F2. Privacy dashboard

**Default:** on, 30 days of history in at most 64 MiB per user, because
silent grants are already logged and this only makes the record readable;
network history can be turned off on its own.

**What the user sees.** In Settings, per app and per day: the files and
folders it opened through portal grants and document-portal entries, the
network destinations it reached (address, port, the name it looked up,
first and last time, bytes each way), location fixes it was given,
camera, microphone and screen use with start times and durations, and USB
devices it used. Every row has a revoke button.

**Mechanism** (builds on the silent-grant log and revocation in
[capabilities.md](capabilities.md), the egress filter and resolver in
"Networking" there, device authorization in [drivers.md](drivers.md), and
F1):

- *Sources.* The broker logs every grant and its first use per day; the
  document portal and the VFS log the first open per day of each grant's
  mount or entry; the kernel egress filter keeps, per identity, a table of
  at most 1,024 destinations per day (remote address, port, protocol,
  first and last time, counts and bytes; destinations beyond 1,024 are
  counted, not listed), and the resolver adds the name an identity looked
  up for an address; F1's sources give sensor sessions; device
  authorization gives USB use. The log service drains the kernel table
  every 60 s.
- *Storage.* Records are kept by the log service in the user's own
  encrypted storage (stage 2: class A, so recording continues while
  locked), at most 64 MiB per user, oldest day dropped first. Each day's
  records are encrypted under a per-day key; expiry wipes that key, so
  expired records stay unreadable even where the drive keeps old blocks.
  Retention is 1, 7, 30 or 90 days, or off. Administrators can't read
  another user's dashboard.
- *Revoke.* One click revokes the grant through the broker, with the
  revocation semantics of [capabilities.md](capabilities.md): open files
  fail with `EIO`, sockets get `ECONNRESET`, a sensor stream ends, a USB
  device's driver is stopped and its identity forgotten.

**What it does not protect against.** It shows only what passes through
mediated paths. What an app does inside its grants, such as the content it
sends to a destination, is not shown. An app that runs its own encrypted
DNS shows only its resolver's address. Covert channels are not shown. A
compromised TCB service can omit records. The history is itself sensitive:
anyone who unlocks the session can read it, which is why it is bounded,
expiring and can be switched off.

**Phase.** Phase 12. Needs the broker and log service (5), the egress filter
and resolver (6), F1 (7) and the settings app (12).

**Test (Phase 12).** A scripted day: a test app gets a folder grant and opens
3 files, connects to 3 test hosts by name, records the microphone for 5 s,
uses the camera and a screen cast, and opens an approved USB device. The
dashboard's query interface lists exactly those events: 0 missing, 0
extra, every time within 1 s. Each revoke takes effect within 1 s (`EIO`,
`ECONNRESET`, stream ended, driver stopped). An offline image of the disk
holds 0 occurrences of the test hosts' names or the file names outside
ciphertext. 10^6 synthetic records leave the store at or under 64 MiB with
the oldest day gone first, and records older than the retention are
unreadable after the daily prune. Cost row: TCP connect rate and 64-byte UDP
send rate with recording on are at least 99% of recording off
([benchmarks.md](benchmarks.md)).

### F3. Hidden encrypted vault

**Default:** off: it costs disk space, needs the user to understand its
limits, and its deniability depends on how it is used.

**What the user sees.** In Settings, "Create vault": a fixed-size encrypted
volume opened with a passphrase. Optionally a hidden vault inside it, with
a second passphrase. Typing either passphrase at "Open vault" opens the
matching volume; nothing on screen, in Settings or on disk says how many
passphrases a vault has. Apps started from an open vault see only that
vault. The Settings text, before creation and on the vault page, is the
"does not protect against" paragraph below.

**What is achievable on this design.** A copy-on-write file system with a
space map, TRIM, snapshots and logs breaks deniability easily: the space
map says which blocks are free, TRIM shows which are unused, snapshots and
logs keep old states and names. So the vault never lives inside a file
system that does any of that. The strongest design that is actually
achievable here is a hidden volume inside the outer volume's free space,
on a fixed region that nothing else writes, trims or snapshots:

**Mechanism** (builds on the volume map and KDF rule in
[storage.md](storage.md), private homes and provenance in
[capabilities.md](capabilities.md), and "Crash reports stay local" in
[threat-model.md](threat-model.md)):

- *A fixed region.* In stage 1 the vault is a set of 1 GiB extents of the
  volume map, owned by the user and marked "vault"; in stage 2 it is a
  reserved region outside the file system's allocatable space, like the
  hibernation region (a requirement for `fs/docs/DESIGN.md`). At creation
  the whole region is filled with output of the kernel generator. The
  discard policy never sends TRIM to any part of a vault region, whether
  or not it holds a hidden volume, because trimming the outer volume's
  free space except one island would show the island.
- *Constant-size header.* The first 64 KiB of the region holds a 32-byte
  salt, the KDF parameters and exactly 8 slots of 512 bytes. A passphrase
  is stretched once (Argon2id under the one KDF rule) and the result is
  tried against all 8 slots with XChaCha20-Poly1305; a slot that opens
  holds a volume key (AES-256-XTS), the volume type and the list of ranges
  that volume occupies (at most 16). Slots not in use are random bytes from
  creation and are never rewritten, so a used slot, an unused slot and a
  hidden volume's slot look alike, and opening tries all 8 whatever the
  passphrase, so the time doesn't depend on which slot opens. No vault is
  sealed to the TPM, and opening one writes nothing to TPM NV indices.
- *Outer and hidden volumes.* The outer volume is an ext4 file system made
  by Optimal's `mkfs` with the fixed feature list, spanning the whole
  region after the header, with its group metadata packed at its start
  (a flex-group size that puts every bitmap and inode table in the first
  eighth of the volume). The hidden volume is placed in ranges at the top
  of the region that the outer file system reports free, skipping the
  outer's backup superblocks. Inside it is another ext4. The outer's block
  allocator fills from the bottom of the volume up. When the hidden volume
  is open, or the outer is opened with both passphrases ("protect hidden
  vault"), the hidden ranges are marked in use in the outer's in-memory
  allocator only, never on disk, and an outer write that would need them
  fails with `ENOSPC`. When the outer is opened alone, the system can't
  know the hidden ranges, and an outer write there destroys hidden data.
  Opening the hidden volume writes nothing outside its own ranges: no
  outer superblock or header field changes. Every vault, with or without
  a hidden volume, is made the same way.
- *No leaks from the running system.* Apps started "in the vault" run as
  vault child identities: their private homes, `XDG_*` directories,
  thumbnails, recent-files lists and shell histories live inside the open
  volume; their `/tmp` is in RAM; they get no folder grant outside the
  vault and no persistent document-portal entry; their pages never go to
  disk swap (compressed swap in RAM only); and the log service, the
  privacy dashboard (F2) and the kernel log keep nothing about them past
  the close of the vault. The vault's mounts exist only in those
  identities' views. Hibernation is refused while any vault is open.
  Kernel crash dumps already exclude user pages.

**What it does not protect against** (also the Settings text and the
scorecard row). It defeats **one-time inspection of a powered-off disk**
by someone who knows Optimal supports vaults, has the outer passphrase,
and finds a vault: they see a vault, its size and an outer volume with
free space, and can't tell whether a hidden volume exists. It does **not**
defeat: two or more images of the disk taken at different times (blocks
the outer volume calls free change between them); a compromised running
system, such as malware, a keylogger or a compromised kernel; someone who
forces the outer volume open while the hidden one is open on a running
machine (the running system knows both); TRIM-pattern analysis on an SSD
if the no-TRIM rule is broken, for example by copying the vault region
with a tool that trims; an SSD's own flash, which may keep earlier
versions of rewritten blocks; an outer volume that is plainly never used,
or whose timestamps show it unused while the vault was opened; data the
user copies or sends out of the vault; and coercion: deniability is a
claim the adversary may not believe. Opening the outer volume alone can
overwrite the hidden volume. The existence and size of a vault are visible
to anyone with the disk.

**Phase.** Phase 12 on stage 1. Needs the volume map (5), private homes and
child identities (5), the desktop (7) and the settings app (12). On stage
2 it waits for the reserved-region requirement in the file system track,
whose done-when reruns this test, and until then Settings says "vault:
stage 1 installs only".

**Test (Phase 12).** (a) 50 vaults with a hidden volume and 50 without, each
8 GiB, made by the vault tool; every outer filled to 30% by the same
scripted workload; every hidden one also used by a vault workload (a text
editor, an image viewer that makes thumbnails, a terminal with shell
history, a browser download); each machine powered off and imaged once. A
distinguisher suite given each image and its outer passphrase (slot
entropy, the NIST SP 800-22 suite over every region the outer reports
free, the outer's metadata and timestamps, the volume map, the system and
home volumes decrypted with their keys, swap, the log service's files,
loader state and TPM NV contents) gets at most 58 of the 100 right (58 is
the 95th percentile of guessing). (b) A scan of those images finds 0
canary bytes, names or hashes from the hidden workload outside the hidden
ranges. (c) The same suite given two images of one disk, before and after
a hidden workload, finds the hidden volume; its success rate is published
as the evidence for the first limit above, not counted as a pass or fail.
(d) The recorded block request stream of a 24-hour workload with `fstrim`
on every volume holds 0 discards to any vault region. (e) With "protect
hidden vault", 10 GiB of outer writes put 0 bytes into the hidden ranges
and the outer reports `ENOSPC` when full. (f) Hibernation with a vault
open is refused, and a canary page of a vault identity is never found in
disk swap.

### F4. Panic lock

**Default:** off. It changes how the machine unlocks (the full passphrase,
not the PIN, until the next boot) and can lose the last second of unsaved
writes, so the user turns it on knowingly; first-run setup offers it.

**What the user sees.** A chord the user picks (default Ctrl+Alt+Shift+
Backspace held for 0.5 s) locks the screen at once. Unlocking needs the
user's full login passphrase; the PIN is refused until the next boot.
Settings offers three modes: **freeze** (the default: apps are frozen and
resume after unlock), **panic hibernate** (Phase 10: an encrypted image is
written, then power off) and **panic power-off**.

**Mechanism** (builds on "Suspend wipes the disk key" and class C eviction
in [storage.md](storage.md), the lock screen that fails closed and the
in-kernel input paths in [threat-model.md](threat-model.md)):

- *Detected in the kernel.* The chord is recognised in the kernel's input
  path, like the secure-attention key, and only from the same trusted
  sources, so injected input can't trigger it and frozen user space can't
  block it.
- *Order.* User space is frozen (every identity's group); dirty pages of
  encrypted volumes are written back for at most 1 s (a setting, 0 to
  10 s); pages still dirty then are dropped and their files keep the
  writeback error (errseq), so up to that bound of recent writes can be
  lost; the volumes are frozen; then the wipe below runs and the lock
  screen takes over.
- *What is wiped.* The volume key and S, every home-volume key and (stage
  2) every per-user, class and per-file key, with their expanded AES key
  schedules and the vector registers of every CPU (an IPI to each); the
  secrets held after capping (each R, the TOTP key and M, both
  stretched-PIN halves, [boot.md](boot.md)); every clean page-cache page
  of an encrypted volume, zeroed, and the dentry and inode caches of those
  volumes, which hold file names; and every key the SSH agent, GPG agent
  and secret service hold decrypted.
- *How it unlocks without the TPM.* PCRs 8 and 9 are capped, so the TPM
  can't release anything again before a reboot. At login the kernel draws
  a random 256-bit panic key P and keeps a blob of P wrapped under the
  user's stretched login passphrase (XChaCha20-Poly1305 under HKDF); P is
  held like the other post-cap secrets. At panic the keys above are
  encrypted under P into a blob in RAM, then P is wiped. Unlock stretches
  the typed passphrase (Argon2id, about 2 s), unwraps P and restores the
  keys; the volumes thaw and the apps resume. With several users logged
  in, the system keys are in every one's blob and each user's home keys
  only in their own. With no session unlocked since boot there is no P,
  and a panic needs a reboot. A RAM dump after a panic holds only blobs
  that need an Argon2id passphrase search, as for a powered-off disk.
- *Panic power-off* zeroes every user page and the page cache before
  `TPM2_Shutdown(CLEAR)` and power-off. The kernel also sets the firmware's
  memory-overwrite request (the TCG MOR variable) at every boot where the
  firmware supports it, so a forced reset in the middle makes the firmware
  clear memory before anything else boots. *Panic hibernate* (Phase 10)
  uses the hibernation path, with the image key wrapped under P instead of
  the PIN, so resume needs the PIN at boot and the full passphrase for the
  image.

**What it does not protect against.** Plaintext already in app memory: in
freeze mode the frozen apps' memory, their compressed swap pages, their
kernel keyrings and their GPU buffers (including what was on screen)
stay, because the apps are to resume. DRAM keeps its contents for seconds
to minutes after power-off (longer when chilled), so a cold-boot attack
that moves the memory soon after a panic power-off can still find what
the zeroing didn't reach, and in freeze mode finds the apps' memory; memory
encryption (AMD SME, Intel TME), where it is on, defeats moving the
modules, not an attack on the running machine. Writes in the last
writeback bound before the chord can be lost. Someone who already
controls the running system before the chord (malware, a compromised
kernel) is not stopped.

**Phase.** Freeze and power-off modes in Phase 5: needs per-user home keys,
the lock screen on the prompt terminal and the in-kernel keyboard paths
(all 5), and group freezing (`cgroup.freeze`), which this feature brings
into Phase 5. Panic hibernate in Phase 10, with hibernation. Stage-2 keys
join when the file system track lands.

**Test (Phase 5, QEMU, 4 GiB guest).** Canary keys are planted as the volume
key, S, a home key, both PIN halves and an SSH agent key; canary
plaintext is read into the page cache from encrypted files, a canary file
name into the dentry cache, and a canary into a test app's memory. The
chord is sent with QEMU `sendkey` on PS/2 and on `usb-kbd` (boot reader),
and 2 s later guest memory is dumped (`dump-guest-memory`) and scanned: 0
hits for every key canary and its AES key schedule, the page-cache
canaries and the file-name canary; the app's canary is found and is
published as the freeze-mode residual. The PIN is refused at the lock
screen in 20 of 20 tries; the passphrase unlocks, the app resumes with its
canary intact and the canary files read back unchanged. In power-off mode,
a dump at the kernel's last instruction before power-off (`-no-shutdown`)
holds 0 hits for the app canary as well. The chord injected through the
virtual keyboard, `uinput` and a Bluetooth test keyboard never triggers a
lock. Time from chord to keys wiped with the writeback bound at 0 is at
most 200 ms on the reference desktop (scorecard row). Phase 10 adds: after
panic hibernate the image holds no key canary, and resume without the full
passphrase is refused.

### F5. Travel mode

**Default:** off.

**What the user sees.** The user marks folders as travel folders. "Start
travel mode" seals them: they disappear from every app and from the
terminal (`ENOENT`), and their keys leave memory. "End travel mode" asks
for the travel passphrase (separate from the login passphrase by default)
on the trusted path and brings them back. An option, "leave the key at
home", moves the travel key out to a file or printed code and removes it
from the device until it is brought back.

**Mechanism** (builds on the volume map and home volumes in
[storage.md](storage.md), class C eviction there, and revocation in
[capabilities.md](capabilities.md)):

- *Stage 1.* Each user's travel folders live in a travel volume, extents
  of the volume map with its own ext4 and AES-XTS key, wrapped only under
  the stretched travel passphrase (no TPM, no PIN, not the login secret).
  Unsealed, it is bind-mounted at each folder's real path in the views
  that hold grants to it. Sealing writes back dirty pages, revokes every
  open file description in it (`EIO`, Settings lists the apps holding
  files open first), unmounts it from every view, zeroes its page cache
  and its dentries, and wipes its key. "Leave the key at home" exports the
  wrapped key and overwrites its header slot with random bytes.
- *Stage 2.* A travel class in the own file system: a class key wrapped
  only under the travel passphrase, evicted on seal as class C is on lock,
  with the travel folder's entry name encrypted under it and left out of
  listings while sealed (a requirement for `fs/docs/DESIGN.md`).
  Snapshots of travel folders (F12) are under the same key.
- The Files app and the thumbnailer keep their caches for travel folders
  inside the travel volume, and the privacy dashboard (F2) stores records
  about them there.

**What it does not protect against.** Anyone with the disk can see that
travel extents exist and their size, and anyone who knows Optimal can ask
for the travel passphrase: travel mode hides folders from someone looking
through an unlocked session, not from coercion (that is F3's job, with
F3's limits). Data in a folder that existed before it was marked stays in
the home volume's freed blocks until they are overwritten, and an SSD may
keep it in flash regardless; folders created as travel folders don't have
this problem. Other apps' own caches (an editor's recent-files list in its
private home) can keep names or previews outside. "Leave the key at home"
overwrites the slot, but an SSD may keep the old slot in flash. While
travel mode is off, the folders are as exposed as any other.

**Phase.** Phase 12 on stage 1. Needs the volume map (5), the desktop (7)
and the settings app (12). Stage 2 with the file system track, whose
done-when reruns this test.

**Test (Phase 12).** `~/TravelTest` is created as a travel folder holding
canary files, which an editor and an image viewer open. After sealing: (a)
`ls ~`, `stat` and `openat` from the terminal and 3 app identities give
`ENOENT` for the folder and list 0 entries; (b) a guest memory dump 2 s
later holds 0 hits for the travel key and the canary plaintext; (c) an
offline image, with the home volume decrypted, holds 0 canary hits; for a
folder converted after it held canaries, the hit count is published as the
residual; (d) with the key left at home, the right passphrase fails to
unseal in 100 of 100 tries, and with the key file restored it unseals with
every canary intact.

### F6. Network leak guard

**Default:** encrypted DNS on, with the plain-DNS fallback rule "ask on each
new network"; the VPN-only kill switch off, because with no VPN configured
it would mean no network.

**What the user sees.** In Settings, network page (trusted path): "Only use
the internet through my VPN", naming the VPN app; while it is on and the
VPN is down, nothing but the VPN app reaches the network, and the status
area says so. "Encrypted DNS" with a provider chosen from a shipped list
(DNS-over-TLS or DNS-over-HTTPS), and the plain-DNS fallback: "never",
"ask on each new network" or "allow".

**Mechanism** (builds on the per-identity egress filter and the resolver in
[capabilities.md](capabilities.md), "Networking"):

- *Enforced in the kernel egress filter.* With the guard on, every
  non-loopback send on any non-tunnel interface (Ethernet, Wi-Fi, USB and
  Bluetooth tethering, IPv4 and IPv6) is refused with `ENETUNREACH`,
  except: DHCPv4 and DHCPv6, router and neighbour solicitation on the link
  (link-local only); and packets from the designated VPN identity to its
  configured server endpoints. Traffic of every identity, the TCB services
  included (resolver, time service, update client), then goes out only
  through the VPN's tunnel interface, a TUN interface that identity owns in
  the system network namespace. Local-network access (private, ULA and
  link-local ranges) is a separate exception, off by default.
- *From boot.* The guard setting is in the TCB settings store, and the
  network stack brings no interface up until the egress filter holds it,
  so nothing leaks before the VPN app starts.
- *The VPN app* is designated on the trusted path, never by a request,
  and Settings says it sees all traffic. Its endpoints are stored as
  addresses; a host name is resolved when the profile is saved and later
  only through the tunnel.
- *Encrypted DNS.* The resolver at `127.0.0.53` sends its upstream queries
  over DNS-over-TLS (RFC 7858) or DNS-over-HTTPS (RFC 8484), with the
  provider's certificate checked against the system image's roots, through
  a pinned, permissively licensed TLS crate counted in the ledger. Until
  the user picks a provider, it uses the network's own resolver upgraded
  through Discovery of Designated Resolvers (RFC 9462) where offered. When
  no encrypted path works, the fallback rule decides: "never" fails the
  lookup (`EAI_AGAIN`), "ask" prompts on the trusted path once per network
  (identified by its gateway's address and link name) and remembers, and
  "allow" uses plain DNS on that network.

**What it does not protect against.** The VPN provider and the DNS provider
see the traffic and the queries they carry. Traffic analysis (timing,
volumes, destinations of the tunnel itself) is not hidden. Server names in
TLS handshakes are visible to whoever sees the tunnel's exit, unless the
site uses Encrypted Client Hello. When the guard is off, or plain DNS is
allowed on a network, that network sees queries. A captive-portal network
needs the guard paused for that network, during which traffic is not
protected and the status area says so. A compromised kernel is not
stopped.

**Phase.** Phase 6. Needs the full network stack, the egress filter, the
resolver and the time service (6) and `/dev/net/tun` (milestone 8, 6).

**Test (Phase 6).** On QEMU with a capture on the host bridge started before
the guest powers on, and a WireGuard test server (an external tool) on
the host: (a) with the guard on and the VPN app not started, 20 boots each
put 0 packets on the wire other than DHCP, link-local solicitations and
the VPN app's handshakes to its endpoint, and an app with network gets
`ENETUNREACH`; (b) 1,000 kills of the VPN app during a download leave 0
packets of the app's flow outside the tunnel, over IPv4 and IPv6 each;
(c) with encrypted DNS on, 10^4 lookups put 0 packets on port 53; (d) with
the encrypted path blocked and fallback "never", 1,000 lookups fail with
`EAI_AGAIN` within 5 s each and 0 packets go to port 53; with "allow", plain
DNS works on that network and on no other. Cost row: the large TCP download
through the tunnel with the guard on is at least 99% of the same with the
guard off ([benchmarks.md](benchmarks.md)).

### F7. Tamper alert

**Default:** on: it only warns, costs nothing measurable, and stays local.

**What the user sees.** Before the PIN, the loader's existing warnings
(Secure Boot off; PK, KEK or db changed) and the anti-phishing code. After
unlock, before the desktop, a plain-English list of what changed since the
last good boot, each with the time of that boot: "Firmware changed",
"Firmware settings changed", "Add-in card firmware changed", "Boot loader
changed", "Disk partition table changed", "Secure Boot keys changed",
"Optimal's kernel or settings changed". A change Optimal itself made (an
update it installed, a dbx update it applied) is listed as "expected" with
its date, not as a warning. Booting is never blocked.

**Mechanism** (builds on "Measurements", "What a correct code proves" and
the anti-phishing code in [boot.md](boot.md)):

- At every boot the kernel already replays the event log against the
  PCRs. After a boot is marked good, the kernel stores a record of it: the
  event digests and types per PCR for PCRs 0 to 5 and 7, and the loader,
  config and kernel digests of PCRs 4, 8 and 9, with the record
  authenticated by HMAC-SHA-256 under HKDF(S, `optimal-tamper-record`) and
  kept on the encrypted system volume. Only a kernel that passed the
  pre-cap check holds S, so root can't forge a record.
- The next boot compares its replayed log with the record, PCR by PCR,
  and maps each difference to its sentence (PCR 0 firmware code, 1
  firmware settings, 2 option ROMs, 3 their settings, 4 boot applications,
  5 the GPT partition table the firmware measures, 7 Secure Boot keys and
  revocations, 8 and 9 Optimal's config and kernel). Changes the update
  client or the dbx flow recorded as its own are "expected". A firmware
  event whose digest changed on each of the first 5 boots (some firmware
  measures a boot counter) is treated as noise on that machine and listed
  in Settings as ignored.
- The loader keeps an unauthenticated copy of the PCR 0 to 5 digests in
  loader state and shows a "may have changed" hint before the PIN; it is a
  hint only, because loader state is writable.
- A record that fails its check gives "the tamper record could not be
  checked", and after a TPM clear (S changes) "TPM was cleared, no
  comparison possible".

**What it does not protect against.** The limits [boot.md](boot.md) already
lists: without a firmware setup password a correct code doesn't prove the
firmware unchanged. Firmware measures itself, so a firmware implant on a
machine without a hardware root of trust for its first measurement can
measure what it likes. A compromised running kernel holds S and can forge
the record. The pre-PIN hint can be suppressed by rewriting loader state;
the authenticated list after unlock can't. Without a TPM (B2 to B4) there
is no event log to check: the kernel compares only the GPT, the loader
files on the ESP and the Secure Boot variables it reads itself, which an
attacker controlling firmware can hide, and Settings says so.

**Phase.** Phase 4b. Needs the TPM driver, event-log replay, S and capping
(4b); shown on the console in 4b and as a trusted-path notification from
Phase 7.

**Test (Phase 4b, QEMU with OVMF and swtpm).** Six changes, each made between
two boots, 20 runs each: another OVMF build (PCR 0), a changed UEFI boot
variable (PCR 1), an option ROM added to a NIC (`romfile`, PCR 2), an extra
db certificate (PCR 7), the loader replaced by another signed build at the
same SVN (PCR 4), and a partition added to the GPT (PCR 5). Each produces
exactly its sentence and no other in 20 of 20 runs. 20 unchanged reboots
produce 0 alerts. An OS-applied kernel update and an OS-applied dbx update
are each listed as "expected" in 20 of 20 runs. A record rewritten by root
through a test hook gives "could not be checked" in 20 of 20 runs.

## Performance

### F8. Game mode overlay

**Default:** off; it is a display aid, toggled by a key or by game mode.

**What the user sees.** A small panel over the focused app: its frame time
for the last frames, P99 and P99.9 frame time over the last 60 s, an
estimated input latency, and the 5 identities using the most CPU over the
last second.

**Mechanism** (builds on the compositor's presentation feedback and
"Desktop awareness" and groups in [scheduler.md](scheduler.md)):

- *Frame times* are the intervals between presentation timestamps of the
  focused surface's committed frames, from the compositor's own
  presentation feedback (the same timestamps `wp_presentation` gives
  apps); P99 and P99.9 are over the frames of the last 60 s (at 144 Hz,
  8,640 frames).
- *Input latency* is estimated as the time from the kernel's timestamp of
  an input event to the presentation of the first frame the surface
  committed after that event was delivered to it. It leaves out scanout
  and the panel's response, so it is labelled "estimated", and its offset
  from the input-latency rig's photodiode measurement is published.
- *CPU users* come from the per-app group accounting the scheduler keeps.
- *Cost.* The panel is redrawn at most 4 times a second into a cached
  texture. When the focused surface is on direct scanout, the panel takes
  its own hardware plane so the game stays on direct scanout; with no free
  plane the compositor composites and the panel says "+1 frame: no free
  plane". It is left out of screen casts, because it names other apps.

**What it does not protect against** (accuracy). The input figure is an
estimate, not photon-to-photon latency. Frame times are those the
compositor presents, not those the game renders internally. On GPUs with
no free plane the overlay itself adds composition.

**Phase.** Phase 8. Needs the compositor (7) and the real GPU driver with
hardware planes (8), measured with the input-latency rig (7).

**Test (Phase 8, bare-metal reference desktop, discrete GPU, 144 Hz).** A
pinned fullscreen Vulkan test app on direct scanout, 20 boots per
condition: with the overlay on, its CPU cost is at most 0.5% (one-sided
95% bound); input-to-display latency P99 (rig) and frame time P99 are no
worse than with it off (the "no worse" rule in
[benchmarks.md](benchmarks.md)); the overlay's P99 and P99.9 equal the
harness's values computed from the same presentation timestamps exactly;
its per-frame times agree with the rig's within 0.5 ms at P99 after the
median offset is removed.

### F9. Focus boost and background freezing

**Default:** both off: each changes how much CPU background work gets, and
a hidden compile or render shouldn't slow down or stop unless the user
asked.

**What the user sees.** Settings, performance page: "Give the app I'm using
more CPU" (×2, ×4 or ×8) and "Freeze apps I can't see", with a list of
exempt apps and why each is exempt.

**Mechanism** (builds on "Desktop awareness" and "Groups" in
[scheduler.md](scheduler.md)):

- *Boost.* The compositor already tells the scheduler which identity is in
  the foreground, whose threads get half the default slice. With the boost
  on, that identity's group `cpu.weight` is multiplied by the chosen factor
  while it has focus. This changes fair shares, not only deadlines: with
  ×4 against one busy background app, the split under contention is 80/20.
  No background group goes below its own weight's share, so nothing
  starves. Real-time and deadline classes are unaffected.
- *Freezing.* An identity none of whose surfaces has had a visible pixel
  on any output for 30 s, and which isn't focused, has its group frozen
  (cgroup v2 `cgroup.freeze`): the scheduler takes its threads off the run
  queues at their next return to user space, with no signal, no `SIGCHLD`
  to the parent and no ptrace stop; its timers keep running and fire after
  the thaw. The same freeze is used for class-C apps while locked
  ([storage.md](storage.md)).
- *Exemptions* (any one keeps an identity running): a PipeWire stream open
  for it (playing audio, or a call); a camera, microphone or screen-cast
  session; network transfer above 1 KiB/s averaged over 30 s, from the
  egress filter's byte counts (downloads); a Background portal grant; the
  terminal identity and everything it runs; the TCB services; and any app
  the user exempts.
- *Thaw* happens before the identity becomes visible or focused, before a
  notification action or a D-Bus activation addressed to it is delivered,
  and before an exemption check flips.

**What it does not protect against** (side effects). A frozen app's network
peers may time out, and a chat app below the 1 KiB/s threshold without a
Background grant misses messages until shown. A frozen app's memory stays
allocated (reclaim and compressed swap still apply). The boost doesn't
speed up an app that is limited by the GPU or I/O.

**Phase.** Phase 10, the scheduler-tuning phase. Needs groups (5), the
compositor's focus and visibility information (7) and PipeWire (6).

**Test (Phase 10, reference desktop).** (a) Freezing: 5 hidden test apps,
one playing audio, one downloading 1 GiB from the test server, one in a
microphone stream, one with a Background grant, one plain: after 30 s
hidden only the plain one is frozen, its CPU time advancing 0 ns over
60 s; the download completes, the audio has 0 underruns, and showing the
frozen app gets its first new frame presented within 100 ms. (b) Boost:
the 60 Hz frame-pacing test app in focus with a pinned compile running at
full width: with ×4 boost its frame time P99 is at most the refresh period
plus 1 ms, and the compile's wall time is at most 1.5 times its time with
the boost off (20 boots each, scorecard rows).

### F10. Power meter and per-app power caps

**Default:** meter on (read-only, cost at most 0.1% CPU); caps off.

**What the user sees.** Per app, its power in watts now and its energy
over the last hour and day, and the part of the machine's power not
attributed to apps ("screen, radios and the rest"), with the stated
accuracy for that machine. Optionally a cap in watts per app.

**Mechanism** (builds on "Frequency control" and "Groups" in
[scheduler.md](scheduler.md) and the battery driver in
[drivers.md](drivers.md)):

- *Sources.* The kernel reads the RAPL energy counters (package, cores and
  DRAM where present; on AMD, core and package) and, from Phase 8, the
  GPU's energy counter, every second, through the core's MSR shim. Battery
  discharge power comes from the battery driver.
- *Attribution.* Each second, package energy above the machine's measured
  idle power is split among identities in proportion to the unhalted
  cycles their threads used (per-CPU `APERF` deltas charged at each
  context switch), which follows frequency changes better than CPU time;
  GPU energy is split by each identity's GPU engine busy time. The idle
  part and everything not covered by counters (screen, radios, storage)
  is shown unattributed. Attributed plus unattributed equals the measured
  total by construction.
- *Not readable by apps.* Fine-grained energy counters leak secrets
  (PLATYPUS), so `/sys/class/powercap` is absent from every app view
  ([compat.md](compat.md)), system-wide power events are refused by
  `perf_event_open` (user-space counters only, lockdown table), and the
  per-app figures, updated at most once a second, are shown only in
  Settings.
- *Caps.* A 1 s control loop adjusts the app's group `cpu.max` so its
  attributed 10 s average stays under the cap, never below 1% of one CPU,
  so the app keeps running. GPU power is not capped.

**What it does not protect against** (accuracy). RAPL is the CPU's own
estimate on many models, not a measurement. Cycle attribution undercounts
apps that are limited by memory bandwidth and overcounts those that run
from cache. Shared work done for an app by a TCB service (the compositor
drawing it, PipeWire mixing it) is charged to that service. Accuracy is
published per reference machine.

**Phase.** Phase 10, with power management. Needs per-app groups (5), the
battery driver (6) and the compositor (7); GPU attribution with the GPU
driver (8).

**Test (Phase 10, reference desktop and laptop, 20 runs each).** (a) Two
pinned CPU-bound loads at three intensities, each first alone (its
increment over idle measured) and then together: each load's attributed
power is within 10% of its solo increment (one-sided 95% bound); (b)
attributed plus unattributed equals the RAPL package total within 1% over
every second; (c) the meter's CPU cost in the idle window is at most 0.1%;
(d) a 10 W cap on a load that draws 25 W keeps its attributed 60 s average
at or under 11 W; (e) an app's `open` of `/sys/class/powercap` gives
`ENOENT` and a system-wide power `perf_event_open` gives `EACCES`.

## Reliability

### F11. System undo from the boot menu

**Default:** on: a snapshot of the settings before every update and every
settings change, kept 30 days in at most 64 MiB, and an "Undo" entry in the
loader menu.

**What the user sees.** In the loader menu, "Go back to…" with up to 8
points (for example "this morning, 08:12, before the update to 1.4"). After
choosing one and unlocking, the system boots with the settings of that
time, and the release of that time when that is still allowed; the screen
says plainly what was restored and what wasn't.

**Mechanism** (builds on A/B slots, loader state, anti-rollback and boot
counting in [boot.md](boot.md), and "User data is separate" there):

- *What is snapshotted.* The system settings (the TCB settings store and
  system-wide configuration) and each user's desktop settings, as a
  transactional copy on the system volume before every change (stage 2: a
  file-system snapshot of the settings tree). User files and app data are
  never part of an undo; F12 covers files.
- *What "undo" restores.* The system image has only two slots, the running
  release and the one before it, so undo can boot at most the previous
  release, and only if its SVN is at or above the anti-rollback floor. To
  a point before a security update whose floor raise has completed, undo
  restores that time's settings on the current release, and the screen
  says why ("Optimal 1.5 stays, because older versions have a fixed
  security hole; your settings are back to this morning"). Undo never
  boots an image below the floor. Before the floor raise has completed,
  the previous slot is still allowed; booting it drops the pending raise
  request (its SVN is lower than the request, as [boot.md](boot.md)
  already rules), and the newer release offers itself again under F14.
- *What undo never restores.* Grants and device authorizations the user
  revoked stay revoked, and a security setting that would be loosened by
  the undo (the prompt level, F1's kill switches, F6's guard) is shown on
  the trusted path after unlock and changed only if confirmed there. A
  settings snapshot older than the running release's settings format is
  migrated forward, never applied raw.
- *The menu.* The loader can't read the encrypted volume, so loader state
  holds the 8 points' labels (time, release, SVN); choosing one writes a
  "restore point n" request that the kernel checks after unlock against
  the authenticated snapshot list, applying it before user space reads
  settings. A forged label can at worst offer a point that doesn't exist,
  which the kernel reports.

**What it does not protect against.** It can't go back more than one
release, below the floor, or in user files. It doesn't undo changes made
outside Optimal (firmware settings). A settings change that damaged the
encrypted volume itself isn't undone.

**Phase.** Phase 12. Needs the loader menu and loader state (4b), the update
client (7) and the settings app (12).

**Test (Phase 12, QEMU with swtpm).** Settings S0 on release R1; update to
R2 (no security fix); settings S1. Undo to S0 boots R1 with settings
byte-identical to the S0 snapshot and the home tree's hashes unchanged; a
grant revoked after S0 stays revoked; a loosened prompt level waits for
confirmation. Then R3, a security release, with its floor raise
completed: undo to before R3 boots R3 with the older settings and shows
the floor message, and in 20 of 20 attempts no image below the floor
boots. A power cut at each write of a restore boots with either the old or
the restored settings complete, never a mix.

### F12. File time-travel

**Default:** on where the own file system is in use: a snapshot of each
user's tree every hour, keeping 24 hourly, 7 daily and 4 weekly ones,
oldest dropped first when free space falls below 10%.

**What the user sees.** In the Files app, "Versions" on any folder: browse it
as it was at an earlier snapshot and restore one file, or all of it, to
its current place. On ext4 volumes, the option says "needs the Optimal file
system" and isn't offered.

**Mechanism** (builds on own file system snapshots, "a user feature" in
[storage.md](storage.md) and the ROADMAP's file system track, the metadata
reserve that keeps snapshot removal possible on a full disk, and the file
view in [capabilities.md](capabilities.md)):

- A snapshot view is shown only for paths the browsing identity can read
  now, through the same file view filtered at that time, so time-travel
  never widens a grant; protected paths (app private homes) are restored
  only from Settings on the trusted path.
- A restored file keeps its provenance tag, mode and extended attributes
  from the snapshot.
- Class-C files in snapshots need the class-C key, so they can't be browsed
  while locked; travel folders' snapshots are under the travel key (F5).
- "Delete from history" removes a file from every snapshot (a requirement
  for `fs/docs/DESIGN.md`); its blocks are freed after the K = 8 commit
  quarantine.

**What it does not protect against.** It is not a backup: a failed or
stolen disk loses the snapshots with the files. Snapshots keep data the
user deleted until they expire or "Delete from history" is used. ext4
volumes have no snapshots, so files deleted there are gone.

**Phase.** The file system track, gated by its done-when; the Files app view
with the everyday apps track.

**Test (file system track).** 10^4 random restore operations (create, wait
for a forced snapshot, modify or delete, restore from the snapshot): 0
restored files differ in bytes, mode, extended attributes or provenance
from the snapshot, and 0 other files change. Snapshot creation and
removal are in the crash-state model and pass the oracle. With the disk
100% full, removing a snapshot succeeds. After "Delete from history" and K
commits, 0 canary bytes are readable through any snapshot or the read-only
rescue mount.

### F13. Plain-English safe mode

**Default:** on: every slot's config has a safe-mode entry, and the loader
offers it after a failed boot.

**What the user sees.** After a boot that crashed, hung or failed its health
check, the loader menu offers "Start in safe mode". Safe mode starts a
plain desktop and says, in sentences, what went wrong last time and what
to try ("The graphics driver crashed 3 times while starting. You can go
back to the previous version of Optimal, or keep using safe mode, which
doesn't use it.").

**Mechanism** (builds on per-slot signed configs, boot counting, the health
check, slot fallback and the recovery image in [boot.md](boot.md), driver
stops and crash reports in [drivers.md](drivers.md) and
[threat-model.md](threat-model.md)):

- *A signed entry.* Each slot's signed config carries a safe-mode entry
  (its command line adds `optimal.safe=1`). PCR 8 already depends on the
  (slot, entry) pair and every pair's digest is signed, so safe mode
  unseals with the PIN like a normal boot.
- *Core drivers only.* Safe mode starts the in-kernel drivers on the boot
  path (tiers 0 and 1), the simple framebuffer, the in-kernel keyboard and
  mouse drivers (the tier-2 USB boot-protocol ones included) and the boot
  device's storage driver; no GPU driver (llvmpipe over
  the simple framebuffer), no Wi-Fi, Bluetooth or audio, no tier-3 driver,
  no autostart entries and no user services beyond the session,
  compositor and portals. Wired networking is off until the user turns it
  on.
- *What failed.* The kernel keeps a boot health record of the last 8 boots
  on the system volume: the outcome (marked good, crashed with the
  component and reason from the crash report header, hung, or the health
  check item that failed), driver stops and restarts in the first 5
  minutes, and apps that crashed within 30 s of login. A published table
  maps each record kind to a sentence and a suggested action.
- *It changes nothing.* A safe-mode boot doesn't decrement or reset the
  slot's tries, doesn't mark the slot good or bad, and never raises the
  floor. Both slots failing still leads to the recovery image.

**What it does not protect against.** A fault in a component safe mode
still uses (the kernel core, the storage driver, the compositor) breaks
safe mode too; the recovery image is then the fallback. The report names
what the logs recorded; a hang with no record is reported as "stopped
responding, cause not recorded".

**Phase.** Phase 12. Needs per-entry signed configs and the loader menu (4b),
the compositor (7) and the settings app (12).

**Test (Phase 12, QEMU with swtpm).** Three faults, 20 runs each: a kernel
whose virtio-gpu driver panics at probe, an autostart test app that
crashes the session within 5 s of login, and an update service made to
fail so the health check fails. After each, the loader offers safe mode;
safe mode reaches the desktop within 60 s without the faulty component;
its report names the right component in 20 of 20 runs and a wrong one in
0; the PIN unseals with no recovery prompt; and loader state's good flag,
tries counter and the floor are unchanged by the safe-mode boot.

## User respect

### F14. No-surprise updates

**Default:** on: updates download and are staged into the inactive slot
automatically, and are switched to only at a restart the user chooses;
"ask before downloading" is an option.

**What the user sees.** "An update is ready" with its full changelog;
"Restart now", "Tonight at…" or "Remind me". Optimal never restarts without
the user saying so. A security update says so, names the fixed advisories,
and shows how many days this machine has been running with them unfixed.

**Mechanism** (builds on the update client, A/B slots, the two-stage floor
raise and loader updates in [boot.md](boot.md)):

- *Staging doesn't restart.* An update writes the inactive slot and its
  config, verifies them and stops there; the slot switch, a loader trial
  boot and a floor raise all happen only at a restart the user chose.
  Optimal has no live patching and no kexec, so nothing changes the
  running kernel.
- *Changelog is signed.* Each release's changelog is part of its signed
  TUF targets metadata and covered by the transparency log, so the text
  shown is the text that was released; a changelog that fails
  verification fails the update.
- *Security updates are offered, never forced.* A staged security update
  shows a daily reminder after 3 days and one at every login after 14
  days, each with the day count. Reminders come from the update client on
  the trusted path, so an app can't fake or suppress one.

**What it does not protect against.** A staged security fix protects
nothing until the restart: until then the machine runs the vulnerable
release, and the day counter says so. Optimal collects nothing from users'
machines, so it can't measure how long users wait; the scorecard publishes
the time from a security release to its availability to the update client
and states that the time to restart is the user's.

**Phase.** Phase 7, with the update client, so the first update client
already follows this policy; the settings-app view in Phase 12.

**Test (Phase 7, QEMU).** A test update server publishes a release with a
changelog. Over 72 hours of advanced guest time with no user input, the
client stages it, and there are 0 restarts and 0 slot switches. "Restart
now" switches slots at that restart. The changelog shown matches the
signed metadata byte for byte, and a release with a tampered changelog is
refused. A security release shows its advisories and the day counter, and
an app's attempts to post or dismiss the reminder through notifications
and D-Bus fail.

### F15. Ad and tracker blocking in the browser

**Default:** on, because tracking requests are a privacy leak by default;
it can be turned off per site.

**What the user sees.** Ads and trackers don't load; a shield in the
address bar shows how many requests were blocked on the page and turns
blocking off for that site.

**Mechanism** (builds on the browser engine track in the
[ROADMAP](../ROADMAP.md) and the signed update client in
[boot.md](boot.md)):

- *In the borrowed browser (interim, Phase 7).* The shipped browser's own
  tracking protection (Firefox's Enhanced Tracking Protection in strict
  mode) is set by the policy file in the system image. No blocker
  extension is shipped: the common ones are GPL, which the image doesn't
  ship ([ROADMAP](../ROADMAP.md), licence); the store can offer them as
  user installs.
- *In the browser engine.* A built-in blocker matching the Adblock Plus
  filter syntax subset used by EasyList: network rules (`||domain^`,
  wildcards, `$third-party`, `$script`, `$image`, `$stylesheet`,
  `$xmlhttprequest`, `$subdocument`, `$domain=`), exceptions (`@@`) and
  element hiding (`##`, `#@#`). It also checks the CNAME chain the system
  resolver returns, so a tracker hidden behind a first-party name is
  matched by its target. EasyList and EasyPrivacy ship as data (CC BY-SA
  3.0), listed with licence and hash in the data ledger.
- *Updates.* Filter lists are targets of their own delegated TUF role,
  signed by a separate online key and covered by the transparency log.
  The update client fetches them daily, verifies them like a release and
  installs them atomically without a restart; a sandboxed helper compiles
  them into the engine's matcher. Lists older than 14 days are flagged in
  Settings.

**What it does not protect against.** First-party and server-side tracking,
fingerprinting the lists don't cover, and trackers no list names. The list
maintainers decide what is blocked. Sites can break, which is why the
per-site switch exists.

**Phase.** Interim tracking protection in Phase 7, with the browser. The
built-in blocker in the browser engine track, gated by its done-when.

**Test.** Phase 7: with the shipped policy, a local test page that embeds
10 resources from domains on the browser's shipped tracking-protection
list makes 0 requests to them. Browser engine track: over a pinned corpus
of 50 offline pages
served locally, with EasyList and EasyPrivacy at pinned versions, 0
requests reach a host the lists block and 0 requests the lists allow are
blocked; a list update installs without a browser restart; a list signed
by the wrong key or missing its inclusion proof is refused; per-request
matching is at most 10 µs at P99 on the reference desktop, and page-load
time over the corpus with blocking on is no worse than with it off
(scorecard rows).

### F16. Export everything

**Default:** available at any time from Settings; exports are encrypted by
default and leave secrets out unless the user selects them.

**What the user sees.** "Export everything" writes one archive: settings,
the list of installed apps with where each came from, and the user's
files, plus a text file describing the layout. "Import" on a new install
restores it, asking again for every grant.

**Mechanism** (builds on the settings store, the store service and Flatpak
launcher, and the file view in [capabilities.md](capabilities.md)):

- *Formats.* A POSIX pax archive; settings as TOML files with a published
  schema per settings group; the app list as JSON (identity, source: store
  ID, Flatpak ref with its remote URL and key fingerprint, AppImage or
  tarball path and hash; the grants it held); files as they are, with
  provenance and other extended attributes as pax records. Encryption with
  the age format (v1), passphrase or recipient key, through a permissively
  licensed implementation.
- *Grants are never imported silently.* Each imported grant is shown for
  approval again, and combined files-and-network approvals are asked
  afresh.
- *Secrets* (SSH and GPG keys, secret-service items) are included only if
  selected, after a confirmation on the trusted path, and only in an
  encrypted export.

**What it does not protect against** (scope). "Open formats" covers
Optimal's own settings and lists; app data is exported in each app's own
format, which Optimal can't convert. Store apps are reinstalled from
their source, not copied; an app no longer available can't be
reinstalled. An unencrypted export, if chosen, is readable by anyone who
gets it.

**Phase.** Phase 12. Needs the settings app and store (12) and the Flatpak
launcher (5).

**Test (Phase 12).** Export from a machine with 5 apps (store, Flatpak,
AppImage, tarball, unpackaged), changed settings and 10 GiB of files with
extended attributes; import on a fresh install: 0 file differences
(content, mode, extended attributes, provenance), 5 of 5 apps reinstalled,
every settings value equal, 0 grants restored without a prompt. The
archive lists with `bsdtar`, decrypts with the reference `age` tool, and
every TOML and JSON file validates against its schema with 0 errors. A
canary secret appears in the export only when selected.

## Customisation

### F17. Themes, tiling layouts, key remapping and per-app rules

**Default:** off: the stock theme, floating windows, no remaps and no rules,
because the defaults are the tested configuration.

**What the user sees.** Themes for windows, icons and the panel; tiling
layouts with presets; key remapping (one key to another, chords, layers);
and per-app rules ("always full screen", "open on workspace 2", "always
floating").

**Mechanism** (builds on the compositor, "Trusted input" and "Prompts can't
be faked" in [capabilities.md](capabilities.md), and the settings portal
there):

- *Remapping is applied after the trust decision.* The secure-attention
  key and F4's chord are recognised in the kernel's input path on the raw
  key events before any remap exists. Remap rules run in the compositor and
  apply only to events delivered to app surfaces; the prompt layer and the
  lock screen receive the device's unremapped events with the keyboard
  layout chosen on the trusted path. Events a remap produces are, like the
  virtual keyboard's, never the secure-attention key and never a prompt
  answer.
- *Rules are keyed by attested identity,* never by the `app_id` a client
  sets, so an app can't claim another app's rules. No rule can put a
  surface above the prompt layer or F1's indicators.
- *Themes are data.* GTK and icon themes go through the settings portal
  (CSS and images, decoded in the sandboxed helper); the prompt layer and
  the indicators keep a fixed look no theme changes.
- Remaps and rules are changed only in Settings, written by the settings
  portal; no app can write them.

**What it does not protect against.** A theme can make an app window look
like a prompt; what makes a prompt trustworthy is that it appears after
the secure-attention key and in the prompt layer, which a theme can't
reach. A remap the user makes can make their own typing in apps confusing.

**Phase.** Phase 12, with the desktop track's built-in tiling. Needs the
compositor (7) and the settings app (12).

**Test (Phase 12).** 10^4 generated remap rules (to and from Ctrl, Alt,
Delete, Backspace, Shift, chords and macros), each exercised from a test
keyboard: 0 prompt-terminal switches, 0 panic locks, 0 prompt answers and
0 lock-screen inputs produced by remapped events, and 1,000 scripted
remaps deliver the expected keysym to an app in 100% of cases. An app that
sets another app's `app_id` gets none of that app's rules, and a rule
"always full screen" never covers the indicators or a prompt.
