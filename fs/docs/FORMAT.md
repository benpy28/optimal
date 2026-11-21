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
generation `g` writes slots `g % 2` and `2 + g % 2`. Mount reads all four
and uses the newest one that fully verifies, falling back to older ones.
A volume needs at least 64 blocks and at most 2^27 blocks (512 TiB).

## Superblock (4096 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `OPTMLFS\x01` |
| 8 | 4 | version = 1 |
| 12 | 4 | incompatible feature flags = 0 (any set bit: refuse to mount) |
| 16 | 16 | volume UUID (random) |
| 32 | 8 | total blocks (must equal the device size) |
| 40 | 4 | block size = 4096 |
| 44 | 1 | cipher suite: 0 plain, 1 XChaCha20-Poly1305 |
| 45 | 3 | zero |
| 48 | 8 | generation (copy of the body's, used to order slots) |
| 56 | 4 | slot number (must match where the copy was read) |
| 60 | 12 | Argon2id parameters: memory KiB, iterations, lanes |
| 72 | 32 | KDF salt |
| 104 | 4 x 80 | key slots: used (1), zero (7), nonce (24), wrapped key (32), tag (16) |
| 424 | 88 | zero |
| 512 | 24 | body nonce |
| 536 | 16 | body MAC |
| 552 | 3512 | body (sealed as kind 6, address = slot number, birth = generation) |
| 4064 | 32 | BLAKE3 of bytes 0..4064 (unkeyed) |

Bytes 0..512 are the *header*. The body starts with the BLAKE3 hash of the
header, so the header is authenticated by the body's MAC.

Body (plaintext): header hash (32), generation (8), four block pointers
(fs tree root, meta tree root, space map root, free log head), next inode
number, next snapshot id, live deadlist id, next deadlist id, newest
snapshot generation (8 each), commit time in ns (i64). The rest is zero
and must be zero.

Key slots: the master key (32 random bytes) is wrapped with
XChaCha20-Poly1305 under `Argon2id(passphrase, salt, params)`, with
associated data `"OFSK" || uuid || slot index (u32)`.

## Block pointer (64 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | block address (0 = null) |
| 8 | 8 | birth generation |
| 16 | 16 | MAC (BLAKE3-128 or Poly1305 tag) |
| 32 | 24 | nonce (zero on plain volumes) |
| 56 | 4 | aux: tree level for tree nodes, free count for bitmaps and tables |
| 60 | 4 | zero |

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

Root table (sealed kind 4): 64 pointers to level-1 tables (null past the
end). Each level-1 table: 64 pointers to bitmap blocks; its own pointer's
aux is the sum of their free counts. Each bitmap (kind 3) covers 32768
blocks, bit set = used; bits past the end of the device are set. A bitmap
pointer's aux is its number of clear bits.

The committed bitmap marks exactly the blocks reachable from the committed
superblock: tree nodes and data of the live tree and every snapshot, the
meta tree, the space map itself, the free log, and the four superblock slots.

## Free log

A chain of blocks (kind 5) from the superblock: magic `0x474f4c46` (u32),
entry count (u32, at most 251), next pointer (64), then `(start, length)`
(u64 each). It lists the blocks freed by this commit. They are free in the
bitmap but must not be allocated until the next commit is durable, because
the previous superblock may still need them.
