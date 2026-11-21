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
  `R_X86_64_RELATIVE` relocations, no text relocations, only RIP-relative
  addressing), so it can run anywhere in the higher half, not only in the
  top 2 GiB. The current static build does **not** count as KASLR, and the
  scorecard says so until the PIE build lands (Phase 1b).
- Randomised: the kernel's virtual base over at least 2^16 equally likely
  2 MiB slots (a 128 GiB window placed in the higher half clear of the
  direct map and the other kernel regions, so at least 16 bits by design);
  the base of the direct map of physical memory (HHDM); and, separately, its
  physical load address (any free 2 MiB-aligned range, the slot count
  logged). The kernel picks its virtual base and direct-map base itself when
  it builds its own page tables (Phase 1b): its entry stub maps the image at
  the chosen base, applies its relocations and jumps there, so both bases
  are randomised the same way under Limine and Optimal Boot. The physical
  address is chosen by the loader: Optimal Boot randomises it; with Limine,
  whatever Limine does, and the scorecard says which.
- **Entropy sources**, in order: a 256-bit seed Optimal Boot takes from the
  firmware's `EFI_RNG_PROTOCOL` and passes to the kernel, then
  `RDSEED`/`RDRAND`, then a fallback that mixes the TSC and other boot-time
  noise; all available sources are hashed together. A boot that used only
  the fallback (for example a Core 2 machine with no firmware RNG) logs
  reduced entropy and the scorecard shows it.
- **Tests (Phase 1 gate).** Entropy can't be shown by sampling a few boots
  (100 samples can never demonstrate more than about 6.6 bits), so it is
  tested in three parts: (1) the slot picker is run 10^6 times on the host
  with its real entropy input path, and must pass a chi-square uniformity
  test and an SP 800-90B min-entropy estimate of its output of at least 9
  bits; (2) each raw source (`EFI_RNG_PROTOCOL` sampled by a UEFI test app on
  OVMF and the reference machines, `RDRAND`/`RDSEED`, the TSC fallback) gets
  its own 90B estimate per sample, published on the scorecard, and the
  fallback collects samples until their summed estimate reaches 64 bits;
  (3) an integration check boots 100 times in QEMU and requires at least 95
  distinct bases. A further test checks that no kernel address appears in
  anything user-visible (the lockdown table).
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
2. **Owner keys.** For people who want no Microsoft-signed loader to boot.
   The user clears the firmware keys in firmware setup (software can't do
   this), and a guided tool then enrols Optimal's (or the owner's own) PK,
   KEK and db. Removing Microsoft's keys would also stop Microsoft-signed
   option ROMs (graphics card, NVMe and network firmware) from running,
   which can leave a black screen or no boot device. So by default the tool
   also enrols the **Microsoft Option ROM UEFI CA 2023**, which signs option
   ROMs only, never loaders, so a card's firmware keeps working after a
   vBIOS update. Option ROMs the event log shows were signed only by the
   2011 UEFI CA (which also signed loaders, so it is never enrolled) are
   enrolled by hash. **Hash-only** (no Microsoft certificate at all) is an
   opt-in: the tool reads every option ROM hash from the PCR 2 event log,
   enrols those, and warns in plain words that updating a card's firmware
   will stop it running until the tool is run again. Old loaders are
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
boot), and with owner keys (path 2). For option ROMs, a test VM carries an
option ROM signed by a test CA standing in for the Option ROM CA; after
owner-key enrolment it must keep working, and after a simulated vBIOS update
(a newer ROM signed by the same CA) it must still work under the default and
be refused under hash-only, with the warning's recovery steps bringing it
back. Revocation is tested too: an SBAT generation bump (path 1) and a dbx
entry (path 2) must each stop an old signed loader.

## Measured boot and the TPM policy

### Measurements

- **TPM driver:** MMIO in tier 0, the TPM protocol and event-log parsing in a
  safe crate (tier 1), CRB and TIS interfaces, found from the ACPI TPM2
  table. Lands in Phase 4b. Before `ExitBootServices`, Optimal Boot uses the
  firmware's `EFI_TCG2_PROTOCOL` instead.
