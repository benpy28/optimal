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
  (NVMe VWC, ATA IDENTIFY), power-loss protection, atomic write unit, and
  discard behaviour. A quirk list (in the tree, with the evidence for each
  entry) covers drives known to ignore flushes; on those, the file system
  disables the volatile cache if the drive allows it and warns if not.
- **Encryption passes flags through.** Whatever encrypts (see below) keeps
  `PREFLUSH`/`FUA` and completion ordering intact; a test checks this by
  recording the request stream below the encryption layer.
- **Errors are never dropped.** A failed write marks the affected pages and
  records the error against the file (below). Retries follow the device's
  own error class; a drive that keeps failing is set read-only, not hidden.

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

**Writeback errors.** Each file keeps an error sequence counter (the same idea
as Linux's errseq). When writeback fails, the counter moves on. Every open
file description sees the error exactly once, on its next `fsync`, `fdatasync`
or `close`, even if another process already saw it and even if the pages were
since cleaned. Pages whose writeback failed stay dirty and are retried; if
the device is gone, they are kept until the file system is unmounted, and the
error stays reported. (This is the PostgreSQL "fsyncgate" case: an error must
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
  errors, plus a proof target (block allocator never double-allocates,
  [verification.md](verification.md) target 12).

## Encryption layering

There are two stages, and the scorecard always says which one a machine is in.

**Stage 1, Phase 4 (before the own file system is the default):** a block
encryption layer under ext4 using AES-256-XTS with the CPU's AES
instructions. XTS gives **confidentiality only**: it hides data on a powered
off disk but doesn't detect an attacker who changes ciphertext (they can't
choose the plaintext, but they can scramble or replay sectors). This is
written in the threat model as "protects a lost or stolen device, does not
stop offline tampering", and the scorecard shows it that way. The sector
number is the XTS tweak, so the tweak mapping must be injective (target 12).

**Stage 2, own file system (default once its track is done):** encryption
moves into the file system. Every block is encrypted and authenticated, with
the authentication tag kept in the parent pointer, so the whole tree is
tamper-evident from the superblock down. There is no separate block layer
under it. On top of that:

- **Per-user keys.** Each user's files are under their own key, wrapped by
  their login secret (and the TPM on S1 machines). Another user, even an
  administrator, can't read them without that user's login.
- **Data protection classes.** Files are in a class: *available after first
  unlock* (most app data, so background sync works) or *only while
  unlocked* (the user's documents, keys, browser profile by default). The
  keys for the second class are wiped from memory a short time after the
  screen locks and come back on unlock.
- **Swap and hibernation** are encrypted with keys that don't survive a cold
  boot (swap) or are sealed as described in [threat-model.md](threat-model.md)
  (hibernation).
- **Cipher choice is measured.** The format records a cipher ID per volume.
  Both AES-256-GCM-style (fast with AES-NI/VAES) and XChaCha20-Poly1305
  (fast without AES instructions, on old CPUs) are allowed; the installer
  picks the faster one on that CPU by benchmark. Both are compared in the
  scorecard.

## Reading and writing other systems' disks (ext4)

ext4 has no formal specification, only documentation of what Linux does, and
dozens of feature flags. Writing to it wrongly corrupts someone's Linux
install. So:

- **Phase 4 mounts ext4 read-only.** If the journal needs recovery, it is
  replayed into an in-memory overlay; the disk is never written.
- **Refuse, don't guess.** Any mount is refused (with a clear message) if the
  volume has an unknown INCOMPAT feature; read-write is refused if it has an
  unknown RO_COMPAT feature, `needs_recovery` is set, multi-mount protection
  is held, or a hibernation image of another OS is present.
- **Read-write only on a published feature list**, and at first only on
  volumes Optimal created, until a differential gate passes: random
  operations on Optimal, then Linux's `e2fsck -fn` is clean and Linux reads
  back the same tree; and the reverse (Linux writes, Optimal checks). Only
  then are other people's volumes writable, and only with listed features.
- **Foreign and removable media are parsed outside the kernel.** USB sticks,
  SD cards and any volume Optimal did not format are mounted through a
  tier-3 file system server ([drivers.md](drivers.md)), because a crafted
  image is a classic physical attack. Only volumes Optimal formatted (and
  can authenticate) are mounted in-kernel.
- GPL tools such as `e2fsck` are used only as external test oracles: never
  linked, vendored or shipped.

## Crash-consistency testing (what "zero corruption" means)

- **Crash-state model.** The block request stream is recorded below the file
  system. A crash state is: everything completed before the last flush, plus
  any subset of the unflushed writes after it, in any order, with any single
  write torn at 512-byte granularity. States are enumerated exhaustively for
  small workloads and sampled randomly for long ones.
- **Oracle.** After every crash state: the file system's own checker reports
  clean; every byte that was fsynced (by the table above) is present; every
  rename is atomic; and **no stale or foreign data** is visible (a file never
  shows blocks that belonged to another file or to free space, which would
  be a confidentiality bug).
- **Volume.** The nightly run reports the number of crash states checked; the
  target is at least 10^7 per night. "7 days" in the roadmap is replaced by
  this count, since wall-clock time isn't a coverage measure.
- **Real power cuts.** Before each release: a relay-controlled power-cut rig,
  at least 1,000 cuts on at least 3 drive models, with the same oracle. This
  needs physical hardware and is listed as a hardware need in the roadmap.

## Benchmarks (so "within 10%" and "beats ext4" mean something)

All storage comparisons run with encryption on for both systems (dm-crypt
AES-XTS for Linux in stage 1; Linux ext4 with dm-crypt versus the own file
system in stage 2), ext4 at its default mount options, on the same drive,
and on an **aged** file system (at least 10 times its capacity written by an
aging workload first), because copy-on-write designs look good when new and
fragment later. The set: fio random read 4k QD1 and QD32, random overwrite 4k
QD32, sequential write 1M QD8, fsync latency P50/P99/P99.9, SQLite in WAL mode,
pgbench, a git checkout and `git status` on a large repo, untar of a source
tree, and metadata microbenchmarks (create, stat, unlink of 1 million small
files). See [benchmarks.md](benchmarks.md).

## Tooling shipped with format version 1

Checker (`fsck`), online scrub, metadata stored twice on single-disk volumes,
a read-only rescue mount that skips damaged subtrees, and an export tool that
copies everything still readable to another disk. The checker is fuzzed with
corrupted images: it must never crash, and every image must end either
repaired or refused with a reason. Format versioning follows the rules in
`fs/docs/FORMAT.md`: compatible, read-only-compatible and incompatible
feature bits; the format is frozen before any user data; a new incompatible
feature is turned on only after an update has been committed (3 healthy
boots), so the A/B rollback slot can always read the current format.
