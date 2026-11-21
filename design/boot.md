# Boot chain, updates and recovery

This is the whole-system chain from power-on to a running, verified system,
plus how updates, rollback and recovery work. Every security requirement
on the loader is in this file, including how it trusts its config. The
loader's internals (file formats, parsing, the Limine protocol it speaks)
go into `bootloader/DESIGN.md`, which is written when the bootloader track
starts. The threats this answers are A4 and A6 in
[threat-model.md](threat-model.md).

## The chain

```
UEFI firmware (Secure Boot)            measures into PCR 0-7
  └─ Optimal Boot (signed EFI binary)  checks the config against its
       │                               built-in config key, measures it
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

**The config-signing key.** Optimal Boot accepts a config only if the
config-signing key signed it. That key is a delegated role in the TUF
hierarchy (Updates, below). It is separate from the disk policy key and
the update keys, kept offline on hardware tokens and used 2-of-3. Its
public half and key ID are built into the Optimal Boot binary. Path 1
trusts it through shim's check of Optimal Boot, and path 2 through db.
Nothing on the ESP or in a UEFI variable can add a config key. Configs
are signed with Ed25519 (RFC 8032). Optimal Boot measures the key ID into
PCR 8, right after its own SVN. To rotate or revoke the key, a loader with
the new key built in ships with a higher loader SVN, in this order, each
step verified before the next: (1) the update client rewrites the other
slot's config and both recovery-image configs with copies signed by both
keys (a config may carry two signatures, and a loader accepts it if the
one by its built-in key verifies); (2) the loader update, trial-booted
(The system image, below); (3) the floor raise and the SBAT/dbx ordering
(Updates) retire every loader that trusts the old key. Only then do
configs signed by the new key alone ship. Until step 3 the release's disk
policies cover the PCR 8 values of both key IDs; after it, only those that
carry the new key ID, so a boot that used the old key doesn't unseal. Tests
(Phase 4b): a config signed by a revoked key is refused by the current
loader, after the floor raise an old loader that accepts it can't unseal,
and a power cut at every write of the rotation still boots without the
recovery key.

**The loader is trusted code.** `bootloader/` follows the rules in
[core.md](core.md) (its own tier-0 cap of 3,000 lines, unsafe inventory,
lints, exact toolchain pin), its harnesses run in the proofs job, its floor
code is in proof target 16's scope, and all of its cryptography (Ed25519,
SHA-256, SHA-512, HMAC-SHA-1 for TOTP, and for salted TPM sessions ECDH
P-256, KDFe/KDFa and AES-128-CFB, with CAVP and Wycheproof vectors) is
`core-crypto` under proof target 14 ([verification.md](verification.md)),
inside the 3,000-line cap; its current `ed25519-dalek` and
`sha2` dependencies are replaced before Phase 4b.

**DMA before the kernel.** Before anything else, Optimal Boot installs a
deny-all IOMMU: every context entry (Intel) or device table entry (AMD)
blocks, with one exception list, the same as in
[drivers.md](drivers.md): identity maps for the ranges the firmware's
RMRR/IVMD entries declare, a read-only map of the firmware framebuffer for
the device scanning it out, and, while firmware drivers still need them,
the boot disk controller and the console keyboard controller. Where
firmware offers `EDKII_IOMMU_PROTOCOL`, the loader installs its own
implementation, which maps for each controller only the buffers its
driver `Map()`s. Otherwise each exception maps RAM only (no MMIO), minus
the loader image and one excluded pool from which every other page the
loader owns is allocated: the IOMMU tables, the stack it switches to, its heap and
page tables, the hand-off pages, the kernel's destination and the pages
for the PIN and every value it unseals. TPM access through CRB or TIS is
MMIO and needs no DMA. The order is fixed: (1) every disk read the boot
needs (config, kernel, modules, the header block) lands in staging
buffers outside the pool; (2) the boot disk controller's exception is
removed and the IOTLB invalidated; (3) each image is copied into the pool
and only then checked and measured there; (4) only then object 3 is
unsealed, the code shown, the PIN read through `ConIn` and the remaining
firmware I/O done (loader-state writes, the platform-hierarchy disable).
The keyboard controller keeps an exception only if it is not below an
external-facing port (the rule in [drivers.md](drivers.md)); a keyboard
that is can't type at the loader prompt, which says to use another one.
The firmware's own keystroke buffers stay reachable by that controller, so
a USB or PCIe `ConIn` keyboard without `EDKII_IOMMU_PROTOCOL` shows the
named "pre-loader secret DMA window (keyboard path)". The table builder is
a pure crate shared with the kernel, and proof target 10 extends to it at
4b (B/T): only the listed exceptions are mapped, no pool page is in any
device's map, each IOTLB invalidation completes before the step after it,
and the tables are re-armed after `ExitBootServices`. It
builds the tables in the translation mode (legacy or scalable) the kernel
will use, chosen from the
IOMMU's capabilities by the rule in [drivers.md](drivers.md). If firmware
already enabled the IOMMU, it follows the kernel's takeover rules: on AMD
it rewrites the inherited device table's entries in place (the table stays
reserved) through the blocking intermediates in
[drivers.md](drivers.md), never V=0 on a live device; on Intel it keeps the inherited
translation mode and loads its root table with SRTP while translation
stays on. Then it calls `ExitBootServices`, drops the keyboard
exception, and before handing over it re-reads the IOMMU state (Intel
GSTS.TES and the root-table address, AMD IommuEn and the device-table base)
and re-arms its tables if a firmware
`ExitBootServices` handler turned them off (EDK2's `IntelVTdDxe` does by
default). Where it can't program translation, it uses VT-d PMRs instead,
placed to cover every page the kernel and loader occupy and the region the
kernel allocates from before its own IOMMU is live, with the RMRRs and the
framebuffer outside them, and with the loader's PIN and unsealed-value
pages inside; the hand-off names the covered ranges, and the kernel
allocates only inside them until its tables are live. A USB keyboard's
firmware transfer buffers lie outside the PMRs, so a PMR-only boot that
reads the PIN shows the named "pre-loader secret DMA window (keyboard
path)"; so does a
boot with no usable IOMMU, which is D2. The kernel
takes these tables over at its own IOMMU setup. D1 requires the
firmware's pre-boot DMA protection: without it a DMA device can patch
firmware-verified code (shim, Optimal Boot) before this step runs, so this
step alone shows the named "pre-loader DMA write window". The kernel also
checks at entry (from the
PMR base and limit registers or the live tables) that every page it and
the loader occupy is covered. A machine that fails that check, or has
neither protection (any machine under Limine without the firmware
protection), shows the "boot DMA window" exposure
([threat-model.md](threat-model.md), [drivers.md](drivers.md)). DMA is
stopped by the bridge or root-port Bus Master Enable plus the IOMMU, never
by a device's own BME. Tests (Phase 4b): on QEMU's `intel-iommu` with OVMF
built with `IntelVTdDxe`, the hostile-device emulator (the vfio-user device
server in [drivers.md](drivers.md)) DMAs into the kernel image between
`ExitBootServices` and the kernel's IOMMU setup, and every write is
blocked; the same on QEMU's `amd-iommu`; the emulator posing as the boot
disk controller writes the IOMMU tables and the loader text while its
exception is live and after it is dropped, and 0 writes land; a PMR setup
that leaves a hole under the kernel image is reported as a boot DMA
window; the emulator, and on the reference laptop a PCIe FPGA DMA reader
(once as a third device, once posing as the boot disk controller), scan
memory from power-on to the kernel's IOMMU setup for a PIN canary, the
TOTP key and M, and find 0; on firmware without pre-boot DMA protection
an FPGA write into the loader image lands and is reported as the
pre-loader DMA write window.

## Interim chain: Limine (until Optimal Boot replaces it)

Limine can check hashes but does not measure into the TPM or speak the shim
protocol. Until Optimal Boot ships, the **shipped** image (not just the test
image) must use a Limine signed with the Optimal key. Its config hash is
enrolled (`limine enroll-config`), and every `kernel_path` and module path
carries a BLAKE2B hash, as `scripts/secureboot.sh` already does for the
attack tests. This is Phase 1 work that is **not done yet**: today
`scripts/build-iso.sh` ships stock unsigned Limine and enrols nothing. It
also adds `BOOTIA32.EFI` and BIOS images and edits the command line in with
`sed`. Phase 1 changes it to the pinned chain, keeping the BIOS image for
B4. A CI check then fails the release build unless the ISO's `limine.conf`
hash is enrolled in its Limine binary. The Phase 1 "old signed image
refused" test can't use an SVN, because Limine has none: the old image's
hash goes into dbx in the test OVMF variable store, and the old image must
fail to boot. That chain is
only *verified* on machines whose firmware trusts the Optimal key (owner keys,
path 2 below). On a stock PC it boots only with Secure Boot off, so the
machine is boot tier B3, and on BIOS it is B4. The scorecard says exactly
that: "verified boot: only with owner keys enrolled; measured boot: no". A
protection only counts if it is on in the image people download, on the
machine they have.

TPM sealing waits for Optimal Boot's measured boot (Phase 4b). Until then disk
encryption is passphrase-only, through the header block's passphrase wrap
from Phase 4a ([storage.md](storage.md)).

**B4 (BIOS) keeps Limine until Optimal Boot's BIOS stage lands**
(bootloader track). Limine has no tries counter, no loader state and no
second loader copy, so B4 uses static A/B entries: `limine.conf` lists the
active slot first (the default) and the other slot second. An update
writes the inactive slot, then writes a new `limine.conf` with that slot
first and the previous one second to a temporary file, reads it back,
checks it and renames it over the old one. If the new slot fails, the user
picks the second entry from the menu. The scorecard says "B4: no automatic
rollback, no floor". Test (Phase 4a, SeaBIOS): stage a slot whose kernel
hangs; choosing the second entry boots the old slot.

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
  address is chosen by the loader. Optimal Boot randomises it. Under
  Limine the scorecard reports physical KASLR as "not done until Optimal
  Boot".
- **Entropy sources** are those of the kernel random generator
  ([core.md](core.md)): credited at their published SP 800-90B estimates,
  the 256-bit seed Optimal Boot takes from the firmware's
  `EFI_RNG_PROTOCOL` and passes to the kernel, `RDSEED` and TSC jitter
  samples; uncredited, `RDRAND`, `TPM2_GetRandom`, interrupt timings and
  the seed file written at shutdown. The slot picker runs before the
  generator, so it hashes every source available that early (all but the
  TPM, interrupt timings and the seed file) with BLAKE2b and takes TSC
  jitter samples until the credited total reaches 64 bits. A boot whose
  only credited source was TSC jitter (for example a Core 2 machine with no
  firmware RNG) logs it and the scorecard shows it.
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

**Shim revoked from outside.** Windows on a dual-boot machine, or a
firmware update, can apply a dbx or SbatLevel update that revokes our shim
(this happened to many shims in 2022 and 2024), and both loader copies and
the recovery image use the same shim. So the release process tracks the
upstream SBAT levels and the UEFI revocation list, and ships a shim at the
new generation to both loader copies, the fallback and both recovery copies
before the old one is revoked. A machine revoked anyway has a documented
way back: Secure Boot off in firmware setup, which boots as B3 and unseals
nothing; reinstall the loader from signed media or the recovery image;
Secure Boot on; a recovery-key boot that reseals. Test (Phase 4b): a test
dbx entry for the shim's hash, written into the OVMF store from outside
Optimal, stops path 1, and the documented steps bring the machine back with
the boot after needing only the PIN; a new shim installed ahead of that
entry keeps booting.

Machines with Secure Boot off or not trusting Optimal are boot tier B3, and
BIOS machines B4 ([threat-model.md](threat-model.md)). Getting shim signed is
outside our control, so the bootloader track's done-when is "path 2 works on
3 real PCs and path 1 is submitted for review", not "Microsoft signed it".

CI covers both: OVMF variable stores with a test CA enrolled in db standing
in for Microsoft's third-party CA and signing our shim (path 1; the Phase 4b
gate uses this, so it never waits on Microsoft), with
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
  firmware's `EFI_TCG2_PROTOCOL` instead. The TPM service lives in the
  kernel, and the kernel alone holds raw TPM access. User space reaches the
  TPM only through the TPM broker API ([threat-model.md](threat-model.md)),
  never with raw commands. The broker has no operation to seal a
  hibernation key, bump the replay counter or the PIN epoch, or reset the
  lockout: all of these are kernel-internal.
- **PCRs:** firmware fills 0 to 7, including PCR 4 (the hash of every EFI
  binary that ran: shim, Optimal Boot) and PCR 7 (Secure Boot state and the
  certificates that verified each binary). Optimal Boot measures **its own
  security version number as the first event in PCR 8**, then the
  config-signing key ID, the config, chosen entry and command line into
  PCR 8, and the kernel and modules into PCR 9, all before it asks for the
  PIN. The event log is passed to the
  kernel and replayed to check it matches the PCRs.
- **PCR 4 is predictable only on the expected path.** CI computes PCR 4 for
  path 1 (shim, then Optimal Boot) and path 2 (Optimal Boot alone), and both
  variants are signed. The first boot from disk replays its own PCR 4 log
  (below). If the log is exactly one of the signed sequences, the signed
  variant is used. Otherwise PCR 4 is bound per machine like PCR 7, with
  the value read from that boot's log. Cases include firmware that measured
  a failed boot entry first, a dual-boot manager that chain-loads Optimal
  Boot, and extra firmware events. When the OS later updates the loader, it
  predicts the new PCR 4 by putting the new loader's digest in place of the
  old one in the logged sequence. If a later recovery-key boot finds the
  machine's variant changed (a boot manager added or removed), it reseals
  every object and redefines the counter, base and time index (below) for
  the new variant, keeping the floor.
- **Sealing happens at the first boot from disk, never in the
  installer.** The installer's own boot has different PCR 4 and PCR 7
  values: USB media, possibly shim when the target is path 2, other boot
  entries. So the installer seals nothing. It sets the TPM's owner and
  lockout authorisation and creates the SRK (below), and writes the Optimal
  header block with the volume key wrapped under the recovery key and the
  volume tags. On the first boot from disk the loader finds no sealed
  objects, so it asks for the recovery key (shown at install a minute
  earlier) and the new PIN or passphrase. Before capping, the kernel
  unwraps the volume key from the header block with the recovery key,
  checks the volume tags and caps; no LUKS2 key slot is read on this or any
  other boot path. After capping it replays that boot's PCR 4 and PCR 7
  log, picks the PCR 4 variant, and works in this order, since policies
  compare against NV values and names that exist only once written: (1)
  the service key and the mark key (the NV policies name them); (2) with
  the owner secret the recovery key unwraps, it defines, increments or
  writes, and locks every NV index: the anti-rollback counter and its base,
  the storage replay counter and its base, the PIN-epoch index, the time
  index and F2's 8 per-user indexes (TPM ownership, below); (3) it reads
  back their values and their names (which change once `TPMA_NV_WRITTEN`
  is set); (4) only then it creates objects 1 to 3 and the lock-screen PIN
  object (`TPM2_Create` needs no matching PCRs). It writes the blobs and the volume key
  wrapped under the TPM key-slot key into the header block; it writes no
  LUKS2 key slot. Each new object is checked by
  running its policy steps in a trial session (`TPM_SE_TRIAL`) and
  comparing the resulting digest with the object's authPolicy (read back
  with `TPM2_ReadPublic`); a real test-unseal can't run after capping. The
  second boot unseals with the PIN alone, before capping, which is the
  real test. If that fails, it falls back to the recovery key, and the
  first boot's sealing is repeated. Any later recovery-key boot (after a
  PCR 7 change made outside Optimal, a TPM clear or a lockout) runs the
  same post-cap sealing for whatever failed, so the boot after it needs only
  the PIN. A volume created in Phase 4a already has the header block with
  its passphrase and recovery-key wraps ([storage.md](storage.md)); its
  first 4b boot takes TPM ownership (owner and lockout secrets, SRK) and
  then seals the same way. Tests (Phase 4b): install from USB through shim,
  signed by a test CA enrolled in db standing in for Microsoft's, onto a
  disk that then boots by path 2, and the reverse; both seal at the first
  boot and unseal at the second with no recovery prompt, also on a swtpm
  whose counters were pre-incremented to a random value. A 4a volume seals
  at its first 4b boot and the boot after needs only the PIN.
- **The Optimal header block** (format, parser, passphrase and
  recovery-key wraps from Phase 4a; the TPM fields from 4b). This is the
  only part of the data volume read before capping, and no Optimal boot
  path ever reads a LUKS2 key slot. It sits at a fixed offset: the last
  16 KiB before the LUKS2 data offset, which is fixed at 16 MiB. The LUKS2
  key-slot area ends before it (`config.keyslots_size` = 16 MiB − 48 KiB,
  on the allow-list in [storage.md](storage.md)), and the system volume's header
  sits ahead of the per-user volume map ([storage.md](storage.md)). Stage 2
  keeps the same 16 KiB at the same offset, outside the file system's
  allocatable space, in exactly this format. It has two 8 KiB copies, each
  starting with a fixed magic that names the stage (read before capping to
  pick the parser), then a version and a CRC-32C. The kernel reads the
  valid copy with the higher version among those whose PIN epoch equals
  the PIN-epoch index, uses the other if that copy's objects fail a policy
  step (never after a wrong PIN), and writes the other, so a power cut
  leaves one intact. Only one operation that writes the other copy is
  pending at a time: a PIN change waits until a pending floor raise or
  PCR 7 reseal has completed at its next boot, and a raise or reseal waits
  for a PIN change to finish; a hibernation blob is added to the pending
  copy when there is one. Tests (Phase 4b): a PIN change requested during
  a pending raise runs after the raising boot, and a power cut at every
  write of either still boots without the recovery key; proof target 16
  models the rule. Each copy has 512 bytes of fixed fields: the
  parameters of the PIN's KDF; for each LUKS2 key slot Optimal writes
  (passphrase, recovery key) its KDF parameters and write generation
  ([storage.md](storage.md)); the names of the SRK and of the
  anti-rollback counter, base, replay-base and PIN-epoch NV indexes; the PIN epoch its objects were
  sealed at; and the current and pending volume tag sets (Volume identity,
  below). Then come 15 fixed slots of 512 bytes: the sealed blobs (objects
  1 to 4 below, the service key and the mark key), the volume key wrapped
  for each way Optimal opens it (TPM key-slot key, passphrase, recovery
  key), the AEAD ciphertexts of S and of the hibernation image key, the
  owner secret and (together with S) the lockout secret wrapped under the
  recovery key, the lockout secret wrapped under S, and F4's panic-hibernate
  key wrapped under the logged-in user's stretched login passphrase
  ([features.md](features.md)). No
  file-system generation or slot number lives here, so in stage 2 these
  bytes are identical whichever superblock slot is newest, and first-boot
  sealing, a floor raise or a hibernation blob rewrites them without a
  file-system commit. Its parser, the kernel's and the loader's own, is
  fuzzed to the in-kernel thresholds in [drivers.md](drivers.md) from
  Phase 4a and is proof target 15 (B/T, Phase 4b), together with the
  PIN-epoch index's read authorisation (`TPMA_NV_AUTHREAD` with an empty
  authValue: the value is not secret, only its writes are guarded). A copy
  whose CRC fails is not used; if neither copy is usable, the boot shows
  the Linux rescue instructions ([storage.md](storage.md)).
- **Capping.** The kernel extends a separator into PCRs 8 and 9, and a
  fresh random 256-bit boot nonce into PCR 9, so capped values differ on
  every boot. It does so unconditionally, whatever the unseal result
  (passphrase fallback included). It does so before it parses any byte of the data volume beyond
  the Optimal header block, and before user space starts. The LUKS2 JSON
  and the volume map are parsed only after capping. Everything that needs
  the pre-cap PCRs happens before then: unsealing objects 1 to 4, a pending
  floor raise (the real unseal of the new objects, then the increment) and
  the lockout reset (below). So nothing that runs later,
  and nothing an attacker puts on a substituted volume, can unseal again
  without rebooting through the loader.
- **Volume identity.** Each LUKS2 header copy has its own tag:
  HMAC-SHA-256, keyed with HKDF(volume key, `optimal-volume-tag`), over
  that copy's raw bytes (binary header and JSON area, 16 KiB; the primary
  at offset 0, the secondary at 16 KiB) and the header block's fields other
  than the tags and the slots; in stage 2, the header area's frozen bytes.
  So the installer can write the tags and every boot path (TPM, recovery
  key, passphrase) can check them. Each header-block copy holds a current
  and a pending tag set, one tag per LUKS2 copy. Once it has unwrapped the
  volume key from the header block, the kernel computes both tags over the
  bytes as read, before S is unsealed and before any JSON is parsed, and
  uses the primary copy if its tag matches either set, else the secondary;
  it never picks a copy by seqid. After capping that copy's JSON is held to
  the allow-list in [storage.md](storage.md) (one crypt segment,
  `aes-xts-plain64`, `sector_size` 4096, offset 16 MiB, `iv_tweak` 0); a
  copy that fails it counts as not verifying. If no copy verifies, the
  kernel wipes the volume key and every unsealed secret, caps and refuses
  the volume, so the secrets are never used on another volume and no
  unauthenticated JSON is parsed while they are live. Every LUKS2 rewrite (a
  passphrase change or recovery-key rotation; first-boot sealing writes
  none) runs in this order: a header-block copy with the new tags as the
  pending set, flush, both LUKS2 copies, flush, a header-block copy with
  them promoted to current, flush. A power cut anywhere leaves a LUKS2 copy
  matching one set.
- **Linux rescue and re-tagging.** The LUKS2 header carries only the
  passphrase and recovery-key slots, which Optimal writes for Linux rescue
  (`cryptsetup`) and never reads at boot. There is no LUKS2 TPM key slot:
  Linux can never reproduce the pre-cap PCR 8 and 9 values it would need. A
  Linux-side header change (`luksAddKey`, a token added) leaves no copy
  whose tag verifies, so the TPM and passphrase paths refuse the volume. A
  recovery-key boot that finds no verifying tag caps with S and every
  sealed secret never unsealed, regenerates the JSON Optimal wrote from the
  header block's key-slot records ([storage.md](storage.md)) and shows the
  differences (key slots, tokens, segments) on the trusted path. If the JSON
  passes the allow-list and the user confirms, it writes new tags in the
  order above and reboots; otherwise it refuses the volume.
- **The sealed objects.** Every floor raise and every predicted PCR 7
  reseal covers objects 1 to 3, and, where PCR 4 is bound per machine, a new loader's PCR 4 reseal covers
  1 to 3. Objects 4 and 5 exist only
  between a hibernate or suspend and the next resume, when neither reseal
  runs. Every object that carries the PIN step (step 5 below) seals only a
  random 256-bit value R; the key material it guards is AEAD-encrypted
  (XChaCha20-Poly1305, random nonce) under HKDF(R ‖ second half of the
  stretched PIN) and stored in the header block or in RAM. So the TPM
  never releases key material, and someone who extracts the TPM's seeds
  (a swtpm state dump; faulTPM against a firmware TPM) still has to search
  the PIN through Argon2id. The secrets the kernel holds after capping, to
  reseal and to suspend (each R, the TOTP key and M, the PIN's stretched
  authorisation half and its second half), are kept encrypted under
  HKDF(S, `optimal-held`), so wiping S at suspend also wipes them.
  1. *Disk secret* R1: steps 1 to 5. HKDF(R1 ‖ PIN half 2) is the TPM
     key-slot key, which unwraps the volume key from the header block.
  2. *Service-key secret* R2: steps 1 to 5. S is AEAD-encrypted under
     HKDF(R2 ‖ PIN half 2) in the header block. It is unsealed only after
     the volume tag matches.
  3. *Anti-phishing TOTP key and mark secret M* (below): steps 1 to 4,
     step 3 binding PCR 7 through a `PolicyOR` over its accepted values,
     so a boot with Secure Boot off, or with PCR 7 changed outside
     Optimal, shows no code. It has no PIN, because the code is shown
     before the PIN is typed.
  4. *Hibernation key* (Phase 10): created at hibernate, after capping and
     before S, the volume key and the PIN half are wiped (the order in
     [storage.md](storage.md)), sealing R4 with steps 1 to 5 at the current floor and with the PCR 7
     branches objects 1 to 3 carry at that moment (a pending `PolicyOR`
     included). The image key is AEAD-encrypted under HKDF(R4 ‖ PIN half
     2); blob and ciphertext go into the header block. Resume from
     hibernation is a boot of the same release that unseals R4 before
     capping and hands the keys over (Resume from hibernation, below). The
     restored kernel and any normal mount erase the blob, so a PCR 7
     reseal, which runs only while the system is up, never finds one, and a
     floor raise leaves a stale one unusable.
  5. *Resume object* (suspend; Phase 10): created at suspend, after
     capping, sealing R5. Its policy is `TPM2_PolicyCounterTimer` with
     resetCount (offset 16 of `TPMS_TIME_INFO`, `TPM_EO_EQ`) equal to its
     value at suspend, `TPM2_PolicyPCR` over PCR 4 and 7 and this boot's
     capped PCR 8 and 9 (boot nonce included), and `TPM2_PolicyAuthValue`;
     it is DA-protected (no `noDA`). An S3 resume leaves resetCount and the
     PCRs unchanged (they survive through `TPM2_Shutdown(STATE)`). A TPM
     Reset changes resetCount; a hard reset after `TPM2_Shutdown(STATE)` is
     a TPM Restart that keeps resetCount but resets the PCRs, and no later
     boot, genuine or replaying the published PCR 8 and 9 events, extends
     the same nonce, so neither can use the object. The blob and the keys
     encrypted under it stay in RAM only.
  The blobs are loaded under the storage root key (SRK), a persistent
  primary object created at install with owner authorisation.
- **TPM use after capping.** At the first boot from disk, the kernel's TPM
  service creates the **service key**, a TPM object (`noDA`, since S is
  random) whose authorisation value is a random 32-byte secret S. Its blob
  and S's ciphertext live in the header block, under object 2. Each boot,
  before capping and after the volume tag matches, the kernel unseals R2
  with the PIN, decrypts S and loads the service key. S stays only in
  kernel memory. S is also wrapped under the recovery key, so a
  recovery-key boot keeps the service key; only a TPM clear loses it. Five
  things are used after capping, each with `TPM2_PolicySecret` against the
  service key in its policy, so only the kernel of a boot that passed the
  pre-cap check can use them, at any time later: per-user home-volume keys;
  the storage replay NV counter, whose value is counter − its own
  write-once base, defined like the anti-rollback base
  ([storage.md](storage.md)); the PIN-epoch index (Changing credentials,
  below); F2's 8 per-user NV indexes; and the lock-screen PIN object. A substituted volume fails the
  tag check, so it never gets S and its init can't bump the replay
  counter. The replay counter's and the PIN-epoch index's write policy is
  `TPM2_PolicyCommandCode(TPM2_NV_Increment)` plus that `PolicySecret`, so
  the kernel can bump them after capping while root, the owner and the
  platform can't. The lock-screen PIN object (without `noDA`) has that
  `PolicySecret` plus `TPM2_PolicyAuthValue` with the PIN's stretched
  authorisation half and a `TPM2_PolicyNV` on the PIN-epoch index
  (`TPM_EO_EQ`), so an old blob never accepts an old PIN after a PIN
  change. The lock screen checks a typed PIN through the TPM and every
  wrong guess counts against the dictionary-attack counter; after exactly
  10 failures it accepts only the passphrase. Its blob lives in the
  system volume; it is created at 4b first-boot sealing. Without a TPM the
  lock screen takes the passphrase only, with persistent exponential
  back-off on disk ([features.md](features.md) F4). Test (Phase 6): a
  scripted USB HID guesser's 11th guess is refused and failedTries rises
  by exactly 10.
- **Resume from suspend.** At suspend the kernel closes open vaults (F3)
  and reseals unsealed travel folders (F5), then creates object 5, encrypts
  under HKDF(R5 ‖ PIN half 2) every key the key inventory in
  [storage.md](storage.md) marks wrapped at suspend (the agents' secret
  memory included), and wipes them and R5. From the panic-lock state (F4) nothing is left
  to wrap and resume asks for the passphrase only. On an S3 wake the
  waking vector clears Bus Master Enable on every bridge and root port and
  re-arms the IOMMU tables before any device or interrupt is re-enabled.
  The firmware's own S3 path runs before that, so a machine whose firmware
  keeps no DMA protection on it shows the named "S3 resume DMA window"
  ([drivers.md](drivers.md) "Power management"). On resume the kernel reads
  the PIN through the in-kernel input paths only (below), from keyboards
  present before suspend only: a keyboard counts as present if its port
  path, VID/PID, serial and descriptor hash match after re-enumeration, so
  an xHCI controller that lost power in S3 still works, and any other USB
  device is refused like one plugged in while locked. It stretches the PIN
  in memory it reserved before suspending (or got by dropping clean page
  cache first), unseals R5 and decrypts the keys. If the TPM comes back
  reset after sleep
  (a known attack on some firmware), resetCount and the PCRs have changed,
  so the resume object is unusable. The kernel logs it as tampering, asks
  for the recovery key (or, on a passphrase-only volume, the passphrase),
  and doesn't trust the TPM for the rest of the boot. Tests (Phase 10):
  the memory scan after suspend finds none of the keys (stage 1 with its
  own canary set, which has no class C); with the swtpm state dumped as
  well as RAM, recovering the keys still needs the Argon2id PIN search;
  reboot, replay the published PCR 8 and 9 events and try the unseal
  (must fail); suspend, hard reset and a genuine boot: object 5 refuses;
  resume after a TPM reset reaches recovery; a USB keyboard
  plugged in while suspended can't type at the resume prompt; with the
  xHCI controller reset across S3, the same keyboard types the PIN; a PCIe
  FPGA attached while suspended can't write kernel memory from the waking
  vector on.
- **Resume from hibernation.** At hibernate the kernel closes open vaults
  and records in the
  loader state which slot made the image, and the loader boots that slot
  for the resume boot even if an update made the other slot active; the
  image of another release is refused ([storage.md](storage.md) has the
  image location and its generation and replay checks). The boot kernel
  unseals R1, R2 and R4 before capping, caps, authenticates the image, and
  writes the volume key, S and 256 random bits for the restored kernel's
  generator ([core.md](core.md)) into a reserved hand-over page named in
  the image header. The restored kernel reads the page, wipes it, erases
  the image blob from both header-block copies and flushes before thawing.
  A floor-raise request stays pending until the next normal boot. A panic
  hibernate ([features.md](features.md) F4) is allowed with one logged-in
  session only (otherwise it is a panic power-off) and creates no object
  4: K is wrapped under P before P is wiped. Its resume unseals nothing:
  the loader boots the slot that made the image with no PIN prompt, the
  boot kernel caps, asks for that user's login passphrase on the trusted
  path, stretches it, unwraps P from slot 15 and K under P, authenticates
  the image and hands P to the restored kernel, which unwraps its keys
  from the P blob as at a panic unlock; no PIN is asked or accepted.

### The sealing policy, step by step

The disk key is never sealed to raw PCR values (every update would break
unlocking). It is a TPM sealed object (without `noDA`, so the
dictionary-attack lockout applies) whose policy is as follows, in this
order. Objects 1 to 4 in the list above use the same shape, with the steps
listed there.

1. `TPM2_PolicyPCR` over **PCR 4, 8 and 9** (the path 1 or path 2 variant),
   or over PCR 8 and 9 alone on machines where PCR 4 is bound per machine.
   This part is what each release's signed policy approves.
2. `TPM2_PolicyAuthorize` with the **disk policy key** and policy reference
   `optimal-disk-v1/f` on signed-variant machines, or
   `optimal-disk-v1-pm/f` where PCR 4 is bound per machine, f being the
   rollback floor the object was sealed at: the TPM accepts step 1's digest
   only if the disk policy key signed it for that reference. The ceremony
   signs PCR 4/8/9 digests only under `optimal-disk-v1/f` and PCR 8/9-only
   digests only under `optimal-disk-v1-pm/f`, so a PCR 8/9-only signature
   never satisfies an object on a signed-variant machine, and on a
   per-machine one step 3 still binds PCR 4. Each slot has its own signed
   config (below), so PCR 8 depends on the slot and the boot entry. For
   every release CI computes the expected PCR 4, 8 and 9 values for each
   PCR 4 variant and each (slot, entry) pair, the two recovery-image copies
   included, so object 3 shows the code there too (they depend only on what
   we build: shim, Optimal Boot, configs, kernel), and the signing ceremony
   signs each digest once for every published floor value f at or below
   the release's SVN. A release below the floor therefore has no signature
   the TPM will accept.
3. `TPM2_PolicyPCR` over **PCR 7** (and PCR 4 where it is bound per
   machine), with the values read from this machine's own event log at the
   first boot from disk. This part is per machine, so it can't be signed in
   CI; it's bound into the sealed object's own policy.
4. `TPM2_PolicyNV` on the anti-rollback counter: value at most base + f
   (`TPM_EO_UNSIGNED_LE`; base and counter below). Once the floor is raised
   past f, this object no longer unseals, so restoring an old object from a
   backup gains nothing. Objects 1 and 2 add a second `TPM2_PolicyNV`, on
   the PIN-epoch index (`TPM_EO_EQ` its value when sealed), so an object
   sealed before a PIN change never unseals after it.
5. `TPM2_PolicyAuthValue`: the user's stretched PIN (below).

PCR 4 is what makes this hold against old or other signed code: an older,
exploitable Optimal Boot, a shim with a known hole, or any other tool the
firmware trusts has a different hash in PCR 4, so even if it extends PCRs 8
and 9 with the published values, the policy doesn't match. On per-machine
machines step 3 carries that check and the `-pm` references keep PCR
8/9-only signatures from reaching any other machine. The loader's own SVN
in PCR 8 covers old loaders whose hash was approved before. The
composition of these policies (every object, NV index and signing
reference) is proof target 17 ([verification.md](verification.md)),
proved per object: a session that satisfies objects 1 to 3 implies PCR 4
matched (signed or per machine) and anti-rollback counter ≤ base + f,
for object 3 also the Secure Boot state through its `PolicyOR` over PCR 7
values, and for objects 1 and 2 also the PIN epoch and the PIN; object 4: PCR 4,
the floor and the PIN; object 5: resetCount equal, PCR 4 and 7, the capped
PCR 8 and 9 with the boot nonce, and the PIN; the lock-screen object:
`PolicySecret` on the service key, the PIN and the PIN epoch; the service
key: its authValue only; every NV index, F2's included: written only
through its stated policy. B/T at 4b (objects 1 to 3, the lock-screen
object, the service key, the NV indexes), extended at 10 (objects 4 and 5).

**When PCR 7 changes.** Some updates change PCR 7: a dbx update, a db
update (such as the 2023 CA migration), a firmware update, and a new shim
or SbatLevel (shim measures SbatLevel into PCR 7 and applies new SBAT
levels itself at boot). When the OS itself applies one of these, it first
predicts the new PCR 7 from the event log and the update. It then reseals
objects 1 to 3 with a `PolicyOR` of the old and new values. It checks each new object by comparing a trial-session
policy digest with the object's authPolicy, because the system is up and
PCR 8 and 9 are capped, so a real test-unseal is impossible. It writes them
into the other header-block copy and only then applies the update. The
next boot unseals them before capping, which is the real test, and after
that boot succeeds the old values are dropped. Changes made outside
Optimal (firmware setup, another OS) can't be predicted: the next boot
shows no anti-phishing code and says what the PCR 7 event log shows
changed, the disk secret doesn't unseal, the user
types the recovery key, and that boot reseals after capping (first-boot
sealing, above). Where PCR 4 is bound per machine, the same predicted
reseal adds a new loader's PCR 4 to objects 1 to 3 before its trial boot.

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
  secrets, which are stored in the header block wrapped only by the
  recovery key, and the persistent SRK is created. At the first boot from
  disk the lockout secret is also wrapped under HKDF(S, `optimal-lockout`)
  for the lockout reset below. So even a process running as root can't
  reset the dictionary-attack lockout, clear the TPM through the owner, or
  redefine the counter. Firmware may leave the platform hierarchy's
  authorisation empty. If so, the loader disables that hierarchy
  (`TPM2_HierarchyControl`) before `ExitBootServices`, so nothing after it
  can clear the TPM through the platform hierarchy.
- **A TPM another OS already provisioned** (Windows in a dual boot): the
  installer reads `TPMA_PERMANENT` and finds ownerAuthSet or
  lockoutAuthSet. It never changes an authorisation it doesn't hold. With
  the lockout authorisation unknown, Optimal works without it: no lockout
  reset after a good boot and no change to the dictionary-attack
  parameters, so the budget is the TPM's existing maxTries and
  recoveryTime (read with `TPM2_GetCapability` and shown at install), and
  wrong PINs and unclean shutdowns since that OS's last reset count
  against it. With the owner authorisation unknown, nothing can be defined,
  so the only way on is a TPM clear the installer explains first (the
  other OS's TPM-bound keys, BitLocker's included, then need their own
  recovery). Test (Phase 4b): a swtpm provisioned Windows-style (lockout
  authorisation set to an unknown value) installs, seals and unseals, and
  no lockout reset is issued.
- **F2's NV pool.** First-boot sealing defines 8 per-user NV indexes of
  32 bytes for F2's ratchet keys ([features.md](features.md)), written only,
  once a day, by a kernel operation under `PolicySecret` on the service key. With the
  other six indexes that is 14 indexes holding 304 bytes of data, plus the
  persistent SRK; install checks the free index count with
  `TPM2_GetCapability` and defines fewer F2 indexes (down to 0) on a TPM
  without room, and the counts on the reference fTPMs are published. A user
  with no index (a 9th user, or none left) gets F2's B2 to B4 limit.
- **The floor never goes below the loader's built-in minimum.** Each
  loader has a minimum config SVN built in, lagging one security release
  (the SVN of the security-fix release before the newest one when it was
  built), and is installed only after a completed floor raise at or above
  that minimum, so a fallback slot and the recovery copies always meet it.
  Older loaders are retired
  through SBAT generations (path 1) or dbx entries (path 2), which live in
  firmware and survive a TPM clear. On every boot the floor is at least
  that minimum.
- **TPM clear or redefinition.** Someone can still clear the TPM from
  firmware setup. That wipes the counter, the base and every sealed
  object, and the records the loader could check are attacker-writable
  (loader state is a runtime-writable UEFI variable, and the loader can't
  check the header block's tag), so the floor can't come from them alone.
  Whenever the anti-rollback counter and its base exist (read at their
  handles; a name that differs from the header block's record is reported
  as tampering but changes nothing), the floor is max(its built-in
  minimum, the highest SVN among the configs on the ESP, counter − base).
  The loader treats the TPM as cleared only when the SRK, the counter or
  the base is missing: the floor is then max(its built-in minimum, the
  highest SVN among the configs present on the ESP), it boots the newest
  image at or above that floor, and it says the TPM was cleared and that no
  anti-phishing code can be shown. If nothing on the ESP reaches its
  built-in minimum, it boots nothing and says to reinstall from signed
  media. An image above the minimum but below the old floor can then boot,
  but every sealed object is gone with the clear, so it opens nothing
  without the recovery key. The recovery-key boot that follows takes
  ownership again, recreates the SRK, creates a new service key and the
  mark key, defines counter and base at that floor, the replay counter and
  its base, the PIN-epoch index, the time index and F2's pool, and reseals
  (first-boot sealing, in its order). A recovery boot never redefines a
  counter that exists, except at a PCR 4 variant change (above), which
  keeps the floor it read from that counter before capping.
  Before the new replay value is accepted, it shows the volume's last
  authenticated commit time and generation ([storage.md](storage.md)) and
  the user confirms on the trusted path that this is the latest state. The
  old S is gone, so each home opens from its passphrase slot alone and its
  TPM part is resealed at that user's next login. Test (Phase 4b): clear
  the swtpm, redefine the base with the same attributes, write the new SRK
  name into the loader state and the header block, delete the newer slots and
  both recovery copies and write an old signed recovery image: the loader
  refuses everything below its built-in minimum, warns and shows no code;
  with a newer image left on the ESP it boots that image instead. Root,
  with no clear, writes a wrong SRK name into the loader state, corrupts
  object 3's slot and deletes the newer configs: the floor stays
  counter − base, and no image below it boots.
- **Lockout parameters:** maxTries 32, recoveryTime 7,200 s (one attempt
  recovered every 2 hours of TPM power-on time), lockoutRecovery 86,400 s.
  After every successful pre-cap unseal of object 1 the kernel, still
  before capping, unwraps the lockout secret with S, issues
  `TPM2_DictionaryAttackLockReset` and wipes the secret, so failedTries
  counts only wrong PINs and unclean shutdowns since the last good boot.
  A locked-out TPM is reset from the recovery-key boot with the lockout
  secret the recovery key unwraps; otherwise one attempt comes back after
  each 7,200 s of TPM power-on time.
- **Unclean shutdowns.** A TPM that wasn't shut down with `TPM2_Shutdown`
  after a DA-protected authorisation was used adds 1 to failedTries at its
  next startup, and every boot uses one. The kernel issues
  `TPM2_Shutdown(CLEAR)` on every reboot, power-off and panic path (the
  panic path drives the TPM interface by polling, with interrupts off) and
  `TPM2_Shutdown(STATE)` at suspend, so only a power cut, a hang or a hard
  reset costs an attempt. The budget is 31 wrong PINs (at the boot prompt
  or the lock screen, which allows at most 10 per unlock) plus unclean
  shutdowns between two good boots; the PIN prompt shows the attempts left
  once failedTries reaches 16.
- **Memory overwrite.** The kernel sets the TCG
  `MemoryOverwriteRequestControl` variable only while a panic mode (F4 in
  [features.md](features.md)) is enabled, so firmware clears RAM after an
  unclean reset only then; its added boot time is measured on the
  reference machines and published (Phase 5).
- **The anti-rollback counter** is an NV counter. TPM counters start at the
  highest value any counter on that TPM has ever had, not at 0, so the floor
  is **counter value − base**, where base is stored in a separate write-once
  NV index (`TPMA_NV_WRITEDEFINE`, written and then locked with
  `TPM2_NV_WriteLock` when defined, so it can't change until undefined).
  Both are defined at the first boot from disk, after capping, with the
  owner secret the recovery key unwraps: the counter is incremented with
  owner authorisation until it is at least the installed release's SVN,
  and base is set so the floor starts at that SVN. After that,
  `TPM2_NV_Increment` is allowed only through the counter's authPolicy: a
  `PolicyOR` of two `TPM2_PolicyAuthorize` branches, for the current and
  the next disk policy key (the same pair as the sealed objects), with
  reference `optimal-floor-v1` on signed-variant machines. Where PCR 4 is
  bound per machine the reference is `optimal-floor-v1-pm`, and each branch
  is followed by `TPM2_PolicySecret` against the service key, which only a
  kernel that unsealed S through this machine's PCR 4 can satisfy (an NV
  authPolicy can't change, so it can't hold a PCR 4 value that changes with
  every loader update). The policies those keys sign are a release's step-1
  PCR digests plus `TPM2_PolicyCommandCode(TPM2_NV_Increment)`, each under
  the reference matching its step-1 shape. So only an approved release's
  kernel before capping can raise the floor, and that kernel raises it no
  higher than its own SVN. An NV index's authPolicy can't change, so
  rotation beyond the next key works by delegation. Before it retires, the
  outgoing key signs `TPM2_PolicyAuthorize(new key)` with reference
  `optimal-floor-delegate-v1`, and the kernel satisfies that with chained
  `PolicyAuthorize` steps. Against the counter, a stolen disk policy key
  can only raise the floor, never lower it, because the counter only
  increases. The recovery image undoes that by redefining counter and base
  with the owner secret, which the recovery key unwraps. So nobody, root
  included, can push the floor above every release (which would make the
  machine unbootable), and neither index can be decreased, rewritten or
  undefined without the owner secret.
- **Parameter encryption:** every command that carries an authValue or
  sensitive data (`TPM2_Create` and its inSensitive R values and PIN half,
  `TPM2_Unseal`, `TPM2_PolicySecret` with S or M,
  `TPM2_DictionaryAttackLockReset`, `TPM2_HierarchyChangeAuth`,
  `TPM2_NV_DefineSpace`) runs in a salted HMAC session bound to the storage
  root key whose name was recorded at install, with parameter encryption in
  both directions, in the loader and the kernel alike; no password session
  is ever used. So neither a passive sniffer nor an active interposer that
  substitutes its own key gets the secret without the PIN. Against an
  active interposer, the PIN is the remaining protection, and the scorecard
  says so. Objects 1, 2, 4 and 5 carry the PIN step and release only an R
  that is useless without the PIN's second half. Only object 3 lacks it: an
  active interposer that obtains it can fake the anti-phishing code, but it
  gets nothing that opens the disk.

### Recovery key, PIN, passphrase

- **Recovery key.** A 256-bit recovery key is created at install, shown once
  and offered for printing. It is never sent anywhere unless the user opts
  in to escrow with a service of their choice. Losing both the TPM state and
  the recovery key loses the data, and the installer says so plainly.
- **The PIN is never the only secret.** The kernel stretches it before
  unsealing, under the one KDF rule in [storage.md](storage.md): Argon2id,
  4 lanes, a per-machine random salt, memory = min(1 GiB, RAM/4) but at
  least 64 MiB, and at least 4 passes. The passes are raised until
  stretching takes about 2 s on that machine, and the parameters are
  stored in the header block. An encrypted install needs at least
  256 MiB of RAM, and CI unlocks at 256 MiB and 512 MiB. One half of the
  stretched result, through HKDF, is the TPM authorisation value; the other
  is mixed with each unsealed R (HKDF of both) to give the key that
  decrypts what that object guards. Someone who extracts the sealed values
  from the TPM still has to search the PIN through Argon2id offline. A PIN
  of at least 6 digits is allowed only with
  a **discrete TPM**. Firmware TPMs (AMD fTPM, Intel PTT, or any
  manufacturer ID not on the discrete list, read with
  `TPM2_GetCapability`) have been broken by physical attack, so there
  install requires a **passphrase estimated at 60 bits or more** (by a
  zxcvbn-style estimator) instead of a PIN, through the same stretching.
- **Changing credentials** (Phase 4b), on the trusted path after the
  current PIN is checked through the lock-screen PIN object. *PIN:* the
  kernel draws new R1 and R2, stretches the new PIN, re-encrypts the TPM
  key-slot key's wrap and S under them, creates objects 1 and 2 and the
  lock-screen object for the new PIN at PIN epoch e + 1 (trial digests),
  writes them into the other header-block copy with epoch e + 1, flushes,
  then increments the PIN-epoch index. Each boot uses the copy whose
  recorded epoch equals the index; if a power cut came before the
  increment, the newer copy is ignored and the prompt says the change
  didn't finish and asks for the old PIN. *Passphrase:* its header-block
  wrap and the LUKS2 passphrase slot are rewritten in the tag order above
  (and, where the passphrase is the TPM authorisation, as for the PIN).
  *Recovery key:* a new key is shown once; the volume key, the owner and
  lockout secrets and S are re-wrapped under it in both header-block copies
  in turn, and the LUKS2 recovery slot in the tag order. The volume key
  doesn't change, so a header backup taken before (or old flash blocks)
  still opens with the old recovery key; Settings offers online
  re-encryption to a new volume key ([storage.md](storage.md)), after which
  it opens nothing. Tests (Phase 4b): restoring the pre-change header-block
  copy and typing the old PIN fails; a power cut at every write of a PIN
  change and of a passphrase change boots without the recovery key; after a
  rotation the old recovery key opens neither copy.
- **Anti-phishing code.** A static secret would be useless: anyone can
  power the machine on and photograph it. Instead the first boot from disk
  generates a 160-bit TOTP key (RFC 6238: HMAC-SHA-1, 30 s steps, 6 digits)
  and a 256-bit mark secret M and seals both as object 3. The kernel shows
  the TOTP key once as a QR code for the user's phone authenticator.
  Optimal Boot unseals object 3 through `EFI_TCG2_PROTOCOL` after
  measuring the kernel, computes the code for the current time and shows it
  above the PIN prompt with the UTC time it used. The user checks it
  against their phone. Another signed chain (an old loader, a signed tool
  faking the prompt) can't unseal the key, so its prompt shows no correct
  code. Object 3 also binds PCR 7, so a dbx, db or firmware update made
  outside Optimal shows no code either; the loader names the change, and
  the boot after the recovery-key boot shows the code again.
- **Time for the code.** The RTC may keep local time (Windows dual boot)
  and drifts while off (about 50 s a month at 20 ppm). At every shutdown
  the kernel, whose clock is NTS-disciplined, records in the loader state
  the RTC-to-UTC offset and the RTC drift rate measured between syncs; the
  loader computes UTC as RTC + offset + rate × (RTC time since that
  shutdown).
- **Clock set forward.** An attacker could set the clock forward and
  photograph a code for a future time. The loader keeps the highest time
  step it has shown in an NV index (the time index) and raises it before
  it shows a code; if the clock is earlier than that step, it warns that a
  code for a later time was shown before. The time index is written only
  through its policy (`TPMA_NV_POLICYWRITE`): `TPM2_PolicySecret` against
  the mark key (a `noDA` TPM object whose authorisation value is M) plus
  `TPM2_PolicyCommandCode(TPM2_NV_Write)`. M comes only out of object 3, so
  only a chain that can show a correct code can move the mark, on
  signed-variant and per-machine machines alike.
- **No code.** The loader says why when it can't show a code: the first
  boot from disk, a TPM clear, a PCR 7 change, or a key that no longer
  unseals. Typing the
  recovery key without a code is safe only in a case the user caused
  (a fresh install, a clear or firmware reset they made); the prompt says
  so. After that boot the kernel generates a new TOTP key and M, seals
  them and shows the QR code once (re-enrolment). Re-enrolment for a lost
  phone is in Settings, on the trusted path, and needs the PIN.
- **What a correct code proves.** It proves the TPM saw the approved
  loader and kernel at or above the floor with the PCR 7 state (Secure
  Boot on, the same PK, KEK, db and dbx) Optimal last sealed to, not that
  the firmware itself (PCRs 0 to 5) is unchanged. Someone who turns Secure
  Boot off to load a key-logging firmware driver therefore gets no code
  shown. The loader also decodes the PCR 7 event log and, if Secure Boot
  is off or PK, KEK or db differs from what the kernel recorded in the
  loader state at the last good boot, says so and not to type the
  recovery key unless the user made that change; such a driver could fake
  that log, so only the missing code is relied on. A firmware setup
  password keeps firmware changes out, and the scorecard says so. The optional tamper alert (F7
  in [features.md](features.md)) extends this comparison to PCRs 0 to 5,
  with an S-authenticated record checked after unlock.
- **Where the PIN or passphrase is typed.** Once Optimal Boot lands (Phase
  4b), it reads it through the UEFI text input protocol (`ConIn`, which
  works with USB keyboards through firmware) before `ExitBootServices`, and
  hands it to the kernel in a dedicated page the kernel zeroes after use;
  the loader wipes its own copies before handing over. It also hands over
  the EFI device path behind `ConIn`, and the kernel's reader trusts that
  device as the one that typed the unlock secret; if no path is known, the
  reader asks for a code shown on screen to be typed after hand-over.
  Unencrypted installs use the keyboard that types the login password.
  Test (Phase 5): the `usb-kbd` test reruns on a B1 swtpm boot. With Limine or on
  BIOS there's no TPM unlock at all, and the kernel asks for the
  **passphrase** on the framebuffer console from Phase 4a. Input comes from
  PS/2, serial, or a USB keyboard through a polled xHCI boot-protocol
  reader. That reader is in-kernel, takes 8-byte boot reports only, and is
  held to the fuzz thresholds for in-kernel parsers in
  [drivers.md](drivers.md). It writes Intel's `XUSB2PR`/`USB3_PSSEN` port
  routing when it starts, and it stops when the Phase 6 xHCI driver binds.
  Internal laptop keyboards on I2C-HID use the in-kernel I2C-HID parser
  once it is promoted with the thresholds. Prompts after resume use only
  these in-kernel paths, never Bluetooth or a tier-3 HID process. Once the
  xHCI driver owns the controller, resume takes the controller back with
  the polled reader for the PIN and hands it to the driver afterwards.
  Tests: in Phase 4a the passphrase is typed on a PS/2, a USB (QEMU
  `usb-kbd` on `qemu-xhci`) and a serial keyboard; in Phase 10 the resume
  PIN is typed the same ways with the Phase 6 xHCI driver bound.

### Tests (Phase 4b gate, QEMU with swtpm)

**Stand-ins at 4b.** The desktop, the update client (Phase 7) and the
prompt VT (Phase 5) come later, so the 4b gate uses: a pre-desktop kernel
console as the trusted path (the prompt VT grows from it at 5), from which
credential changes, the re-tag, the confirmation after a clear,
re-enrolment and the lock-screen PIN object's check run; a signed test
health agent that marks a slot or loader good; and a minimal signed
slot/loader stager that does `BootNext` re-arming, the `BootOrder` flip,
floor-raise stage 1 and config-key rotation step 1. The tests that use
the stager or the health agent rerun with the update client at 7; the HID
guesser test stays at 6.

- **Unseal and tamper.** A normal boot unseals. A kernel update, a rollback
  to a release at or above the floor, an OS-applied dbx update, and an SBAT
  bump with a new shim all unseal with no recovery prompt. A dbx change
  made outside the OS shows no anti-phishing code and names the PCR 7
  change, asks for the recovery key, and the boot after it needs only the
  PIN. Each of these
  fails to unseal: a tampered loader, config, command line, kernel, module
  or system-image root hash; an **older signed Optimal Boot**; a **signed
  EFI tool that replays the published PCR 8 and 9 events**; an image below
  the floor; an old sealed object restored after a floor raise.
- **PCR 4 variants.** Path 1 and path 2 installs use the signed variant. An
  install whose first boot follows a failed boot entry, and one
  chain-loaded from a second boot manager (dual boot), bind PCR 4 per
  machine, and both then unseal across a loader update. On a signed-variant
  machine, a signed EFI tool that replays PCR 8 and 9 and presents a
  PCR 8/9-only signature (`-pm` reference) can't unseal object 3,
  increment the counter or write the time index; on a per-machine machine
  the same tool fails at step 3 and at the counter's `PolicySecret`.
- **Capping and volume identity.** After boot, a second unseal attempt from
  the running system fails. Take a substituted data volume: an attacker's
  LUKS2 volume with a known passphrase and a hostile init. The tag check
  refuses it before its JSON is parsed, S is never unsealed, and the
  passphrase fallback opens it only after capping. From it, neither the
  real disk secret nor S can be unsealed, and its init can't bump the
  replay counter. A secondary LUKS2 header copy with a higher seqid and a
  valid checksum but no verifying tag is ignored and the primary used
  (header fuzzer); one flipped bit in the secondary copy still boots; a
  power cut at every write of first-boot sealing and of a passphrase change
  still boots; `cryptsetup luksAddKey` from Linux makes the TPM path refuse
  the volume, and a recovery-key boot shows the added slot, re-tags after
  confirmation and the boot after needs only the PIN. The first boot and a
  recovery-key boot read no LUKS2 key slot.
- **Every sealed object survives every reseal.** This runs after a floor
  raise, after an OS-applied dbx update, after a PIN change and after
  first-boot sealing: each new object's authPolicy matches its
  trial-session digest, and the next boot unseals it before capping. In
  Phase 4b, an object created after
  capping under `PolicySecret` against the service key unseals and the
  anti-phishing code is correct. The same suite reruns with a real
  second-user login in Phase 5 and a real hibernate and resume in Phase 10.
- **Bus and root.** swtpm traffic, captured over install, first-boot
  sealing, normal boots, a floor raise, a lockout reset and a PIN change,
  contains none of these, each searched for separately: R1 to R5, S, M, the
  TOTP key, either PIN half, the owner secret and the lockout secret; and
  no command uses a password session. A man-in-the-middle proxy in front of
  swtpm can't unseal the disk secret or S without the PIN. With the swtpm
  state (seeds included) dumped, the values objects 1 and 2 release open
  nothing without the Argon2id PIN search. From root: incrementing any of
  the three counters, rewriting either base, undefining any index, writing
  the time index and resetting the lockout all fail, and the TPM broker
  offers none of them.
- **PIN and phishing.** An swtpm reporting a firmware-TPM manufacturer ID
  makes install refuse a PIN and accept only a passphrase of 60 estimated
  bits; the secret unsealed without the PIN doesn't open the volume. A
  signed EFI tool can't show a correct anti-phishing code. A boot with the
  clock set back after a forward-set boot warns. With the RTC 2 minutes
  off, and with the RTC in local time 1 hour from UTC, the loader shows the
  code the reference TOTP implementation gives for true UTC. A TPM clear
  shows no code and names the clear; the recovery-key boot after it
  re-enrols, and the next boot shows a correct code. With Secure Boot
  turned off in the OVMF setup, the loader shows no code and the Secure
  Boot warning.
- **Lockout.** 64 power cuts at random points after the PIN unseal, plus
  10 kernel panics: failedTries is at most 1 at every boot, read with
  `TPM2_GetCapability`, and no boot asks for the recovery key. 32 wrong
  PINs lock the TPM; one more attempt is allowed after 7,200 s of swtpm
  power-on time, and the recovery-key boot resets it at once.
- **Anti-rollback.** A swtpm counter pre-incremented to 10^6 before install
  gives the right floor; a floor raise completes across a reboot; an
  increment after capping fails. Power is cut at every step of a raise:
  each slot write, each recovery copy, the reseal of each sealed object,
  each header-block write, the request, each real unseal of a new object
  and each increment. The machine boots each time without the recovery
  key. A reseal seeded with a wrong PCR digest fails its real unseal at the
  raising boot, the floor stays unchanged and that boot and the next need
  only the PIN. A
  raise approved by a rotated-in disk policy key passes through the
  delegation chain.
  On B2 the OS can't write the floor variable. Deleting the variable gives
  floor = max(the loader's built-in minimum, the highest SVN on the ESP). A
  signed EFI tool can reset it on path 1, as the scorecard states, but
  can't run under owner keys. After a raise, the recovery image boots. The
  floor and raise state machine, the loader's floor code included, is
  proof target 16 (B/T, Phase 4b) in [verification.md](verification.md).
- **Loader updates.** A new loader that hangs after `ExitBootServices`, and
  one that fails the kernel hand-off, is back on the old loader at the
  next boot with `BootOrder` never changed; after 3 failed trials the
  update client stops and reports. Power is cut during each write of a
  loader directory, the `BootOrder` flip and the fallback copy, and the
  machine boots each time. An SBAT or dbx revocation is applied only once
  both loader copies carry the new generation, and an attempt to apply it
  earlier is refused. Power is cut at every step of a config-key rotation
  (each dual-signed config, the loader update, the floor raise), and the
  machine boots each time without the recovery key. A loader update
  staged while a floor raise is pending waits for the raise, and a power
  cut at each step of that sequence still boots without the recovery key;
  a loader whose built-in minimum is above the completed floor is not
  installed.
- **Slots.** Power is cut during every loader-state write and the loader
  boots a valid slot each time; flipping one bit in the active slot's image
  makes the read fail, the kernel sets "slot bad", and the next boot runs
  the other slot and reports it; a validly signed config with a missing or
  wrong inclusion proof is refused by the loader. With the OVMF variable
  store full, the loader boots the last-good slot, the kernel reports the
  failed write and the update client stages nothing. A machine with no
  network adapter marks a new slot good.

## The system image

- The OS lives on a **read-only system image** protected by a Merkle hash
  tree (SHA-256, 4 KiB blocks, the same idea as dm-verity but our own format
  and code). Its root hash, together with the tree's geometry (block size,
  data size, tree offset) and salt, is on the kernel command line inside the
  signed config, so all of it is both checked and measured. The command
  line names the slot by a fixed partition type GUID (one per slot, one
  per recovery copy), so PCR 8 is the same on every machine, and the kernel
  looks for that GUID only on the disk the loader booted from (the loader
  hands over that disk's GPT disk GUID), so a USB partition carrying the
  same type can't stand in for a slot.
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
- **The loader has two copies too.** A FAT ESP isn't crash-safe, so shim
  and Optimal Boot (or Optimal Boot alone on path 2) live in two loader
  directories, `\EFI\optimal\loader-a` and `loader-b`, each with its own
  `Boot####` entry. A loader update writes the inactive directory, reads it
  back and checks every hash, then trial-boots it with `BootNext`, which
  firmware clears before booting it, while `BootOrder` stays on the old
  loader. So a new loader that hangs or fails anywhere, after
  `ExitBootServices` included, is followed by a boot of the old one. The
  loader state has its own loader tries counter (3): the update client
  re-arms `BootNext` at most 3 times, then stops and reports. Only after
  user space marks the new loader good (the slot health check below, run
  under it) does the update client flip `BootOrder`, then copy the loader
  to `\EFI\BOOT\BOOTX64.EFI`, the third fallback, and then to the other
  directory. PCR 4 measures image hashes, not paths, so every copy gives
  the same PCR 4. The recovery image is started by whichever loader copy
  booted.
