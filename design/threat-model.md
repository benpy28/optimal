# Threat model

Security is pillar 2 and an absolute focus. This file says exactly what that
means: who we defend against, what we protect, what has to be trusted, and,
for every attack path, which mechanism closes it and which test proves it. A
security claim that isn't in the table at the bottom isn't a claim Optimal
makes.

## The headline promise, stated precisely

"Nobody can read a user's data remotely" means:

1. **Code that arrives from the network** (a web page, an email attachment, a
   downloaded app, a game, a Windows program under Wine) runs only with the
   rights of the app that runs it. It cannot read files, keys, the screen,
   the microphone, the camera or input outside that app's grants.
2. **Nothing on the network can talk to the machine** unless the user started
   that conversation or explicitly opened a service.
3. **The vendor (us) cannot read user data either.** No telemetry, no key
   escrow, no remote access, no account required. Updates can change code,
   so update signing is part of the trusted base and has its own defences
   ([boot.md](boot.md)).
4. **A stolen or lost device** that is powered off, or suspended with its
   keys evicted, gives up no user data.

What it does **not** promise: that an app the user granted access to a file
won't misuse that file. Grants are explicit and narrow so the damage is
bounded, and the default grant for an app is nothing.

## Attackers

| # | Attacker | Can do | Example |
|---|---|---|---|
| A1 | Remote network | send any packet to any open port or reply path | worm, malicious Wi-Fi network, malicious DNS reply |
| A2 | Malicious content | make an app parse hostile input | web page, image, font, video, document, game mod |
| A3 | Malicious app | run arbitrary user code inside one app sandbox, with that app's grants | trojan from a store or website, compromised game |
| A4 | Compromised update or dependency | ship code that we sign, or that our build pulls in | stolen signing key, poisoned crate, poisoned Limine tag |
| A5 | Malicious device | plug in USB, Thunderbolt/USB4, PCIe, a monitor (DDC/EDID), Bluetooth | BadUSB keyboard, DMA attack device, malformed USB descriptors |
| A6 | Evil maid | hands on the machine while it is off, suspended or locked; can reflash the disk, read the disk, sniff the TPM bus | stolen laptop, hotel room |
| A7 | Local user on a shared machine | log in as another user | family PC |

Out of scope, said out loud so nobody assumes otherwise: an attacker who can
decap chips, a malicious CPU or firmware vendor, and cold-boot attacks on
DRAM while the machine is running and unlocked. (Memory encryption, AMD SME
and Intel TME, is enabled where present to raise the bar on the last one,
and the scorecard says whether it is on.)

## Assets

User files; credentials and keys (disk keys, app secrets, SSH and browser
keys); screen contents, keyboard and mouse input, clipboard; microphone,
camera and location; the integrity of the OS itself (so it can keep the
other promises).

## Trusted computing base

Everything below can break every promise if it has a bug. It is kept as small
as possible, gets the heaviest testing, and is listed so nobody pretends the
TCB is "just the unsafe core".

- CPU microcode and platform firmware (not ours, see degraded modes below).
- The bootloader ([boot.md](boot.md)).
- The unsafe kernel core (tier 0) and **every in-kernel safe-Rust component**
  (tiers 1 and 2 in [drivers.md](drivers.md)). Safe Rust removes memory
  corruption, not logic bugs, so an in-kernel component is still trusted.
- The capability broker and permission prompts ([capabilities.md](capabilities.md)).
- The compositor (it sees every pixel and every key) and the input stack.
- The update client and the release signing keys.

Not trusted: every app, every tier-3 driver, every network peer, every
device, every file system image that Optimal did not create.

## Hardware security tiers ("runs on almost anything" without lying)

Some protections need hardware that old or cheap PCs don't have. Optimal still
runs there, but it never claims a protection it can't give. Each machine is in
one tier, shown at install, in Settings and on the scorecard (one column per
tier).

| Tier | Needs | What holds |
|---|---|---|
| **S1 Full** | UEFI Secure Boot, TPM 2.0, IOMMU with interrupt remapping | everything in the table below |
| **S2 No TPM** | UEFI Secure Boot, IOMMU | as S1, but disk unlock is passphrase-only (no sealing), and evil-maid tampering of the boot chain is detected by Secure Boot but not by measurement |
| **S3 No IOMMU** | anything without a working IOMMU | DMA attacks are **not** blocked. External PCIe tunnels (Thunderbolt/USB4) are disabled. Internal devices use bounce buffers so driver bugs can't DMA into arbitrary memory, but a malicious internal device can. Big warning at install |
| **S4 Legacy BIOS** | BIOS boot | no verified or measured boot at all; passphrase-only disk encryption; everything that doesn't depend on firmware still holds |

