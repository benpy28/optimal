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
    // 100000 blocks need ceil(100000 / 32768) = 4 bitmaps, which fit under
    // one level-1 table (64 bitmaps = 2^21 blocks), so depth is 1. Only the
    // bitmaps holding superblock slots are materialised: bitmap 0 (blocks
    // 0, 1) and bitmap 3 (99998 / 32768 = 3, blocks 99998, 99999).
    // Root free counts: 32768-2, 32768, 32768, 100000-3*32768-2 = 1694,
    // then zero for the 60 bitmaps past the end. Sum 99996 = 100000 - 4.
    use crate::space::Space;
    let s = Space::format(100_000).unwrap();
    assert_eq!(s.free_blocks(), 99_996);
    assert_eq!(s.levels(), 1);
    let (tables, bmps, kids) = s.shape();
    assert_eq!((tables, bmps), (1, 2));
    assert_eq!(&kids[..5], &[32_766, 32_768, 32_768, 1_694, 0]);
    assert!(kids[5..].iter().all(|&k| k == 0));
    // Depth by size: one level covers 2^21 blocks (8 GiB), each extra level 64x.
    assert_eq!(Space::levels_for(1 << 21).unwrap(), 1);
    assert_eq!(Space::levels_for((1 << 21) + 1).unwrap(), 2);
    assert_eq!(Space::levels_for(1 << 32).unwrap(), 3); // 16 TiB
    assert_eq!(Space::levels_for(1 << 37).unwrap(), 4); // 512 TiB
    assert_eq!(Space::levels_for(1 << 45).unwrap(), 5); // 128 PiB
    assert!(Space::levels_for((1 << 45) + 1).is_err());
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
    // A fresh 4096-block volume needs one bitmap (4096 < 32768) directly
    // under the root table (depth 1). It uses 4 superblock slots + 1 root
    // table + 1 bitmap + 1 tree leaf (the root inode) = 7 blocks. The meta
    // tree is empty and nothing was freed, so there is no free log.
    let mut rng = Rng::new(3);
    let opts = FormatOptions {
        passphrase: None,
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
    };
    let mut fs =
        Filesystem::format(MemDevice::new(4096), &opts, &mut rng, Box::new(ZeroClock)).unwrap();
    assert_eq!(fs.statfs().free_blocks, 4096 - 7);
    assert!(
        fs.store.space.held.is_empty(),
        "{:?} gen {}",
        fs.store.space.held,
        fs.generation()
    );
    // Writing 10000 bytes adds ceil(10000/4096) = 3 data blocks. The commit
    // rewrites the leaf, the bitmap and the root table (old copies freed but
    // quarantined) and writes 1 free-log block recording those 3 frees.
    // Used: 4 + 2 + 1 leaf + 3 data + 1 log = 11.
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, &[5u8; 10_000]).unwrap();
    fs.commit().unwrap();
    let g = fs.generation();
    assert_eq!(fs.statfs().free_blocks, 4096 - 11);
    assert_eq!(fs.store.space.allocatable_blocks(), 4096 - 11 - 3);
    let first = fs.store.space.held.front().unwrap().clone();
    assert_eq!((first.generation, first.set.len()), (g, 3));
    // The batch from commit g stays held through commits g+1 .. g+7 and is
    // released by commit g+8 (QUARANTINE_COMMITS = 8).
    for k in 1..=8u64 {
        fs.mark_dirty();
        fs.commit().unwrap();
        assert_eq!(fs.generation(), g + k);
        let still = fs.store.space.held.iter().any(|h| h.generation == g);
        assert_eq!(still, k < 8, "after commit g+{k}");
        // Every held batch is younger than 8 commits.
        assert!(fs.store.space.held.iter().all(|h| g + k - h.generation < 8));
    }
}