- **Loader state** (per-slot and loader tries counters, good and bad flags,
  the active slot, the slot a hibernation image needs, a pending
  floor-raise request, the SRK name, the RTC offset and drift for the
  anti-phishing code, and the PK, KEK and db digests of the last good boot)
  lives in two UEFI non-volatile
  variables, each with a version number and a CRC-32C. The loader reads the
  valid one with the higher version and every write goes to the other, so a
  power cut during a write leaves the previous state intact. State is not
  trusted for security: the worst a corrupted or root-written state can do
  is pick the other valid slot or the recovery image, and the floor never
  drops below the loader's built-in minimum. If a loader-state write fails
  (variable store full or worn), the loader boots the slot last marked
  good, never a slot on trial, the kernel reports the failed write, and the
  update client stages nothing until a write succeeds.
- **User data is separate**, on the encrypted own file system. Rolling back
  the OS never rolls back anyone's documents. File system snapshots are a
  user feature (undo for your files, F12 in [features.md](features.md)),
  not the update mechanism. System undo from the loader menu (F11 there)
  never goes below the floor: it restores settings and the previous slot,
  except after a security update, whose floor raise re-images the other
  slot with the current release, so undo then restores settings and
  snapshots only, on the same release. The scorecard says "after a
  security update there is no older release to fall back to".
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
  checkpoint cosigned by a threshold of the witness set built into the
  loader (at least 4 independent witnesses, threshold 2, so losing any 2
  still lets releases ship); Optimal Boot checks both against the log key
  and witness keys built into it, and refuses the config otherwise (Phase
  4b). The witness set and threshold change through a signed loader
  update whose own config carries a checkpoint cosigned under the old set,
  or through an emergency witness-set change signed by the offline root
  (2 of 3, its keys built into the loader) and logged, which also needs a
  cosignature by at least 1 old witness, or else 30 days in the public log
  plus a trusted-path notice before the update client applies it. Each
  witness set has a monotonic version, built into the loader that carries
  it, so the floor raise that ships a new set retires loaders with the
  old one. The log and the 4 witnesses, run by
  parties other than the release signers, are provided by the owner and
  are a 4b entry condition. The update client checks the same before
  installing, and before any reseal it checks that the new loader's hash
  and every disk policy it reseals to are logged. So a release signed in
  secret (stolen key, coercion) can't be installed by the update client or
  booted by a current loader without being logged. Firmware doesn't check
  the log and the TPM accepts a disk-policy signature whether logged or
  not, so a secretly signed loader placed with physical access is not
  covered at boot: it is caught only by the update client's check before
  resealing. Tests (Phase 4b): an unlogged
  but validly signed release is refused by both; with one witness removed
  a release still boots; the witness set is rotated with one old witness
  dead; an emergency change signed by the root and cosigned by the one
  live old witness is accepted; a root-signed change with no old-witness
  cosignature while 2 or more old witnesses are alive is refused.
