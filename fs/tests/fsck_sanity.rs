use optimalfs::sim::Rng;
use optimalfs::*;

#[test]
fn fsck_reports_and_detects() {
    let mut rng = Rng::new(1);
    let opts = FormatOptions {
        passphrase: None,
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
    };
    let mut fs =
        Filesystem::format(MemDevice::new(4096), &opts, &mut rng, Box::new(ZeroClock)).unwrap();
    let f = fs.create(ROOT_INO, b"a", 0o644).unwrap();
    fs.write(f, 0, &vec![7u8; 40_000]).unwrap();
    fs.snapshot_create(b"s").unwrap();
    fs.write(f, 0, &vec![8u8; 20_000]).unwrap();
    fs.commit().unwrap();
    let r = fsck::check(&mut fs, true).unwrap();
    println!("{r:?}");
    assert!(r.ok());
    assert!(r.data_blocks_verified >= 10, "{r:?}");
    assert_eq!(r.snapshots, 1);
    // Corrupt every block that holds file data and check fsck notices.
    // (Clean unmount, so mount skips its recovery check, which would
    // otherwise fall back to a commit from before the data was written.)
    let mut dev = fs.unmount().unwrap();
    let img = dev.image_mut();
    let mut hits = 0;
    for b in 0..4096 {
        let blk = &mut img[b * 4096..(b + 1) * 4096];
        if blk.iter().all(|&x| x == 7) {
            blk[100] ^= 1;
            hits += 1;
        }
    }
    assert!(hits > 0);
    let mut fs =
        Filesystem::mount(dev, &MountOptions::default(), &mut rng, Box::new(ZeroClock)).unwrap();
    assert_eq!(fs.fell_back_from(), None);
    let r = fsck::check(&mut fs, true).unwrap();
    println!("{:?}", r.errors);
    assert!(!r.ok());
}
