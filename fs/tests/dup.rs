//! Metadata stored twice: every metadata block has two physical copies
//! found through the chunk table. One bad copy anywhere must never lose or
//! change anything: reads fall back to the other copy, the bad one is
//! reported and rewritten, and a moved chunk keeps older commits readable.

mod common;
use common::*;
use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;

const BS: usize = 4096;

fn cfg() -> GenCfg {
    GenCfg {
        max_write: 40_000,
        max_offset: 200_000,
        remounts: false,
        ..Default::default()
    }
}

/// A populated volume (committed) and its harness.
fn populated(seed: u64, enc: bool, blocks: u64, chunk: u32, ops: usize) -> Harness {
    let mut h = Harness::new_chunked(blocks, enc, seed, chunk).unwrap();
    let mut r = Rng::new(seed);
    for _ in 0..ops {
        let op = gen_op(&mut r, &h.model, &cfg());
        h.apply(&op).unwrap();
    }
    h.apply(&Op::Commit).unwrap();
    h
}

/// (logical, copy 0, copy 1) of every used metadata block.
fn meta_blocks(fs: &mut Filesystem<CrashDevice>) -> Vec<(u64, u64, u64)> {
    let mut out = Vec::new();
    for (id, e) in fs.metadata_chunks().unwrap() {
        for off in 0..e.len as u64 {
            if e.is_used(off) {
                let l = fs.metadata_address(id, off);
                let (p0, p1) = fs.resolve_metadata(l).unwrap();
                assert_eq!((p0, p1), (e.p0 + off, e.p1 + off));
                out.push((l, p0, p1));
            }
        }
    }
    out
}

fn damage(r: &mut Rng, img: &mut [u8], b: u64) {
    let blk = b as usize * BS..(b as usize + 1) * BS;
    match r.below(3) {
        0 => {
            let i = blk.start + r.below(BS as u64) as usize;
            img[i] ^= 1 << r.below(8);
        }
        1 => img[blk].fill(0),
        _ => r.fill(&mut img[blk]),
    }
}

fn remount(img: Vec<u8>, enc: bool, seed: u64) -> Filesystem<CrashDevice> {
    Filesystem::mount(
        CrashDevice::from_image(img, seed),
        &mount_opts(enc),
        &mut Rng::new(seed ^ 0x77),
        Box::new(ZeroClock),
    )
    .unwrap_or_else(|e| panic!("seed {seed:#x}: mount failed: {e:?}"))
}

#[test]
fn every_copy_pair_differs() {
    // Drives that deduplicate or compress could merge identical copies; no
    // two copies may be equal, and they should differ almost everywhere
    // (a random mask matches a byte 1 time in 256: about 16 per block).
    for (i, enc) in [false, true].into_iter().enumerate() {
        let mut h = populated(0xd1ff + i as u64, enc, 3000, 0, 400);
        let blocks = meta_blocks(h.fs());
        assert!(blocks.len() > 8, "{}", blocks.len());
        let img = h.fs().device().clean_image();
        let pairs = blocks
            .iter()
            .map(|&(_, a, b)| (a, b))
            .chain(h.fs().chunk_table_blocks_for_tests().unwrap());
        for (a, b) in pairs {
            let x = &img[a as usize * BS..][..BS];
            let y = &img[b as usize * BS..][..BS];
            let same = x.iter().zip(y).filter(|(p, q)| p == q).count();
            assert!(
                same < 100,
                "enc {enc}: copies at {a} and {b} share {same} bytes"
            );
        }
    }
}