- **When updates apply.** Staging never restarts the machine: the slot
  switch, loader trial boots and floor raises happen only at a restart the
  user chose, and each release's changelog is in its signed metadata (F14
  in [features.md](features.md)).
- **Time.** Metadata expiry needs a trustworthy clock, so the time service
  uses NTS (authenticated NTP), and the clock is never accepted earlier
  than a floor: the newest metadata time the client has seen, and before
  the update client exists, the signed build time in the running release's
  config. Spoofed NTP from a hostile network can't freeze updates; the
  spoofed-NTP test uses the stored, signed time floor until Phase 7. Until
  NTS has set the clock, TLS certificate checks (NTS-KE and F6's encrypted
  DNS, whose default provider is pinned by IP address) use the time floor
  as the earliest possible time: a certificate is refused if its notAfter
  is before the floor, and its full validity period is checked once NTS
  has set the clock. Test (Phase 6): with the RTC reset to 2000-01-01, NTS
  sets the
  clock and name lookups work, with no plain port-53 query.
- **Anti-rollback.** Each release, the recovery image included, has a
  security version number (SVN) in its signed config. The minimum allowed
  SVN (the floor) is counter − base in the TPM (B1, above), or on B2 (no
  TPM) a non-volatile UEFI variable with boot-services access only (no
  `RUNTIME_ACCESS`), so the OS can't write it. Any binary the firmware
  trusts can, though, since it runs before `ExitBootServices`; on path 1
  that includes every Microsoft-signed loader and shell. If the variable
  is missing or corrupt, the floor is max(the loader's built-in minimum,
  the highest SVN among the configs on the ESP), and on every boot it is at
  least that built-in minimum (TPM ownership, above). The scorecard says that B2 anti-rollback holds against OS-level
  attackers only, and against physical ones only with owner keys. The
  loader refuses to boot an image below the floor, and the TPM
  policy refuses to unseal for one. The floor is raised only when a release
  fixes a security bug, so ordinary rollback to the previous release keeps
  working, and in two stages:
  1. After the new release has booted and passed its health check, user
     space writes the other slot and both recovery-image copies at the new
     SVN n and verifies all three against their signed roots, so the
     fallback slot now holds the current release. On B1 the kernel, while
     up, seals objects 1 to 3 again at floor n (their R values, the TOTP
     key and M; the AEAD ciphertexts in the header block don't change),
     checks each new object's authPolicy against a trial-session policy
     digest, and writes the full set into the other header-block copy, so
     the old set stays in the first. Only then does it write a "commit SVN
     n" request into the loader state.
  2. On the next boot the loader checks the signatures and SVNs of the
     other slot's config and both recovery-image configs and hands the
     result to the kernel (image contents were verified in stage 1). On B1
     the kernel, before capping, checks that n is at most its own SVN and
     that the loader's check found SVN ≥ n everywhere, then unseals every
     new object for real (the counter is still at most base + n, so they
     unseal), which is the real test. Only if all of that succeeds does it
     increment the counter until counter − base = n and clear the request.
     Otherwise it drops the request, so the floor stays unchanged; if a
     new object failed to unseal, it boots from the old set (the
     header-block rule above) and, after capping, rewrites the newer copy
     from it, so no boot needs the recovery key. A boot that resumes from
     hibernation leaves the request pending for the next normal boot. On B2 the loader makes the same checks,
     writes the floor variable and clears the request. A power cut at any
     point leaves either the old floor with every object working, or the
     new floor with every new one.

  SBAT generations (path 1) and dbx entries (path 2) revoke old loaders the
  same way. A revocation is applied only after both loader directories and
  the fallback carry the new generation and verify, which happens only
  after the new loader was marked good. The predicted PCR 7 reseal above
  runs before it.
- **Boot counting.** Each slot has a tries counter (3) in the loader state.
  The loader decrements it on every boot attempt; user space marks the slot
  good after its health check, which is local only: the desktop (or on a
  headless install the login service) is up, the network stack has
  initialised (no adapter or connection needed), the update service is
  running and the system image verified its reads so far. When tries reach
  0, or the slot is marked bad, the loader boots the other slot. A resume
  from hibernation runs the slot the image was made by (Resume from
  hibernation, above), and the next normal boot runs the active one. Test:
  install a kernel that hangs before user space; the machine is back on the
  old slot within 3 boots without any key press. Hibernating after an
  update made the other slot active resumes into the image's slot (Phase
  10, [storage.md](storage.md)). B4 has no boot counting (Interim chain,
  above). Each slot's config also carries a signed safe-mode entry, whose
  boots leave tries, flags and the floor unchanged (F13 in
  [features.md](features.md)).
