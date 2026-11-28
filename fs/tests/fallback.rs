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

/// After a fallback mount, the very first commit must be written even when
/// nothing changed, and it must outrank the skipped commit. Otherwise the
/// skipped superblock copies stay the newest on disk, every later mount has
/// to verify and skip them again, and the promise that the first commit
/// rewrites all four slots is not kept. Found by the nightly soak
/// (lying.rs, seed 0x4240e0dac68, lies 4, image 1: 20 random ops that
/// happened to change nothing, then a commit that did not run).
#[test]
fn first_commit_after_fallback_outranks_the_skipped_commit() {
    let blocks = 1024u64;
    for enc in [false, true] {
        let opts = FormatOptions {
            passphrase: enc.then(|| b"pw".to_vec()),
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
        fs.write(f, 0, &[0x11u8; 5000]).unwrap();
        fs.commit().unwrap();
        let prev = fs.generation();
        let mut dev = fs.into_device();
        dev.flush().unwrap();
        let old = dev.clean_image();

        let mut fs = mount_enc(dev, 2, enc);
        let f = fs.lookup(ROOT_INO, b"f").unwrap();
        fs.write(f, 0, &[0x22u8; 5000]).unwrap();
        fs.commit().unwrap();
        let newest = fs.generation();
        assert!(newest > prev);
        let mut dev = fs.into_device();
        dev.flush().unwrap();
        let new = dev.clean_image();

        // Only the newest commit's superblock pair reached the disk; every
        // other block it wrote was lost, so its tree fails its MACs.
        let mut img = old.clone();
        let p = (newest % 2) as usize;
        for b in [p, blocks as usize - 1 - p] {
            img[b * 4096..(b + 1) * 4096].copy_from_slice(&new[b * 4096..(b + 1) * 4096]);
        }
        let mut fs = mount_enc(CrashDevice::from_image(img, 3), 4, enc);
        assert_eq!(fs.generation(), prev, "enc {enc}: should fall back");
        assert_eq!(fs.fell_back_from(), Some(newest), "enc {enc}");

        // No changes at all, then a commit.
        fs.commit().unwrap();
        assert!(
            fs.generation() > newest,
            "enc {enc}: generation {} after the first commit, skipped {newest}",
            fs.generation()
        );
        let g = fs.generation();
        let mut dev = fs.into_device();
        dev.flush().unwrap();
        let fs = mount_enc(dev, 5, enc);
        assert_eq!(
            fs.generation(),
            g,
            "enc {enc}: remount picked another commit"
        );
        assert_eq!(fs.fell_back_from(), None, "enc {enc}: stale copies remain");
    }
}

fn mount_enc(dev: CrashDevice, seed: u64, enc: bool) -> Filesystem<CrashDevice> {
    let mo = MountOptions {
        passphrase: enc.then(|| b"pw".to_vec()),
        dirty_limit_bytes: 64 * 1024,
        quarantine_min_ns: 0,
        ..Default::default()
    };
    Filesystem::mount(dev, &mo, &mut Rng::new(seed), Box::new(ZeroClock)).unwrap()
}