#[test]
fn bit_flip_in_copy1_is_found_by_recovery_and_repaired() {
    // The design review's case: one flipped bit in copy 1 of a block the
    // last commit wrote. The unclean mount reads both copies of recent
    // blocks, uses copy 0, reports copy 1 and rewrites it at the next commit.
    for seed in 0..8u64 {
        let enc = seed % 2 == 1;
        let mut h = populated(0xb17 + seed, enc, 2000, chunk_for_seed(seed), 60);
        // The last commit changes the tree, so its root is recent too.
        h.fs().create(ROOT_INO, b"last", 0o644).unwrap();
        h.fs().unlink(ROOT_INO, b"last").unwrap();
        h.fs().commit().unwrap();
        let (root_l, tree_l) = h.fs().roots_for_tests();
        let target = if seed % 4 < 2 { root_l } else { tree_l };
        let (p0, p1) = h.fs().resolve_metadata(target).unwrap();
        let mut img = h.fs().device().clean_image();
        let mut r = Rng::new(seed);
        let bit = r.below(8 * BS as u64) as usize;
        img[p1 as usize * BS + bit / 8] ^= 1 << (bit % 8);
        let damaged = img[p1 as usize * BS..(p1 as usize + 1) * BS].to_vec();
        let model = h.model.clone();
        let fs = remount(img, enc, seed);
        let st = fs
            .recovery_stats()
            .expect("unclean mount runs recovery")
            .clone();
        assert_eq!(st.bad_copies, 1, "seed {seed}: {st:?}");
        assert_eq!(
            fs.fell_back_from(),
            None,
            "one bad copy is no reason to fall back"
        );
        let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
        rh.verify(&model);
        let rep = fsck::check(rh.fs(), true).unwrap();
        assert!(rep.ok(), "{:?}", rep.errors);
        assert_eq!((rep.bad_copies, rep.repairs_pending), (1, 1));
        rh.fs().commit().unwrap();
        let rep = fsck::check(rh.fs(), true).unwrap();
        assert!(rep.ok() && rep.bad_copies == 0, "{:?}", rep.errors);
        assert_eq!(rh.fs().io_stats().copies_repaired, 1);
        // Repaired by moving copy 1's run, never in place: copy 0 stays,
        // copy 1 is elsewhere (still in the other half), and the damaged
        // block is untouched (quarantined for the commits that map it).
        let (q0, q1) = rh.fs().resolve_metadata(target).unwrap();
        assert_eq!(q0, p0, "seed {seed}");
        assert_ne!(q1, p1, "seed {seed}: copy 1 moved");
        let total = rh.fs().device().clean_image().len() as u64 / BS as u64;
        assert!(q0.abs_diff(q1) >= (total - 4) / 4, "seed {seed}: {q0} {q1}");
        let now = rh.fs().device().clean_image();
        assert_eq!(&now[p1 as usize * BS..(p1 as usize + 1) * BS], &damaged[..]);
    }
}

#[test]
fn one_bad_copy_of_every_block_loses_nothing() {
    // Damage one copy (chosen at random) of every metadata block and of
    // every chunk-table block. The volume must mount, read back exactly,
    // and a scrub must find exactly the damaged copies and repair them all.
    for seed in 0..6u64 {
        let enc = seed % 2 == 0;
        let mut h = populated(0x0bad + seed, enc, 3000, chunk_for_seed(seed + 1), 150);
        let model = h.model.clone();
        // Unmount cleanly, then list the blocks of that final commit.
        let dev = h.fs.take().unwrap().unmount().unwrap();
        let mut fs = remount(dev.clean_image(), enc, seed);
        let mut pairs: Vec<(u64, u64)> = meta_blocks(&mut fs)
            .iter()
            .map(|&(_, a, b)| (a, b))
            .collect();
        pairs.extend(fs.chunk_table_blocks_for_tests().unwrap());
        let mut img = fs.into_device().clean_image();
        let mut r = Rng::new(seed ^ 0x5eed);
        for &(a, b) in &pairs {
            let which = if r.chance(1, 2) { a } else { b };
            damage(&mut r, &mut img, which);
        }
        let fs = remount(img, enc, seed);
        let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
        rh.verify(&model);
        let sc = fsck::scrub_metadata(rh.fs()).unwrap();
        // A flipped bit could in principle land where it changes nothing?
        // No: every bit of a copy is covered by the MAC, so all are found.
        assert_eq!(sc.bad_copies, pairs.len() as u64, "seed {seed}");
        // Each is repaired, or the repair commit itself stopped using the
        // block first (a bitmap rewritten by an earlier move) and it needed
        // none. Never both, never neither.
        assert_eq!(sc.repaired + sc.dropped, pairs.len() as u64, "seed {seed}");
        assert!(
            sc.repaired >= pairs.len() as u64 * 3 / 4,
            "seed {seed}: {sc:?}"
        );
        assert_eq!(sc.unreadable, 0);
        let rep = fsck::check(rh.fs(), true).unwrap();
        assert!(
            rep.ok() && rep.bad_copies == 0,
            "seed {seed}: {:?}",
            rep.errors
        );
        // Remount: everything still there, nothing bad left.
        let dev = rh.fs.take().unwrap().unmount().unwrap();
        let fs = remount(dev.clean_image(), enc, seed + 100);
        let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
        rh.verify(&model);
        let sc = fsck::scrub_metadata(rh.fs()).unwrap();
        assert_eq!((sc.bad_copies, sc.repaired, sc.dropped), (0, 0, 0));
    }
}

