# OptimalFS design

OptimalFS is Optimal's own filesystem: copy-on-write, every block verified,
optional authenticated encryption, atomic commits and instant snapshots.
It is a clean-room design. No code or format was taken from ZFS, btrfs,
bcachefs, APFS or any other filesystem; ideas such as copy-on-write trees
and birth times are long-standing public techniques, and everything here
was written from first principles. The byte-level format is in
[FORMAT.md](FORMAT.md).

The crate is `no_std` + `alloc` and talks to storage only through the
`BlockDevice` trait, so it runs on the host today (tests, tools) and links
into the kernel unchanged in Phase 4.

## Goals, in order

1. **Never lose or corrupt committed data,** whatever the power or the disk does.
2. **Never return wrong data.** Every byte read is verified; damage is reported, not passed on.
3. **Nobody reads your data without the key,** remotely or with the disk in hand.
4. **Fast** on NVMe: large sequential writes, few flushes, no double writes.
5. **Instant snapshots and rollback,** for user undo and safe system changes.

## Crash consistency

The device contract (`BlockDevice`): writes may sit in a volatile cache and
can be lost, reordered or torn until a flush completes. The torn-write
model is the storage design's: after a power cut, every write completed
before the last completed flush is durable, and **any subset** of the
writes since then may have landed, in any order, each one possibly torn at
512-byte granularity into a mix of its old and new sectors. NVMe, SATA and
SCSI promise exactly this and no more. Drives that lie about flushes are a
separate, weaker model (below).

Nothing is ever overwritten in place. A commit:

1. writes all new data and metadata to free blocks (merged into large requests);
2. flushes;
3. writes the new superblock to two slots at opposite ends of the disk;
4. flushes.

Until step 3 lands, every older superblock still points at an intact tree,
so a power cut at any moment leaves the volume at the old commit or the new
one. That is the only guarantee the design needs from the disk.