#[test]
fn quarantine_time_window_hand_worked() {
    // A clock that ticks 10 s per reading, with a 30 s window: a batch
    // freed at commit g (time t) needs both g+8 commits and t+30 s. Commits
    // are > 10 s apart here, so the commit count is the binding limit; with
    // a 200 s window the time is, and the batch must outlive commit g+8.
    struct Tick(i64);
    impl crate::fs::Clock for Tick {
        fn now_ns(&mut self) -> i64 {
            self.0 += 10_000_000_000;
            self.0
        }
    }
    for (window_s, expect_release_at) in [(30i64, 8u64), (200, 0)] {
        let mut rng = Rng::new(4);
        let opts = FormatOptions {
            passphrase: None,
            kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
            quarantine_min_ns: window_s * 1_000_000_000,
        };
        let mut fs =
            Filesystem::format(MemDevice::new(4096), &opts, &mut rng, Box::new(Tick(0))).unwrap();
        let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.write(f, 0, &[1u8; 100]).unwrap();
        fs.commit().unwrap();
        let g = fs.generation();
        let t0 = fs.body.commit_time_ns;
        let mut released = 0;
        for k in 1..=30u64 {
            fs.mark_dirty();
            fs.commit().unwrap();
            if released == 0 && !fs.store.space.held.iter().any(|h| h.generation == g) {
                released = k;
                assert!(fs.body.commit_time_ns - t0 >= window_s * 1_000_000_000);
            }
        }
        if expect_release_at != 0 {
            assert_eq!(released, expect_release_at);
        } else {
            assert!(
                released > 8,
                "time window must hold past 8 commits, got {released}"
            );
        }
    }
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

fn ptr_level(level: u64) -> BlockPtr {
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
    fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4200), level in 0u64..20) {
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

#[test]
fn kdf_for_ram_hand_worked() {
    // memory = clamp(RAM/4, 64 MiB, 1 GiB); passes = ceil(4 GiB-passes / memory),
    // clamped to 4..=64 (in KiB: 4194304 / mem_kib).
    use crate::KdfParams;
    let gib = 1u64 << 30;
    let cases = [
        (16 * gib, 1_048_576, 4), // capped at 1 GiB
        (4 * gib, 1_048_576, 4),
        (3 * gib, 786_432, 6), // 4194304 / 786432 = 5.33 -> 6
        (2 * gib, 524_288, 8),
        (gib, 262_144, 16),
        (128 << 20, 65_536, 64), // RAM/4 = 32 MiB, raised to the 64 MiB floor
        (0, 65_536, 64),
    ];
    for (ram, mem, it) in cases {
        let p = KdfParams::for_ram(ram);
        assert_eq!(
            (p.mem_kib, p.iterations, p.lanes),
            (mem, it, 1),
            "ram {ram}"
        );
        assert!(p.meets_floor());
    }
    assert_eq!(KdfParams::for_ram(16 * gib), KdfParams::DEFAULT);
    assert!(!KdfParams::INSECURE_FAST_FOR_TESTS.meets_floor());
    // Format refuses an encrypted volume below the floor.
    let weak = FormatOptions {
        passphrase: Some(b"pw".to_vec()),
        kdf: KdfParams {
            mem_kib: 32 * 1024,
            iterations: 4,
            lanes: 1,
        },
        quarantine_min_ns: 0,
    };
    let r = Filesystem::format(
        MemDevice::new(4096),
        &weak,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    );
    assert!(matches!(r, Err(crate::Error::Invalid(_))));
}

#[test]
fn replay_bump_reaches_all_four_slots() {
    // After bump_replay every superblock copy carries the new value: the
    // commit is written to its own pair, flushed, then to the other pair.
    let opts = FormatOptions {
        passphrase: None,
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
    };
    let mut fs = Filesystem::format(
        MemDevice::new(4096),
        &opts,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    assert_eq!(fs.replay_value(), 0);
    fs.create(ROOT_INO, b"x", 0o644).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.bump_replay().unwrap(), 1);
    assert_eq!(fs.bump_replay().unwrap(), 2);
    // An ordinary commit carries the value forward.
    fs.create(ROOT_INO, b"y", 0o644).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.replay_value(), 2);
    assert_eq!(fs.bump_replay().unwrap(), 3);
    let g = fs.generation();
    let mut dev = fs.into_device();
    let cands = crate::superblock::candidates(&mut dev).unwrap();
    assert_eq!(cands.len(), 4);
    let sealer = crate::crypto::Sealer::new(cands[0].header.uuid, None, &mut Rng::new(2));
    for c in &cands {
        let b = crate::superblock::open_body(c, &sealer).unwrap();
        assert_eq!(
            (b.generation, b.replay_value),
            (g, 3),
            "slot {}",
            c.header.slot
        );
        // Its history is the previous commits, newest first, with their values.
        assert_eq!(b.history[0].replay_value, 2);
        assert_eq!(b.history[0].generation, g - 1);
    }
}

#[test]
fn header_bytes_identical_in_every_slot_and_commit() {
    // The plaintext header (bytes 0..512) must not change per slot or per
    // commit: the volume tag and the pre-cap reader rely on fixed bytes.
    // It changes only when a key slot does.
    let opts = FormatOptions {
        passphrase: Some(b"pw".to_vec()),
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
    };
    let mut fs = Filesystem::format(
        MemDevice::new(4096),
        &opts,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    let headers = |fs: &mut Filesystem<MemDevice>| -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
        let img = fs.device_mut().image_mut();
        [0usize, 1, 4094, 4095]
            .iter()
            .map(|&b| img[b * 4096..b * 4096 + 512].to_vec())
            .collect()
    };
    for i in 0..5u8 {
        fs.create(ROOT_INO, &[b'a' + i], 0o644).unwrap();
        fs.commit().unwrap();
    }
    let h1 = headers(&mut fs);
    assert!(h1.iter().all(|h| *h == h1[0]), "slots differ");
    // Bytes 424..512 are reserved and zero.
    assert!(h1[0][424..].iter().all(|&x| x == 0));
    fs.create(ROOT_INO, b"z", 0o644).unwrap();
    fs.commit().unwrap();
    assert_eq!(headers(&mut fs)[0], h1[0], "a commit changed the header");
}

#[test]
fn feature_bits_hand_worked() {
    // Unknown read-only-compatible bit: read-only mount works, read-write
    // is refused. Unknown incompatible bit: refused, reported as such.
    let opts = FormatOptions {
        passphrase: None,
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
    };
    for (ro, inc) in [(1u32, 0u32), (0, 1)] {
        let mut fs = Filesystem::format(
            MemDevice::new(4096),
            &opts,
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap();
        fs.header.ro_compat = ro;
        fs.header.incompat = inc;
        fs.mark_dirty();
        fs.commit().unwrap();
        fs.mark_dirty();
        fs.commit().unwrap(); // both pairs carry the bit
        let dev = fs.into_device();
        let rw = crate::MountOptions {
            quarantine_min_ns: 0,
            ..Default::default()
        };
        let ro_opts = crate::MountOptions {
            read_only: true,
            ..rw.clone()
        };
        let r = Filesystem::mount(dev.clone(), &rw, &mut Rng::new(2), Box::new(ZeroClock));
        assert!(
            matches!(r, Err(crate::Error::Unsupported(_))),
            "{:?}",
            r.err()
        );
        let r = Filesystem::mount(dev, &ro_opts, &mut Rng::new(2), Box::new(ZeroClock));
        assert_eq!(r.is_ok(), inc == 0, "{:?}", r.err());
    }
}