A machine is in the lowest tier it qualifies for. An S1 machine with the IOMMU
switched off in firmware is S3, and the OS says so and explains how to fix it.

## Rules every component follows

- **Default deny.** Apps start with no grants. The network is closed inbound.
  Devices start unauthorised.
- **Outbound network is a grant.** An app that holds any user-file grant
  gets no network access unless the user allowed it (one prompt at first
  use, remembered). An app with no file grants can have network access by
  default, because then it has nothing of the user's to leak. File grants
  are scoped to files or folders the user picked through the file chooser,
  never "all of home". This closes the most common real-world "remote read":
  an app that can both read files and phone home.
- **Untrusted input is parsed outside the TCB where possible.** Foreign file
  systems, USB class drivers, Wi-Fi and Bluetooth stacks, image and font
  decoders run in tier 3 or in app sandboxes by default ([drivers.md](drivers.md)).
  What stays in the kernel (TCP/IP, the system file system) is continuously
  fuzzed from the phase it lands in.
- **Lockdown.** No interface lets user space run unsigned code in ring 0 or
  read or write kernel memory. See the table below.
- **Side channels.** Mitigations against leaks *between processes or from the
  kernel to a process* are never off by default, whatever they cost; their
  cost is measured and published. Concretely: KPTI-style page-table isolation
  on CPUs vulnerable to Meltdown-class bugs (with PCID to keep it cheap);
  index masking on every attacker-influenced array index in syscall, network,
  USB and file system parsing paths, enforced by a wrapper type
  (`Untrusted<usize>` can only index through a masking accessor) because a
  bounds check in safe Rust is itself a Spectre v1 gadget; retpoline or IBRS
  per CPU model; core scheduling so mutually distrusting processes never
  share an SMT core on CPUs affected by L1TF/MDS-class bugs (SMT stays on
  otherwise); buffer clearing on kernel exit where the CPU needs it. All of
  this depends on microcode, which is loaded first ([boot.md](boot.md)).
- **USB authorisation.** A new USB device gets its class driver only after
  authorisation: automatic for storage and audio when the session is
  unlocked, a prompt for anything new while locked, and a new keyboard or
  HID device must type a shown code before its keystrokes are accepted
  (stops BadUSB). Thunderbolt/USB4 PCIe tunnels need explicit approval per
  device, remembered by device identity.
- **Crash reports stay local.** Dumps are written to the user's encrypted
  storage, redacted of app memory by default, and never uploaded unless the
  user chooses to send one, each time, after seeing what is in it.
- **Supply chain.** Every downloaded build input is pinned by SHA-256 (the
  Limine tree and any other fetched source, not just a git tag); `cargo-deny`
  (licences, advisories, duplicate versions) and `cargo-vet` (every
  dependency audited, with a recorded reason for any `unsafe`) run in CI;
  release builds are reproducible and a second builder must produce
  identical bytes before anything is signed; release signing keys never
  live in CI ([boot.md](boot.md)).

## Lockdown table

Linux has many ways for root to reach ring 0. Optimal has no "root that can
do anything"; these are refused or restricted for everyone, always, not only
under Secure Boot.

| Interface | Behaviour |
|---|---|
| `init_module`, `finit_module`, `delete_module` | `ENOSYS`. Optimal has no loadable kernel modules; drivers are built in or run in tier 3 |
| `kexec_load` | `EPERM` |
| `kexec_file_load` | only images signed with the release key, only from the update service |
| `/dev/mem`, `/dev/kmem`, `/dev/port` | do not exist |
| `iopl`, `ioperm` | `EPERM`; port I/O for tier-3 drivers is granted only through a device capability for that device's own ports |
| raw PCI config writes (`/sys/bus/pci/.../config`, `resource*` mmap) | read-only and filtered; writes only via a device capability, never to another device or to MSI/BAR registers |
| `/proc/kcore`, `/proc/kallsyms`, `/proc/modules`, `dmesg` | kcore absent; kallsyms and modules show zero addresses; kernel log readable only by the system log service, kernel pointers hashed |
| eBPF (`bpf()`) | not available to apps. Classic BPF for seccomp and socket filters only. A privileged, verified eBPF subset is a later, separate decision behind this table |
| `perf_event_open` | own process only, user-space counters only, no kernel samples, for everyone |
| hibernation | image encrypted and authenticated with a key sealed to the TPM (S1) or derived from the passphrase; resume refuses an image it can't authenticate |
| MSR access (`/dev/cpu/*/msr`) | does not exist |
| ACPI table override, custom DSDT | not supported |

