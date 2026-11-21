# Storage: block layer, durability, encryption layering, ext4

This file covers the kernel side of storage: the block layer, what `fsync`
and friends promise, the page cache, how encryption is layered, and the rules
for touching other people's disks. The own copy-on-write file system's format
and internals are in `fs/docs/DESIGN.md` and `fs/docs/FORMAT.md`; this file
sets the requirements it must meet.

## Block layer

- **Requests carry ordering and durability flags:** `PREFLUSH` (flush the
  device's volatile cache before this write) and `FUA` (this write is durable
  when it completes). The NVMe driver maps them to Flush and the FUA bit, the
  AHCI driver to FLUSH CACHE EXT and FUA writes. If a device has no FUA, the
  block layer emulates it with a write followed by a flush.
- **Ordering is by completion, never submission.** NVMe gives no ordering
  between queues or even within one, so a file system's commit block is
  submitted only after the writes it depends on have *completed*, and then
  with `PREFLUSH|FUA`. The block layer API makes this the only way to write
  a commit (a commit takes the completion tokens of what it covers).
- **Per-drive facts are probed, not assumed:** volatile write cache present
  (NVMe VWC, ATA IDENTIFY word 85 bit 5), atomic write unit, and discard
  behaviour. Power-loss protection can't be probed (neither NVMe nor ATA
  reports it), so a drive with VWC=1 is treated as **unprotected** (a
  completed write isn't durable until a flush completes) unless a quirk-list
  entry with evidence says it has power-loss protection; only VWC=0 or such
  an entry makes a completed write durable. The quirk list (in the tree,
  with the evidence for each entry) also covers drives known to ignore
  flushes; on those, the file system disables the volatile cache if the
  drive allows it and warns if not.
- **Encryption passes flags through.** Whatever encrypts (see below) keeps
  `PREFLUSH`/`FUA` and completion ordering intact; a test checks this by
  recording the request stream below the encryption layer.
- **Errors are never dropped.** A failed write marks the affected pages and
  records the error against the file (below). A write that fails with a
  retryable status (NVMe DNR=0, ATA aborted or interface CRC error) is
  retried 3 times, the last after a controller reset; a non-retryable status
  (NVMe DNR=1, ATA uncorrectable or ID-not-found) is not retried. A device is
  set read-only, never hidden, when 16 writes have failed all retries within
  10 minutes, or at once when it reports itself read-only (NVMe Critical
  Warning bit 3, ATA write-protect). Every file system on Optimal, the own
  one included, follows this rule and the errseq rule below; none stops all
  writes on a single error. Test (Phase 4a): injected retryable and
  non-retryable errors on virtio-blk and NVMe reach the expected outcome.
- **Device hangs are not driver hangs.** A command that hasn't completed
  within its timeout (30 s for I/O, as Linux's default) means the device
  stalled, not the driver: the tier-1 NVMe or AHCI driver resets the
  controller itself (NVMe Abort of the command, then CC.EN=0 and
  re-initialisation if the abort doesn't complete in 5 s; AHCI COMRESET of
  the port) and re-sends through the replay log below, exactly as after a
  restart. A kernel crash is never the result of a device stall. PCIe
  Completion Timeout is set to range A (at most 10 ms) on every device that
  supports it, and drivers never do MMIO reads with interrupts off, so a
  removed device can't stall a CPU past the watchdog ([drivers.md](drivers.md)).
  Test (Phase 4a): an emulated NVMe and AHCI device stops completing
  commands for 60 s; the controller is reset, no write is lost, no
  application sees an error, and the latency gate holds.
- **Driver restarts are invisible when nothing was lost.** When a storage
  driver is restarted ([drivers.md](drivers.md)), the block layer keeps its
  in-flight requests and resubmits them to the new instance. A controller
  reset can drop the drive's volatile cache, so every write that completed
  since the last successful flush is treated as **not completed**: it is
  re-issued, and any commit that depends on it waits until it and a new
  flush complete. To re-issue it, the block layer keeps a per-device replay
  log of completed, unflushed writes on every drive treated as unprotected
  (above; on others a completed write is durable and nothing is kept). Page-cache writes are kept by reference: the page stays
  write-protected until the flush completes, and a process writing to it
  meanwhile gets a bounce copy (stable pages, below). `O_DIRECT` writes are
  copied into the log when they complete, because the caller may reuse its
  buffer. A completed write fully covered by a later completed write to the
  same blocks is dropped from the log. When the log reaches 64 MiB the block
  layer issues a flush and empties the log when it completes. After a
  restart the log is re-sent in completion order, then flushed, then the
  held commits go out. Applications see `EIO` only if three restarts in a
  row fail. Tests (Phase 4a): overlapping writes to the same blocks
  completed in both orders, then a driver kill before the flush; the disk
  must hold the later-completed data, for page-cache and `O_DIRECT` writes.

## What durability calls promise (the Linux contract)

Databases, package managers and git depend on these exact behaviours, so they
are written down and each has a crash test.

| Call | Promise after it returns successfully |
|---|---|
| `fsync(fd)` | the file's data and metadata (size, times, block map) are on stable media, including the device cache |
| `fdatasync(fd)` | data plus the metadata needed to read it back (size, block map) are stable |
| `fsync` on a directory | the directory's entries (creates, unlinks, renames into it) are stable |
| `rename(a, b)` over an existing `b` | atomic: after a crash, `b` is either the old or the new file, never missing or partial. Durable once the directory is fsynced |
| `O_SYNC`, `O_DSYNC` | each write behaves as write plus `fsync`/`fdatasync` |
| `O_DIRECT` | bypasses the page cache, aligned I/O only (`EINVAL` otherwise, as on Linux); durability still needs `fsync` unless the file was opened `O_DSYNC` |
| `sync_file_range` | starts or waits for writeback of the range only; makes **no** durability promise, same as Linux |
| `syncfs`, `sync` | everything on that file system (or all) is stable |
| `msync(MS_SYNC)` | the mapped range's data and the metadata to read it back are stable (what LMDB and other mmap databases rely on) |
| `fsync` on a newly created file | the file **and its directory entry** are stable (as ext4 does; many programs rely on it, though POSIX doesn't promise it) |
| `fallocate`, `ftruncate` | the size and allocation change is stable after `fsync` of the file |
| `link`, `unlink`, `O_TMPFILE` + `linkat` | the directory change is stable after `fsync` of the directory; an `O_TMPFILE` file linked in is complete, never partial |
| replace-by-rename without an `fsync` of the new file | the new file's data is committed in the same transaction as the rename (the ext4 `auto_da_alloc` behaviour), so after a crash `b` is the old file or the complete new one. This is stronger than POSIX and is what editors and config writers depend on |

**Writeback errors.** Each file keeps an error sequence counter (the same idea
as Linux's errseq). When writeback fails, the counter moves on. Every open
file description sees the error exactly once, on its next `fsync`, `fdatasync`
or `close`, even if another process already saw it and even if the pages were
since cleaned. Pages whose writeback failed stay dirty and are retried. If
the device is gone, up to 64 MiB (or 1% of RAM, whichever is smaller) of
them are kept per device in case it comes back; beyond that, or after the
device is gone for 10 minutes, they are dropped, and the error stays reported
on every file that had them. A test pulls a USB stick mid-write. (This is the PostgreSQL "fsyncgate" case: an error must
never be silently forgotten.)

Tests: an ALICE/CrashMonkey-style suite that, for each row above, records the
block request stream, generates every crash state at every flush boundary
(plus random reorderings of unflushed writes, see below) and checks the
promise holds. SQLite's own crash tests and a git and package-install crash
workload join the Linux compatibility suite (milestone 4 in
[compat.md](compat.md)).

## Page cache, mmap and copy-on-write

- **One page cache.** `read`/`write` and shared file `mmap` see the same pages.
  Dirty tracking for mapped pages uses the page tables' dirty bits, collected
  by write-protecting mapped pages when writeback starts.
- **Stable pages.** A page being checksummed, encrypted or written is
  write-protected (or copied to a bounce page if a process writes to it at
  that moment), so the bytes on disk always match their checksum.
- **Space for copy-on-write.** Writing a page of a CoW file needs new space at
  writeback time, which is too late to report `ENOSPC`. So space is reserved
  when the page is first dirtied: in `write()` (which then returns `ENOSPC`)
  or at the first write fault on a shared mapping (which then delivers
  `SIGBUS`, the same as Linux on a full file system). A metadata reserve of
  2% of the volume, minimum 256 MiB and maximum 8 GiB, guarantees deletes,
  truncates and snapshot removal always succeed on a full disk.
- **Truncate versus mmap.** Truncating a mapped file unmaps the cut-off pages
  first; touching them afterwards delivers `SIGBUS`, as on Linux.
- Tests: an xfstests-style group for mmap, truncate, `ENOSPC` and writeback
  errors, plus proof target 12a at Phase 4a (block allocator never
  double-allocates, commits are ordered after the writes they cover, restart
  re-sends every unflushed write; [verification.md](verification.md)).

## Encryption layering

There are two stages, and the scorecard always says which one a machine is in.

**Stage 1, Phase 4a (before the own file system is the default):** a block
encryption layer under ext4 using AES-256-XTS with the CPU's AES
instructions, in the LUKS2 on-disk format (implemented from its public
specification, so Linux can open the same volume and the rescue tools work).
All cipher, hash and KDF code (AES-XTS, AES-GCM, GHASH, ChaCha20-Poly1305,
SHA-2, HKDF, Argon2id) is the core-crypto crate, proof target 14 (T from
Phase 4a: CAVP and Wycheproof vectors, differential tests, constant-time
check; [verification.md](verification.md)).
XTS gives **confidentiality only**: it hides data on a powered-off disk but
doesn't stop an attacker who changes ciphertext. They can't choose the
plaintext they produce, but they can scramble sectors or put back older
copies of them. So in stage 1:

- the in-kernel ext4 requires `metadata_csum`, so a scrambled metadata block
  fails its checksum (with probability 1 − 2^-32);
- the in-kernel ext4 is hardened as a parser of untrusted input anyway,
  because replayed old sectors carry valid checksums: all on-disk indices go
  through `Untrusted<T>`, it is fuzzed with crafted images and with
  "replayed sector" images (a mix of sectors from different points in an
  image's history), and it must never crash or corrupt memory, only refuse
  the volume or return `EIO`;
- the scorecard says "encryption: confidentiality only, no tamper
  detection, nothing protected while locked", and promise 4 in
  [threat-model.md](threat-model.md) is reported as **partial** until stage
  2: a suspended machine has no disk key in memory (below), but its page
  cache still holds plaintext.

**What is parsed before capping.** Before capping ([boot.md](boot.md)) the
kernel parses only the fixed-format Optimal header block (the last 16 KiB
before the 16 MiB LUKS2 data offset, layout in [boot.md](boot.md))
of the system volume (sealed blobs and volume tag; the tag is checked over
the raw header bytes before anything else is parsed). The LUKS2 JSON header
and the volume map (below) are parsed only after capping.

**KDF (one rule everywhere).** Every human secret (PIN, passphrase, fTPM
passphrase, LUKS2 key slots, the own file system's slots) is stretched with
Argon2id, 4 lanes, memory = min(1 GiB, RAM/4) but at least 64 MiB, at least
4 passes, with the passes raised at install until unlocking takes about
2 seconds on that machine (checked on the reference laptop in
[benchmarks.md](benchmarks.md)). The parameters are stored per key slot and
shown on the scorecard. The minimum RAM for an encrypted install is
256 MiB; below it the installer refuses to create an encrypted install and
the scorecard says "disk not encrypted: under 256 MiB RAM". PBKDF2 is used
only for the slot of the 256-bit random recovery key, whose entropy makes a
slow KDF pointless. Tests (Phase 4a): a slot below the floor for its
machine's RAM is refused by the installer, the key-slot tools and the
unlock path; CI unlocks an encrypted install on 256 MiB and 512 MiB guests.

**Suspend wipes the disk key.** On suspend, dirty pages are written back
and the volume is frozen. On B1, the kernel then wraps the LUKS2 volume key
and the service-key secret S into a **resume object**: a TPM object whose
policy is this boot's capped PCR 8/9 values (they persist across S3 through
`TPM2_Shutdown(STATE)`) plus `TPM2_PolicyAuthValue` (the stretched PIN);
it is listed with the other sealed objects in [boot.md](boot.md). Then the
volume key and S are wiped from memory. On resume the PIN is read through
the in-kernel input paths only ([threat-model.md](threat-model.md)), the
resume object releases the volume key and S, and the volume is thawed. A
TPM reset during suspend makes the resume object unusable, and the machine
goes to recovery. Without a TPM, resume asks for the passphrase. Resume
reserves the KDF's memory before suspending, or drops clean page cache
first if it can't. Test (Phase 10): suspend, scan memory for the volume key
and S, resume with and without the right secret, and resume after a TPM
reset (must reach recovery). Home volumes (below) are frozen and their keys
wiped the same way; each thaws when its user next unlocks their session.

**Per-user home volumes (Phase 5).** Each user's home is its own volume, so
another user's files aren't readable by them even before the own file
system exists. The system volume's Optimal header block and LUKS2 header
sit at a fixed offset at the start of the outer device, before the volume
map, so nothing in the map is read before capping. The map, stored twice,
lists extents of 1 GiB that belong to the system volume or to one user's
home volume; it is authenticated with HMAC-SHA-256 under a key derived by
HKDF from the system volume key, and parsed only after capping and after
its tag checks. Its user count and per-user sizes are plaintext metadata
and are listed as such on the scorecard. Each home volume is a
LUKS2 volume with its own volume key and key slots, unwrapped at that
user's login (from their passphrase, plus on B1 the TPM through the
post-cap `PolicySecret` path in [boot.md](boot.md); test: a second user
logs in long after capping on B1), and holds an ext4 created by Optimal's
`mkfs`. Its extents are encrypted only by that key (the system key doesn't cover them, so
nothing is encrypted twice). Home volumes are mounted in-kernel as internal
volumes, and grow by adding extents and resizing ext4 online
(`resize_inode`); shrinking is offline. The map is still parsed as
untrusted input and fuzzed, and its format is documented so a Linux rescue
system can open each volume with a `dm-linear` table. The XTS tweak is the
sector number within each volume, so the tweak mapping must be injective
per key; that and the map's extent rules are proof target 12b (Phase 5).
The Phase 5 storage benchmarks run on a home volume and must be within 10%
of the same benchmarks on the system volume, and are compared against
Linux ext4 on dm-crypt over `dm-linear` with the same extents.

**Stage 2, own file system (default once its track is done):** encryption
moves into the file system. Every block is encrypted and authenticated, with
the authentication tag kept in the parent pointer, so the whole tree is
tamper-evident from the superblock down. There is no separate block layer
under it. Before capping the kernel parses only the own file system's
frozen plaintext header, bytes 0..512 of the volume (sealed blobs and volume
tag, same rule as the stage-1 header block); everything else is read after
capping. The pre-cap parser of both stages is fuzzed to the in-kernel
thresholds in Phase 4b. On top of that:

- **Per-user keys.** Each user's files are under their own key, wrapped by
  their login secret (and on B1 by the TPM, through the same post-cap
  `PolicySecret` path as home volumes). Another user, even an
  administrator, can't read them without that user's login.
- **Data protection classes** (modelled on iOS's, because they're what
  makes "a locked laptop gives up nothing" true while apps keep working):
  - *A, after first unlock:* app caches and data that background work needs.
    Keys stay in memory until shutdown.
  - *B, complete unless open:* the default for app data, including browser
    profiles. Every class-B file has its own random per-file key, wrapped
    by the class-B key; a file created while locked has its per-file key
    wrapped instead with a public key whose private half is present only
    while unlocked, and rewrapped under the class-B key at the next unlock.
    All of a class-B file's block subkeys derive from its per-file key, so
    no class-B key is needed to write. Files open at lock time keep working
    (their per-file keys stay until closed), so downloads and database
    writes continue. Opening an existing class-B file while locked fails
    with `EACCES`.
  - *C, only while unlocked:* the user's documents and the secret stores.
    A short time after the screen locks (default 10 seconds), dirty pages
    are written back, then clean pages are evicted from the page cache and
    zeroed, compressed-swap pages are dropped (class C never goes to swap),
    and the keys are wiped. Access while locked fails with `EACCES`.
    Plaintext an app copied into its own memory can't be evicted, so apps
    holding class-C files open are frozen from lock until unlock.
  The test: lock the machine, then scan the page cache, kernel slab,
  bounce and DMA buffers, swap (in RAM and on disk) and the key store for
  class-C keys and for plaintext canaries from class-C files; none may be
  found, and the frozen apps must not have run. Browsers downloading for an
  hour while locked must finish without errors.
- **Swap** is encrypted with a key that doesn't survive a cold boot.
- **Hibernation.** Class-C eviction (and in stage 1 the suspend steps
  above) runs before the image is written, so the image holds no class-C
  key or plaintext. The image key is a per-boot random key generated before
  capping and sealed at once into a persistent TPM object with the full
  disk policy shape (signed PCR 4/8/9 policy, PCR 7, the floor check and
  `TPM2_PolicyAuthValue` with the stretched PIN; [boot.md](boot.md), which
  lists it among the sealed objects every floor raise and PCR 7 reseal
  covers); the key stays in kernel memory until hibernation encrypts the
  image with it. Resume from hibernation is a fresh boot that unseals it
  before capping, with the PIN. It is not a post-cap object. Without a TPM
  the image key is wrapped under a key derived by HKDF from the volume key,
  which resume gets from the passphrase like any boot. Format and rollback
  rules are in [threat-model.md](threat-model.md). Test (Phase 10):
  hibernate, scan the image for class-C canaries, resume with a wrong PIN,
  resume through an old signed loader (PCR 4 differs; must fail), and try
  the unseal after capping (must fail).
- **Cipher choice is measured.** The format records a cipher ID per volume.
  Allowed: AES-256-GCM (fast with AES-NI and VAES) and XChaCha20-Poly1305
  (fast without AES instructions). The installer picks the faster one on
  that CPU by benchmark; both are on the scorecard.
- **Nonces never repeat, even after a rewind.** A crash fallback, a
  lying-drive fallback or a replayed image can rewind the volume to an older
  state, so nonces can't come from on-disk state alone. Each mount draws a
  fresh random 128-bit **prefix**; each block written in that mount takes the
  next value of a 64-bit in-memory counter, and the block's 192-bit nonce
  (prefix, counter) is stored with its tag in the parent pointer.
  XChaCha20-Poly1305 uses that nonce directly. AES-GCM, with its 96-bit
  nonce, uses a per-mount subkey HKDF(key, prefix) and the counter,
  zero-padded to 96 bits, as its nonce; the key is the per-file key for
  class-B files and the volume or class key otherwise. A rewound volume gets
  a new prefix at its next mount, so its new writes never reuse a
  (key, nonce) pair; if the counter would wrap, the file system draws a new
  prefix (and subkey) without unmounting. Nonce uniqueness, including across crash fallback, lying-drive
  fallback and replay, is proof target 12c, gated by the file system
  track's done-when, with the named axiom "RNG output uniform; salt
  collision <= 2^-64 per volume lifetime", reported as "B, probabilistic".
- **Whole-volume replay.** Tags in parent pointers stop tampering, but an
  attacker could write back a complete older image of the volume. On B1
  machines the superblock carries a replay value bound to a TPM NV counter
  (an ordinary counter, not `TPMA_NV_ORDERLY`, whose jump after a power
  cut would refuse the genuine volume). The counter is bumped only at mount
  and at resume, never per commit, to spare NV write endurance. A bump with
  the counter at *c* commits *c*+1 in two consecutive superblock
  generations (all four superblock slots), flushes, and only then
  increments the counter, so a torn or lost newest superblock still falls
  back to a generation carrying *c*+1. Mount accepts a superblock whose value is
  **at least** the counter's, so a crash between the two steps is harmless,
  and refuses a lower one: any image captured while the machine was off or
  suspended is refused once the next mount or resume has bumped the
  counter. The counter's increment is authorised only through the kernel
  TPM service's post-cap `PolicySecret` path ([boot.md](boot.md)), not by
  owner or platform auth, so root can't advance it to lock the user out, and
  S is released only after the volume tag matches, so a substituted volume
  can't bump it. A
  drive that lost the flushed superblock looks like a replay; the user can
  accept the older state with the recovery key. On machines without a TPM,
  replay is **not detected**, and the scorecard says so. Tests (file system
  track): restore an old image and mount it (refused); a power cut between
  the superblock flush and the increment, and after the increment, must both
  mount; tearing the newest superblock right after a bump must mount with
  no prompt.
- **What the own file system's design must specify** (`fs/docs/DESIGN.md`,
  with a table mapping each requirement here to the section that meets it):
  the key hierarchy (volume, system, per-user, per-class and class-B
  per-file keys, so no single key reads every user's names and sizes), a
  TPM key-slot kind, file-name encryption, the exact list of metadata left
  in plaintext (for example sizes rounded to blocks, timestamps), the
  discard (TRIM) policy and what it reveals, the nonce scheme above with
  its replay-value superblock field, the frozen plaintext header bytes
  0..512, free-block quarantine for the lying-drive window below, a
  per-file `fsync` intent log replayed at mount (so `fsync` of one file
  doesn't commit the whole volume; benchmarked as `fsync` latency under
  background writeback against ext4), the error policy of the block layer
  above, and its own proof targets (copy-on-write never overwrites a live
  block; commits are atomic).

## Reading and writing other systems' disks (ext4)

ext4 has no formal specification, only documentation of what Linux does, and
dozens of feature flags. Writing to it wrongly corrupts someone's Linux
install. So:

- **Optimal's own ext4 volumes are read-write from Phase 4a.** Optimal ships
  its own clean-room `mkfs` and creates volumes with a fixed feature list,
  the ext4 default of e2fsprogs 1.47.2:
  - COMPAT: `has_journal` (ordered data mode), `ext_attr`, `resize_inode`,
    `dir_index`, `orphan_file`;
  - INCOMPAT: `filetype`, `extent`, `64bit`, `flex_bg`,
    `metadata_csum_seed`;
  - RO_COMPAT: `sparse_super`, `large_file`, `huge_file`, `dir_nlink`,
    `extra_isize`, `metadata_csum`, and `orphan_present` (set while the
    orphan file has entries).

  Features not on the list (among them `inline_data`, `encrypt`,
  `casefold`, `verity`, `fast_commit`, `ea_inode`, `large_dir`, `meta_bg`,
  `mmp`, `quota`, `project`, `sparse_super2`) aren't written. The Linux side
  of every test makes volumes with exactly
  `mke2fs -t ext4 -O none,has_journal,ext_attr,resize_inode,dir_index,orphan_file,filetype,extent,64bit,flex_bg,metadata_csum_seed,sparse_super,large_file,huge_file,dir_nlink,extra_isize,metadata_csum`,
  using e2fsprogs 1.47.2 pinned by source hash in the tree.
- **The differential gate is a Phase 4a done-when item:** random operations
  on Optimal, then Linux's `e2fsck -fn` is clean and Linux reads back the
  same tree; and the reverse (Linux writes with only listed features,
  Optimal checks and reads). The crash oracle's checker for ext4 (Phase 4a
  on) runs on each crash state twice, because almost every state has
  `needs_recovery` set and `e2fsck -n` doesn't replay the journal: (a)
  Optimal mounts the state (replaying the journal), unmounts, then
  `e2fsck -fn` must be clean; (b) on a copy, Linux replays the journal (a
  Linux mount in a VM) and the resulting tree must equal (a)'s. Both run as
  external tools.
- **Other ext4 volumes** (a dual-boot Linux install) are mounted read-only
  through the tier-3 file system server (below), like all foreign media.
  If the journal needs recovery it is replayed into an in-memory overlay;
  the disk is never written. Read-write on volumes Optimal didn't create
  comes only after the differential gate has also passed on images made by
  current Linux with the listed features.
- **Refuse, don't guess.** Any mount is refused (with a clear message) if the
  volume has an unknown INCOMPAT feature; read-write is refused if it has an
  unknown RO_COMPAT feature, `needs_recovery` is set by another system, MMP
  is held, or another OS's hibernation image is present.
- **Foreign and removable media are parsed outside the kernel.** USB sticks,
  SD cards, image files and any volume Optimal did not format are mounted
  through a tier-3 file system server ([drivers.md](drivers.md)), because a
  crafted image is a classic physical attack. Only the system's own internal
  volumes are mounted in-kernel, with the hardening above.
- GPL tools such as `e2fsck` are used only as external test oracles: never
  linked, vendored or shipped.

## Crash-consistency testing (what "zero corruption" means)

- **Crash-state model.** The block request stream is recorded below the file
  system. A crash state is: every write that completed before the last
  completed flush was submitted, plus every FUA write that completed (those
  are mandatory), plus any subset of the other writes, in any order, with
  any subset of those unflushed writes torn at 512-byte granularity (a power
  cut can tear every write in flight). **Second-order states** crash again during
  recovery or journal replay. States are enumerated exhaustively for small
  workloads and sampled randomly for long ones.
- **A lying-drive model** injects a drop point: flushes after it are
  ignored, and "really persisted" means the state at the drop point plus
  any subset of later writes. Data loss is then unavoidable. The own file
  system quarantines freed blocks for **K = 8 commits or 30 seconds,
  whichever is longer**, so no block of the last K commits' trees is
  reused; within that window it must still mount at its last commit before
  the drop point, losing only commits after it. Tests drop flushes 1..K
  commits back. A drop more than K commits back is outside the promise:
  the volume is reported unrecoverable and opened with the read-only rescue
  mount (below). ext4 overwrites in place and has no such fallback, so for
  ext4 the only promise is that the volume is clean after `e2fsck -fy`.
- **Oracle.** After every crash state: the file system's own checker reports
  clean; every byte that was fsynced (by the table above) is present; every
  rename is atomic; and **no stale or foreign data** is visible (a file never
  shows blocks that belonged to another file or to free space, which would
  be a confidentiality bug); and **data that was durable before is
  unchanged** by later, unrelated writes.
- **Volume.** The nightly run reports the number of distinct crash states
  checked; the target is at least 10^7 per night. States are deduplicated
  by a hash of the block versions they contain, so identical disk images
  count once. The runner is sized to fit 10^7 states in 4 hours at the
  measured checking rate (states are copy-on-write overlays on one base
  image, checked in parallel, one per core); if the rate drops, runners are
  added, the target stays.
- **Real power cuts.** Before each release: a relay-controlled power-cut rig,
  at least 1,000 cuts on at least 3 drive models (Phase 4a included), with
  the same oracle. The first run is on ext4 over LUKS2, before the first release with read-write
  ext4 (Phase 4a); the own file system gets its own before it becomes the
  default. This needs physical hardware and is listed as a hardware need in
  the roadmap.

## Benchmarks (so "within 10%" and "beats ext4" mean something)

All storage comparisons run with encryption on for both systems (Linux with
dm-crypt AES-XTS in stage 1, with `no_read_workqueue` and `no_write_workqueue`
set and recorded; Linux ext4 with dm-crypt versus the own file system in stage
2), ext4 at its default mount options (recorded), on the same drive, with
HWP/CPPC configured on both kernels as [benchmarks.md](benchmarks.md)
requires for every Linux comparison (Phase 4a included).

- **Aging without wearing out drives.** Copy-on-write designs look good when
  new and fragment later, so benchmarks run on aged file systems. Each file
  system is aged once (at least 10 times its size written by an aging
  workload) on a fixed 64 GiB partition and saved as an image; each run
  restores the image at block level, so drives aren't worn out by repeated
  aging.
- **Drive state is controlled.** Before each run the drive is preconditioned
  to steady state (SNIA PTS method), with TRIM state and drive temperature
  recorded. Storage tail metrics have their own noise ceiling
  ([benchmarks.md](benchmarks.md)).
- **The benchmark set is one table in [benchmarks.md](benchmarks.md)**, not
  repeated here. Phase 4a has static programs only (no threads or io_uring
  yet): QD1 is fio's `psync` engine, and QD32 is 32 forked static `psync`
  processes, run the same way on Linux; SQLite in WAL mode and untar are
  static binaries. An in-kernel block benchmark at QD32 is tracked against
  Optimal's own history only, never compared to Linux.

## Tooling shipped with format version 1

Checker (`fsck`), online scrub, metadata stored twice on single-disk volumes,
a read-only rescue mount that skips damaged subtrees, and an export tool that
copies everything still readable to another disk. The checker is fuzzed with
corrupted images: it must never crash, and every image must end either
repaired or refused with a reason. Format versioning follows the rules in
`fs/docs/FORMAT.md`: compatible, read-only-compatible and incompatible
feature bits; the format is frozen before any user data; a new incompatible
feature is turned on only when **both** A/B slots and the recovery image run
a release that supports it (so in practice two consecutive releases after it
is introduced), and only after the newer one has been committed (3 healthy
boots). A rollback can then always read the volume. Test: enable a feature,
roll back, mount.
