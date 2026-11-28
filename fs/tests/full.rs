//! A full volume refuses new work cheaply and recovers once space is freed.

use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;

fn format(blocks: u64, enc: bool) -> Filesystem<CrashDevice> {
    let opts = FormatOptions {
        passphrase: enc.then(|| b"pw".to_vec()),
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
        intent_log_blocks: None,
    };
    Filesystem::format(
        CrashDevice::new(blocks, 1),
        &opts,
        &mut Rng::new(7),
        Box::new(ZeroClock),
    )
    .unwrap()
}

/// Fills a small volume with one file, then asks for more until a
/// create is refused. The first refusal may cycle up to K + 1 commits to
/// drain quarantined blocks. Asking again with nothing changed must fail at
/// once without writing (it used to cycle K + 1 empty commits per call, so
/// a program retrying on a full disk kept the device busy for nothing).
/// After the big file is deleted, creates work again.
#[test]
fn repeated_refusals_on_a_full_volume_write_nothing() {
    for (blocks, enc) in [1024u64, 2048, 4096, 16384]
        .into_iter()
        .flat_map(|b| [(b, false), (b, true)])
    {
        let mut fs = format(blocks, enc);
        let big = fs.create(ROOT_INO, b"big", 0o644).unwrap();
        let mut off = 0u64;
        while fs.write(big, off, &[0x5a; 16 * 1024]).is_ok() {
            off += 16 * 1024;
            assert!(off < blocks * 8192, "{blocks} enc {enc}: never filled");
        }
        fs.commit().unwrap();
        let mut n = 0u32;
        let first = loop {
            match fs.create(ROOT_INO, format!("f{n}").as_bytes(), 0o644) {
                Ok(_) => n += 1,
                Err(e) => break e,
            }
            assert!(
                n < 100_000,
                "{blocks} enc {enc}: creates never ran out of space"
            );
        };
        assert_eq!(first, Error::NoSpace, "{blocks} enc {enc}");
        assert!(!fs.has_room_for(0, 8, false).unwrap(), "{blocks} enc {enc}");
        eprintln!(
            "FILL {blocks} enc {enc}: data blocks {} of {blocks}",
            off / 4096
        );
        let before = fs.device().write_points();
        let g = fs.generation();
        for i in 0..5 {
            assert_eq!(
                fs.create(ROOT_INO, format!("again{i}").as_bytes(), 0o644),
                Err(Error::NoSpace),
                "{blocks} enc {enc}"
            );
            assert_eq!(fs.mkdir(ROOT_INO, b"d", 0o755), Err(Error::NoSpace));
            assert_eq!(fs.symlink(ROOT_INO, b"s", b"t"), Err(Error::NoSpace));
        }
        assert_eq!(
            fs.device().write_points(),
            before,
            "{blocks} enc {enc}: refusals with nothing changed wrote to the device"
        );
        assert_eq!(fs.generation(), g, "{blocks} enc {enc}");

        // Freeing space makes the volume usable again.
        fs.unlink(ROOT_INO, b"big").unwrap();
        fs.create(ROOT_INO, b"after", 0o644).unwrap();
        fs.commit().unwrap();
        let r = fsck::check(&mut fs, true).unwrap();
        assert!(r.ok(), "{blocks} enc {enc}: {:?}", r.errors);
    }
}