A CI test (`lockdown`) tries every row and fails if any succeeds. It runs from
Phase 3 for the rows that exist then, and grows with the ABI.

## Every attack path, its defence and its test

| Path | Defence | Test (phase it becomes a gate) |
|---|---|---|
| A1 → open port | nothing listens by default; default-deny inbound firewall | external port scan of a fresh install finds 0 open TCP/UDP ports (6) |
| A1 → network stack bug | TCP/IP in safe Rust, fuzzed; index masking | network fuzzing (malformed packets, TCP state machine) runs nightly from 6; Phase 6 needs 10^9 executions with no open crash |
| A1 → Wi-Fi/Bluetooth driver | tier 3 drivers, own IOMMU domain | device-side fuzzing of the driver (8) |
| A2 → app parser bug | the app is sandboxed, so a hijacked app has only its grants | sandbox escape suite (below) |
| A3 → other apps' data | per-app file view (mount namespace), capabilities, `/proc` isolation | hostile-process suite: a process tries to read another app's files, `~/.ssh`, another process's memory (`/proc/pid/mem`, `process_vm_readv`, `ptrace`), forge or guess a capability, map another process's frames. Every attempt must fail (3, grows every phase) |
| A3 → kernel bug | memory safety, proofs, fuzzing, SMEP/SMAP, KASLR (defence in depth only), CET | syscall fuzzing (4 on), proof targets 4 to 6, lockdown test |
| A3 → screen, input, mic, camera | only the compositor and broker can grant them; prompts are drawn by the compositor where apps can't fake or cover them | prompt-spoofing test: an app drawing a fake prompt can't obtain a grant; an app can't read another window's pixels or keys (7) |
| A3 → exfiltration | outbound network is a grant for apps holding file grants | test app with a file grant and no network grant can't open any socket except AF_UNIX to its own portals (5) |
| A3 → side channels | mitigations above, core scheduling | published Spectre/MDS/L1TF proof-of-concept suite must fail to leak across processes or from the kernel (3 on) |
| A4 → signed malicious update | offline root key, threshold signing, transparency log, reproducible builds | update tests in [boot.md](boot.md) (12; signing design from 4) |
| A4 → poisoned dependency | pinned hashes, cargo-vet, cargo-deny | CI fails on an unpinned fetch or unvetted crate (now) |
| A5 → DMA attack | IOMMU deny-all from Phase 1, per-device domains, ATS off for external ports, interrupt remapping | rogue QEMU device tries DMA and MSI outside its domain; must fault and be logged (1c) |
| A5 → BadUSB | USB authorisation and typed confirmation for new HID | test with an emulated USB keyboard that types immediately: no keystroke accepted (6) |
| A5 → malformed descriptors | USB class drivers in tier 3, fuzzed | descriptor fuzzing (6) |
| A6 → read the disk | encryption ([storage.md](storage.md)); keys sealed to measured boot (S1) or passphrase (S2 to S4) | disk image read offline shows only ciphertext and unauthenticated metadata (4) |
| A6 → tamper with the boot chain | Secure Boot, signed config, measured boot, PCR policy | tamper tests in [boot.md](boot.md): tampered loader, config, kernel, initramfs, old signed image. Each must refuse to boot or refuse to unseal (4) |
| A6 → tamper with ciphertext | authenticated encryption in the own file system; XTS (interim) documented as confidentiality only | bit-flip in an encrypted block is detected on read (own FS track) |
| A6 → sniff the TPM bus | salted, encrypted TPM sessions for unseal | unseal traffic captured from swtpm contains no key material (4) |
| A6 → guess the PIN | TPM dictionary-attack lockout; Argon2id for passphrases | lockout engages after the configured number of tries (4) |
| A6 → suspended laptop | optional "evict keys on suspend"; user-data class keys evicted on lock | after lock, a DMA or memory read finds no data-class key (own FS track) |
| A7 → other user's data | per-user keys, per-user file trees | second user can't read the first user's files even as an admin (12) |

Every row's test runs in CI from the phase in brackets and stays forever. The
scorecard lists each row as pass, fail or not yet built, per hardware tier.
