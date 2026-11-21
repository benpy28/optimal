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
   ([storage.md](storage.md)), whose keys are evicted by default.

What it does **not** promise: that an app the user granted access to a file
won't misuse that file. Grants are explicit and narrow so the damage is
bounded, and the default grant for an app is nothing.

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
broken in hours); on those machines, and against an active bus interposer on
any TPM, disk protection is only as strong as the PIN, and the scorecard says
so.

## Trusted computing base

Everything below can break every promise if it has a bug. It is kept small,
tested hardest, and listed so nobody pretends the TCB is "just the unsafe
core". Each user-space TCB process has a code-size budget on the scorecard.

- CPU microcode and platform firmware (not ours; see hardware tiers).
- The bootloader ([boot.md](boot.md)).
- The unsafe kernel core (tier 0), every in-kernel safe-Rust component (tiers
  1 and 2 in [drivers.md](drivers.md)), and every dependency of theirs.
- **User-space TCB:** the broker and portals ([capabilities.md](capabilities.md)),
  the D-Bus broker and its per-app proxies, the compositor (with the Mesa
  driver it loads) and XWayland servers' host side, the seat and input
  service, the audio server's permission layer (PipeWire), the lock screen,
  the network configuration services (DHCP client, resolver, time service,
  Wi-Fi supplicant), and the update client.
- Release and policy signing keys.

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
| **B2** | Secure Boot on and trusting Optimal, no TPM | verified boot; passphrase-only unlock; no measurement, so an attacker who can get firmware to run their signed code isn't detected by the disk unlock |
| **B3** | UEFI with Secure Boot off, or not trusting Optimal (with or without a TPM) | **no verified boot**: the boot chain checks its own signatures but anyone can replace it. TPM sealing is not offered (it would seal to an unverified chain). Passphrase-only unlock |
| **B4** | Legacy BIOS | as B3 |

Until Optimal Boot path 1 or 2 works on a given PC, that PC is B3, and the
scorecard says "verified boot: only with owner keys enrolled".

**DMA axis**

| Tier | Needs | What holds |
|---|---|---|
| **D1** | a working IOMMU with interrupt remapping | DMA attacks blocked; per-device isolation |
| **D2** | no IOMMU, or it's off in firmware | DMA attacks from a malicious device are **not** blocked. External PCIe tunnels are refused. Driver bugs still can't aim DMA at kernel data (bounce buffers), but a malicious internal device can. Big warning at install, with how to turn the IOMMU on if the firmware can |

## Rules every component follows

### Default deny

Apps start with no grants. The network is closed inbound. Devices start
unauthorised. There is no "root that can do anything".

### Files and network together are one explicit grant

The most common real-world "remote read" is an app that can both read files
and phone home. So:

- **Network access** is declared in the app's manifest and approved at
  install, or requested later through the network portal. It is never
  prompted for lazily inside a system call: an app without it gets
  `ENETUNREACH` from `connect()` and `EACCES` from creating internet sockets,
  immediately.
