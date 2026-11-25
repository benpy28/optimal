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
into the kernel unchanged in the file system track, after Phase 4a.

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

## Duplicated metadata

Every metadata block (tree nodes, space-map blocks, free-log blocks, the
root block) is stored twice, on single-disk volumes too, so one bad sector
or a small damaged region never loses a directory or the volume. File data
is stored once (users who want two copies of data use two devices).

**Logical addresses.** Pointers stay 64 bytes and hold a *logical* address
for metadata. Metadata is allocated in chunks of up to 64 blocks
(`auto_chunk_blocks`: the device size / 64, clamped to 4..64 and rounded
down to a power of two; set at format). A chunk is backed by two physical
runs, normally half the device apart. The MAC binds the logical address,
so nothing above the chunk table knows a physical metadata address, and a
chunk can be moved by copying it and changing one entry.

**The chunk table** maps chunk ids to `(copy 0 run, copy 1 run, used
slots, length)`. It is a copy-on-write radix tree (leaves of 128 entries,
tables of 32 children with free and mapped counts, depth grown as needed)
whose blocks are themselves stored twice. Its root is in every root set:
the superblock core of this commit and of each of the 8 commits in the
history. So a fallback to an older commit resolves that commit's logical
addresses through the table that commit wrote. Freed metadata blocks and
chunk runs go through the same K = 8 commit quarantine as everything else,
and a chunk is released only when it is empty and none of its slots are
still quarantined.

**The two copies differ.** Copy 1 is copy 0 passed through a second,
keyed, per-block map (a second encryption pass under a per-copy subkey on
encrypted volumes; a keyed BLAKE3 mask on plain ones), so a drive that
deduplicates or compresses cannot collapse them into one physical block.
Both are checked against the one MAC in the parent: a copy is good only if
it turns back into exactly what the parent committed. Nothing from copy 1
is used before that check. On plain volumes the mask key is public (as is
the checksum key), so it only stops deduplication.

**Reads and repair.** A normal read uses copy 0 and falls back to copy 1.
After an unclean shutdown, recovery reads both copies of every recent
block, and a scrub (`scrub_metadata`) reads both copies of everything. A
bad copy is repaired at the next commit by *moving* it: its chunk run is
rebuilt at a fresh place from the good blocks, the chunk entry is changed,
and the old run is quarantined. A bad chunk-table block is rewritten to a
new pair. Nothing live is ever overwritten in place, so a torn or
misdirected repair write cannot damage a committed block, and a failing
region is left behind rather than written again. A moved block keeps its
old birth under unchanged parents, so the birth-pruned recovery walk would
never reach it: each commit records the last generation that moved a run
of live blocks, and if that falls inside the window, recovery reads all
metadata (data blocks are still checked by birth). Moves are rare (repairs
and relocation), so the usual unclean mount stays cheap. Read-only mounts count
bad copies but never write.

**Cost.** Metadata writes double; data writes don't. A fresh 4096-block
volume uses 134 blocks (4 superblock slots, 2 x 64 for chunk 0, 2 for the
chunk table root) where a single-copy design would use 8; on real volumes
the extra is about the size of the metadata, which is a small fraction of
the data.

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
- The VFS (the file system track, after 4a) owns errseq, page retention while a device is gone,
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
- **Duplicated metadata**: a flipped bit in either copy (found, reported,
  repaired by moving), one bad copy of every metadata and chunk-table block
  at once (nothing lost, all found by a scrub), both copies bad (never wrong
  data), every copy pair differs on disk, chunk moves keep reading back and
  keep the old runs quarantined, fallback through each commit's own chunk
  table, the chunk table against a reference model, and decoder fuzzing.
  The lying-drive suite moves chunks inside the window and prints the
  fallback-depth distribution it saw (K must stay at least twice the
  deepest).
- **Mutation checks**: deliberate bugs (wrong snapshot rule, missing flush,
  no quarantine, stale tail, ...) must each be caught by the suite. For
  duplicated metadata: no quarantine for moved runs or freed logical
  blocks, fallback through the newest chunk table, identical copies, copy 1
  accepted without the MAC check, repairs never done, a repair of the wrong
  copy, stale chunk entries after a move or repair, reads that ignore the
  write batch, copy 1 in the same half, and releasing a chunk with
  quarantined slots.
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
- Duplicated metadata through a logical address and a copy-on-write chunk
  table (design review rounds 5 and 6). Pointers stay 64 bytes. Rejected:
  pointers with two physical addresses (wider pointers, and every move
  rewrites parents); a non-copy-on-write chunk table (a fallback would
  resolve through the wrong mapping).
- Copy 1 differs from copy 0 by a keyed per-block map, checked through the
  parent's MAC. Rejected: a separate tag for copy 1 in a block trailer (it
  costs space in every metadata block, and proves nothing the parent's MAC
  doesn't); byte-identical copies (a deduplicating drive collapses them).
