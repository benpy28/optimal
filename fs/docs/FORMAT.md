# OptimalFS on-disk format, version 1

All integers are little-endian. A block is 4096 bytes. Block numbers are
`u64`. "Sealed" means the block was passed through the volume's sealer (see
*Sealing*) and its MAC and nonce live in the pointer that refers to it.

## Device layout

| Blocks | Contents |
|---|---|
| 0, 1 | Superblock slots 0 and 1 |
| 2 .. N-3 | Allocatable space (everything else lives here) |
| N-2, N-1 | Superblock slots 3 and 2 |

Slot `s` lives at block `0, 1, N-1, N-2` for `s = 0, 1, 2, 3`. Commit
generation `g` writes slots `g % 2` and `2 + g % 2` (and, after a fallback
mount or for a replay-value bump, then the other pair too). A volume needs
at least 64 blocks and at most 2^45 blocks (128 PiB).

## Superblock (4096 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `OPTMLFS\x01` |
| 8 | 4 | version = 1 |
| 12 | 4 | incompatible feature bits = 0 (any unknown bit: refuse to mount) |
| 16 | 16 | volume UUID (random) |
| 32 | 8 | total blocks (must equal the device size) |
| 40 | 4 | block size = 4096 |
| 44 | 1 | cipher suite: 0 plain, 1 XChaCha20-Poly1305 |
| 45 | 3 | zero |
| 48 | 4 | compatible feature bits (unknown bits ignored) |
| 52 | 4 | read-only-compatible feature bits (unknown bits: read-only mount only) |
| 56 | 4 | zero |
| 60 | 12 | Argon2id parameters: memory KiB, iterations, lanes |
| 72 | 32 | KDF salt |
| 104 | 4 x 80 | key slots: kind (1), zero (7), nonce (24), wrapped key (32), tag (16) |
| 424 | 88 | reserved, zero |
| 512 | 4 | slot number (must match where the copy was read) |
| 516 | 4 | zero |
| 520 | 8 | generation (orders the slots) |
| 528 | 24 | body nonce |
| 552 | 16 | body MAC |
| 568 | 3496 | body (sealed as kind 6, address = slot number, birth = generation) |
| 4064 | 32 | BLAKE3 of bytes 0..4064 (unkeyed) |

Bytes 0..512 are the *header*. **They are frozen**: identical in all four
slots and in every commit; they change only when a key slot changes. This
is what the volume identity tag and any reader before the key is known
rely on. Per-slot values (slot, generation) are in the trailer at 512..528,
which the body MAC authenticates (as the block address and birth). The body
starts with the BLAKE3 hash of the header, so the header is authenticated
by the body's MAC too.

