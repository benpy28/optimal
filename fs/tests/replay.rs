//! A power cut at any write point of a replay-counter bump leaves either the
//! old value or the new one, and once the bump returned, never the old one.

use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;

#[test]
fn bump_is_atomic_at_every_write_point() {
    let opts = FormatOptions {
        passphrase: Some(b"pw".to_vec()),
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
    };
    let mo = MountOptions {
        passphrase: Some(b"pw".to_vec()),
        quarantine_min_ns: 0,
        ..Default::default()
    };
    let build = || {
        let mut fs = Filesystem::format(
            CrashDevice::new(2048, 9),
            &opts,
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap();
        let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.write(f, 0, &[1u8; 9000]).unwrap();
        fs.commit().unwrap();
        fs.bump_replay().unwrap();
        fs
    };
    let start = build().device().write_points();
    let total = {
        let mut fs = build();
        fs.bump_replay().unwrap();
        fs.device().write_points() - start
    };
    assert!(total >= 6, "bump wrote only {total} points");
    for cut in 0..=total {
        for img_no in 0..8u64 {
            let mut fs = build();
            fs.device_mut().cut_at = Some(start + cut);
            let done = fs.bump_replay().is_ok();
            let mut dev = fs.into_device();
            // Odd images tear every superblock write that was in flight.
            let img = if img_no % 2 == 1 {
                dev.crash_image_tearing(&[0, 1, 2046, 2047])
            } else {
                dev.crash_image()
            };
            let fs = Filesystem::mount(
                CrashDevice::from_image(img, cut ^ (img_no << 8)),
                &mo,
                &mut Rng::new(3),
                Box::new(ZeroClock),
            )
            .unwrap_or_else(|e| panic!("cut {cut}: mount failed: {e:?}"));
            let v = fs.replay_value();
            assert!(v == 1 || v == 2, "cut {cut}: replay {v}");
            if done {
                assert_eq!(v, 2, "cut {cut}: bump returned but value rolled back");
            }
        }
    }
}