- **PCRs:** firmware fills 0 to 7, including PCR 4 (the hash of every EFI
  binary that ran: shim, Optimal Boot) and PCR 7 (Secure Boot state and the
  certificates that verified each binary). Optimal Boot measures **its own
  security version number as the first event in PCR 8**, then the config,
  chosen entry and command line into PCR 8, and the kernel and modules into
  PCR 9, all before it asks for the PIN. The event log is passed to the
  kernel and replayed to check it matches the PCRs.
- **PCR 4 is predictable only on the expected path.** CI computes PCR 4 for
  path 1 (shim, then Optimal Boot) and path 2 (Optimal Boot alone), and both
  variants are signed. At install the PCR 4 event log is replayed: if it is
  exactly one of the signed sequences, the signed variant is used; if it
  isn't (firmware that measured a failed boot entry first, a dual-boot
  manager that chain-loads Optimal Boot, extra firmware events), PCR 4 is
  bound per machine like PCR 7 (below), with the value read from the event
  log, and when the OS updates the loader it predicts the new PCR 4 by
  replacing the loader's digest in the logged sequence.
- **Capping.** The kernel extends a separator into PCRs 8 and 9
  unconditionally, whatever the unseal result (passphrase fallback
  included), before it parses any byte of the data volume beyond its LUKS2
  header and before user space starts. Everything that needs the pre-cap
  PCRs (the disk secret, the TPM service secret below, a pending floor
  raise, the hibernation key) happens before that. So nothing that runs
  later, and nothing an attacker puts on a substituted volume, can unseal
  again without rebooting through the loader.
- **Volume identity.** The sealed object holds the key-slot secret and an
  identity tag, HMAC-SHA-256 keyed with that secret over the LUKS2 header's
  UUID and volume key digest. After unsealing, the kernel recomputes the tag
  from the header on disk and refuses (and caps) on mismatch, so the secret
  is never used on another volume.
- **TPM use after capping.** At install the kernel's TPM service creates the
  **service key**, a persistent TPM object whose authorisation value is a
  random 32-byte secret S, and seals S under the same policy as the disk
  secret minus the PIN step. Each boot, before capping, the kernel unseals S
  and loads the service key; S stays only in kernel memory. Objects used
  after capping (per-user home-volume keys, the storage replay NV counter,
  the hibernation key in [storage.md](storage.md)) carry
  `TPM2_PolicySecret` against the service key in their policies, so only
  the kernel of a boot that passed the pre-cap PCR check can use them, at
  any time later. The replay counter's policy is
  `TPM2_PolicyCommandCode(TPM2_NV_Increment)` plus that `PolicySecret`, so
  the kernel can bump it at mount and resume after capping while root, the
  owner and the platform can't.
- **Resume.** If the TPM's PCRs come back reset after sleep (a known attack
  on some firmware), the kernel treats it as tampering: it logs it, requires
  the PIN or passphrase, and doesn't trust the TPM for the rest of the boot.

### The sealing policy, step by step

The disk key is never sealed to raw PCR values (every update would break
unlocking). It is a TPM sealed object (without `noDA`, so the
dictionary-attack lockout applies) whose policy is, in this order:

1. `TPM2_PolicyPCR` over **PCR 4, 8 and 9** (the path 1 or path 2 variant),
   or over PCR 8 and 9 alone on machines where PCR 4 is bound per machine.
   This part is what each release's signed policy approves.
2. `TPM2_PolicyAuthorize` with the **disk policy key** and policy reference
   `optimal-disk-v1/f`, where f is the rollback floor the object was sealed
   at: the TPM accepts step 1's digest only if the disk policy key signed it
   for that reference. Each slot has its own signed config (below), so PCR 8
   depends on the slot and the boot entry. For every release CI computes
   the expected PCR 4, 8 and 9 values for each PCR 4 variant and each
   (slot, entry) pair (they depend only on what we build: shim, Optimal
   Boot, configs, kernel), and the signing ceremony signs each digest once
   for every published floor value f at or below the release's SVN. A
   release below the floor therefore has no signature the TPM will accept.
3. `TPM2_PolicyPCR` over **PCR 7** (and PCR 4 where it is bound per
   machine), with the values read from this machine's own event log at
   install. This part is per machine, so it can't be signed in CI; it's
   bound into the sealed object's own policy.
