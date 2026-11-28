//! Unit tests for the safeguards of the write-failure rebuild that the
//! workload tests in `tests/write_fail.rs` can't reach: each injects the
//! exact condition its safeguard exists for.

use crate::fs::{Filesystem, FormatOptions, ROOT_INO, ZeroClock};
use crate::sim::{CrashDevice, Rng};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

fn opts() -> FormatOptions {
    FormatOptions {
        passphrase: None,
        kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
    }
}

fn fresh() -> Filesystem<CrashDevice> {
    Filesystem::format(
        CrashDevice::new(4096, 9),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap()
}

/// Generation and slot of every superblock copy on `dev`.
fn slots(dev: &mut CrashDevice) -> Vec<(u32, u64)> {
    let cands = crate::superblock::candidates(dev).unwrap();
    let sealer = crate::crypto::Sealer::new(cands[0].header.uuid, None, &mut Rng::new(2));
    let mut v: Vec<(u32, u64)> = cands
        .iter()
        .map(|c| {
            (
                c.header.slot,
                crate::superblock::open_body(c, &sealer).unwrap().generation,
            )
        })
        .collect();
    v.sort();
    v
}

/// The commit's second superblock write fails once, after the first one
/// reached slot 1 or 0 with generation g+1. That copy may survive on disk
/// with roots the retry frees and reuses, so the retry must be numbered
/// g+2 (never a second, different g+1) and must overwrite every slot that
/// still takes writes (so no stale g+1 copy outranks older good ones). The
/// slot whose write failed is not written again on this mount; the next
/// mount tries it again.
#[test]
fn failed_superblock_write_numbers_above_it_and_writes_around_the_slot() {
    let setup = || {
        let mut fs = fresh();
        fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.commit().unwrap();
        let ino = fs.create(ROOT_INO, b"b", 0o644).unwrap();
        fs.write(ino, 0, &[7u8; 10_000]).unwrap();
        fs
    };
    // Reference run: find the commit's superblock writes.
    let mut fs = setup();
    let g = fs.generation();
    let first = fs.device().writes;
    fs.device_mut().trace = Some(Vec::new());
    fs.commit().unwrap();
    let trace = fs.device_mut().trace.take().unwrap();
    let total = 4096;
    let addr_slot = |b: u64| [0, 1, total - 1, total - 2].iter().position(|&a| a == b);
    let sb: Vec<(u64, u32)> = trace
        .iter()
        .enumerate()
        .filter_map(|(i, (b, _))| addr_slot(*b).map(|s| (first + i as u64, s as u32)))
        .collect();
    assert_eq!(sb.len(), 2, "an ordinary commit writes its own pair");
    let dead = sb[1].1;

    let mut fs = setup();
    fs.device_mut().faults.fail_write_at = Some(sb[1].0);
    fs.commit().unwrap();
    assert_eq!(
        fs.generation(),
        g + 2,
        "retry numbered above the failed commit"
    );
    assert_eq!(fs.dead_slots, 1 << dead);
    // Later commits on this mount write two live slots each, never the dead one.
    for _ in 0..3 {
        fs.create(ROOT_INO, format!("c{}", fs.generation()).as_bytes(), 0o644)
            .unwrap();
        fs.device_mut().trace = Some(Vec::new());
        fs.commit().unwrap();
        let t = fs.device_mut().trace.take().unwrap();
        let written: Vec<u32> = t
            .iter()
            .filter_map(|(b, _)| addr_slot(*b).map(|s| s as u32))
            .collect();
        assert_eq!(written.len(), 2, "two copies of every commit: {written:?}");
        assert!(!written.contains(&dead));
    }
    let gen_now = fs.generation();
    let mut dev = fs.into_device();
    let v = slots(&mut dev);
    for (s, sg) in &v {
        if *s == dead {
            assert!(*sg < g + 2, "dead slot {s} was written: {v:?}");
        }
    }
    assert_eq!(
        v.iter().filter(|(_, x)| *x == gen_now).count(),
        2,
        "newest commit on two slots: {v:?}"
    );
    let mut fs = Filesystem::mount(
        dev,
        &crate::fs::MountOptions::default(),
        &mut Rng::new(3),
        Box::new(ZeroClock),
    )
    .unwrap();
    assert_eq!(fs.generation(), gen_now);
    let ino = fs.lookup(ROOT_INO, b"b").unwrap();
    let mut buf = vec![0u8; 10_000];
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 10_000);
    assert!(buf.iter().all(|&x| x == 7));
    // A new mount tries every slot again (the failure was transient).
    assert_eq!(fs.dead_slots, 0);
    fs.create(ROOT_INO, b"after", 0o644).unwrap();
    fs.commit().unwrap();
    fs.create(ROOT_INO, b"after2", 0o644).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.dead_slots, 0);
}