Key slot kinds: 0 empty, 1 passphrase (Argon2id), 2 reserved for
TPM-sealed slots (stage 2; exempt from the Argon2id floor). Other values:
refuse to mount. The master key (32 random bytes) is wrapped with
XChaCha20-Poly1305 under `Argon2id(passphrase, salt, params)`, with
associated data `"OFSK" || uuid || slot index (u32)`. Format refuses
passphrase slots below the floor of 64 MiB and 4 passes; the default is
min(1 GiB, RAM / 4) with passes raised to keep about 2 s. Mount refuses
parameters above 4 GiB or 64 passes (a crafted image can't exhaust memory).

### Body

| Size | Field |
|---|---|
| 32 | header hash |
| 344 | this commit's *core* (below) |
| 64 | fsync intent-log root: reserved, must be null in v1 |
| 4 | history length H (at most 8) |
| 4 | zero |
| H x 344 | the previous H commits' cores, newest first, strictly older generations |

The rest is zero and must be zero.

Core (344 bytes): generation (8); four block pointers (fs tree root, meta
tree root, space map root, free log head); next inode number, next
snapshot id, live deadlist id, next deadlist id, newest snapshot
generation (u64 each); commit time in ns (i64); replay value (u64); flags
(u64: bit 0 = cleanly unmounted at this commit, other bits refuse); window
generation (u64, at most the generation).

The *window generation* is the oldest commit still inside the free-block
quarantine when this commit was made (at most generation - 7). Every block
the commit references that was born at or after it may be one a lying
drive lost; every older block is protected by the quarantine.

## Block pointer (64 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | block address (0 = null) |
| 8 | 8 | birth generation |
| 16 | 16 | MAC (BLAKE3-128 or Poly1305 tag) |
| 32 | 24 | nonce (zero on plain volumes) |
| 56 | 8 | aux: tree level for tree nodes; free blocks below for space-map pointers; depth (1..5) for the space map root |

## Sealing

Associated data for every block is 36 bytes:
`"OFS" || kind (u8) || address (u64) || birth (u64) || uuid (16)`.
Kinds: 1 tree node, 2 file data, 3 bitmap, 4 space table, 5 free log, 6 superblock body.

- Plain volumes: MAC = first 16 bytes of keyed BLAKE3(ad || block), key =
  `derive_key("OptimalFS v1 plain checksum key", "OptimalFS v1 plain volume key .." || uuid)`.
- Encrypted volumes: XChaCha20-Poly1305 with key
  `derive_key("OptimalFS v1 block encryption key", master)`; the block is
  encrypted in place and the tag is the MAC. Nonce = 16-byte random
  per-mount prefix || 64-bit counter.

Directory names are hashed with keyed BLAKE3 (first 8 bytes) under a key
derived from the master key (or the plain checksum key).

## Trees

Two copy-on-write B+trees share one node format: the filesystem tree (id 1,
snapshotted) and the meta tree (id 2).

Key (17 bytes): object (u64), kind (u8), offset (u64), ordered as a tuple.

Node (4096 bytes): magic `0x4e4f` (u16), level (u8, 0 = leaf, at most 16),
tree id (u8), item count (u16, at least 1), zero (u16), birth (u64, equals
the pointer's), then items, then zero padding.
- Leaf item: key (17), value length (u16, at most 1900), value.
- Internal item: key (17), child pointer (64). The child's level is this
  node's level minus one and its birth is not later than this node's.

Keys strictly increase within a node and across the tree. In an internal
node, key `i` is a lower bound for child `i` and every key under child `i`
is below key `i+1`.

### Filesystem tree items

| Key | Value |
|---|---|
| (ino, 1, 0) | inode, 80 bytes: kind (u8: 1 file, 2 dir, 3 symlink), zero (3), permission bits (u32, at most 0o7777), uid, gid, nlink, zero (4), size (u64: bytes, or entry count for a directory), atime, mtime, ctime, btime (i64 ns), parent (u64: directories only) |
| (dir, 2, name hash) | entry bucket: count (u16, at least 1), then per entry: inode (u64), kind (u8), name length (u8), name |
| (ino, 3, first file block) | extent: physical block (u64), length in blocks (u32, 1..64), zero (u32), birth (u64), base nonce (24), then one 16-byte MAC per block. Block `i` uses nonce counter base + `i` |
| (ino, 4, 0) | symlink target |

Inode 1 is the root directory (its parent is itself). Inode numbers are
never reused.

### Meta tree items

| Key | Value |
|---|---|
| (snapshot id, 10, 0) | snapshot: generation (u64), fs tree root (pointer), deadlist id, next inode number (u64 each), creation time (i64), name length (u8, at most 64), name |
| (deadlist id, 11, seq) | count (u16, 1..78), then `(address, length, birth)` (u64 each) |

## Space map

A copy-on-write radix tree. Leaves are bitmaps (kind 3), each covering
32768 blocks, bit set = used; bits past the end of the device are set.
Above them are `levels` levels of tables (kind 4) of 64 pointers each,
where `levels` is the smallest of 1..5 with 32768 x 64^levels >= N; the
single top table is the space map root, whose pointer's aux is `levels`.
Every other pointer's aux is the number of free blocks below it, and must
equal the sum of its children's (or a bitmap's clear bits).

A **null** child pointer means "never written": every block under it is
free except the superblock slots, and its aux must be exactly that count.
So formatting writes only the root and the paths to the first and last
bitmaps, whatever the size of the device, and a sparse map stays small.
Children wholly past the end of the device are null with aux 0.

The committed bitmap marks exactly the blocks reachable from the committed
superblock: tree nodes and data of the live tree and every snapshot, the
meta tree, the space map itself, the free log, and the four superblock slots.

## Free log

A chain of blocks (kind 5) from the superblock: magic `0x474f4c46` (u32),
entry count (u32, at most 126), next pointer (64), then entries of
`(start, length, generation, commit time ns)` (u64, u64, u64, i64). It
lists the blocks freed by the last commits that are still quarantined, each
tagged with the commit that freed them. They are free in the bitmap but are
not allocated until 8 later commits have completed (K = 8), and also a
minimum time has passed if the mount option sets one (default none). So
every commit of the last 8 stays fully intact on disk.

## Lying drives

A drive may acknowledge flushes and still lose the writes. The promise:
if at most K = 8 commits were lost that way, mount lands on the last commit
before the drop point (or a later one that fully landed), losing only
commits after it. Mount reads every intact superblock copy, then tries
their commits and history entries newest first. After an unclean shutdown
(no clean flag) a candidate must read back every block it references that
was born at or after its window generation, file data included; the first
that does is mounted. Only if none does, mount takes the newest candidate
whose metadata verifies and reports the files with lost data (reads of
those fail; nothing wrong is returned). A drop more than K commits back is
outside the promise: that is the rescue mount's job.

After mounting an older commit than the newest copy on disk, the next
generation is numbered above every copy seen, and the first commit is
written to all four slots.

## Feature bits and versioning

Unknown compatible bits are ignored; unknown read-only-compatible bits
allow only a read-only mount; unknown incompatible bits refuse the mount.
Version 1 defines none. The format is frozen before any user data.
