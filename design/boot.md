# Boot chain, updates and recovery

This is the whole-system chain from power-on to a running, verified system,
plus how updates, rollback and recovery work. The bootloader's own internals
(file formats, parsing, the Limine protocol it speaks) are in
`bootloader/DESIGN.md`; this file sets the requirements it and the kernel must
meet together. The threats this answers are A4 and A6 in
[threat-model.md](threat-model.md).

## The chain

```
UEFI firmware (Secure Boot)            measures into PCR 0-7
  └─ Optimal Boot (signed EFI binary)  checks the signed config, measures it
       │                               into PCR 8, measures kernel and
       │                               modules into PCR 9
       └─ kernel (hash pinned in the signed config)
            └─ system image (read-only, Merkle tree; root hash on the
               signed kernel command line, so it is covered by PCR 8)
                 └─ user data (own encrypted file system, unlocked by a key
                    the TPM releases only to this exact chain)
```

Every arrow is a check that refuses to continue on mismatch. Nothing in the
chain can be skipped by a boot option: the command line lives in the signed
config, and the loader ignores any edited copy.

## Interim chain: Limine (until Optimal Boot replaces it)

Limine can check hashes but does not measure into the TPM or speak the shim
protocol. Until Optimal Boot ships, the **shipped** image (not just the test
image) uses a Limine signed with the Optimal key, with its config hash
enrolled (`limine enroll-config`) and a BLAKE2B hash on every `kernel_path`
and module path, exactly as `scripts/secureboot.sh` does for the attack tests;
`build-iso.sh` and the release ISO use the same pinned chain. That chain is
only *verified* on machines whose firmware trusts the Optimal key (owner keys,
path 2 below). On a stock PC it boots only with Secure Boot off, so the
machine is boot tier B3, and on BIOS it is B4. The scorecard says exactly
that: "verified boot: only with owner keys enrolled; measured boot: no". A
protection only counts if it is on in the image people download, on the
machine they have.

TPM sealing waits for Optimal Boot's measured boot (Phase 4b). Until then disk
encryption is passphrase-only ([storage.md](storage.md)).

## KASLR

- The kernel is built position-independent (`relocation-model=pie`, only
  `R_X86_64_RELATIVE` relocations, no text relocations), so the loader can
  place it anywhere. The current static build does **not** count as KASLR,
  and the scorecard says so until the PIE build lands (Phase 1b).
- Randomised: the kernel's virtual base (at least 512 equally likely 2 MiB
  slots, so at least 9 bits by design), its physical load address, and the
  base of the direct map of physical memory (HHDM). Optimal Boot randomises
  all three; with Limine, whatever Limine randomises, and the scorecard lists
  which.
- **Entropy sources**, in order: the firmware's `EFI_RNG_PROTOCOL`, then
  `RDSEED`/`RDRAND`, then a fallback that mixes the TSC and other boot-time
  noise. A boot that used only the fallback (for example a Core 2 machine
  with no firmware RNG) logs reduced entropy and the scorecard shows it.
- **Tests.** Entropy can't be shown by sampling a few boots (100 samples can
  never demonstrate more than about 6.6 bits), so it is tested in two parts:
  (1) the slot picker is run 10^6 times on the host with its real entropy
  input path, and must pass a chi-square uniformity test and an SP 800-90B
  min-entropy estimate of at least 9 bits; (2) an integration check boots
  100 times in QEMU and requires at least 80 distinct bases. A third test
  checks that no kernel address appears in anything user-visible (the
  lockdown table).
- KASLR is defence in depth only. Timing attacks on the kernel's address
  (prefetch, EntryBleed-style) exist, so nothing in Optimal's security
  depends on the kernel's address staying secret.

## Microcode

Mitigations depend on the microcode revision, so microcode loads first:

- The loader passes the vendor microcode blob as a module, hash pinned in the
  signed config like everything else.
- The kernel applies it on the boot CPU before choosing any mitigation, on
  every other CPU before it runs anything (Phase 2), and again after resume
  from sleep (Phase 10).
- The loaded revision is logged and shown on the scorecard. If the revision
  is older than the one the mitigation table needs, the kernel picks the
  safer software mitigation and says so.
