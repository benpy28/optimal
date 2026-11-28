//! Unit tests for writing around dead superblock slots and for undoing
//! operations, each with the exact condition it guards against.

use crate::error::Error;
use crate::fs::{Filesystem, FormatOptions, MountOptions, ROOT_INO, ZeroClock};
use crate::sim::{CrashDevice, Rng};
use crate::superblock::{plan_waves, slot_addr};
use alloc::boxed::Box;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

const TOTAL: u64 = 4096;

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
        CrashDevice::new(TOTAL, 9),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap()
}

fn remount(fs: Filesystem<CrashDevice>) -> Filesystem<CrashDevice> {
    let dev = fs.into_device();
    Filesystem::mount(
        dev,
        &MountOptions::default(),
        &mut Rng::new(3),
        Box::new(ZeroClock),
    )
    .unwrap()
}

/// Superblock slots written by the next commit, in order.
fn commit_slots(fs: &mut Filesystem<CrashDevice>) -> crate::Result<Vec<u32>> {
    fs.device_mut().trace = Some(Vec::new());
    let r = fs.commit();
    let t = fs.device_mut().trace.take().unwrap();
    r?;
    Ok(t.iter()
        .filter_map(|(b, _)| (0..4u32).find(|&s| slot_addr(s, TOTAL) == *b))
        .collect())
}

/// Hand-worked plans. Even generations own slots (0, 2), odd ones (1, 3).
#[test]
fn plan_waves_hand_cases() {
    let one = |v: &[u32]| v.iter().map(|&s| vec![s]).collect::<Vec<_>>();
    // All slots working: own pair in one wave; all slots front then back.
    assert_eq!(plan_waves(4, false, 0).unwrap(), vec![vec![0, 2]]);
    assert_eq!(plan_waves(5, false, 0).unwrap(), vec![vec![1, 3]]);
    assert_eq!(
        plan_waves(5, true, 0).unwrap(),
        vec![vec![0, 1], vec![2, 3]]
    );
    // Slot 0 dead. Gen 4 owns (0, 2): 2, then the other pair's 1 to make two.
    assert_eq!(plan_waves(4, false, 0b0001).unwrap(), one(&[2, 1]));
    // Gen 5 owns (1, 3), both live: still one slot per wave.
    assert_eq!(plan_waves(5, false, 0b0001).unwrap(), one(&[1, 3]));
    // All live slots, own first.
    assert_eq!(plan_waves(4, true, 0b0001).unwrap(), one(&[2, 1, 3]));
    // Both front slots dead: the back pair, one at a time.
    assert_eq!(plan_waves(4, false, 0b0011).unwrap(), one(&[2, 3]));
    assert_eq!(plan_waves(5, false, 0b0011).unwrap(), one(&[3, 2]));
    // One live slot: refused, nothing written.
    assert_eq!(plan_waves(4, false, 0b0111), Err(Error::DeviceFailing));
    assert_eq!(plan_waves(4, true, 0b1110), Err(Error::DeviceFailing));
}

/// A slot that never takes writes again: the commit that finds it retries
/// around it, every later commit lands on two other slots, and a remount
/// (which forgets dead slots) finds it again the same way.
#[test]
fn a_dead_slot_is_written_around() {
    let mut fs = fresh();
    let dead = 2u32;
    fs.device_mut()
        .faults
        .bad_blocks
        .insert(slot_addr(dead, TOTAL));
    for i in 0..8 {
        fs.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        let w = commit_slots(&mut fs).unwrap();
        assert!(!fs.degraded());
        assert_eq!(fs.dead_slots, 1 << dead);
        if fs.last_commit_attempts == 1 {
            assert_eq!(w.len(), 2, "commit {i}: {w:?}");
        }
        assert!(i == 0 || fs.last_commit_attempts == 1, "commit {i} retried");
    }
    let g = fs.generation();
    let mut fs = remount(fs);
    assert_eq!(fs.generation(), g);
    for i in 0..8 {
        assert!(fs.lookup(ROOT_INO, format!("f{i}").as_bytes()).is_ok());
    }
    fs.create(ROOT_INO, b"again", 0o644).unwrap();
    fs.commit().unwrap();
    fs.create(ROOT_INO, b"again2", 0o644).unwrap();
    fs.commit().unwrap();
    assert_eq!(fs.dead_slots, 1 << dead);
    let mut fs = remount(fs);
    fs.lookup(ROOT_INO, b"again2").unwrap();
}

/// Both front slots dead: commits go to the back pair, one slot per
/// flushed wave, and the volume mounts at the newest commit.
#[test]
fn two_dead_slots_leave_two() {
    let mut fs = fresh();
    for s in [0, 1] {
        fs.device_mut()
            .faults
            .bad_blocks
            .insert(slot_addr(s, TOTAL));
    }
    for i in 0..6 {
        fs.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        let w = commit_slots(&mut fs).unwrap();
        if fs.last_commit_attempts == 1 {
            let mut s = w.clone();
            s.sort();
            assert_eq!(s, vec![2, 3], "commit {i}");
        }
    }
    assert_eq!(fs.dead_slots, 0b0011);
    let g = fs.generation();
    let mut fs = remount(fs);
    assert_eq!(fs.generation(), g);
    fs.lookup(ROOT_INO, b"f5").unwrap();
}