#[test]
fn both_copies_bad_never_returns_wrong_data() {
    // Damaging both copies of one metadata block is beyond duplication:
    // mount may fall back or fail, and reads may fail, but nothing read
    // may ever differ from a committed state.
    for seed in 0..16u64 {
        let enc = seed % 2 == 1;
        let mut h = populated(0xb0b0 + seed, enc, 2500, chunk_for_seed(seed), 100);
        let committed = h.committed.clone();
        let blocks = meta_blocks(h.fs());
        let mut img = h.fs().device().clean_image();
        let mut r = Rng::new(seed);
        let (_, a, b) = blocks[r.below(blocks.len() as u64) as usize];
        damage(&mut r, &mut img, a);
        damage(&mut r, &mut img, b);
        let Ok(mut fs) = Filesystem::mount(
            CrashDevice::from_image(img, seed),
            &MountOptions {
                read_only: true,
                ..mount_opts(enc)
            },
            &mut Rng::new(seed),
            Box::new(ZeroClock),
        ) else {
            continue;
        };
        let want = committed
            .get(&fs.generation())
            .expect("mounted a generation that was committed")
            .live
            .clone();
        for (&ino, n) in &want.nodes {
            if n.kind == FileKind::File {
                let mut buf = vec![0u8; n.data.len()];
                if let Ok(len) = fs.read(ino, 0, &mut buf) {
                    assert_eq!(&buf[..len], &n.data[..len], "seed {seed}: wrong data");
                }
            }
        }
    }
}

#[test]
fn relocated_chunks_read_back_and_old_runs_stay_quarantined() {
    for seed in 0..4u64 {
        let enc = seed % 2 == 1;
        let chunk = [0, 4, 1, 16][seed as usize];
        let mut h = populated(0x4e10 + seed, enc, 4000, chunk, 120);
        let model = h.model.clone();
        let before = h.fs().metadata_chunks().unwrap();
        let old_img = h.fs().device().clean_image();
        let avail0 = h.fs().statfs().available_blocks;
        for (id, _) in &before {
            h.fs().relocate_metadata_chunk(*id).unwrap();
        }
        assert_eq!(
            h.fs().relocate_metadata_chunk(1 << 40),
            Err(Error::NotFound)
        );
        h.fs().commit().unwrap();
        let after = h.fs().metadata_chunks().unwrap();
        let img = h.fs().device().clean_image();
        for (id, e) in &before {
            // Every chunk still mapped moved both copies; the old runs
            // still hold their old bytes (quarantined, not reused).
            if let Some((_, n)) = after.iter().find(|(j, _)| j == id) {
                assert!(
                    n.p0 != e.p0 && n.p1 != e.p1,
                    "seed {seed} chunk {id} did not move"
                );
                for off in 0..e.len as usize {
                    for p in [e.p0 as usize + off, e.p1 as usize + off] {
                        assert_eq!(img[p * BS..][..BS], old_img[p * BS..][..BS], "seed {seed}");
                    }
                }
            }
        }
        // The old runs are quarantined, so less is available for now.
        assert!(h.fs().statfs().available_blocks < avail0);
        h.verify(&model);
        h.fsck();
        // After K = 8 more commits the old runs are free again.
        for _ in 0..9 {
            h.fs().commit_forced_for_tests().unwrap();
        }
        assert!(
            h.fs().statfs().available_blocks + 64 >= avail0,
            "seed {seed}"
        );
        let dev = h.fs.take().unwrap().into_device();
        let fs = remount(dev.clean_image(), enc, seed);
        let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
        rh.verify(&model);
        rh.fsck();
    }
}

