//! Unit tests: hand-worked cases (the expected values are worked out in the
//! comments) and decoder fuzzing (random bytes must never cause a panic).

use crate::codec::Reader;
use crate::device::MemDevice;
use crate::fs::{Attr, Extent, Filesystem, FormatOptions, ROOT_INO, ZeroClock, decode_dirents};
use crate::ptr::BlockPtr;
use crate::rangeset::RangeSet;
use crate::sim::Rng;
use crate::tree::{Node, TreeId};
use alloc::boxed::Box;
use alloc::vec;
use proptest::prelude::*;

#[test]
fn rangeset_hand_worked() {
    let mut s = RangeSet::new();
    // [10,15) and [15,20) touch, so they merge into [10,20): one range, 10 blocks.
    assert!(s.insert(10, 5));
    assert!(s.insert(15, 5));
    assert_eq!(s.to_vec(), vec![(10, 10)]);
    assert_eq!(s.len(), 10);
    // Overlapping insert is refused and changes nothing.
    assert!(!s.insert(19, 3));
    assert_eq!(s.len(), 10);
    // Removing [12,14) splits it into [10,12) and [14,20): 2 + 6 = 8 blocks.
    assert!(s.remove(12, 2));
    assert_eq!(s.to_vec(), vec![(10, 2), (14, 6)]);
    assert_eq!(s.len(), 8);
    assert!(!s.remove(11, 2)); // block 12 is no longer present
    assert_eq!(s.next_range_from(12), Some((14, 20)));
    assert_eq!(s.next_range_from(11), Some((11, 12)));
    assert!(s.overlaps(5, 6) && !s.overlaps(5, 5) && !s.overlaps(12, 2));
}

#[test]
fn superblock_slots_hand_worked() {
    use crate::superblock::{slot_addr, slots_for_gen};
    // Generation 7 is odd, so it uses slots 1 and 3: block 1 and block total-2.
    assert_eq!(slots_for_gen(7), [1, 3]);
    assert_eq!(slot_addr(1, 1000), 1);
    assert_eq!(slot_addr(3, 1000), 998);
    // Generation 8 uses slots 0 and 2: block 0 and the last block.
    assert_eq!(slots_for_gen(8), [0, 2]);
    assert_eq!(slot_addr(2, 1000), 999);
}

#[test]
fn space_shape_hand_worked() {
    // 100000 blocks need ceil(100000 / 32768) = 4 bitmap blocks, all in one
    // level-1 table. Free = 100000 - 4 superblock slots = 99996.
    let s = crate::space::Space::format(100_000).unwrap();
    assert_eq!(s.free_blocks(), 99_996);
    assert_eq!(s.tables().2.len(), 4);
    assert_eq!(s.tables().1.len(), 1);
}

#[test]
fn extent_slice_hand_worked() {
    // An encrypted extent at block 100, 10 blocks, base nonce counter 50.
    // Its blocks 3..7 start at block 103, are 4 long, and their first nonce
    // counter is 50 + 3 = 53.
    let mut nonce = [7u8; 24];
    nonce[16..].copy_from_slice(&50u64.to_le_bytes());
    let macs: alloc::vec::Vec<[u8; 16]> = (0..10u8).map(|i| [i; 16]).collect();
    let e = Extent {
        phys: 100,
        len: 10,
        birth: 9,
        nonce,
        macs,
    };
    let enc = e.encode();
    assert_eq!(enc.len(), 48 + 10 * 16);
    assert_eq!(Extent::decode(&enc, 0).unwrap(), e);
    let sl = e.slice(3, 7);
    assert_eq!(sl.len, 4);
    assert_eq!(sl.phys, 103);
    assert_eq!(u64::from_le_bytes(sl.nonce[16..].try_into().unwrap()), 53);
    assert_eq!(sl.macs[0], [3u8; 16]);
    // Plain volumes use all-zero nonces, which stay zero.
    assert_eq!(crate::crypto::nonce_add(&[0u8; 24], 5), [0u8; 24]);
}

#[test]
fn fresh_volume_space_hand_worked() {
    // A fresh 4096-block volume uses: 4 superblock slots + 1 space root
    // table + 1 level-1 table + 1 bitmap (4096 < 32768) + 1 tree leaf
    // (the root inode) = 8 blocks. The meta tree is empty and nothing was
    // freed, so there is no free log.
    let mut rng = Rng::new(3);
    let opts = FormatOptions {
        passphrase: None,
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
    };
    let mut fs =
        Filesystem::format(MemDevice::new(4096), &opts, &mut rng, Box::new(ZeroClock)).unwrap();
    assert_eq!(fs.statfs().free_blocks, 4096 - 8);
    // Writing 10000 bytes adds ceil(10000/4096) = 3 data blocks. The commit
    // rewrites the leaf, the bitmap, the L1 table and the root table (their
    // old copies are freed but quarantined) and writes 1 free-log block for
    // those frees. Used: 4 + 3 tables/bitmap + 1 leaf + 3 data + 1 log = 12.
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, &[5u8; 10_000]).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.statfs().free_blocks, 4096 - 12);
    // Quarantine: the 4 freed blocks can't be allocated until the next commit.
    let st = fs.statfs();
    assert_eq!(fs.store.space.allocatable_blocks(), st.free_blocks - 4);
}