/// Seeded random work on small, mostly full volumes: creates, writes,
/// overwrites, unlinks and commits. Ordinary operations may be refused with
/// `NoSpace`, but only when statfs shows the volume full; unlink and commit
/// are never refused; nothing ever poisons the volume; and fsck stays clean.
#[test]
fn seeded_full_volume_churn_never_poisons() {
    let scale = std::env::var("OFS_TORTURE_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1u64);
    let base: u64 = std::env::var("OFS_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x0f5_2026);
    for i in 0..4 * scale {
        let seed = base ^ (i * 0x9e37_79b9) ^ 0xf011;
        let blocks = [1024u64, 1536, 2048, 4096][(i % 4) as usize];
        let enc = i % 2 == 1;
        let mut fs = format(blocks, enc);
        let mut r = Rng::new(seed);
        let mut names: Vec<Vec<u8>> = Vec::new();
        let (mut refused, mut deleted) = (0u32, 0u32);
        for step in 0..1500 {
            let ctx = || format!("seed {seed:#x} blocks {blocks} enc {enc} step {step}");
            match r.below(10) {
                0..=3 => {
                    let name = format!("f{}", r.below(128)).into_bytes();
                    match fs.create(ROOT_INO, &name, 0o644) {
                        Ok(_) => names.push(name),
                        Err(Error::Exists) => {}
                        Err(Error::NoSpace) => {
                            refused += 1;
                            assert!(
                                !fs.has_room_for(0, 8, false).unwrap(),
                                "{}: create refused with room",
                                ctx()
                            );
                        }
                        Err(e) => panic!("{}: create: {e:?}", ctx()),
                    }
                }
                4..=7 if !names.is_empty() => {
                    let name = &names[r.below(names.len() as u64) as usize];
                    let ino = fs.lookup(ROOT_INO, name).unwrap();
                    let len = 1 + r.below(200_000) as usize;
                    match fs.write(ino, r.below(1 << 20), &vec![step as u8; len]) {
                        Ok(_) => {}
                        Err(Error::NoSpace) => refused += 1,
                        Err(e) => panic!("{}: write: {e:?}", ctx()),
                    }
                }
                8 if !names.is_empty() => {
                    let name = names.swap_remove(r.below(names.len() as u64) as usize);
                    fs.unlink(ROOT_INO, &name)
                        .unwrap_or_else(|e| panic!("{}: unlink: {e:?}", ctx()));
                    deleted += 1;
                }
                _ => fs
                    .commit()
                    .unwrap_or_else(|e| panic!("{}: commit: {e:?}", ctx())),
            }
        }
        fs.commit().unwrap();
        let rep = fsck::check(&mut fs, true).unwrap();
        assert!(rep.ok(), "seed {seed:#x}: {:?}", rep.errors);
        assert!(
            refused > 0 && deleted > 0,
            "seed {seed:#x}: the volume never filled"
        );
    }
}

/// Below the minimum size the space kept back would leave too little to
/// use, so format refuses it by name.
#[test]
fn format_refuses_volumes_below_the_minimum() {
    let opts = FormatOptions {
        passphrase: None,
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
        intent_log_blocks: None,
    };
    let r = Filesystem::format(
        CrashDevice::new(MIN_BLOCKS - 1, 1),
        &opts,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    );
    assert!(
        matches!(r, Err(Error::Invalid(m)) if m.contains("too small")),
        "{r:?}"
    );
    let mut fs = format(MIN_BLOCKS, false);
    fs.create(ROOT_INO, b"ok", 0o644).unwrap();
    fs.commit().unwrap();
}

/// Metadata-only pressure: fill a small volume with empty files until
/// creates are refused, then delete every one of them. Deletes free almost
/// no data, so each one mostly costs fresh metadata blocks and the volume
/// sits at the delete floor; every delete must still succeed (none may be
/// refused or poison the volume), and afterwards creates work again.
#[test]
fn deleting_everything_on_a_metadata_full_volume_always_works() {
    for (blocks, enc) in [1024u64, 2048, 4096]
        .into_iter()
        .flat_map(|b| [(b, false), (b, true)])
    {
        let mut fs = format(blocks, enc);
        // Some data first, so the metadata limit is reached sooner.
        let big = fs.create(ROOT_INO, b"big", 0o644).unwrap();
        let mut off = 0u64;
        while fs.has_room_for(16, 9, false).unwrap() {
            fs.write(big, off, &[7u8; 4096 * 16]).unwrap();
            off += 4096 * 16;
        }
        let mut n = 0u32;
        loop {
            match fs.create(ROOT_INO, format!("e{n}").as_bytes(), 0o644) {
                Ok(_) => n += 1,
                Err(Error::NoSpace) => break,
                Err(e) => panic!("{blocks} enc {enc}: create: {e:?}"),
            }
            assert!(n < 200_000);
        }
        assert!(n > 10, "{blocks} enc {enc}: only {n} files fit");
        for i in 0..n {
            fs.unlink(ROOT_INO, format!("e{i}").as_bytes())
                .unwrap_or_else(|e| panic!("{blocks} enc {enc}: unlink e{i} of {n}: {e:?}"));
        }
        fs.commit().unwrap();
        fs.create(ROOT_INO, b"again", 0o644)
            .unwrap_or_else(|e| panic!("{blocks} enc {enc}: create after deleting: {e:?}"));
        fs.commit().unwrap();
        let r = fsck::check(&mut fs, true).unwrap();
        assert!(r.ok(), "{blocks} enc {enc}: {:?}", r.errors);
    }
}
