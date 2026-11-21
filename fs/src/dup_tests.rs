//! Unit tests for duplicated metadata: the copy-1 mask, two-copy reads
//! with fallback and repair, and the chunk table against a reference model.

use crate::chunk::{ChunkTable, Entry, LEAF_ENTRIES, LOGICAL, ids_at};
use crate::crypto::{BlockKind, Sealer, SecretKey};
use crate::device::{BLOCK_SIZE, BlockDevice, MemDevice};
use crate::io::{CopyMode, Io, zero_block};
use crate::sim::{Rng, SparseDevice};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

fn mk(enc: bool) -> Sealer {
    if enc {
        Sealer::new([3; 16], Some(&SecretKey([7u8; 32])), &mut Rng::new(5))
    } else {
        Sealer::new([3; 16], None, &mut Rng::new(5))
    }
}

fn sealers() -> Vec<(&'static str, Sealer)> {
    let master = SecretKey([7u8; 32]);
    let mut rng = Rng::new(11);
    alloc::vec![
        ("plain", Sealer::new([3; 16], None, &mut rng)),
        ("encrypted", Sealer::new([3; 16], Some(&master), &mut rng)),
    ]
}

fn pattern(seed: u8) -> crate::io::Block {
    let mut b = zero_block();
    for (i, x) in b.iter_mut().enumerate() {
        *x = (i as u8).wrapping_mul(31) ^ seed;
    }
    b
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

#[test]
fn copy_mask_hand_worked() {
    // Copy 0 is the sealed block; copy 1 the plaintext under the copy-1
    // key (encrypted) or masked with the copy-1 XOF (plain). The mask is
    // pseudo-random, so the copies agree in a byte with probability 1/256:
    // about 16 of 4096 bytes. Require at least 4000 to differ.
    let l = LOGICAL | 5;
    for (name, mut s) in sealers() {
        let p = pattern(1);
        let mut c0 = p.clone();
        let (nonce, mac) = s.seal(BlockKind::Node, l, 7, &mut c0[..]);
        let mut c1 = p.clone();
        s.seal_copy1(BlockKind::Node, l, 7, &nonce, &mut c1[..]);
        assert!(differing(&c0[..], &c1[..]) >= 4000, "{name}");
        // Plain copy 0 is the plaintext itself; encrypted is not.
        assert_eq!(c0[..] == p[..], name == "plain");
        assert!(differing(&c1[..], &p[..]) >= 4000, "{name}");
        // The swap is an involution mapping each copy onto the other.
        let mut x = c1.clone();
        s.swap_copy(BlockKind::Node, l, 7, &nonce, &mut x[..]);
        assert_eq!(x[..], c0[..], "{name}");
        s.swap_copy(BlockKind::Node, l, 7, &nonce, &mut x[..]);
        assert_eq!(x[..], c1[..], "{name}");
        // Copy 1, swapped back, opens under the parent's MAC.
        let mut y = c1.clone();
        s.swap_copy(BlockKind::Node, l, 7, &nonce, &mut y[..]);
        s.open(BlockKind::Node, l, 7, &nonce, &mac, &mut y[..]).unwrap();
        assert_eq!(y[..], p[..], "{name}");
        // Plain volumes have a zero nonce, so the mask depends on kind,
        // address and birth; encrypted ones on the (unique) nonce.
        if name == "plain" {
            for (k, a, b) in [(BlockKind::Bitmap, l, 7), (BlockKind::Node, l + 1, 7), (BlockKind::Node, l, 8)] {
                let mut z = p.clone();
                s.seal_copy1(k, a, b, &nonce, &mut z[..]);
                assert!(differing(&z[..], &c1[..]) >= 4000, "{name} {k:?} {a} {b}");
            }
        } else {
            let n2 = s.next_nonce();
            let mut z = p.clone();
            s.seal_copy1(BlockKind::Node, l, 7, &n2, &mut z[..]);
            assert!(differing(&z[..], &c1[..]) >= 4000, "{name}");
        }
    }
}

/// A device and Io with one duplicated block at physical 10 (copy 0) and
/// 20 (copy 1); returns the pointer and both copies' bytes.
fn dup_fixture(s: Sealer) -> (Io<MemDevice>, crate::ptr::BlockPtr, Vec<u8>, Vec<u8>) {
    let mut io = Io::new(MemDevice::new(64), s);
    let p = io.write_dup(BlockKind::Node, LOGICAL | 9, 10, 20, 4, pattern(2), 0);
    io.write_batch().unwrap();
    let mut c0 = alloc::vec![0u8; BLOCK_SIZE];
    let mut c1 = alloc::vec![0u8; BLOCK_SIZE];
    io.dev.read(10, &mut c0).unwrap();
    io.dev.read(20, &mut c1).unwrap();
    (io, p, c0, c1)
}

fn flip(io: &mut Io<MemDevice>, block: u64, bit: usize) {
    let off = block as usize * BLOCK_SIZE + bit / 8;
    io.dev.image_mut()[off] ^= 1 << (bit % 8);
}

#[test]
fn bit_flip_in_either_copy_falls_back_reports_and_repairs() {
    // The design review's ask: one flipped bit in copy 1 makes a
    // both-copies read return copy 0's (correct) contents and report copy
    // 1; the repair rewrites copy 1 byte for byte. And the mirror case.
    for name in ["plain", "encrypted"] {
        for bit in [0usize, 7, 8 * 1000 + 3, 8 * 4095 + 7] {
            for (bad, good) in [(20u64, 10u64), (10, 20)] {
                let (mut io, p, c0, c1) = dup_fixture(mk(name == "encrypted"));
                let orig = if bad == 10 { c0.clone() } else { c1.clone() };
                flip(&mut io, bad, bit);
                // Normal reads use copy 0 and only touch copy 1 if it fails.
                io.copy_mode = CopyMode::Either;
                let b = io.read_dup(BlockKind::Node, &p, 10, 20).unwrap();
                assert_eq!(b[..], pattern(2)[..], "{name} bit {bit} bad {bad}");
                let expect_either = (bad == 10) as u64;
                assert_eq!(io.stats.bad_copies, expect_either, "{name} {bit} {bad}");
                // Reading both copies always finds it.
                io.copy_mode = CopyMode::Both;
                let b = io.read_dup(BlockKind::Node, &p, 10, 20).unwrap();
                assert_eq!(b[..], pattern(2)[..]);
                assert_eq!(io.stats.bad_copies, expect_either + 1);
                assert!(io.repair_queued(bad) && !io.repair_queued(good));
                io.queue_repairs();
                io.write_batch().unwrap();
                let mut now = alloc::vec![0u8; BLOCK_SIZE];
                io.dev.read(bad, &mut now).unwrap();
                assert_eq!(now, orig, "{name}: repair restores the exact bytes");
                let n = io.stats.bad_copies;
                io.read_dup(BlockKind::Node, &p, 10, 20).unwrap();
                assert_eq!(io.stats.bad_copies, n, "healed");
            }
        }
    }
}

#[test]
fn dup_reads_reject_what_the_parent_did_not_commit() {
    for name in ["plain", "encrypted"] {
        let enc = name == "encrypted";
        let mk = || mk(enc);
        // Both copies damaged: an error, never data.
        let (mut io, p, _, _) = dup_fixture(mk());
        flip(&mut io, 10, 100);
        flip(&mut io, 20, 200);
        io.copy_mode = CopyMode::Both;
        assert!(io.read_dup(BlockKind::Node, &p, 10, 20).is_err(), "{name}");
        // Copies in each other's place: neither verifies (copy 1's bytes
        // are not copy 0's), so a swapped or misdirected copy is caught.
        let (mut io, p, c0, c1) = dup_fixture(mk());
        io.dev.write(10, &c1).unwrap();
        io.dev.write(20, &c0).unwrap();
        assert!(io.read_dup(BlockKind::Node, &p, 10, 20).is_err(), "{name} swapped");
        // A stale copy 1: an earlier version of the same logical block with
        // the same birth (a crashed attempt at this generation, replayed).
        // On plain volumes the nonce is zero, so only the parent's MAC can
        // tell; copy 1 must be checked against it.
        let (mut io, p, _, _) = dup_fixture(mk());
        let old = io.write_dup(BlockKind::Node, LOGICAL | 9, 30, 31, 4, pattern(3), 0);
        io.write_batch().unwrap();
        let _ = old;
        let mut stale = alloc::vec![0u8; BLOCK_SIZE];
        io.dev.read(31, &mut stale).unwrap();
        io.dev.write(20, &stale).unwrap();
        flip(&mut io, 10, 5);
        io.copy_mode = CopyMode::Either;
        assert!(io.read_dup(BlockKind::Node, &p, 10, 20).is_err(), "{name} stale copy 1");
        // With repairs off (read-only mounts) a bad copy is counted only.
        let (mut io, p, _, _) = dup_fixture(mk());
        io.repair_enabled = false;
        flip(&mut io, 10, 9);
        io.read_dup(BlockKind::Node, &p, 10, 20).unwrap();
        assert_eq!((io.stats.bad_copies, io.pending_repairs()), (1, 0));
        // verify_copies reports each copy without side effects.
        assert_eq!(io.verify_copies(BlockKind::Node, &p, 10, 20), (false, true));
        assert_eq!(io.stats.bad_copies, 1);
    }
}

/// The chunk table against a plain map of id -> entry: random sets and
/// clears (growing the depth to 3), lookups, searches, commits with fresh
/// homes, reopening from the device, and exact accounting of the old
/// copies each commit frees.
#[test]
fn chunk_table_against_model() {
    for seed in 0..6u64 {
        let mut rng = Rng::new(0xc4a7 + seed);
        let total = 1u64 << 40;
        let shift = (seed % 7) as u32;
        let mut io = Io::new(
            SparseDevice::new(total),
            Sealer::new([1; 16], None, &mut Rng::new(seed)),
        );
        let mut t = ChunkTable::format(total, shift);
        let mut model: BTreeMap<u64, Entry> = BTreeMap::new();
        let mut next_home = 1u64 << 39;
        let mut generation = 0u64;
        let cmax = 1u64 << shift;
        // Ids drawn from a few clusters, so leaves fill up and the depth
        // must grow past 0, 1 and 2 (capacity 128, 4096, 131072).
        let pick_id = |r: &mut Rng| -> u64 {
            match r.below(4) {
                0 => r.below(300),
                1 => 4000 + r.below(300),
                2 => 130_000 + r.below(2000),
                _ => r.below(200_000),
            }
        };
        let mut prev_ptrs: BTreeSet<(u64, u64)> = BTreeSet::new();
        for step in 0..1500 {
            let id = pick_id(&mut rng);
            if rng.chance(2, 3) {
                while id >= t.capacity() {
                    t.grow().unwrap();
                }
                let e = if rng.chance(1, 5) {
                    Entry::default()
                } else {
                    let len = 1 + rng.below(cmax) as u32;
                    let p0 = 2 + id * 200;
                    Entry {
                        p0,
                        p1: p0 + 100,
                        used: rng.next_u64() & Entry::mask(len),
                        len,
                    }
                };
                t.set_entry(&mut io, id, e).unwrap();
                if e.mapped() {
                    model.insert(id, e);
                } else {
                    model.remove(&id);
                }
            }
            // Lookups agree.
            let q = pick_id(&mut rng);
            assert_eq!(
                t.entry(&mut io, q).unwrap(),
                model.get(&q).copied().unwrap_or_default(),
                "seed {seed} step {step} id {q}"
            );
            let want_unmapped = (0..t.capacity()).find(|i| !model.contains_key(i));
            assert_eq!(t.find_unmapped(&mut io).unwrap(), want_unmapped, "seed {seed} step {step}");
            let from = pick_id(&mut rng);
            let want_free = model
                .range(from..)
                .find(|(_, e)| e.free_slots() > 0)
                .map(|(&i, _)| i);
            assert_eq!(t.find_free(&mut io, from).unwrap(), want_free, "seed {seed} step {step} from {from}");
            let (f, m) = model
                .values()
                .fold((0, 0), |(f, m), e| (f + e.free_slots(), m + 1));
            assert_eq!((t.free_slots(), t.mapped_chunks()), (f, m), "seed {seed} step {step}");
            // Commit now and then: every dirty node gets a fresh pair.
            if step % 97 == 96 {
                generation += 1;
                let mut homes = BTreeMap::new();
                for k in t.dirty_nodes() {
                    homes.insert(k, (next_home, next_home + 1));
                    next_home += 2;
                }
                let root = t.write(&mut io, generation, &homes).unwrap();
                io.write_batch().unwrap();
                // The old copies freed are exactly the written nodes'
                // previous homes that are no longer referenced.
                let now: BTreeSet<(u64, u64)> = t
                    .all_node_ptrs(&mut io)
                    .unwrap()
                    .iter()
                    .map(|(_, p)| (p.addr, p.aux))
                    .collect();
                let freed: BTreeSet<(u64, u64)> = t.pending_frees.drain(..).collect();
                let gone: BTreeSet<(u64, u64)> = prev_ptrs.difference(&now).copied().collect();
                assert_eq!(freed, gone, "seed {seed} gen {generation}");
                prev_ptrs = now;
                // Reopen from the device: identical contents and counts.
                let depth = t.depth();
                let mut u = ChunkTable::open(&mut io, total, shift, root, depth).unwrap();
                let all: Vec<(u64, Entry)> = model.iter().map(|(&i, &e)| (i, e)).collect();
                assert_eq!(u.all_entries(&mut io).unwrap(), all, "seed {seed} gen {generation}");
                assert_eq!((u.free_slots(), u.mapped_chunks()), (f, m));
                // verify_since reads exactly the nodes born at or after.
                let born_now = u
                    .all_node_ptrs(&mut io)
                    .unwrap()
                    .iter()
                    .filter(|(_, p)| p.birth >= generation)
                    .count() as u64;
                assert_eq!(u.verify_since(&mut io, generation).unwrap(), born_now);
                t = u;
            }
        }
        assert!(t.depth() >= 2, "seed {seed}: depth {}", t.depth());
        assert!(ids_at(t.depth()) > 130_000 || t.depth() == 3);
        let _ = LEAF_ENTRIES;
    }
}

#[test]
fn chunk_table_decoder_rejects_bad_entries() {
    // Hand-made leaves: a valid entry decodes; each kind of bad one is
    // refused as corruption (never a panic, never accepted).
    let total = 1000u64;
    let mut io = Io::new(MemDevice::new(total), Sealer::new([1; 16], None, &mut Rng::new(1)));
    let good = Entry {
        p0: 10,
        p1: 500,
        used: 0b101,
        len: 4,
    };
    let bads = [
        Entry { len: 5, ..good },                 // longer than the 4-block chunk size
        Entry { used: 0b10000, ..good },          // used bit past len
        Entry { p0: 1, ..good },                  // in the superblock area
        Entry { p1: 996, ..good },                // runs into the back slots
        Entry { p1: 12, ..good },                 // copies overlap
        Entry { p0: 0, p1: 0, used: 1, len: 0 },  // unmapped but not zero
    ];
    for (i, e) in core::iter::once(good).chain(bads).enumerate() {
        let mut t = ChunkTable::format(total, 2);
        t.set_entry(&mut io, 3, e).unwrap();
        let mut homes = BTreeMap::new();
        homes.insert((0, 0), (100, 200));
        let root = t.write(&mut io, 1, &homes).unwrap();
        io.write_batch().unwrap();
        let r = ChunkTable::open(&mut io, total, 2, root, 0);
        assert_eq!(r.is_ok(), i == 0, "case {i}: {e:?}");
    }
}

proptest::proptest! {
    #[test]
    fn chunk_node_decoder_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 4096..=4096), level in 0u32..3) {
        let total = 1u64 << 30;
        let mut io = Io::new(SparseDevice::new(total), Sealer::new([1; 16], None, &mut Rng::new(1)));
        let mut b = zero_block();
        b.copy_from_slice(&bytes);
        // Seal the random bytes as a valid-MAC node, so decoding (not the
        // MAC) is what gets exercised.
        let kind = if level == 0 { BlockKind::ChunkLeaf } else { BlockKind::ChunkTable };
        let p = io.write_dup(kind, 100, 100, 200, 1, b, 200);
        io.write_batch().unwrap();
        let _ = ChunkTable::open(&mut io, total, 6, p, level);
    }
}