#[test]
fn fallback_reads_each_commit_through_its_own_chunk_table() {
    // Commit A, then commit B that moves every chunk. If B's new runs are
    // lost (a lying drive), mount must fall back to A and read A's
    // metadata through A's own chunk table, at the old (quarantined) runs.
    for seed in 0..6u64 {
        let enc = seed % 2 == 0;
        let mut h = populated(0xfa11 + seed, enc, 3000, chunk_for_seed(seed), 80);
        let ga = h.fs().generation();
        let model_a = h.model.clone();
        for (id, _) in h.fs().metadata_chunks().unwrap() {
            h.fs().relocate_metadata_chunk(id).unwrap();
        }
        h.fs().commit().unwrap();
        let gb = h.fs().generation();
        assert_eq!(gb, ga + 1);
        let moved = h.fs().metadata_chunks().unwrap();
        let mut img = h.fs().device().clean_image();
        for (_, e) in &moved {
            for p in [e.p0, e.p1] {
                img[p as usize * BS..(p + e.len as u64) as usize * BS].fill(0);
            }
        }
        let fs = remount(img, enc, seed);
        assert_eq!(fs.generation(), ga, "seed {seed}");
        let mut rh = Harness::from_mounted(fs, model_a.clone(), enc, seed);
        rh.verify(&model_a);
        rh.fsck();
        // And it keeps working from there.
        let mut r = Rng::new(seed);
        for _ in 0..30 {
            let op = gen_op(&mut r, &rh.model, &cfg());
            rh.apply(&op).unwrap();
        }
        rh.apply(&Op::Commit).unwrap();
        rh.fsck();
    }
}

#[test]
fn emptied_chunks_are_released() {
    // Grow the metadata over many small chunks, delete everything, and let
    // the quarantine pass: the chunks go back to free space.
    let mut h = Harness::new_chunked(16000, false, 31, 2).unwrap();
    let free0 = h.fs().statfs().free_blocks;
    let chunks0 = h.fs().metadata_chunks().unwrap().len();
    for i in 0..3000 {
        let name = format!("file-{i:04}");
        let f = h.fs().create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
        h.fs().write(f, 0, &[i as u8; 300]).unwrap();
        if i % 500 == 499 {
            h.fs().commit().unwrap();
        }
    }
    h.fs().commit().unwrap();
    let grown = h.fs().metadata_chunks().unwrap().len();
    assert!(grown > chunks0 + 10, "{chunks0} -> {grown}");
    for i in 0..3000 {
        let name = format!("file-{i:04}");
        h.fs().unlink(ROOT_INO, name.as_bytes()).unwrap();
    }
    h.fs().commit().unwrap();
    for _ in 0..12 {
        h.fs().commit_forced_for_tests().unwrap();
    }
    let left = h.fs().metadata_chunks().unwrap().len();
    eprintln!("chunks: {chunks0} at format, {grown} grown, {left} left");
    // What stays mapped: chunks holding the live metadata or a slot still
    // in quarantine from the last K commits.
    assert!(left * 3 <= grown, "{grown} chunks shrank only to {left}");
    // Physical space comes back with them: 2 copies of 2 blocks per chunk.
    let free = h.fs().statfs().free_blocks;
    assert!(
        free + 4 * left as u64 + 64 >= free0,
        "free {free} vs {free0}"
    );
    h.fsck();
}