/// Three dead slots: commits stop (degraded) instead of overwriting the
/// one copy left in place; changes stay readable in memory; once the
/// device takes writes again a commit (which tries every slot again)
/// lands them.
#[test]
fn three_dead_slots_degrade_until_they_work() {
    let mut fs = fresh();
    fs.create(ROOT_INO, b"kept", 0o644).unwrap();
    fs.commit().unwrap();
    for s in [0, 1, 2] {
        fs.device_mut()
            .faults
            .bad_blocks
            .insert(slot_addr(s, TOTAL));
    }
    let ino = fs.create(ROOT_INO, b"pending", 0o644).unwrap();
    fs.write(ino, 0, b"in memory").unwrap();
    assert!(fs.commit().is_err());
    assert!(fs.degraded());
    assert!(!fs.poisoned());
    assert_eq!(fs.dead_slots & 0b0111, 0b0111);
    assert_eq!(
        fs.create(ROOT_INO, b"refused", 0o644),
        Err(Error::DeviceFailing)
    );
    let mut buf = [0u8; 9];
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 9);
    assert_eq!(&buf, b"in memory");
    fs.device_mut().faults.bad_blocks.clear();
    fs.commit().unwrap();
    assert!(!fs.degraded());
    assert_eq!(fs.dead_slots, 0);
    let mut fs = remount(fs);
    let ino = fs.lookup(ROOT_INO, b"pending").unwrap();
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 9);
    assert_eq!(&buf, b"in memory");
    fs.lookup(ROOT_INO, b"kept").unwrap();
}

/// A read failing inside an unlink after the file's data was released:
/// undo brings the data back (its blocks allocated again) and fsck finds
/// the space map exact.
#[test]
fn failed_unlink_keeps_the_file_and_its_blocks() {
    let mut fs = fresh();
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let ino = fs.create(ROOT_INO, b"big", 0o644).unwrap();
    fs.write(ino, 0, &data).unwrap();
    for i in 0..300 {
        fs.create(ROOT_INO, format!("pad{i}").as_bytes(), 0o644)
            .unwrap();
    }
    fs.commit().unwrap();
    let mut undone = 0;
    // Fail each read point of the unlink in turn (two reads in a row).
    for k in 0..40 {
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.unlink(ROOT_INO, b"big");
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => break,
            Err(Error::Io(_)) => {
                undone += 1;
                assert!(!fs.poisoned());
                assert_eq!(fs.lookup(ROOT_INO, b"big"), Ok(ino));
                let mut buf = vec![0u8; data.len()];
                assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), data.len());
                assert!(buf == data, "k {k}: data changed");
                fs.commit().unwrap();
                let rep = crate::fsck::check(&mut fs, true).unwrap();
                assert!(rep.ok(), "k {k}: {:#?}", rep.errors);
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    assert!(undone >= 3, "only {undone} failures");
    assert_eq!(fs.lookup(ROOT_INO, b"big"), Err(Error::NotFound));
    fs.commit().unwrap();
    let rep = crate::fsck::check(&mut fs, true).unwrap();
    assert!(rep.ok(), "{:#?}", rep.errors);
}

/// A truncate that fails part way leaves the dirty data alone: blocks past
/// the new end and the tail are dropped or zeroed only on success.
#[test]
fn failed_truncate_leaves_dirty_data() {
    let mut fs = fresh();
    let ino = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    let committed: Vec<u8> = (0..100_000u32).map(|i| (i % 13) as u8).collect();
    fs.write(ino, 0, &committed).unwrap();
    for i in 0..300 {
        fs.create(ROOT_INO, format!("pad{i}").as_bytes(), 0o644)
            .unwrap();
    }
    fs.commit().unwrap();
    // Uncommitted overwrite past the cut, and one block of new data.
    fs.write(ino, 50_000, &[9u8; 20_000]).unwrap();
    let mut want = committed.clone();
    want[50_000..70_000].fill(9);
    let mut failures = 0;
    for k in 0..40 {
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.truncate(ino, 30_001);
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => break,
            Err(Error::Io(_)) => {
                failures += 1;
                assert!(!fs.poisoned());
                let mut buf = vec![0u8; want.len()];
                assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), want.len());
                assert!(buf == want, "k {k}: contents changed");
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    assert!(failures >= 1, "no failure injected");
    let mut buf = vec![0u8; 40_000];
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 30_001);
    assert_eq!(&buf[..30_001], &want[..30_001]);
    fs.write(ino, 40_000, b"x").unwrap();
    let mut buf = vec![0u8; 40_001];
    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 40_001);
    assert!(
        buf[30_001..40_000].iter().all(|&b| b == 0),
        "old bytes exposed"
    );
}
