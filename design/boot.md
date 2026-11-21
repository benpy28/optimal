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

Limine can check hashes but does not measure into the TPM. Until Optimal Boot
ships, the **shipped** image (not just the test image) must:

- use a Secure-Boot-signed Limine with its config hash enrolled
  (`limine enroll-config`), and a BLAKE2B hash on every `kernel_path` and
  module path, exactly as `scripts/secureboot.sh` does for the attack tests.
  `build-iso.sh` and the release ISO use the same pinned chain;
- report on the scorecard: "boot chain verified: yes; boot chain measured:
  **no**". A protection only counts if it is on in the image people download.

TPM sealing of the disk key waits for measured boot (Optimal Boot). Until
then disk encryption is passphrase-only ([storage.md](storage.md)).

## KASLR

- The kernel is built position-independent (`relocation-model=pie`, only
  `R_X86_64_RELATIVE` relocations, no text relocations), so the loader can
  place it anywhere. A static kernel gets no randomisation, so the current
  static build does **not** count as KASLR and the scorecard says so until
  the PIE build lands (Phase 1).
- Randomised: the kernel's virtual base (at least 9 bits of entropy, 2 MiB
  aligned), its physical load address, and the base of the direct map of
  physical memory (HHDM). Optimal Boot randomises all three; with Limine,
  whatever Limine randomises, and the scorecard lists which.
- Test (`kaslr`, Phase 1 done-when): boot 100 times, collect the three bases
  over serial, require the measured entropy to meet the target and no two
  identical bases in a row. A second test checks that no kernel address
  appears in anything user-visible (see the lockdown table).
- KASLR is defence in depth only. Timing attacks on the kernel's address
  (prefetch, EntryBleed-style) exist; the page-table isolation in
  [threat-model.md](threat-model.md) is what actually hides kernel memory.

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

Real PCs trust Microsoft's keys, not ours. Three supported paths, all tested:

1. **Microsoft-signed shim.** A shim signed by the Microsoft UEFI CA 2023
   carries the Optimal certificate and launches Optimal Boot. This requires
   passing shim review (SBAT metadata, NX compatibility, and the lockdown
   policy in [threat-model.md](threat-model.md), which Optimal already meets
   by design). The 2011 CA expired in June 2026, so machines whose db only
   trusts the 2011 CA need a firmware or db update first; the installer
   detects this and explains it.
2. **Owner keys.** A guided tool, run once from the installer, puts the
   firmware in setup mode and enrols Optimal's (or the owner's own) PK, KEK
   and db. Best security, since nothing Microsoft signs can boot.
3. **Machine Owner Key (MOK).** For machines where the third-party CA is
   off (many Secured-core PCs) and the owner can't or won't clear keys:
   shim plus a MOK enrolled at first boot.

Machines with Secure Boot off or unsupported are hardware tier S4 or S2 in
[threat-model.md](threat-model.md). Getting shim signed is outside our control,
so the bootloader track's done-when is "path 2 works on 3 real PCs, path 1
submitted for review, path 3 works on 1 real PC", not "Microsoft signed it".

CI covers all three: OVMF variable stores with only Microsoft's keys, with the
third-party CA removed, with owner keys, and with a MOK. Revocation is tested
too: a dbx entry and an SBAT generation bump must stop an old signed loader.

## Measured boot and the TPM policy

- **TPM driver** (tier 0 crate, CRB and TIS interfaces, found from the ACPI
  TPM2 table). It lands in Phase 4 with disk encryption. All unseal traffic
  uses salted, encrypted sessions, so sniffing the TPM bus gets nothing.
- **PCRs:** firmware fills 0 to 7; Optimal Boot puts the config, chosen entry
  and command line in PCR 8 and the kernel and modules in PCR 9. The event
  log is passed to the kernel and replayed to check it matches the PCRs.
- **Sealing policy.** The disk key is never sealed to raw PCR values (every
  update would break unlocking). Instead it is sealed with
  `TPM2_PolicyAuthorize` to the Optimal policy key. For every release, CI
  computes the expected PCR 8 and 9 values (they depend only on what we
  build) and the release process signs a policy: "PCR 7 is in Secure Boot
  mode with an allowed db, PCR 8 and PCR 9 equal these values, **and** the
  anti-rollback counter is at most this release's security version". The
  PCR 7 value comes from the machine's own event log at install and is
  re-checked after firmware or dbx updates. Firmware PCRs 0 to 6 are not in
  the policy because firmware updates change them unpredictably; their
  values are recorded and a change is shown to the user at next unlock.
- **Recovery key.** A 256-bit recovery key is created at install, shown once
  and offered for printing. It is never sent anywhere unless the user opts
  in to escrow with a service of their choice. Losing both the TPM state and
  the recovery key loses the data, and the installer says so plainly.
- **PIN.** On S1 machines unlocking needs the TPM *and* a PIN (TPM policy
  `PolicyAuthValue`), so a stolen machine can't just boot to unlock. The TPM's
  dictionary-attack lockout limits guessing.
- **Where the PIN is typed.** Optimal Boot reads it through the UEFI text
  input protocol (works with USB keyboards through firmware, before the
  kernel has any keyboard driver) and hands it to the kernel in a dedicated
  page the kernel zeroes after use. On BIOS or with Limine, the kernel asks
  on the framebuffer console with PS/2 or serial input (Phase 4) and with
  USB keyboards once xHCI lands (Phase 6).

Tests (Phase 4 gate, in QEMU with swtpm): a normal boot unseals; a kernel
update, a rollback to a version at or above the floor, and a dbx update all
unseal with no recovery prompt; a tampered loader, config, command line,
kernel, module or system-image root hash fails to unseal; an image below the
floor fails to unseal; swtpm traffic contains no key bytes.

## The system image

- The OS lives on a **read-only system image** protected by a Merkle hash
  tree (SHA-256, 4 KiB blocks, the same idea as dm-verity but our own format
  and code). Its root hash is on the kernel command line inside the signed
  config, so it is both checked and measured.
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
- **Transparency.** Every signed release hash is appended to a public log, so
  a release signed in secret (stolen key, coercion) is visible.
- **Anti-rollback.** Each release has a security version number (SVN) in its
  signed config. The minimum allowed SVN (the floor) lives in a TPM NV
  counter (S1) or an authenticated UEFI variable (S2, weaker and documented).
  The loader refuses to boot an image below the floor, and the TPM policy
  refuses to unseal for one. The floor is raised only after the new release
  has booted and passed its health check, and only when that release fixes a
  security bug, so ordinary rollback to the previous release keeps working.
  SBAT generations do the same for the bootloader itself.
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
| PIE kernel and KASLR test; hash-pinned, enrolled Limine in the shipped image; microcode on the boot CPU | 1 |
| Microcode on every CPU | 2 |
| TPM driver, measured boot (with Optimal Boot), PCR policy, recovery key, PIN entry, tamper tests | 4 |
| Read-only verified system image, A/B slots, boot counting, recovery image, signed updates, anti-rollback | 4 (format and checks), 12 (update client) |
| Secure Boot paths 1 to 3 on real PCs | bootloader track, finishing before Phase 7 |
| Microcode after resume | 10 |