4. `TPM2_PolicyNV` on the anti-rollback counter: value at most base + f
   (`TPM_EO_UNSIGNED_LE`; base and counter below). Once the floor is raised
   past f, this object no longer unseals, so restoring an old object from a
   backup gains nothing.
5. `TPM2_PolicyAuthValue`: the user's stretched PIN (below).

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
- **Lockout parameters:** 32 failed attempts before lockout; one attempt
  recovered every 2 hours.
- **The anti-rollback counter** is an NV counter. TPM counters start at the
  highest value any counter on that TPM has ever had, not at 0, so the floor
  is **counter value − base**, where base is stored in a separate write-once
  NV index (`TPMA_NV_WRITEDEFINE`, written and then locked with
  `TPM2_NV_WriteLock` at install, so it can't change until undefined). At
  install the counter is incremented with owner authorisation until it is at
  least the installed release's SVN, and base is set so the floor starts at
  that SVN. After install, `TPM2_NV_Increment` is allowed only through a
  disk-policy-key-signed policy over the release PCRs (step 1's digests,
  reference `optimal-floor-v1`), so only an approved release's kernel before
  capping can raise it, and that kernel raises it no higher than its own
  SVN. So nobody, root included, can push the floor above every release
  (which would make the machine unbootable), and neither index can be
  decreased, rewritten or undefined without the owner secret.
- **Parameter encryption:** every unseal uses a salted session bound to the
  storage root key whose name was recorded at install, with parameter
  encryption in both directions, so neither a passive sniffer nor an active
  interposer that substitutes its own key gets the secret without the PIN.
  Against an active interposer, the PIN is the remaining protection, and the
  scorecard says so.

### Recovery key, PIN, passphrase

- **Recovery key.** A 256-bit recovery key is created at install, shown once
  and offered for printing. It is never sent anywhere unless the user opts
  in to escrow with a service of their choice. Losing both the TPM state and
  the recovery key loses the data, and the installer says so plainly.
- **The PIN is never the only secret.** It is stretched with Argon2id
  (1 GiB, 3 passes, 4 lanes, a per-machine random salt) by the kernel before
  unsealing. One half of the result, through HKDF, is the TPM authorisation
  value; the other is mixed with the unsealed secret (HKDF of both) to give
  the key-slot key. Someone who extracts the sealed secret from the TPM
  still has to search the PIN through Argon2id offline. A PIN of at least 6
  digits is allowed only with a **discrete TPM**. Firmware TPMs (AMD fTPM,
  Intel PTT, or any manufacturer ID not on the discrete list, read with
  `TPM2_GetCapability`) have been broken by physical attack, so there
  install requires a **passphrase estimated at 60 bits or more** (by a
  zxcvbn-style estimator) instead of a PIN, through the same stretching.
- **Anti-phishing secret.** At install the user picks a phrase or small
  image, sealed under the disk secret's policy minus the PIN step (so to
  PCR 4, 7, 8 and 9 and the floor). Optimal Boot unseals it through
  `EFI_TCG2_PROTOCOL` after measuring the kernel and shows it above the PIN
  prompt. Another signed chain (an old loader, a signed tool faking the
  prompt) can't unseal it, so a prompt without it is a fake.
- **Where the PIN or passphrase is typed.** Once Optimal Boot lands (Phase
  4b), it reads it through the UEFI text input protocol (`ConIn`, which
  works with USB keyboards through firmware) before `ExitBootServices`, and
  hands it to the kernel in a dedicated page the kernel zeroes after use;
  the loader wipes its own copies before handing over. With Limine or on
  BIOS there's no TPM unlock at all, and the kernel asks for the
  **passphrase** on the framebuffer console from Phase 4a, with PS/2,
  serial, or a USB keyboard through a polled xHCI boot-protocol reader
  (in-kernel, 8-byte boot reports only, held to the fuzz thresholds for
  in-kernel parsers in [drivers.md](drivers.md)) until the full USB stack
  lands in Phase 6. The kernel uses the same input for prompts after resume.
  Test (Phase 4a): the passphrase is typed on a PS/2, a USB (QEMU `usb-kbd`
  on `qemu-xhci`) and a serial keyboard.

### Tests (Phase 4b gate, QEMU with swtpm)