/// Blocks in the bad set are never reported as available, free or not:
/// 64 bad blocks lower `available_blocks` by exactly 64.
#[test]
fn bad_blocks_are_not_available() {
    let mut fs = fresh();
    let before = fs.statfs().available_blocks;
    fs.store.space.bad.add(1000, 64);
    assert_eq!(fs.statfs().available_blocks, before - 64);
    // And the allocator skips them: fill the 64 blocks around them.
    let ino = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.set_alloc_cursor_for_tests(990);
    fs.write(ino, 0, &[1u8; 40 * 4096]).unwrap();
    fs.commit().unwrap();
    let mut buf = [0u8; 4096];
    for b in 1000..1064 {
        fs.store.io.read_raw(b, &mut buf).unwrap();
        assert!(buf.iter().all(|&x| x == 0), "bad block {b} was written");
    }
}

/// A rebuild in the middle of a transaction (no commit attempted) must
/// leave the space map exactly as it was: same free and allocatable
/// counts, same quarantine. The last commit released old batches from
/// quarantine (more than K commits have freed blocks), which the reopened
/// map must release too.
#[test]
fn rebuild_reproduces_the_space_map_exactly() {
    let mut fs = fresh();
    for i in 0..(crate::space::QUARANTINE_COMMITS + 4) {
        let name = alloc::format!("f{i}");
        let ino = fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
        fs.write(ino, 0, &[3u8; 20_000]).unwrap();
        if i > 0 {
            let old = alloc::format!("f{}", i - 1);
            fs.unlink(ROOT_INO, old.as_bytes()).unwrap();
        }
        fs.commit().unwrap();
    }
    // An open transaction with frees and early-flushed data.
    fs.unlink(ROOT_INO, b"f11").ok();
    let ino = fs.create(ROOT_INO, b"new", 0o644).unwrap();
    fs.write(ino, 0, &[5u8; 30_000]).unwrap();
    fs.flush_data_for_tests().unwrap();
    let state = |fs: &Filesystem<CrashDevice>| {
        (
            fs.store.space.free_blocks(),
            fs.store.space.allocatable_blocks(),
            fs.store.space.held_ranges(),
            fs.store.space.freed_now.to_vec(),
        )
    };
    let before = state(&fs);
    let g = fs.txn_generation();
    fs.rebuild(g, false, Vec::new()).unwrap();
    assert_eq!(state(&fs), before);
    fs.commit().unwrap();
    assert!(crate::fsck::check(&mut fs, true).unwrap().ok());
}

/// After a failed commit no cached node is one the failed commit wrote
/// (they are dirty again; a cached copy would only waste memory, or worse
/// be found under a reused address).
#[test]
fn failed_commit_leaves_no_written_node_in_the_cache() {
    let mut fs = fresh();
    for i in 0..50u32 {
        let name = alloc::format!("n{i}");
        fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
    }
    let g = fs.txn_generation();
    // Every write fails, so the commit gives up (after its attempts wrote
    // nodes into memory each time).
    fs.device_mut().faults.all_writes_fail = true;
    assert!(fs.commit().is_err());
    assert!(fs.degraded());
    assert!(
        !fs.store.cache.births().contains(&g),
        "a node written by the failed commit is still cached"
    );
}

/// A device that fails every write: one `commit` call makes exactly
/// COMMIT_ATTEMPTS attempts, then gives up degraded; a second call makes
/// as many again.
#[test]
fn commit_attempts_are_bounded() {
    let mut fs = fresh();
    fs.create(ROOT_INO, b"a", 0o644).unwrap();
    fs.device_mut().faults.all_writes_fail = true;
    assert!(fs.commit().is_err());
    assert_eq!(fs.last_commit_attempts, crate::fs::COMMIT_ATTEMPTS);
    assert!(fs.degraded());
    assert!(fs.commit().is_err());
    assert_eq!(fs.last_commit_attempts, crate::fs::COMMIT_ATTEMPTS);
}

/// Once the bad set passes its limit (1/64 of the volume, at least 256
/// blocks), a failed attempt is not retried: the volume degrades at once.
/// Below the limit the same failure is retried and the commit succeeds.
#[test]
fn bad_set_limit_stops_retries() {
    for (preset, want_ok) in [(100u64, true), (300, false)] {
        let mut fs = fresh();
        for i in 0..20u32 {
            let name = alloc::format!("n{i}");
            fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
        }
        fs.store.space.bad.add(3000, preset);
        let first = fs.device().writes;
        fs.device_mut().faults.fail_write_at = Some(first);
        fs.device_mut().faults.fail_write_persists = true;
        let r = fs.commit();
        assert_eq!(r.is_ok(), want_ok, "preset {preset}: {r:?}");
        if want_ok {
            assert_eq!(fs.last_commit_attempts, 2);
        } else {
            assert_eq!(fs.last_commit_attempts, 1);
            assert!(fs.degraded());
        }
    }
}