- Microcode and device firmware are redistributable vendor binaries, not our
  code. They are allowed as data under the firmware policy in the
  [ROADMAP licence section](../ROADMAP.md#core-architecture-decisions).

## Secure Boot on real PCs

Real PCs trust Microsoft's keys, not ours. Two supported paths, both tested:

1. **Microsoft-signed shim.** A shim signed by the Microsoft UEFI third-party
   CA (2023) carries the Optimal certificate and launches Optimal Boot. This
   requires passing shim review (SBAT metadata, NX compatibility, and the
   lockdown policy in [threat-model.md](threat-model.md), which Optimal
   already meets by design). Machines whose db trusts only the 2011 CA,
   which expired in June 2026, need a firmware or db update first; machines
   with the third-party CA switched off (many Secured-core PCs) need it
   switched back on in firmware setup. The installer detects both and
   explains the steps. Old loaders are revoked through SBAT generations.
2. **Owner keys.** For people who want nothing Microsoft signs to boot. The
   user clears the firmware keys in firmware setup (software can't do this),
   and a guided tool then enrols Optimal's (or the owner's own) PK, KEK and
   db. Because removing Microsoft's keys stops Microsoft-signed option ROMs
   (graphics card, NVMe and network firmware) from running, which can leave
   a black screen or no boot device, the tool first reads the option ROM
   hashes from the PCR 2 event log and enrols them in db. Old loaders are
   revoked by dbx entries signed with the KEK, which the owner (or Optimal,
   if the owner chose Optimal's keys) holds.

There is no MOK-only path: shim itself is signed by the third-party CA, so
where that CA is off, shim can't load.

Machines with Secure Boot off or not trusting Optimal are boot tier B3, and
BIOS machines B4 ([threat-model.md](threat-model.md)). Getting shim signed is
outside our control, so the bootloader track's done-when is "path 2 works on
3 real PCs and path 1 is submitted for review", not "Microsoft signed it".

CI covers both: OVMF variable stores with Microsoft's keys (path 1), with
the third-party CA removed (expected: the installer's explanation, not a
boot), and with owner keys (path 2), including a test VM with a signed
option ROM that must keep working after owner-key enrolment. Revocation is
tested too: an SBAT generation bump (path 1) and a dbx entry (path 2) must
each stop an old signed loader.

## Measured boot and the TPM policy

### Measurements

- **TPM driver:** MMIO in tier 0, the TPM protocol and event-log parsing in a
  safe crate (tier 1), CRB and TIS interfaces, found from the ACPI TPM2
  table. Lands in Phase 4b.
- **PCRs:** firmware fills 0 to 7, including PCR 4 (the hash of every EFI
  binary that ran: shim, Optimal Boot) and PCR 7 (Secure Boot state and the
  certificates that verified each binary). Optimal Boot measures **its own
  security version number as the first event in PCR 8**, then the config,
  chosen entry and command line into PCR 8, and the kernel and modules into
  PCR 9. The event log is passed to the kernel and replayed to check it
  matches the PCRs.
- **Capping.** As soon as the disk key is unsealed, the kernel extends a
  separator into PCRs 8 and 9, so nothing that runs later (any bug in the
  running system) can unseal again without rebooting through the loader.
- **Resume.** If the TPM's PCRs come back reset after sleep (a known attack
  on some firmware), the kernel treats it as tampering: it logs it, requires
  the PIN or passphrase, and doesn't trust the TPM for the rest of the boot.

### The sealing policy, step by step

The disk key is never sealed to raw PCR values (every update would break
unlocking). It is a TPM sealed object (without `noDA`, so the
dictionary-attack lockout applies) whose policy is, in this order:

1. `TPM2_PolicyPCR` over **PCR 4, 8 and 9**, plus `TPM2_PolicyNV` checking
   that the anti-rollback counter is at most this release's security version
   number. This part is what each release's signed policy approves.
2. `TPM2_PolicyAuthorize` with the **disk policy key** and policy reference
   `optimal-disk-v1`: the TPM accepts step 1's digest only if the disk policy
   key signed it. For every release, CI computes the expected PCR 4, 8 and 9
   values (they depend only on what we build: shim, Optimal Boot, config,
   kernel), and the signing ceremony signs that digest.
3. `TPM2_PolicyPCR` over **PCR 7**, with the value read from this machine's
   own event log at install. This part is per machine, so it can't be
   signed in CI; it's bound into the sealed object's own policy.
4. `TPM2_PolicyAuthValue`: the user's PIN.

PCR 4 is what makes this hold against old or other signed code: an older,
exploitable Optimal Boot, a shim with a known hole, or any other tool the
firmware trusts has a different hash in PCR 4, so even if it extends PCRs 8
and 9 with the published values, the policy doesn't match. The loader's own
SVN in PCR 8 covers old loaders whose hash was approved before.

**When PCR 7 changes.** When the OS itself applies a dbx update, a db update
(such as the 2023 CA migration) or a firmware update, it first predicts the
new PCR 7 from the event log and the update, and reseals with a `PolicyOR` of
the old and new values; after the next successful boot it drops the old one.
Changes made outside Optimal (firmware setup, another OS) can't be predicted,
so the next boot asks for the recovery key, and the tests say so.

**The disk policy key** is its own key, not the update signing key: offline,
held on hardware tokens with 2-of-3 threshold use, used only in the release
signing ceremony. Every policy it signs is published in the transparency log.
To allow rotation without a recovery prompt, the sealed object accepts either
the current or the next disk policy key (a `PolicyOR` of two
`PolicyAuthorize` branches); a rotation retires the current key and names a
new next key on the following reseal. If the key is ever compromised, that is
announced, and every machine reseals to the next key at its next update.

### TPM ownership and the anti-rollback counter

- At install, the owner and lockout authorisation values are set to random
  secrets, which are stored only wrapped by the recovery key. So even a
  process running as root can't reset the dictionary-attack lockout, clear
  the TPM, or redefine the counter.
- **Lockout parameters:** PIN at least 6 digits (a longer passphrase is
  allowed); 32 failed attempts before lockout; one attempt recovered every
  2 hours.
- **The anti-rollback counter** is an NV counter whose own policy allows
  `TPM2_NV_Increment` only through a disk-policy-key-signed policy that
  checks the release PCRs and that the new value is not above the running
  release's SVN. So nobody, root included, can push the floor above every
  release (which would make the machine unbootable), and it can never be
  decreased or undefined without the lockout secret.
- **Parameter encryption:** every unseal uses a salted session bound to the
  storage root key whose name was recorded at install, with parameter
  encryption in both directions, so neither a passive sniffer nor an active
  interposer that substitutes its own key gets the secret without the PIN.
  Against an active interposer, the PIN is the remaining protection, and the
  scorecard says so.

### Recovery key, PIN entry

- **Recovery key.** A 256-bit recovery key is created at install, shown once
  and offered for printing. It is never sent anywhere unless the user opts
  in to escrow with a service of their choice. Losing both the TPM state and
  the recovery key loses the data, and the installer says so plainly.
- **Where the PIN or passphrase is typed.** Optimal Boot reads it through the
  UEFI text input protocol (works with USB keyboards through firmware,
  before the kernel has any keyboard driver) and hands it to the kernel in a
  dedicated page the kernel zeroes after use; the loader wipes its own
  copies before handing over. With Limine or on BIOS there's no TPM unlock
  at all, and the kernel asks for the **passphrase** on the framebuffer
  console with PS/2 or serial input (Phase 4) and USB keyboards once xHCI
  lands (Phase 6).

### Tests (Phase 4b gate, QEMU with swtpm)

A normal boot unseals. A kernel update, a rollback to a release at or above
the floor, and an OS-applied dbx update all unseal with no recovery prompt. A
dbx change made outside the OS asks for the recovery key. Each of these fails
to unseal: a tampered loader, config, command line, kernel, module or
system-image root hash; an **older signed Optimal Boot**; a **signed EFI tool
that replays the published PCR 8 and 9 events**; an image below the floor.
After unseal, a second unseal attempt from the running system fails (PCRs
capped). swtpm traffic contains no key bytes, and a man-in-the-middle proxy
in front of swtpm can't unseal without the PIN. From root: incrementing the
counter past the release SVN, undefining it, and resetting the lockout all
fail.

## The system image

- The OS lives on a **read-only system image** protected by a Merkle hash
  tree (SHA-256, 4 KiB blocks, the same idea as dm-verity but our own format
  and code). Its root hash, together with the tree's geometry (block size,
  data size, tree offset) and salt, is on the kernel command line inside the
  signed config, so all of it is both checked and measured.
