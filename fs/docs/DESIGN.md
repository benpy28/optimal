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
can be lost, reordered or torn at 512-byte granularity until a flush
completes. NVMe, SATA and SCSI all promise exactly this and no more.

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
describe exactly what T+1 references), but not handed out until T+2 starts.
The free log records each commit's quarantine so it survives a remount.
This also means the *previous* superblock always stays fully valid, which
is what lets mount fall back past a damaged newest commit.

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
under Argon2id(passphrase) (256 MiB, 3 passes by default). TPM sealing and
per-user keys plug in as other slot kinds later. Subkeys are derived with
BLAKE3's key derivation. Nonces are a 128-bit random prefix chosen at each
mount plus a counter, so a nonce never repeats, even after a crash replays
generation numbers. Superblock bodies are encrypted too; only the header
(geometry, KDF parameters, wrapped keys) is in the clear.

What an attacker with the disk can still see: the volume size, which blocks
are in use and when they were written, and the KDF parameters. They cannot
see names, sizes, contents, directory structure or counts.

Known limit: replaying a *whole* old disk image can't be detected by the
disk alone. The fix is binding the commit generation to a TPM monotonic
counter, which the boot design already provides; it lands with the kernel
integration.

## Data structures

- **One B+tree for all file metadata**: inodes, directory entries (keyed by
  a keyed hash of the name), extents and symlink targets, keyed by
  `(object, kind, offset)`, so a file's items sit together on disk.
- **Extents** of up to 64 blocks (256 KiB) with one MAC per block, so a
  partial overwrite only rewrites the blocks it touches.
- **Space map**: a three-level copy-on-write radix of bitmaps with free
  counts in the pointers, so allocation skips full regions without I/O.
  Allocation is next-fit for large contiguous runs.
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

Nothing panics on bad disk data. Damage surfaces as `Error::Corrupt`. An
I/O error or corruption found in the middle of an operation stops all
further writes until remount, so a half-applied change can never be
committed; the disk still holds the last good commit.

## Testing

The filesystem is the piece where a bug loses people's files, so it gets
the heaviest testing in the project:

- **Model-based tests**: long random workloads (create, write, truncate,
  rename, links, snapshots, rollback, remount, ...) run against the real
  filesystem and a simple in-memory reference model. Every result is
  compared, full state is compared regularly, and fsck runs throughout.
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
- One-commit quarantine for freed blocks: costs nothing measurable and keeps
  the fallback superblock intact.
- Birth-time deadlists rather than per-block reference counts: snapshot
  creation is O(1) and the live write path never updates a refcount tree.
- Inode numbers are never reused (64-bit), so stale handles can't alias.
- Keyed name hashes: no hash-flooding, and the hash order leaks nothing.

## Not done yet (tracked, in order)

1. Benchmarks against ext4 (fio-style workloads on the same device).
2. Kani proofs for the space map and tree invariants.
3. Inline data for small files and an AES-GCM-style suite.
4. Concurrency (the API is `&mut self` today; the kernel will wrap it first).
5. A metadata reserve sized for real disks (2%, 256 MiB to 8 GiB), online
   scrub, duplicated metadata, a rescue mount and an export tool.
6. Orphan handling for files that are unlinked while still open (needs the VFS).
7. Kernel VFS integration (Phase 4).
