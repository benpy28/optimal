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

Two flushes per commit, no journal, no double writes of data (except file
data an `fsync` puts in the intent log, below, which the next commit writes
again).

## Intent log (fsync without a commit)

Code: `src/fs/ilog.rs`; format: FORMAT "Intent log". Storage design: the
per-file fsync intent log that carries file data and namespace operations.

- **Where.** A ring of blocks set aside at format, right after the front
  superblock slots: 1/64 of the volume, 16 to 16384 blocks (64 KiB at the
  1024-block minimum, 64 MiB from 4 GiB up). It is marked used in the space
  map for good, so nothing else lands there and no allocation, claim or
  quarantine is involved. Each commit's superblock body names the ring
  position where its *chain* starts (after the previous chain, so wear
  moves round the ring).
- **What `fsync(f)` writes.** One *batch*: every namespace and attribute
  record since the last commit or batch (create, mkdir, symlink, link,
  unlink, rmdir, rename, setattr, truncate: arguments, new inode number and
  the clock readings the operation took), plus a *state record* for `f`
  (its inode and every block changed since it was last committed or
  logged). A file renamed or linked in those records gets a state record
  too: once the rename is durable, the target is the complete new file
  (the storage design's replace-by-rename row). Writes add no record; they
  mark blocks. A directory fsync writes the records alone. Then one flush.
  Example (the hand test): mkdir, create, a 10,000-byte write, a setattr,
  fsync: 4 ring blocks and 1 flush, against a commit's tree paths, space
  map, root block, chunk table, 2 superblocks and 2 flushes.
- **What it leaves out.** Other files' data: a background writer's blocks
  never ride along with an unrelated fsync (test: 2 MiB of another file
  dirty, the fsync of a 4 KiB file writes 2 ring blocks). Namespace records
  all go into every batch because their order matters and they are small.
- **Blocks.** Each ring block is sealed on its own (nonce and MAC inside
  it, kind 10, its address and the chain's commit generation as associated
  data) and carries its sequence number in the chain and a last-of-batch
  flag. Replay reads from the chain start until a block fails to verify
  (an older chain's block has another generation), has the wrong sequence
  number, or the ring ends, and applies only complete batches. A batch
  starts on a fresh block: no block that was flushed is written again while
  its chain is live, so a torn write loses only the batch in flight.
- **Replay** runs at every mount, after recovery picked the commit (a
  cleanly unmounted commit's chain is empty unless a later mount fsynced
  into it, which the clean flag does not show; an empty chain costs one
  block read): the records go through the ordinary operations, with the
  recorded clock readings, so inode numbers, every timestamp and directory
  sizes come out identical (the hand test compares whole inodes); state
  records put the inode and blocks back. A writable mount then commits, and
  the chain is done. A read-only mount replays in memory and writes
  nothing. Replay never commits part way (a space check that would need a
  commit refuses instead). A commit from a history ring (fallback mount)
  has no chain recorded and replays nothing.
- **A batch that fails to apply** (only a bug could log one: the original
  operations succeeded on the same state) or does not decode is never
  applied half way: mount starts again and applies only the batches before
  it, and reports it (`LogStats::replay_stopped`).
- **When fsync commits instead.** No ring; a read-only or degraded volume;
  a commit that failed since the last good one (its superblock may have
  landed with a new chain start); a commit made inside an operation (to
  free space); a snapshot operation; a file with more changed blocks than
  the ring; a batch that does not fit in the rest of the ring; or a log
  write or flush that fails (after a failed flush, data written since the
  last good one is made dirty again first, as for a failed commit). A
  commit is always correct, so every refusal falls back to it.
- **Lying drives.** The log does not change the K = 8 fallback promise: a
  fallback mounts a commit as before and replays its chain only if that
  commit's own superblock body was read. The VFS's one-commit-per-second
  limit is what keeps 8 commits spread over real time; fsync bursts in
  between go to the ring.
- **Not done yet.** The fsync-latency benchmarks against ext4 (fsync of a
  4 KiB file alone and while a background writer streams, the 1,000-fsync
  burst, the dpkg-style loop; storage set 4a) and the published `syncfs`
  bound need the benchmark step. Large fsyncs log their data inline (twice
  written); writing big runs in place and logging pointers (as ZFS does
  for large writes) waits for those numbers.

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

## Space accounting

Freed blocks stay quarantined for 8 commits, metadata is written twice, and
a metadata slot freed by a commit can't be reused inside that window either.
So a commit costs fresh space even when nothing changed, and getting freed
space back after a delete takes up to 9 more commits. A full volume must be
able to do that without running dry, or it would lock up (issue 27: a
400-block volume poisoned itself after a delete and a create).

Before any operation that allocates, `ensure_space` checks that, after the
operation, the volume could still complete its next commit and then 9 empty
commits that drain quarantined space back into use. Metadata is counted in
slots: free, unquarantined slots of mapped chunks come first, and every
further chunk costs two copies of `chunk_blocks`. A commit's own overhead
(root block, free log, bitmaps and tables) is the larger of what the last
18 commits used and a structural floor (4 plus one per space-map level, one
chunk-table pair per level plus one). On top of that, deletes leave 1% of
the volume (at least 32 blocks), and other operations leave enough more
that a delete can still be committed and drained when they are refused.

When space is short, `ensure_space` commits the open transaction, then
simulates the drain from the quarantined batches before writing anything.
If the drain can't produce enough space, the operation is refused with
`NoSpace` and nothing is written, so a program retrying on a full disk
costs no I/O. No commit is started that the volume can't afford.

Measured on one-file fills (`tests/full.rs`), the data that fits before the
first refusal is 540 of 1024 blocks, 1500 of 2048, 3348 of 4096 and 15256
of 16384. Above about 65536 blocks the 2% gap dominates. Because the space
kept back is a fixed few hundred blocks, `format` refuses volumes below
1024 blocks (4 MiB).

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
system stops all writes on a single error. By the time a write error
reaches this crate the block layer has already retried it in place, so the
file system only ever retries at a new block. In this crate:

- Reads never poison anything. A read that fails returns the error and the
  volume carries on.
- **Operations are all or nothing.** An operation that fails part way (a
  read error or checksum failure in the middle of a rename, say) is undone
  in memory before it returns its error, and the volume stays writable
  (`Filesystem::op`). While an operation runs, the trees log every item
  they change with its old value (`Store::undo`; the old value is moved
  there, not copied), and file data an item stops pointing at is logged as
  released. Undo puts the items back newest first and takes those releases
  out of the transaction's space-map log, and only if there were any (or a
  free failed part way) rebuilds the space map from the last commit and the
  log, as a failed commit does. Tree nodes made dirty on the way stay
  dirty: they hold the same items, and the free of their old copy stays in
  the log. Changes to the dirty file data (a deleted file's, a shrunk
  file's tail) are queued and made only once the operation succeeded; a
  write puts its data in after its last step that can fail. Snapshot
  operations, which replace whole trees and free blocks a tree still
  points at, keep the whole trees instead (dirty nodes are shared, so that
  is a pointer copy; they commit first, so the trees are clean) and replay
  the space-map log up to the checkpoint. A commit inside an operation
  moves the checkpoint past it, since what is durable is not undone; a
  snapshot operation whose own commit fails is undone too. So a snapshot
  rollback that fails after its middle commit keeps the newer snapshots it
  already deleted.
- **Commits undo their own tree changes the same way**: a data flush that
  fails while building extents (a read in the punch of old extents, or no
  space) is undone as a whole, its allocations taken out of the log and
  every block back in dirty memory; a snapshot deadlist append that fails
  is undone item by item and its pending list kept. Then the commit is
  rebuilt and retried as below. A read the commit needed that failed its
  checksum is retried the same way, since the damage may have been in
  transit; if it persists, the commit attempts run out and the volume
  degrades.
- **A commit whose writes fail keeps its transaction.** The blocks of every
  failed request join a per-mount bad set, widened to their aligned group
  of 64 blocks (256 KiB; failing blocks come in regions, such as a worn
  flash erase block), and are never allocated again on this mount. The
  in-memory state is then rebuilt on top of the last commit
  (`Filesystem::rebuild`): the space map is opened again from the last
  commit and the transaction's own space-map changes (`Store::log`: every
  free made through the trees or the file data path, and every file-data
  allocation) are replayed onto it; tree nodes the failed commit wrote
  become dirty again (`Tree::unwrite`); file blocks that were not written
  go back to dirty memory (decrypted from the write queue); chunks with a
  copy on a bad block are queued to move. If the failed commit had started
  writing superblocks, later commits are numbered above it and rewrite all
  four slots, as after a fallback mount. Then the commit runs again, with
  new homes for everything.
- **A failed flush** leaves the durability of everything written since the
  last good flush unknown. The rebuild reads back each file block of the
  transaction written since then, checks it against its MAC and makes it
  dirty again, so the retry writes it to a new home and flushes it. A block
  that no longer reads back intact is lost, and the volume stops (see
  below) rather than commit a transaction that points at lost data.
- **Bounded**: at most 3 attempts per `commit` call (the storage design's
  "at most 3 writeback cycles"), and at most 1/64 of the volume (at least
  256 blocks) in the bad set. Past either, `commit` returns the I/O error
  and the volume is **degraded**: new changes are refused with
  `Error::DeviceFailing`, reads (including of the uncommitted changes)
  work, and a later `commit` call tries again (3 more attempts, keeping
  the bad blocks found so far). Success clears the state.
- **A device failing as a whole** (unplugged, dead, refusing writes) is
  told apart from bad blocks: when not one request of a batch of several
  could be written, nothing is marked bad, so a device that comes back is
  not left with most of its space condemned.
- **An early data flush** (dirty data past the limit, inside `write`) that
  fails is not the write's failure: the data is in the transaction, so
  `write` succeeds, the data stays in memory, the space map is rebuilt and
  the commit retries (durability is promised by `commit`, as writeback
  errors surface at fsync). Three failed early flushes before a commit
  succeeds degrade the volume, so memory can't grow without bound.
- A commit refused for space inside the commit (space accounting makes this
  a bug, but it is handled) is rebuilt the same way and returns `NoSpace`;
  the volume stays writable.
- **Superblock slots that fail are written around.** A slot is a fixed
  block and cannot move, so a slot whose write fails is not written again
  on this mount (a later mount, or a commit on a degraded volume, tries it
  again), and the retry writes around it. With a dead slot every commit
  still lands on two slots: its own pair's live slots, then live slots of
  the other pair, one slot per flushed wave, so at most one copy is in
  flight and a power cut leaves either the new commit or the previous one
  whole on some slot. With fewer than two live slots the commit is refused
  before any write and the volume degrades (`Error::DeviceFailing`):
  overwriting the one copy left in place could tear it with nothing to fall
  back to. A dead slot may keep an old copy; it is older than every live
  one, so mount only reaches it if all live copies fail, and then checks
  it like any fallback.
- **Still stops the volume** (`Error::Poisoned`, remount to continue, the
  disk holds the last good commit): a failure while undoing an operation
  or rebuilding after a failed commit (both read the last commit back, so
  a device that stopped reading stops the volume), an unwritten file block
  that no longer reads back intact, and internal inconsistencies other than
  checksum failures.
- The VFS (the file system track, after 4a) owns errseq, page retention
  while a device is gone, and the read-only switch.

What undo costs on the success path: one undo record per changed tree
item (key and moved old value, no copy) and per data release, dropped when
the operation returns; see "Testing" for the measurement. Rejected: copying
the trees per operation with copy-on-write in memory (tried first; creates
went from about 4 to 15 microseconds, since every operation then copies
each node it touches).

What the rebuild costs on the success path: one log entry (at most 40 bytes) per
space-map change made through the trees or the data path (freed tree nodes
and extents, data runs), one reference-count increment per tree node a
commit writes, and a range set of blocks written since the last flush.
Measured on a 781 MiB in-memory device, release build, best of 5, plain
suite: 2000 creates with a 64 KiB write each and a commit every 100
(0.219 s before, 0.216 to 0.223 s after), 256 MiB written in 1 MiB writes
then one commit (0.456 to 0.493 s before, 0.444 to 0.461 s after), and
20000 random 4 KiB overwrites of a 64 MiB file with a commit every 1000
(0.316 to 0.331 s before, 0.326 to 0.328 s after): no difference beyond
run-to-run noise. Blocks freed in the same transaction that allocated them
(file data flushed early, then overwritten or a failed write's new home)
are reusable at once rather than quarantined, since no commit references
them.

## Testing

The filesystem is the piece where a bug loses people's files, so it gets
the heaviest testing in the project:

- **Model-based tests**: long random workloads (create, write, truncate,
  rename, links, snapshots, rollback, remount, ...) run against the real
  filesystem and a simple in-memory reference model. Every result is
  compared, full state is compared regularly, and fsck runs throughout.
- **Write failures** (`tests/write_fail.rs`): every write of a random
  workload (with snapshots and remounts) failing for good, its blocks gone
  bad (a superblock slot included: commits write around it), and every
  flush failing once: every operation still succeeds and
  matches the model, fsck is clean, a remount shows the committed state. A
  failed write followed by a power cut at each of the next 24 write
  points: mount gives the last returned commit or a state the interrupted
  operation commits. A device that stops taking writes degrades the
  volume and recovers. A failed flush that loses the device cache never
  yields a commit that claims the lost data. Unit tests
  (`src/write_fail_tests.rs`) inject the exact condition behind each
  safeguard the workloads can't reach: a failed second superblock write
  (the retry is numbered above it and rewrites all four slots), bad blocks
  in the available count, a mid-transaction rebuild reproducing the space
  map exactly, the attempt bound and the bad-set limit. 18 hand mutations
  of the safeguards, 18 caught.
- **Operations failing part way** (`tests/op_undo.rs`): a random workload
  with snapshots, remounts, frequent commits and the node cache dropped
  before each operation fails the k-th and (k+1)-th device read of every
  operation, for every k from 0 to 11, half the runs as an I/O error and
  half as damaged data. After each failure the volume is not stopped, the
  live tree and every snapshot equal the model before the operation, a
  commit and fsck are clean, and the operation retried succeeds. Unit
  tests (`src/op_undo_tests.rs`): hand-worked superblock wave plans, a
  slot dead for good (each later commit on two other slots, remount finds
  it again), two dead slots, three dead slots (degraded, changes readable,
  a commit lands them once the device works), an unlink failing at each
  read point after its data was released (data intact, fsck exact; also
  under a snapshot, then deleting the snapshot), a truncate failing part
  way (dirty data untouched), a commit whose data flush fails at each read
  point, a snapshot whose commit fails for good, losing a third slot, and
  the counters a failed create or snapshot moved. 29 hand mutations of the
  new safeguards, 26 caught. Survivors: deferring the drop of a shrunk
  file's dirty blocks and undoing a deadlist append item by item guard
  conditions no current path reaches (a file with dirty data always has a
  dirty inode leaf, so nothing after the drop reads; after a deadlist
  append's first insert its path is dirty, so later inserts cannot read).
  The flag for a space-map free that fails part way is tested directly
  (`free_failing_on_its_second_bitmap_block_is_undone`: a free spanning two
  bitmap blocks whose second bitmap read fails; without the rebuild the
  first half is handed out again). No current path makes such a free (data
  runs come from one bitmap block, metadata frees are one block), so the
  guard is defence for crafted images and future callers.
- **Intent log** (`tests/intent_log.rs`, `src/fs/ilog.rs` unit tests):
  hand cases for each promise (file fsync with whole inodes and inode
  numbers compared after a power cut, directory fsync, replace by rename
  through a directory fsync or an unrelated file's fsync, a background
  writer's data left out, shrink and regrow with early-flushed blocks read
  back, a commit ending the chain, the fallbacks to a commit, read-only
  replay writing nothing, fsync after a clean mount), the ring block layout
  worked by hand, and a batch that fails to replay, does not decode, or has
  a flipped bit. Power cuts at every write point of random workloads with
  fsyncs mixed in (plain and encrypted, and sampled long runs): the
  harness keeps the state replay must rebuild (the last commit, every
  namespace operation since, each file's data as of its last logged state)
  and the mounted volume must equal it before or after the interrupted
  call, pass fsck and keep working.
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

- Intent log in a fixed ring set at format, like a journal. Why: no
  allocator interplay (log blocks need no claim at mount, no quarantine, no
  space-accounting term, no pre-allocated chain head per commit) and
  sequential writes. Rejected: a ZIL-style chain of allocated blocks
  (claimed at mount, freed through quarantine, one head block allocated by
  every commit).
- Logical records replayed through the ordinary operations with their
  recorded clock readings. Why: small batches, and replay reuses the code
  the model-based tests already check. Rejected: logging changed tree
  blocks (physical, much larger per fsync, and it ties the log to node
  layout).
- Per-file data, all namespace records in every batch. Why: a background
  writer's data stays out of other files' fsyncs, while namespace order
  stays exact. Rejected: logging the whole open transaction (an fsync
  would carry every dirty byte on the volume).
- File data inline in the ring for now. Why: simplest correct form; the
  benchmarks decide whether large runs should go in place. Rejected for
  now: in-place data with logged pointers (needs claims at mount).
- A batch that fails to replay makes mount start over with the batches
  before it. Why: no half-applied batch, no new undo machinery for a case
  only a bug reaches. Rejected: failing the mount (it would then try an
  older commit and lose more).

- Copy-on-write tree, not a journal: atomic commits and snapshots fall out
  of it, and data is written once. Rejected: ext4-style journaling (double
  writes, no snapshots, no data checksums).
- Space checks plan the post-delete drain up front (slot-aware, refuse
  without writing) and keep its cost back, rather than cycling commits until
  space appears. Rejected: shorter quarantine (it is the fallback depth),
  and letting a commit fail for space and roll back (ZFS-style reserved
  space is simpler and never loses the transaction).
- A failed commit is rebuilt from a log of the transaction's space-map
  changes replayed onto the last committed map, with the trees kept and
  their written nodes turned dirty again. Rejected: cloning the in-memory
  state before each commit (a copy of every cached table per commit); an
  undo log inside the space map (every internal copy-on-write step would
  need an inverse); replaying whole operations (inode numbers, times and
  hashes would have to be replayed exactly).
- An operation that fails part way is undone from an undo log of the tree
  items it changed (old values moved in, no copies) and the data it
  released. Rejected: copy-on-write snapshots of the trees per operation
  (each operation copies every node it touches, about 4x slower creates);
  stopping the volume (one transient read error would end all writes).
  Snapshot operations keep whole trees instead: they replace trees and
  free blocks still referenced, which an item log can't express.
- A superblock slot whose write fails is written around for the mount,
  with each commit still on two slots, one per flushed wave. Rejected:
  persisting dead slots (format change; a mount retries them, which costs
  one failed attempt); allowing a single live slot (an in-place overwrite
  of the only copy can tear).
- Failed writes move to new blocks and condemn their 256 KiB group for the
  mount, and are not retried in place: the block layer already retried, and
  writing again to a region that just failed is what repair avoids too. A
  batch where no request worked is a failing device, not bad blocks.
  Rejected: probing each block of a failed request on its own (an in-place
  retry, and slow convergence on regions); persisting the bad set (needs a
  format change; it waits for the rescue tooling).
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
| Lying drives: K = 8 commits (raised if the power-cut rig ever records a fallback deeper than K / 2), fallback within K, rescue beyond | Crash consistency ("Lying drives"); FORMAT "Lying drives" | done; distribution printed by the lying suite; rig data needs hardware; rescue mount open |
| Root history: previous K commits' full root sets, each with the chunk table root | FORMAT "Body" (core) | done |
| Free-block and chunk-free quarantine for K commits | Crash consistency; Duplicated metadata | done |
| Lying-drive test with a metadata-chunk move inside the window | Testing ("Duplicated metadata") | done |
| Fixed plaintext header area (bytes 0..512, unused bytes reserved and zero), generation and slot in the authenticated trailer | FORMAT "Superblock" | done |
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
| Per-file fsync intent log, benchmarked under background writeback; it also carries namespace operations (directory fsync, link, rename, O_TMPFILE plus linkat), syncfs latency has a published bound, and a dpkg-style create, fsync, rename, directory-fsync loop joins the tests | Intent log; FORMAT "Intent log" | done: the log, namespace operations and replay, power-cut tested (O_TMPFILE waits for orphan handling in the VFS); open: the benchmarks against ext4 and the published syncfs bound |
| Error policy: failed write retried at a new block, at most 3 writeback cycles | Errors | done: commits, data flushes and dead superblock slots; operations that fail part way are undone |
| Metadata stored twice: 64-byte pointers with logical addresses, paired chunks, copy-on-write chunk table in every root set, MAC binds the logical address | Duplicated metadata; FORMAT "Duplicated metadata" | done |
| The two copies differ; no (copy 1 key, nonce) pair repeats | Duplicated metadata; FORMAT "The second copy" | done (copy 1 checked through the parent's MAC rather than a trailer tag, as agreed in design review) |
| Online scrub | Duplicated metadata (`scrub_metadata`) | done for metadata on an idle volume; online (concurrent) scrub open |
| Checker (fsck), fuzzed with corrupted images | Testing | done |
| Read-only rescue mount, export tool | | open |
| Feature bits: compat, ro-compat, incompat; frozen before user data | FORMAT "Feature bits and versioning" | done |
| Zero on free for page-cache frames of encrypted volumes | page cache interface (kernel) | with the VFS: noted for the page cache interface |

## Not done yet (tracked, in order)

1. Benchmarks against ext4 (fio-style workloads on the same device),
   including a scrub that reads copy 1 first, and the intent log's: fsync
   latency alone and under background writeback, the fsync burst, the
   dpkg-style loop, and a published `syncfs` bound.
2. In-place data for large fsyncs if those benchmarks call for it.
3. Kani proofs for the space map, chunk table and tree invariants.
4. Inline data for small files and an AES-GCM-style suite.
5. Concurrency (the API is `&mut self` today; the kernel will wrap it first).
6. A metadata reserve sized for real disks (2%, 256 MiB to 8 GiB), online
   scrub, a rescue mount and an export tool.
7. Orphan handling for files that are unlinked while still open (needs the VFS).
8. Kernel VFS integration (the file system track, after 4a), including the
   key hierarchy, TRIM, suspend and hibernation rules, and zeroing freed
   page-cache frames of encrypted volumes.