- A block is checked **after** the DMA that read it has completed and its
  mapping has been removed from the device's IOMMU domain, so a malicious
  disk can't change a block after it was checked.
- Every block is checked on read. A bad block makes that read fail with
  `EIO`, logs the error, and on the next boot the loader falls back to the
  other slot (below). The kernel doesn't panic, because a single bad sector
  shouldn't take the machine down mid-session.
- There are two slots, **A and B**. An update writes the inactive slot, so
  the running system is never modified in place.
- **User data is separate**, on the encrypted own file system. Rolling back
  the OS never rolls back anyone's documents. File system snapshots are a
  user feature (undo for your files), not the update mechanism.
- Test: flip one bit in the inactive slot's image, boot it, the read fails,
  the fallback happens and the event is reported.

## Updates

- **Signing keys.** A TUF-style hierarchy: an offline root key (held on
  hardware tokens, threshold 2 of 3, never on a networked machine) signs the
  online keys; targets, snapshot and timestamp metadata expire (timestamp
  daily, so a frozen mirror is detected within a day). Release signing never
  happens in GitHub Actions with a long-lived key; CI produces the artefacts,
  a reproducible second build checks them byte for byte, then the signer
  signs.
- **Transparency.** Every signed release hash, and every disk policy the
  disk policy key signs, is appended to a public append-only log. The update
  client installs a release only with an **inclusion proof** against a log
  checkpoint cosigned by at least 2 independent witnesses, so a release
  signed in secret (stolen key, coercion) can't be installed without being
  visible. Test: an unlogged but validly signed release is refused.