#[test]
fn fragmented_free_space_gets_shorter_chunks() {
    // With no free run of 64 blocks left, new chunks are shorter (down to
    // one block per copy) instead of failing while space remains.
    let mut h = Harness::new_chunked(4096, false, 77, 64).unwrap();
    let mut names = Vec::new();
    for i in 0.. {
        let name = format!("f{i:05}");
        let Ok(f) = h.fs().create(ROOT_INO, name.as_bytes(), 0o644) else {
            break;
        };
        if h.fs().write(f, 0, &[1u8; 4096]).is_err() {
            break;
        }
        names.push(name);
        if i % 50 == 49 && h.fs().commit().is_err() {
            break;
        }
    }
    let _ = h.fs().commit();
    // Free every other file: free space is now holes of about one block.
    for n in names.iter().step_by(2) {
        h.fs().unlink(ROOT_INO, n.as_bytes()).unwrap();
    }
    h.fs().commit().unwrap();
    for _ in 0..10 {
        h.fs().commit_forced_for_tests().unwrap();
    }
    // More metadata: a directory full of names.
    let d = h.fs().mkdir(ROOT_INO, b"d", 0o755).unwrap();
    for i in 0..600 {
        let name = format!("a-fairly-long-name-to-fill-tree-leaves-{i:05}");
        if h.fs().create(d, name.as_bytes(), 0o644).is_err() {
            break;
        }
    }
    h.fs().commit().unwrap();
    let short = h
        .fs()
        .metadata_chunks()
        .unwrap()
        .iter()
        .filter(|(_, e)| e.len < 64)
        .count();
    assert!(
        short > 0,
        "expected shortened chunks on a fragmented device"
    );
    h.fsck();
}

#[test]
fn moving_any_single_chunk_keeps_its_slot_map_exact() {
    // Each chunk moved on its own, on a fresh mount (so the space map is
    // clean and marking the new runs rewrites a bitmap, freeing the old
    // bitmap block, which may sit in the very chunk being moved). The
    // chunk's used-slot map must end exactly as fsck derives it.
    for seed in 0..4u64 {
        let enc = seed % 2 == 0;
        let chunk = [1, 4, 0, 2][seed as usize];
        let mut h = populated(0x5107 + seed, enc, 3000, chunk, 80);
        let model = h.model.clone();
        let img = h.fs.take().unwrap().unmount().unwrap().clean_image();
        let ids: Vec<u64> = remount(img.clone(), enc, seed)
            .metadata_chunks()
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert!(ids.len() >= 2, "seed {seed}");
        for id in ids {
            let fs = remount(img.clone(), enc, seed);
            let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
            rh.fs().relocate_metadata_chunk(id).unwrap();
            rh.fs().commit().unwrap();
            let rep = fsck::check(rh.fs(), true).unwrap();
            assert!(rep.ok(), "seed {seed} chunk {id}: {:?}", rep.errors);
            rh.verify(&model);
        }
    }
}

#[test]
fn a_bad_chunk_table_copy_alone_is_repaired_by_rewriting_it() {
    // Only one copy of one chunk-table block is damaged, so nothing else
    // in the repairing commit touches the table: the repair itself must
    // rewrite that block to a new pair, leaving the damaged one in place.
    for seed in 0..6u64 {
        let enc = seed % 2 == 1;
        // Chunk size 1 on a larger volume makes a deeper table.
        let chunk = [1, 1, 4, 0, 1, 2][seed as usize];
        let mut h = populated(0x7ab1e + seed, enc, 6000, chunk, 200);
        let model = h.model.clone();
        let img = h.fs.take().unwrap().unmount().unwrap().clean_image();
        let mut fs = remount(img.clone(), enc, seed);
        let pairs = fs.chunk_table_blocks_for_tests().unwrap();
        let mut r = Rng::new(seed);
        let (a, b) = pairs[r.below(pairs.len() as u64) as usize];
        let bad = if r.chance(1, 2) { a } else { b };
        let mut img = img;
        damage(&mut r, &mut img, bad);
        let damaged = img[bad as usize * BS..][..BS].to_vec();
        let fs = remount(img, enc, seed);
        let mut rh = Harness::from_mounted(fs, model.clone(), enc, seed);
        let sc = fsck::scrub_metadata(rh.fs()).unwrap();
        assert_eq!(
            (sc.bad_copies, sc.repaired, sc.dropped),
            (1, 1, 0),
            "seed {seed}"
        );
        let rep = fsck::check(rh.fs(), true).unwrap();
        assert!(
            rep.ok() && rep.bad_copies == 0,
            "seed {seed}: {:?}",
            rep.errors
        );
        let now = rh.fs().chunk_table_blocks_for_tests().unwrap();
        assert!(
            !now.iter().any(|&(x, y)| x == bad || y == bad),
            "seed {seed}: still used"
        );
        let img = rh.fs().device().clean_image();
        assert_eq!(
            &img[bad as usize * BS..][..BS],
            &damaged[..],
            "never written in place"
        );
        rh.verify(&model);
    }
}