- **Recovery image.** A small signed, read-only recovery system with its
  own SVN, stored as two copies on their own partitions, so a power cut
  while one is rewritten leaves the other. The loader boots the newest valid
  copy at or above the floor, from the loader menu or automatically when
  both slots fail. Its PCR 8 and 9 values are signed like a slot's, so the
  anti-phishing code shows above its recovery-key prompt. It accepts the
  recovery key, can reinstall a slot from a signed image on USB or the
  network, and can copy user data off.

## Phase placement

| Item | Phase |
|---|---|
| PIE kernel and KASLR (kernel-chosen virtual and direct-map bases) with its tests; hash-pinned, enrolled Limine in the shipped image (not done yet) with the CI enrolment check; old signed image refused through a dbx hash entry in the test OVMF store; microcode on the boot CPU | 1 |
| Microcode on every CPU | 2 |
| Verified system image format and block checks; A/B slot layout; B4 static A/B Limine entries; passphrase prompt with PS/2, serial and the polled USB boot keyboard reader; the Optimal header block format, parser and fuzzing with its passphrase and recovery-key wraps (no boot path reads a LUKS2 key slot) | 4a |
| `MemoryOverwriteRequestControl` only while a panic mode is enabled, boot cost measured | 5 |
| Lock-screen HID-guesser test; TLS time check from the time floor with the RTC-reset test | 6 |
| TPM driver, measured boot (entry conditions: Optimal Boot's measured boot and boot counting, Secure Boot path 2 on the reference laptop, and the transparency log with 4 witnesses at threshold 2 running, owner-provided), the emergency witness-set change, shim revocation tracking and the documented way back, path 1 through a test CA in db, config-signing key (Ed25519) and its rotation, the loader under core.md's rules with `core-crypto` (TPM session crypto included), the loader's pre-boot IOMMU installed first, with the excluded pool or `EDKII_IOMMU_PROTOCOL` implementation, every disk read and the disk exception dropped before the PIN and any unseal, re-arm, PMR coverage check, target 10 extended to the table builder, the hostile boot-disk tests and the FPGA canary scan, PCR 4 variants and the `-pm` references, first-boot sealing after capping in its order (keys, NV indexes, read-back, objects) with trial-digest checks, the boot nonce at capping, the lock-screen PIN object, F2's NV pool, dual boot with an already-provisioned TPM, the one-pending-operation rule, the 4b stand-ins (kernel console as trusted path, test health agent, slot/loader stager), the header block's TPM fields and 15 slots (proof target 15 at B/T), the sealing policy with the PIN epoch, proof target 17, capping and per-copy volume tags with pending sets and the recovery-key re-tag, sealed objects 1 to 3 sealing only R, the service key and its recovery-key wrap, salted HMAC sessions for every secret-carrying command, TPM ownership, platform-hierarchy disable, the built-in minimum SVN and clear detection (any boot with no code included), counter, base, replay counter and its base, PIN-epoch index and time index, lockout reset and `TPM2_Shutdown` on every path, stretched PIN and fTPM passphrase rule, PIN, passphrase and recovery-key changes, PIN entry in the loader, anti-phishing TOTP with RTC correction and re-enrolment, per-slot configs, loader state, two loader copies with `BootNext` trial boots, slot-bad fallback, local health check, two-stage floor raise with the real unseal before the increment, revocation ordering, recovery image copies with signed PCR 8/9, slots by partition type GUID on the boot disk, the loader's transparency check, proof target 16, all tests above; outside audit of the TPM service, loader and core at the end of 4b | 4b |
| Signed update client with TUF metadata and transparency inclusion proofs | 7 |
| Secure Boot path 2 on 3 real PCs and path 1 submitted for review | bootloader track, finishing before Phase 7 |
| Microcode after resume; the resume object (object 5) and resume PIN input with keyboard identity across re-enumeration; S3 waking-vector IOMMU re-arm and its FPGA test; the hibernation key (object 4), the hand-over page and the resume-slot rule | 10 |
| Optimal Boot's BIOS stage, replacing B4's static Limine entries | bootloader track |