- **Time.** Metadata expiry needs a trustworthy clock, so the time service
  uses NTS (authenticated NTP), and the client never accepts a time earlier
  than the newest metadata it has already seen. Spoofed NTP from a hostile
  network can't freeze updates.
- **Anti-rollback.** Each release has a security version number (SVN) in its
  signed config. The minimum allowed SVN (the floor) lives in a TPM NV
  counter (B1) or an authenticated UEFI variable (B2, weaker and documented).
  The loader refuses to boot an image below the floor, and the TPM policy
  refuses to unseal for one. The floor is raised only after the new release
  has booted and passed its health check, and only when that release fixes a
  security bug, so ordinary rollback to the previous release keeps working.
  When the floor is raised, the other slot can no longer boot its older
  release, so the new release is copied into it straight away, keeping a
  bootable fallback. SBAT generations (path 1) and dbx entries (path 2) do
  the same for the bootloader itself.
- **Boot counting.** Each slot has a tries counter (3) in the loader's state.
  The loader decrements it on every boot attempt; user space marks the slot
  good after its health check (desktop up, network stack up, update service
  running). When tries reach 0 the loader boots the other slot. Test: install
  a kernel that hangs before user space; the machine is back on the old slot
  within 3 boots without any key press.
- **Recovery image.** A small signed, read-only recovery system on its own
  partition, booted from the loader menu or automatically when both slots
  fail. It accepts the recovery key, can reinstall a slot from a signed image
  on USB or the network, and can copy user data off.

## Phase placement

| Item | Phase |
|---|---|
| PIE kernel and KASLR tests; hash-pinned, enrolled Limine in the shipped image; microcode on the boot CPU | 1 |
| Microcode on every CPU | 2 |
| TPM driver, measured boot (Optimal Boot's measured boot is an entry condition), the sealing policy, TPM ownership and counter setup, recovery key, PIN entry, tamper tests | 4b |
| Read-only verified system image, A/B slots, boot counting, recovery image, signed updates, anti-rollback | 4 (format and checks), 12 (update client) |
| Secure Boot paths 1 and 2 on real PCs | bootloader track, finishing before Phase 7 |
| Microcode after resume | 10 |
