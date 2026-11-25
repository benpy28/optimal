//! The previous commit must stay fully intact, so mount can fall back to it
//! when the newest superblock copies are damaged, even across a remount
//! and even after new data was written (but not committed) since.

use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;

fn mount(dev: CrashDevice, seed: u64) -> Filesystem<CrashDevice> {
    let mo = MountOptions {
        dirty_limit_bytes: 64 * 1024,
        quarantine_min_ns: 0,
        ..Default::default()
    };
    Filesystem::mount(dev, &mo, &mut Rng::new(seed), Box::new(ZeroClock)).unwrap()
}

#[test]
fn previous_commit_survives_remount_and_reuse() {
    let blocks = 1024u64;
    let opts = FormatOptions {
        passphrase: None,
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
    };
    let mut fs = Filesystem::format(
        CrashDevice::new(blocks, 1),
        &opts,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    let x = vec![0x11u8; 200_000];
    fs.write(f, 0, &x).unwrap();
    fs.commit().unwrap(); // generation T-1 holds X
    let prev = fs.generation();
    fs.write(f, 0, &vec![0x22u8; 200_000]).unwrap();
    fs.commit().unwrap(); // generation T frees X's blocks (quarantined)
    let newest = fs.generation();
    let mut dev = fs.into_device();
    dev.flush().unwrap();

    // Remount and fill the disk with uncommitted data. Without the free log,
    // the allocator would happily reuse X's blocks here.
    let mut fs = mount(dev, 2);
    let g = fs.create(ROOT_INO, b"g", 0o644).unwrap();
    let mut off = 0;
    while fs.write(g, off, &vec![0x33u8; 64 * 1024]).is_ok() && off < 3_000_000 {
        off += 64 * 1024;
    }
    let mut dev = fs.into_device();
    dev.flush().unwrap();

    // Damage both copies of the newest superblock.
    let mut img = dev.clean_image();
    let p = (newest % 2) as usize;
    for b in [p, blocks as usize - 1 - p] {
        img[b * 4096 + 100] ^= 0xff;
    }
    let mut fs = mount(CrashDevice::from_image(img, 3), 4);
    assert_eq!(
        fs.generation(),
        prev,
        "should have fallen back to the previous commit"
    );
    let f = fs.lookup(ROOT_INO, b"f").unwrap();
    let mut buf = vec![0u8; x.len()];
    assert_eq!(fs.read(f, 0, &mut buf).unwrap(), x.len());
    assert!(buf == x, "previous commit's data was overwritten");
    let r = fsck::check(&mut fs, true).unwrap();
    assert!(r.ok(), "{:?}", r.errors);
}