- **Unseal and tamper.** A normal boot unseals. A kernel update, a rollback
  to a release at or above the floor, and an OS-applied dbx update all
  unseal with no recovery prompt. A dbx change made outside the OS asks for
  the recovery key. Each of these fails to unseal: a tampered loader,
  config, command line, kernel, module or system-image root hash; an
  **older signed Optimal Boot**; a **signed EFI tool that replays the
  published PCR 8 and 9 events**; an image below the floor; an old sealed
  object restored after a floor raise.
- **PCR 4 variants.** Path 1 and path 2 installs use the signed variant; an
  install after a failed boot entry and one chain-loaded from a second boot
  manager (dual boot) bind PCR 4 per machine, and both then unseal across a
  loader update.
- **Capping and volume identity.** After boot, a second unseal attempt from
  the running system fails. A substituted data volume (an attacker's LUKS2
  volume with a known passphrase and a hostile init) is refused by the tag
  check, the passphrase fallback opens it only after capping, and from it
  the real disk secret can't be unsealed. A second user logs in long after
  capping on B1 through the service key.
- **Bus and root.** swtpm traffic contains no key bytes, and a
  man-in-the-middle proxy in front of swtpm can't unseal without the PIN.
  From root: incrementing either counter, rewriting the base, undefining
  either, and resetting the lockout all fail.
- **PIN and phishing.** An swtpm reporting a firmware-TPM manufacturer ID
  makes install refuse a PIN and accept only a passphrase of 60 estimated
  bits; the secret unsealed without the PIN doesn't open the volume. A
  signed EFI tool can't show the anti-phishing secret.
- **Anti-rollback.** A swtpm counter pre-incremented to 10^6 before install
  gives the right floor; a floor raise completes across a reboot; an
  increment after capping fails; power is cut at every step of a raise
  (each slot write, each recovery copy, the request, the reseal, each
  increment) and the machine boots each time; on B2 the OS can't write the
  floor variable; after a raise, the recovery image boots.
- **Slots.** Power is cut during every loader-state write and the loader
  boots a valid slot each time; flipping one bit in the active slot's image
  makes the read fail, the kernel sets "slot bad", and the next boot runs
  the other slot and reports it; a validly signed config with a missing or
  wrong inclusion proof is refused by the loader.

## The system image

- The OS lives on a **read-only system image** protected by a Merkle hash
  tree (SHA-256, 4 KiB blocks, the same idea as dm-verity but our own format
  and code). Its root hash, together with the tree's geometry (block size,
  data size, tree offset) and salt, is on the kernel command line inside the
  signed config, so all of it is both checked and measured. The command
  line names the slot by its fixed GPT partition label, so PCR 8 is the
  same on every machine.
- A block is checked **after** the DMA that read it has completed and its
  mapping has been removed from the device's IOMMU domain, so a malicious
  disk can't change a block after it was checked.
- Every block is checked on read. A bad block makes that read fail with
  `EIO` and logs the error; the kernel doesn't panic, because a single bad
  sector shouldn't take the machine down mid-session. It writes a **"slot
  bad"** flag into the loader state, and the next boot runs the other slot.
- There are two slots, **A and B**, each with its own directory on the EFI
  system partition holding its own signed config (kernel hash, command line,
  SVN, transparency proof). An update writes the inactive slot and its
  directory, so the running system and its config are never modified in
  place.
- **Loader state** (per-slot tries counters, good and bad flags, the active
  slot, a pending floor-raise request) lives in two UEFI non-volatile
  variables, each with a version number and a CRC-32C. The loader reads the
  valid one with the higher version and every write goes to the other, so a
  power cut during a write leaves the previous state intact. State is not
  trusted for security: the worst a corrupted or root-written state can do
  is pick the other valid slot or the recovery image.
- **User data is separate**, on the encrypted own file system. Rolling back
  the OS never rolls back anyone's documents. File system snapshots are a
  user feature (undo for your files), not the update mechanism.
- Test (Phase 4a): a flipped block in the image is caught with `EIO`. The
  slot fallback after it is tested in Phase 4b, with the loader state.

## Updates

- **Signing keys.** A TUF-style hierarchy: an offline root key (held on
  hardware tokens, threshold 2 of 3, never on a networked machine) signs the
  online keys; targets, snapshot and timestamp metadata expire (timestamp
  daily, so a frozen mirror is detected within a day). Release signing never
  happens in GitHub Actions with a long-lived key; CI produces the artefacts,
  a reproducible second build checks them byte for byte, then the signer
  signs.