The subtle part is reuse. Blocks freed while building commit T+1 are still
referenced by T (and T is what we fall back to if T+1's superblock is torn).
So freed blocks are *quarantined*: cleared in the bitmap (the bitmap must
describe exactly what T+1 references), but not handed out until K = 8
later commits have completed. The free log records each commit's
quarantine, tagged with its generation, so it survives a remount. So the
last 8 commits always stay fully intact on disk.

**Lying drives.** Some drives acknowledge a flush and keep the data in a
volatile cache anyway, so a power cut loses commits that "completed". The
storage design's promise is: if at most K = 8 commits were lost, mount
lands on the last commit before the drop point. Three pieces make that
true. Quarantine keeps those commits' blocks intact. Every superblock body
carries the root sets of the previous 8 commits, newest first, so even if
every later superblock write landed and nothing else did, the last honest
commit is still listed. And after an unclean shutdown, mount reads back
every block a candidate references that was born inside its quarantine
window (the *window generation* in the superblock), data included, and
falls back to an older candidate if anything is missing. Copy-on-write
makes that check cheap: everything a recent commit changed sits under
pointers with recent births, so older subtrees are skipped unread. A clean
unmount sets a flag that skips the check. After a fallback, new commits are
numbered above every copy on disk and the first is written to all four
slots. Losses more than 8 commits back are outside the promise and go to
the rescue mount. Full commits are rate-limited by the VFS (fsync bursts go
to the intent log), so 8 commits span real time.

Two flushes per commit, no journal, no double writes of data.

## Integrity and encryption

Every pointer carries the child's 16-byte MAC, so the volume is a hash tree
rooted in the superblock. The MAC binds the block to its kind, address,
birth generation and the volume UUID, which catches:

- bit rot and torn blocks;
- misdirected writes (right data, wrong place) and lost writes (stale but
  internally valid blocks), because the parent expects a specific version;
- blocks from another volume.

Plain volumes use keyed BLAKE3. Encrypted volumes use XChaCha20-Poly1305
on every block, data and metadata alike, so the tag doubles as the MAC.
This is stronger than a separate disk-encryption layer (AES-XTS): XTS has
no integrity, so an attacker with the disk can flip ciphertext bits or roll
individual blocks back unnoticed. Here, any change to any block is detected.

Keys: a random 256-bit master key, wrapped in up to four key slots, each
under Argon2id(passphrase). The cost is sized to the machine:
min(1 GiB, RAM / 4) of memory, never below 64 MiB, with passes raised to
keep about 2 s and never below 4 (`KdfParams::for_ram`). Format refuses
weaker settings. A TPM-sealed slot kind is reserved (kind 2) and exempt
from the Argon2id floor; per-user and per-class keys come with stage 2. Subkeys are derived with
BLAKE3's key derivation. Nonces are a 128-bit random prefix chosen at each
mount plus a counter, so a nonce never repeats, even after a crash replays
generation numbers. Superblock bodies are encrypted too; only the header
(geometry, KDF parameters, wrapped keys) is in the clear.

What an attacker with the disk can still see: the volume size, which blocks
are in use and when they were written, and the KDF parameters. They cannot
see names, sizes, contents, directory structure or counts.

Whole-image replay can't be detected by the disk alone. The superblock
carries a replay value for the kernel to bind to a TPM counter.
`bump_replay` raises it in a commit written to all four slots in two
waves (slots 0 and 1, flush, then slots 2 and 3, flush) and only then may the caller advance the TPM counter. A power cut
at any write point of the bump leaves the old or the new value, never a
rolled-back one after the bump returned.

## Data structures

- **One B+tree for all file metadata**: inodes, directory entries (keyed by
  a keyed hash of the name), extents and symlink targets, keyed by
  `(object, kind, offset)`, so a file's items sit together on disk.
- **Extents** of up to 64 blocks (256 KiB) with one MAC per block, so a
  partial overwrite only rewrites the blocks it touches.
- **Space map**: a copy-on-write radix of bitmaps, 1 to 5 levels deep by
  device size (up to 128 PiB), with free counts in the pointers, so
  allocation skips full regions without I/O. Never-written regions are null
  pointers meaning "all free", so format is O(1) in the device size and
  tables load lazily. Allocation is next-fit for large contiguous runs.
- **Delayed allocation**: file data and tree nodes get disk addresses only
  when written, which keeps files and tree nodes contiguous.

## Snapshots

A snapshot is just a saved tree root: O(1), and it shares every block.
Space accounting uses birth generations and deadlists (details in
`src/snapshot.rs`). Deleting a snapshot costs time proportional to what
changed since, not to the volume size. fsck checks the exact accounting
invariant `blocks(previous) \ blocks(x) = deadlist(x)` for every snapshot.

The OS image itself is a separate read-only, verified A/B image (see the
storage design), so filesystem snapshots are a user feature: undo, backups,
and safe changes to user data.

## Errors

Nothing panics on bad disk data. Damage surfaces as `Error::Corrupt`.

The storage design's error policy is in the block layer: retryable device
errors are retried 3 times (the last after a controller reset), a device
goes read-only after 16 failed writes in 10 minutes or when it says so, and
writeback errors are reported once per open file through errseq. No file
system stops all writes on a single error. In this crate:

- Reads never poison anything. A read that fails returns the error and the
  volume carries on.
- A failed write *inside a mutating operation or a commit* means the
  in-memory state no longer matches any commit, so today the volume
  refuses further changes until remount, and the disk still holds the last
  good commit. This is open against the storage design: the fix (next on
  the list) is to rebuild the failed commit from the dirty in-memory state
  with fresh allocations, so a permanently bad block is reallocated, not
  retried forever, and nothing dirty is lost.
- The VFS (Phase 4) owns errseq, page retention while a device is gone,
  and the read-only switch.

## Testing

The filesystem is the piece where a bug loses people's files, so it gets
the heaviest testing in the project:

- **Model-based tests**: long random workloads (create, write, truncate,
  rename, links, snapshots, rollback, remount, ...) run against the real
  filesystem and a simple in-memory reference model. Every result is
  compared, full state is compared regularly, and fsck runs throughout.
- **Lying drives**: flushes are acknowledged but lost for 1 to 8 commits,
  then power is cut. Images are steered into the worst cases (every later
  superblock write landed; or only the newest pair landed and the other pair
  kept its old content). Mount must land on the last honest commit, equal it
  exactly, pass fsck, keep working, and survive a second lying round.
- **Huge volumes** on a sparse device: 16 TiB, 128 PiB and ragged sizes,
  with data placed in the far regions.
- **Power cuts at every write point**: a workload is replayed and power is
  cut at each write and flush in turn. The simulated disk resolves its
  cache adversarially (any version of each block, or a sector-by-sector
  mix). The recovered volume must mount, equal exactly one committed state,
  pass fsck (including "no stale data past EOF"), and keep working.
- **Damaged images**: bit flips, zeroed and random blocks, misdirected and
  stale blocks. No panic, and no read may ever return wrong data.
- **Decoder fuzzing**: random and mutated bytes into every on-disk decoder.
- **Hand-worked cases**: small examples with the expected numbers worked
  out in the comments (space usage, slot placement, nonce arithmetic).
- **Mutation checks**: deliberate bugs (wrong snapshot rule, missing flush,
  no quarantine, stale tail, ...) must each be caught by the suite.
- **Nightly soak** in CI runs all of this with a much larger scale and new
  seeds every night. A failing seed is printed so it can be replayed with
  `OFS_SEED=<seed>`.

## Decision log

- Copy-on-write tree, not a journal: atomic commits and snapshots fall out
  of it, and data is written once. Rejected: ext4-style journaling (double
  writes, no snapshots, no data checksums).
- Checksum in the parent pointer, not in the block: catches lost and
  misdirected writes, which an in-block checksum can't.
- XChaCha20-Poly1305 first: fast everywhere in software and its large nonce
  makes random per-mount prefixes safe. An AES-GCM-style suite (faster with
  AES-NI/VAES) gets its own suite id later, picked by benchmark.
- Four superblock slots at both ends of the disk: survives a damaged region
  at either end and a torn superblock write.
- K = 8 commit quarantine for freed blocks, plus an 8-deep root history in
  every superblock body: survives drives that drop up to 8 commits of
  flushes. Rejected: a separate root ring (the history is already stored in
  every slot); a time-based hold (unbounded space cost).
- Header bytes identical in every slot and commit, with slot and generation
  in a trailer: the pre-key reader and volume tag can rely on fixed bytes.
- Birth-time deadlists rather than per-block reference counts: snapshot
  creation is O(1) and the live write path never updates a refcount tree.
- Inode numbers are never reused (64-bit), so stale handles can't alias.
- Keyed name hashes: no hash-flooding, and the hash order leaks nothing.

## Compliance with the storage design

How each requirement `design/storage.md` places on this file system is met.

| Requirement | Where | State |
|---|---|---|
| Copy-on-write, never overwrite a live block | Crash consistency | done (proof target pending Kani) |
| Atomic commits | Crash consistency | done, power-cut tested at every write point |
| Torn-write model: any subset of unflushed writes, torn per 512 bytes | Crash consistency; `sim::CrashDevice` | done |
| Lying drives: fallback within K = 8 commits, rescue beyond | Lying drives; FORMAT "Lying drives" | done (rescue mount open) |
| Frozen plaintext header bytes 0..512 | FORMAT "Superblock" | done |
| Nonce scheme: 128-bit per-mount prefix + 64-bit counter | Integrity and encryption | done |
| Replay value in the superblock, bump across all slots | Integrity and encryption | done (TPM binding: stage 2) |
| KDF floor min(1 GiB, RAM/4), >= 64 MiB, >= 4 passes | Integrity and encryption | done |
| TPM key-slot kind | FORMAT "Superblock" (kind 2 reserved) | reserved, stage 2 |
| Key hierarchy: per-user, per-class, class-B per-file keys | | open, stage 2 |
| File-name encryption | names live only in encrypted tree nodes | done |
| Plaintext metadata list | Integrity and encryption ("what an attacker can see") | done |
| Discard (TRIM) policy | | open (no discard issued yet) |
| Per-file fsync intent log | FORMAT body (root reserved) | open |
| Block-layer error policy (retry, errseq, read-only) | Errors | partly: commit rebuild after a write failure open |
| Feature bits: compat, ro-compat, incompat | FORMAT "Feature bits" | done |
| fsck, scrub, duplicated metadata, rescue mount, export tool | Testing; Not done yet | fsck done; others open |
| Suspend and hibernation key handling, stage 1 to 2 migration | | open, kernel integration |

## Not done yet (tracked, in order)

1. Duplicated metadata on single-disk volumes: pointers keep a logical
   address, and a chunk table in the superblock maps each metadata chunk to
   two physical copies (both byte-identical, since the MAC covers the
   logical address). The chunk-table format is frozen in v1 even if the
   second copy ships later.
2. Rebuilding a commit after a write failure (reallocate, keep dirty state).
3. The per-file fsync intent log, with an fsync-under-writeback benchmark.
4. Benchmarks against ext4 (fio-style workloads on the same device).
5. Kani proofs for the space map and tree invariants.
6. Inline data for small files and an AES-GCM-style suite.
7. Concurrency (the API is `&mut self` today; the kernel will wrap it first).
8. A metadata reserve sized for real disks (2%, 256 MiB to 8 GiB), online
   scrub, a rescue mount and an export tool.
9. Orphan handling for files that are unlinked while still open (needs the VFS).
10. Kernel VFS integration (Phase 4), including the key hierarchy, TRIM,
    suspend and hibernation rules.
