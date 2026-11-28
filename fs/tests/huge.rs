//! Multi-terabyte volumes on a sparse device: format is O(1) in the size,
//! data lands in far regions, and everything survives a remount and fsck.

use optimalfs::sim::{Rng, SparseDevice};
use optimalfs::*;

fn opts() -> FormatOptions {
    FormatOptions {
        passphrase: None,
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
        intent_log_blocks: None,
    }
}

fn mount_o() -> MountOptions {
    MountOptions {
        quarantine_min_ns: 0,
        ..Default::default()
    }
}

fn exercise(blocks: u64) {
    let mut fs = Filesystem::format(
        SparseDevice::new(blocks),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    // Format touched only a handful of blocks however big the device is:
    // at most 2 copies x (tree leaf + root block + space root + 3 paths of
    // 4 tables and a bitmap, to the first block, to copy 1 of chunk 0 half
    // a device away, and to the last block) + the chunk leaf's 2 copies +
    // 2 superblocks = 2 x 18 + 2 + 2 = 40 (the zeroed slots aren't stored).
    assert!(
        fs.device().stored_blocks() <= 40,
        "{}",
        fs.device().stored_blocks()
    );
    assert_eq!(fs.statfs().total_blocks, blocks);
    // Files in the first region, the middle, the last bitmap and near the end.
    let spots = [0, blocks / 2, blocks - 40_000, blocks - 200];
    let mut files = Vec::new();
    for (i, &at) in spots.iter().enumerate() {
        fs.set_alloc_cursor_for_tests(at);
        let name = format!("f{i}");
        let f = fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
        let data: Vec<u8> = (0..300_000u32)
            .map(|x| (x as u8) ^ (i as u8 * 37 + 1))
            .collect();
        fs.write(f, 0, &data).unwrap();
        fs.commit().unwrap();
        files.push((name, data));
    }
    let r = fsck::check(&mut fs, true).unwrap();
    assert!(r.ok(), "{:?}", r.errors);
    let used = blocks - fs.statfs().free_blocks;
    let dev = fs.unmount().unwrap();
    let stored = dev.stored_blocks();
    assert!(stored < 2_000, "stored {stored}");
    let mut fs = Filesystem::mount(dev, &mount_o(), &mut Rng::new(2), Box::new(ZeroClock)).unwrap();
    assert_eq!(blocks - fs.statfs().free_blocks, used);
    for (name, data) in &files {
        let f = fs.lookup(ROOT_INO, name.as_bytes()).unwrap();
        let mut buf = vec![0u8; data.len()];
        assert_eq!(fs.read(f, 0, &mut buf).unwrap(), data.len());
        assert!(buf == *data, "{name}");
    }
    // Delete everything far away; the space comes back exactly.
    for (name, _) in &files {
        fs.unlink(ROOT_INO, name.as_bytes()).unwrap();
        fs.commit().unwrap();
    }
    let r = fsck::check(&mut fs, true).unwrap();
    assert!(r.ok(), "{:?}", r.errors);
}

#[test]
fn sixteen_tib() {
    exercise(1 << 32); // 16 TiB: three table levels
}

#[test]
fn odd_size_just_over_one_level() {
    exercise((1 << 21) + 12_345); // 8 GiB + a bit: two levels, ragged end
}

#[test]
fn largest_supported_and_one_past() {
    exercise(1 << 45); // 128 PiB: five levels
    let r = Filesystem::format(
        SparseDevice::new((1 << 45) + 1),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    );
    assert!(matches!(r, Err(Error::Unsupported(_))), "{:?}", r.err());
}

#[test]
fn format_footprint_hand_worked() {
    // Logical metadata after format = tree leaf + root block + space root
    // + the distinct tables and bitmaps on the paths to bitmap 0, to the
    // bitmap holding copy 1 of chunk 0 (half the usable area away) and to
    // the last bitmap. Each is stored twice, plus the chunk leaf's two
    // copies and the two superblock slots gen 1 writes. The intent log
    // ring (16384 blocks on volumes this big) sits at 2, before chunk 0,
    // and format writes none of it.
    //  16 TiB (3 levels): 3 paths x (2 tables + 1 bitmap) = 9, + 3 = 12.
    //  8 GiB + 12345 (2 levels): bitmaps 0, 32, 64; level-1 tables 0 and 1
    //   (bitmaps 0 and 32 share table 0): 3 + 2 = 5, + 3 = 8.
    //  128 PiB (5 levels): 3 x (4 + 1) = 15, + 3 = 18.
    for (blocks, logical) in [(1u64 << 32, 12u32), ((1 << 21) + 12_345, 8), (1 << 45, 18)] {
        let mut fs = Filesystem::format(
            SparseDevice::new(blocks),
            &opts(),
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap();
        let ch = fs.metadata_chunks().unwrap();
        assert_eq!(ch.len(), 1);
        let e = ch[0].1;
        assert_eq!(
            (e.p0, e.len, e.used.count_ones()),
            (2 + LOG_MAX_BLOCKS, 64, logical),
            "{blocks}"
        );
        assert_eq!(e.p1, e.p0 + (blocks - 4) / 2, "{blocks}");
        assert_eq!(
            fs.device().stored_blocks(),
            2 * logical as usize + 2 + 2,
            "{blocks}"
        );
    }
}