#[test]
fn seal_binds_kind_address_birth_and_volume() {
    // A sealed block must only open with the exact kind, address, birth and
    // volume it was sealed for. Checked for both plain and encrypted volumes.
    let master = crate::crypto::SecretKey([9u8; 32]);
    for key in [None, Some(&master)] {
        let mut rng = Rng::new(1);
        let mut s = crate::crypto::Sealer::new([1u8; 16], key, &mut rng);
        let other = crate::crypto::Sealer::new([2u8; 16], key, &mut rng);
        let mut b = vec![0x5au8; 4096];
        let (nonce, mac) = s.seal(crate::crypto::BlockKind::Node, 77, 5, &mut b);
        use crate::crypto::BlockKind::*;
        for (kind, addr, birth) in [(Data, 77, 5), (Node, 78, 5), (Node, 77, 4), (Node, 77, 6)] {
            let mut c = b.clone();
            assert!(
                s.open(kind, addr, birth, &nonce, &mac, &mut c).is_err(),
                "{kind:?} {addr} {birth}"
            );
        }
        let mut c = b.clone();
        assert!(other.open(Node, 77, 5, &nonce, &mac, &mut c).is_err());
        let mut c = b.clone();
        c[4095] ^= 1;
        assert!(s.open(Node, 77, 5, &nonce, &mac, &mut c).is_err());
        let mut c = b.clone();
        s.open(Node, 77, 5, &nonce, &mac, &mut c).unwrap();
        assert!(c.iter().all(|&x| x == 0x5a));
        // Encrypted volumes really encrypt.
        assert_eq!(key.is_some(), b.iter().any(|&x| x != 0x5a));
    }
}

#[test]
fn tree_shrinks_after_mass_delete() {
    // Insert 20000 small items, delete all but every 20th (1000 left), then
    // commit. 1000 items of 19 + 8 = 27 bytes are 27000 bytes; with every
    // leaf at least a quarter full (1020 bytes) that is at most 27 leaves,
    // plus at most a few internal nodes. Without rebalancing, the ~180
    // original leaves would mostly survive half-empty.
    use crate::tree::{Key, NodeCache, Store, Tree};
    let mut rng = Rng::new(5);
    let io = crate::io::Io::new(
        MemDevice::new(4096),
        crate::crypto::Sealer::new([0; 16], None, &mut rng),
    );
    let mut st = Store {
        io,
        space: crate::space::Space::format(4096).unwrap(),
        cache: NodeCache::new(64),
        latest_snap_gen: 0,
        dead_pending: vec![],
    };
    let mut t = Tree::new(TreeId::Meta, BlockPtr::NULL).unwrap();
    for i in 0..20_000u64 {
        t.insert(&mut st, Key::new(i, 1, 0), i.to_le_bytes().to_vec())
            .unwrap();
    }
    t.write(&mut st, 1).unwrap();
    st.io.write_batch().unwrap();
    for i in 0..20_000u64 {
        if i % 20 != 0 {
            assert!(t.remove(&mut st, &Key::new(i, 1, 0)).unwrap());
        }
    }
    let root = t.write(&mut st, 2).unwrap();
    st.io.write_batch().unwrap();
    let mut nodes = 0;
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        nodes += 1;
        let n = st.read_node(TreeId::Meta, &p).unwrap();
        for (_, c) in &n.kids {
            if let crate::tree::Child::Clean(cp) = c {
                stack.push(*cp);
            }
        }
    }
    assert!(nodes <= 27 + 3, "{nodes} nodes for 1000 items");
    for i in (0..20_000u64).step_by(20) {
        assert_eq!(
            t.get(&mut st, &Key::new(i, 1, 0)).unwrap(),
            Some(i.to_le_bytes().to_vec())
        );
    }
}

fn ptr_level(level: u32) -> BlockPtr {
    BlockPtr {
        addr: 5,
        birth: 3,
        mac: [0; 16],
        nonce: [0; 24],
        aux: level,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4200), level in 0u32..20) {
        let _ = Node::decode(&bytes, TreeId::Fs, &ptr_level(level));
        let _ = Node::decode(&bytes, TreeId::Meta, &ptr_level(level));
        let _ = Extent::decode(&bytes, 1);
        let _ = decode_dirents(&bytes, 1);
        let _ = Attr::decode(1, &bytes);
        let _ = crate::snapshot::decode_dead(&bytes, 1);
        let _ = crate::snapshot::SnapRecord::decode(1, &bytes);
        let _ = BlockPtr::decode(&mut Reader::new(&bytes, "t", 0));
    }

    /// Structured mutation: a valid node with a few bytes changed must decode
    /// to an error or to a node that still satisfies the decode invariants.
    #[test]
    fn node_mutations_never_panic(seed in any::<u64>(), flips in 1usize..16) {
        let mut r = Rng::new(seed);
        let mut node = Node { level: 0, ..Default::default() };
        for i in 0..(1 + r.below(40)) {
            let mut v = vec![0u8; r.below(80) as usize];
            r.fill(&mut v);
            node.items.push((crate::tree::Key::new(i, 1, r.below(5)), v));
        }
        let b = node.encode(TreeId::Fs, 3);
        let mut b = b.to_vec();
        for _ in 0..flips {
            let i = r.below(b.len() as u64) as usize;
            b[i] ^= 1 << r.below(8);
        }
        if let Ok(n) = Node::decode(&b, TreeId::Fs, &ptr_level(0)) {
            for w in n.items.windows(2) {
                prop_assert!(w[0].0 < w[1].0);
            }
        }
    }
}