- **Persistent file grants** (folders, or files granted to an app for
  longer than one use) and network access can be held together only if the
  user approved that combination explicitly, in a prompt that says so in
  plain words ("This app will be able to read *Documents* and send data to
  the internet").
- **A file the user picks** in the file chooser for one use (an upload, an
  attachment, "open this photo") is handed to the app as an fd even if the
  app has network access, because the user chose to give that file to that
  app. It doesn't widen the app's grants.
- **Secrets are used, not read.** SSH keys, GPG keys, browser and app
  passwords, and tokens live in agent services (an SSH agent, a GPG agent,
  the secret service), each its own app. Programs use them through the
  agent's socket; the agent confirms the first use per session with a
  compositor-drawn prompt naming the requesting app. The files themselves
  (`~/.ssh`, `~/.gnupg`, keyrings, browser profiles, every app's data
  directory) are **protected paths**, never part of any folder grant.

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
either way, which is what keeps `curl | sh` from reaching SSH keys.

### No confused deputies

The launcher portal, the open-URI portal, file and URL handlers and D-Bus
activation never pass a command line, arguments, a `file://` URI or a file
to another app on an app's behalf without a compositor-drawn confirmation
naming both apps. The terminal can't be started with a command by another
app at all.

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
- **Kernel keyrings** are per app; **`uinput`** is available only to the
  input service and compositor.
- **Clipboard** contents go only to the focused app on paste; background
  reads need a grant.

### Untrusted input is parsed outside the TCB where possible

Foreign and removable file systems, USB class drivers, HID report
descriptors, Wi-Fi and Bluetooth, image and font decoding for the TCB, and
network configuration protocols (DHCP, router advertisements) run in tier 3
or in sandboxed services ([drivers.md](drivers.md)). What stays in the kernel
(TCP/IP, the system file system, the boot-protocol keyboard path) is
continuously fuzzed from the phase it lands in.

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
| MDS, TAA, MMIO stale data, RFDS | buffer clearing on kernel exit; on CPUs where SMT siblings can read each other, **SMT is off by default** (core scheduling alone doesn't protect kernel data on the sibling) |
| GDS | microcode mitigation required; without it, AVX gather is disabled for apps |

Everything depends on microcode, which loads first ([boot.md](boot.md)). The
proof-of-concept suite runs every published PoC for each class against the
running kernel; the pass criterion is **zero bytes of secret recovered** above
chance over 10^6 attempts.

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

Dumps go to the crashing user's encrypted storage (kernel dumps to an
administrator-only location, readable only with consent). They exclude key
pages (tested: no key bytes in any dump), are redacted of app memory by
default, and are never uploaded unless the user chooses to send one, each
time, after seeing what is in it.

### Supply chain

From Phase 1: every downloaded build input is pinned by SHA-256 (the Limine
tree, firmware packages, container images, GitHub Actions by commit hash);
`cargo-deny` and `cargo-vet` gate every change; and release builds are
reproducible and a second builder must produce identical bytes before
anything is signed. Release signing keys never live in CI ([boot.md](boot.md)).

## Lockdown table

Optimal has no all-powerful root; these hold for everyone, always. The
`lockdown` CI test checks each row's **expected result** exactly, from Phase 3
for the rows that exist then, growing with the ABI.

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
| `perf_event_open` | own process, user-space counters only |
| `userfaultfd` | user-mode faults only (no kernel-mode faults) |
| `open_by_handle_at`, `name_to_handle_at` | `EPERM` (they bypass path-based views) |
| `uinput`, `/dev/tpm*`, `/dev/tpmrm*` | absent from app views; only the input service and the TPM service hold them |
| NVMe and SCSI passthrough ioctls, firmware download | absent from app views; only the storage service, with signed firmware |
| EFI variables (`/sys/firmware/efi/efivars`) | read-only; writes only by the update and boot services, never to PK, KEK, db, dbx, MOK or boot entries except through their signed flows |
| `mount` of block devices or image files (loop), from any namespace | only through the tier-3 file system server ([storage.md](storage.md)), never an in-kernel parser |
| `CAP_*` inside a user namespace | grants authority only over objects owned by that namespace, never kernel-wide ([capabilities.md](capabilities.md)) |
| hibernation | image encrypted and authenticated (key sealed to the TPM on B1, from the passphrase otherwise), carrying the release's security version; resume refuses an image it can't authenticate or whose version is below the rollback floor |
| ACPI table override, custom DSDT | not supported |

## Every attack path, its defence and its test

The phase in brackets is when the test becomes a gate. Every test stays in CI
forever. The scorecard lists each row as pass, fail or not yet built, per
hardware tier.

| Path | Defence | Test (phase) |
|---|---|---|
| A1 → open port | nothing listens; default-deny inbound firewall | IPv4 and IPv6 scans of a fresh install find 0 open ports (6) |
| A1 → network stack | TCP/IP in safe Rust, fuzzed, index masking | 10^9 fuzz executions, no open crash; IPv6 RA/NDP/ICMPv6 included (6) |
| A1 → DHCP, DNS, NTP, Wi-Fi supplicant | each a sandboxed service in the TCB list, fuzzed; DNS-over-TLS or DNSSEC validation where available; time from NTS with the last-seen update metadata time as a floor | each service fuzzed 10^8 executions; spoofed-NTP test can't roll the clock back below the floor (6) |
| A1 → Wi-Fi, Bluetooth drivers | tier 3, own IOMMU domain | device-side fuzzing 24 h with no open crash (6) |
| A2 → app parser bug | the app is sandboxed | sandbox escape suite (7) |
| A3 → other apps' data | per-app file views, capabilities, per-app `/proc` | hostile-process suite: read another app's files, protected paths, another process's memory (`/proc/pid/mem`, `process_vm_readv`, `ptrace`), forge or guess a capability, map another app's frames. All fail (3, grows every phase) |
| A3 → kernel | memory safety, proofs, fuzzing, SMEP/SMAP, KASLR, CET | syscall fuzzing (3), proof targets 4 to 6 (3), lockdown test (3) |
| A3 → shared channels | D-Bus proxy, PipeWire permissions, Wayland security contexts, per-app XWayland, per-app keyrings | for each channel, an app without the grant tries to use it and fails (7) |
| A3 → confused deputy | portals pass nothing without confirmation | an app tries the launcher, open-URI, handlers and D-Bus activation to run a command or open a file through another app; all need confirmation (7) |
| A3 → screen, input, mic, camera | grants only through portals; prompts drawn by the compositor | prompt-spoofing test; an app can't read another window's pixels or keys (7) |
| A3 → exfiltration | files + network is one explicit grant; secrets used through agents | **exfiltration suite**: a canary file in a granted folder and a canary SSH key; an app (and a script run from the terminal) without the combined grant tries every channel (sockets of every family, io_uring socket ops, every portal, clipboard, D-Bus, DNS lookups, notifications, launching a networked app); the canary must not reach an external sink (6 for sockets, 7 in full) |
| A3 → side channels | the mitigation matrix | the PoC suite recovers zero secret bytes above chance (3) |
| A4 → signed malicious update | offline root, threshold signing, transparency log with witnessed checkpoints, reproducible builds | an unlogged but validly signed release is refused; a stale-metadata mirror is detected within 24 h (12; signing design from 4) |
| A4 → poisoned dependency | pinned hashes, cargo-vet, cargo-deny | CI fails on an unpinned fetch or unvetted crate (1) |
| A5 → DMA attack | IOMMU deny-all, per-device domains, ATS off by default, interrupt remapping | hostile QEMU device's DMA and MSI writes outside its domain fault and are logged (1c) |
| A5 → BadUSB | per-interface authorisation; typed confirmation for HID | an emulated keyboard that types at once gets no keystroke accepted; a composite device's hidden HID is held (6) |
| A5 → malformed descriptors | HID and class drivers in tier 3, fuzzed | descriptor and report-descriptor fuzzing 24 h clean (6) |
| A6 → read the disk | encryption ([storage.md](storage.md)) | an offline disk image shows only ciphertext and the documented plaintext metadata (4) |
| A6 → tamper with the boot chain | Secure Boot, signed config, measured boot, signed TPM policy over PCR 4, 7, 8, 9 | tamper tests in [boot.md](boot.md), including an older signed loader and a signed tool replaying the event log (4b) |
| A6 → tamper with ciphertext | stage 1: metadata checksums plus hardened parsing; stage 2: authenticated encryption | random and replayed sector writes to an encrypted ext4 image never crash or corrupt the kernel (4); bit flips detected in stage 2 (file system track) |
| A6 → TPM bus | salted, parameter-encrypted sessions bound to the SRK recorded at install | swtpm traffic contains no key bytes; a swtpm man-in-the-middle proxy fails to unseal without the PIN (4b) |
| A6 → TPM reset or lost state on resume | a PCR reset while running is treated as tamper | resume after a forced TPM reset requires the PIN and logs tamper (10) |
| A6 → guess the PIN | TPM dictionary-attack lockout with owner and lockout authorisation set at install | lockout engages after 32 tries; root can't reset it (4b) |
| A6 → locked or suspended laptop | protected-class keys and page cache evicted on lock | after lock, a memory scan finds no protected-class key and no plaintext canary from protected files (file system track; stage 1 shows "not protected while locked") |
| A7 → other user's data | per-user keys (stage 1: per-user encryption of home directories; stage 2: in the file system), per-user file trees, limited administrators | a second user, including an administrator, can't read the first user's files or reset their way in (5) |
| Promise 3 → telemetry | nothing phones home | a fresh install left idle 24 h contacts only the update and time hosts, all logged (12) |