- **Transparency.** Every signed release hash, and every disk policy the
  disk policy key signs, is appended to a public append-only log. Each
  signed config carries the inclusion proof of its release hash and a log
  checkpoint cosigned by at least 2 independent witnesses; Optimal Boot
  checks both against the log and witness keys built into it, and refuses
  the config otherwise (Phase 4b). The update client checks the same before
  installing. So a release signed in secret (stolen key, coercion) can't be
  installed or booted without being visible. Test: an unlogged but validly
  signed release is refused by both.
- **Time.** Metadata expiry needs a trustworthy clock, so the time service
  uses NTS (authenticated NTP), and the clock is never accepted earlier
  than a floor: the newest metadata time the client has seen, and before
  the update client exists, the signed build time in the running release's
  config. Spoofed NTP from a hostile network can't freeze updates; the
  spoofed-NTP test uses the stored, signed time floor until Phase 7.
- **Anti-rollback.** Each release, the recovery image included, has a
  security version number (SVN) in its signed config. The minimum allowed
  SVN (the floor) is counter − base in the TPM (B1, above), or on B2 (no
  TPM) a non-volatile UEFI variable with boot-services access only (no
  `RUNTIME_ACCESS`), so only the loader, before `ExitBootServices`, can
  write it. The loader refuses to boot an image below the floor, and the TPM
  policy refuses to unseal for one. The floor is raised only when a release
  fixes a security bug, so ordinary rollback to the previous release keeps
  working, and in two stages:
  1. After the new release has booted and passed its health check, user
     space writes the other slot and both recovery-image copies at the new
     SVN n, verifies all three against their signed roots, and only then
     writes a "commit SVN n" request into the loader state.
  2. On the next boot the kernel before capping (on B2 the loader, before
     `ExitBootServices`) checks n ≤ its own SVN and that the other slot and
     both recovery copies carry SVN ≥ n and verify, else drops the request.
     On B1 it seals the disk secret again at floor n, test-unseals the new
     object, stores it beside the old one in the LUKS2 header, increments
     the counter until counter − base = n, and clears the request; on B2 the
     loader writes the floor variable, then clears the request. A power cut at any point leaves either the
     old floor with both objects working or the new floor with the new one.

  SBAT generations (path 1) and dbx entries (path 2) do the same for the
  bootloader itself.
- **Boot counting.** Each slot has a tries counter (3) in the loader state.
  The loader decrements it on every boot attempt; user space marks the slot
  good after its health check (desktop up, network stack up, update service
  running). When tries reach 0, or the slot is marked bad, the loader boots
  the other slot. Test: install a kernel that hangs before user space; the
  machine is back on the old slot within 3 boots without any key press.
- **Recovery image.** A small signed, read-only recovery system with its
  own SVN, stored as two copies on their own partitions, so a power cut
  while one is rewritten leaves the other. The loader boots the newest valid
  copy at or above the floor, from the loader menu or automatically when
  both slots fail. It accepts the recovery key, can reinstall a slot from a
  signed image on USB or the network, and can copy user data off.

## Phase placement

| Item | Phase |
|---|---|
| PIE kernel and KASLR (kernel-chosen virtual and direct-map bases) with its tests; hash-pinned, enrolled Limine in the shipped image; microcode on the boot CPU | 1 |
| Microcode on every CPU | 2 |
| Verified system image format and block checks; A/B slot layout; passphrase prompt with PS/2, serial and the polled USB boot keyboard reader | 4a |
| TPM driver, measured boot (Optimal Boot's measured boot and boot counting are an entry condition), PCR 4 variants, the sealing policy, capping and volume identity, the service key, TPM ownership, counter and base, recovery key, stretched PIN and fTPM passphrase rule, PIN entry in the loader, anti-phishing secret, per-slot configs, loader state, slot-bad fallback, two-stage floor raise, recovery image copies, the loader's transparency check, all tests above | 4b |
| Signed update client with TUF metadata and transparency inclusion proofs | 7 |
| Secure Boot paths 1 and 2 on real PCs | bootloader track, finishing before Phase 7 |
| Microcode after resume | 10 |