- The copy 1 key is derived once per volume from the block key; its nonce
  is the block's own, unique per write across mounts. Rejected: a copy 1
  key per mount (adds a key derivation per read for no gain, since the
  nonce is already unique).
- Repair moves the bad copy (round 7). Rejected: rewriting in place (it
  overwrites a live block, and writes again to a region that just failed).
- One KDF for every internal subkey: BLAKE3 `derive_key`.

## Compliance with the storage design

Every requirement `design/storage.md` places on this file system, and the
section that meets it. This table must be complete (no "open" rows) before
the file system track is done.

| Requirement (storage.md) | Where it is met | State |
|---|---|---|
| Copy-on-write never overwrites a live block (proof target) | Crash consistency; Duplicated metadata (repair moves) | done; Kani proof open |
| Commits are atomic (proof target) | Crash consistency | done, power-cut tested at every write point; Kani proof open |
| Torn-write model: any subset of unflushed writes, torn per 512 bytes | Crash consistency; `sim::CrashDevice` | done |
| Lying drives: K = max(8, 2 x deepest recorded fallback), fallback within K, rescue beyond | Crash consistency ("Lying drives"); FORMAT "Lying drives" | done; distribution printed by the lying suite; rig data needs hardware; rescue mount open |
| Root history: previous K commits' full root sets, each with the chunk table root | FORMAT "Body" (core) | done |
| Free-block and chunk-free quarantine for K commits | Crash consistency; Duplicated metadata | done |
| Lying-drive test with a metadata-chunk move inside the window | Testing ("Duplicated metadata") | done |
| Fixed plaintext header area, generation and slot in the body | FORMAT "Superblock" | done |
| Stage-1 header-block format and stage magic | | open, stage 2 migration |
| Nonce scheme: random per-mount prefix + counter | Integrity and encryption | done |
| Replay value in the superblock, bump across all slots in two waves | Integrity and encryption | done (TPM binding with the kernel) |
| KDF floor min(1 GiB, RAM / 4), >= 64 MiB, >= 4 passes | Integrity and encryption | done |
| TPM key-slot kind | FORMAT "Superblock" (kind 2 reserved) | reserved, stage 2 |
| Key hierarchy: volume, system, per-user, per-class, class-B per-file keys | | open, stage 2 |
| File-name encryption | names live only in encrypted tree nodes | done |
| Exact list of plaintext metadata | Integrity and encryption ("what an attacker can see") | done |
| Discard (TRIM) policy and what it reveals | | open (no discard issued yet) |
| Rate limit on full commits | VFS (Crash consistency notes it) | with the VFS |
| Hibernation region, stage 1 to 2 migration | | open, kernel integration |
| Optional features: vault region (F3), travel class key (F5), remove a file from every snapshot (F12) | | open |
| Per-file fsync intent log, benchmarked under background writeback | FORMAT body (root reserved) | open (next but one) |
| Error policy: failed write retried at a new block, at most 3 writeback cycles | Errors | partly: commit rebuild after a write failure is next |
| Metadata stored twice: 64-byte pointers with logical addresses, paired chunks, copy-on-write chunk table in every root set, MAC binds the logical address | Duplicated metadata; FORMAT "Duplicated metadata" | done |
| The two copies differ; no (copy 1 key, nonce) pair repeats | Duplicated metadata; FORMAT "The second copy" | done (copy 1 checked through the parent's MAC rather than a trailer tag, as agreed in design review) |
| Online scrub | Duplicated metadata (`scrub_metadata`) | done for metadata on an idle volume; online (concurrent) scrub open |
| Checker (fsck), fuzzed with corrupted images | Testing | done |
| Read-only rescue mount, export tool | | open |
| Feature bits: compat, ro-compat, incompat; frozen before user data | FORMAT "Feature bits and versioning" | done |
| Zero on free for page-cache frames of encrypted volumes | page cache interface (kernel) | with the VFS: noted for the page cache interface |

## Not done yet (tracked, in order)

1. Rebuilding a commit after a write failure (reallocate, keep dirty state).
2. The per-file fsync intent log, with an fsync-under-writeback benchmark.
3. Benchmarks against ext4 (fio-style workloads on the same device),
   including a scrub that reads copy 1 first.
4. Kani proofs for the space map, chunk table and tree invariants.
5. Inline data for small files and an AES-GCM-style suite.
6. Concurrency (the API is `&mut self` today; the kernel will wrap it first).
7. A metadata reserve sized for real disks (2%, 256 MiB to 8 GiB), online
   scrub, a rescue mount and an export tool.
8. Orphan handling for files that are unlinked while still open (needs the VFS).
9. Kernel VFS integration (the file system track, after 4a), including the
   key hierarchy, TRIM, suspend and hibernation rules, and zeroing freed
   page-cache frames of encrypted volumes.
