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

/// A volume with one fragmented file "big" (2000 one-block extents, every
/// other block, so about 30 leaves of extents) and 300 more files, all
/// committed, then mounted with the smallest node cache (16 nodes): an
/// operation on "big" reads its extent leaves, evicting earlier ones, and
/// reads them again after it has released data. Block 1 of "big" is then
/// overwritten without a commit (dirty data). Returns the volume, the
/// inode and the expected contents.
fn fragmented() -> (Filesystem<CrashDevice>, u64, Vec<u8>) {
    let mut fs = Filesystem::format(
        CrashDevice::new(16384, 9),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    let ino = fs.create(ROOT_INO, b"big", 0o644).unwrap();
    let bs = 4096usize;
    let mut want = vec![0u8; 4000 * bs];
    for i in 0..2000usize {
        let b = vec![(i % 251) as u8 + 1; bs];
        fs.write(ino, (2 * i * bs) as u64, &b).unwrap();
        want[2 * i * bs..(2 * i + 1) * bs].copy_from_slice(&b);
    }
    want.truncate(3999 * bs);
    for i in 0..300 {
        fs.create(ROOT_INO, format!("pad{i}").as_bytes(), 0o644)
            .unwrap();
    }
    fs.commit().unwrap();
    let dev = fs.into_device();
    let mut fs = Filesystem::mount(
        dev,
        &MountOptions {
            cache_nodes: 16,
            ..MountOptions::default()
        },
        &mut Rng::new(3),
        Box::new(ZeroClock),
    )
    .unwrap();
    fs.write(ino, bs as u64, &[0xee; 4096]).unwrap();
    want[bs..2 * bs].fill(0xee);
    (fs, ino, want)
}

fn contents(fs: &mut Filesystem<CrashDevice>, ino: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len + 4096];
    let n = fs.read(ino, 0, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// An unlink failing at each of its read points, including after the
/// file's data was released: the file, its dirty data and its blocks come
/// back (fsck finds the space map exact after a commit), and the unlink
/// then works.
#[test]
fn failed_unlink_keeps_the_file_and_its_blocks() {
    let mut failures = 0;
    for k in 0.. {
        let (mut fs, ino, want) = fragmented();
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.unlink(ROOT_INO, b"big");
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => break,
            Err(Error::Io(_)) => {
                failures += 1;
                assert!(!fs.poisoned());
                assert_eq!(fs.lookup(ROOT_INO, b"big"), Ok(ino));
                assert!(
                    contents(&mut fs, ino, want.len()) == want,
                    "k {k}: data changed"
                );
                fs.commit().unwrap();
                let rep = crate::fsck::check(&mut fs, true).unwrap();
                assert!(rep.ok(), "k {k}: {:#?}", rep.errors);
                fs.unlink(ROOT_INO, b"big").unwrap();
                fs.commit().unwrap();
                let rep = crate::fsck::check(&mut fs, true).unwrap();
                assert!(rep.ok(), "k {k} after unlink: {:#?}", rep.errors);
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    // Reads after the releases (re-reading evicted extent leaves) are most
    // of the op's reads here.
    assert!(failures >= 40, "only {failures} failures");
}

/// A truncate failing at each read point (some after it released the
/// data past the cut) leaves the contents, dirty data included, exactly as
/// before; the one that succeeds cuts and zeroes the tail.
#[test]
fn failed_truncate_leaves_dirty_data() {
    let cut = 1000 * 4096 + 1;
    let mut failures = 0;
    for k in 0.. {
        let (mut fs, ino, want) = fragmented();
        // Dirty data past the cut, too.
        let mut want = want;
        fs.write(ino, 3000 * 4096, &[0x5a; 4096]).unwrap();
        want[3000 * 4096..3001 * 4096].fill(0x5a);
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.truncate(ino, cut as u64);
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => {
                assert!(contents(&mut fs, ino, want.len())[..] == want[..cut]);
                fs.write(ino, 1001 * 4096, b"x").unwrap();
                let c = contents(&mut fs, ino, want.len());
                assert!(
                    c[cut..1001 * 4096].iter().all(|&b| b == 0),
                    "old bytes exposed"
                );
                break;
            }
            Err(Error::Io(_)) => {
                failures += 1;
                assert!(!fs.poisoned());
                assert!(
                    contents(&mut fs, ino, want.len()) == want,
                    "k {k}: contents changed"
                );
                fs.commit().unwrap();
                let rep = crate::fsck::check(&mut fs, true).unwrap();
                assert!(rep.ok(), "k {k}: {:#?}", rep.errors);
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    assert!(failures >= 20, "only {failures} failures");
}

/// As the unlink test, with a snapshot holding the file: its releases go
/// to the pending deadlist, and undo must take them out again, or deleting
/// the snapshot later frees blocks the live file still uses.
#[test]
fn failed_unlink_under_a_snapshot_keeps_the_deadlist_exact() {
    let mut failures = 0;
    for k in (0..).step_by(3) {
        let (mut fs, ino, want) = fragmented();
        fs.commit().unwrap();
        let snap = fs.snapshot_create(b"s").unwrap();
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.unlink(ROOT_INO, b"big");
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => break,
            Err(Error::Io(_)) => {
                failures += 1;
                assert!(!fs.poisoned());
                fs.commit().unwrap();
                fs.snapshot_delete(snap).unwrap();
                fs.commit().unwrap();
                assert!(
                    contents(&mut fs, ino, want.len()) == want,
                    "k {k}: data changed"
                );
                let rep = crate::fsck::check(&mut fs, true).unwrap();
                assert!(rep.ok(), "k {k}: {:#?}", rep.errors);
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    assert!(failures >= 10, "only {failures} failures");
}

/// A commit whose data flush fails while replacing old extents (a read in
/// the punch, at each read point): the flush is undone (old extents back,
/// its allocations out of the log) and the commit's retry lands the data;
/// nothing leaks.
#[test]
fn failed_flush_inside_a_commit_is_undone() {
    let mut failures = 0;
    for k in 0..200 {
        let (mut fs, ino, mut want) = fragmented();
        // Overwrite every 8th extent, so the flush punches all over the file.
        for i in (0..2000usize).step_by(8) {
            fs.write(ino, (2 * i * 4096) as u64, &[0x77; 4096]).unwrap();
            want[2 * i * 4096..(2 * i + 1) * 4096].fill(0x77);
        }
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.commit();
        fs.device_mut().faults.fail_reads = None;
        assert!(!fs.poisoned(), "k {k}");
        if fs.last_commit_attempts > 1 {
            failures += 1;
        }
        r.unwrap();
        assert!(
            contents(&mut fs, ino, want.len()) == want,
            "k {k}: data changed"
        );
        let rep = crate::fsck::check(&mut fs, true).unwrap();
        assert!(rep.ok(), "k {k}: {:#?}", rep.errors);
        if fs.last_commit_attempts == 1 && k > 20 {
            break;
        }
    }
    assert!(failures >= 10, "only {failures} failed attempts");
}

/// A snapshot whose own commit fails for good is undone: once the device
/// works again, the next commit does not land the snapshot the caller was
/// told failed.
#[test]
fn snapshot_whose_commit_fails_is_undone() {
    let mut fs = fresh();
    fs.create(ROOT_INO, b"a", 0o644).unwrap();
    fs.commit().unwrap();
    fs.device_mut().faults.all_writes_fail = true;
    assert!(fs.snapshot_create(b"s").is_err());
    fs.device_mut().faults.all_writes_fail = false;
    fs.mark_dirty();
    fs.commit().unwrap();
    assert!(fs.snapshot_list().unwrap().is_empty());
    let mut fs = remount(fs);
    assert!(fs.snapshot_list().unwrap().is_empty());
    fs.lookup(ROOT_INO, b"a").unwrap();
}

/// Two slots already dead and a third failing: the retry finds fewer than
/// two live slots, refuses before writing, and degrades the volume.
#[test]
fn losing_the_third_slot_degrades_at_once() {
    let mut fs = fresh();
    for s in [0, 1] {
        fs.device_mut()
            .faults
            .bad_blocks
            .insert(slot_addr(s, TOTAL));
    }
    for i in 0..3 {
        fs.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        fs.commit().unwrap();
    }
    assert_eq!(fs.dead_slots, 0b0011);
    assert!(!fs.degraded());
    fs.device_mut()
        .faults
        .bad_blocks
        .insert(slot_addr(2, TOTAL));
    fs.create(ROOT_INO, b"g", 0o644).unwrap();
    assert_eq!(fs.commit(), Err(Error::DeviceFailing));
    assert!(fs.degraded());
    assert_eq!(fs.last_commit_attempts, 2);
}

/// Counters an operation moves before failing are put back: the inode
/// counter (create, at every read point) and the newest-snapshot
/// generation (snapshot create, failing at its metadata-tree read).
#[test]
fn failed_operations_put_counters_back() {
    let mut fs = fresh();
    for i in 0..400 {
        fs.create(ROOT_INO, format!("p{i}").as_bytes(), 0o644)
            .unwrap();
    }
    fs.commit().unwrap();
    let mut failed = 0;
    for k in 0..30 {
        let ino0 = fs.next_ino_for_tests();
        let snap0 = fs.store.latest_snap_gen;
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.create(ROOT_INO, format!("n{k}").as_bytes(), 0o644);
        fs.device_mut().faults.fail_reads = None;
        if r.is_err() {
            failed += 1;
            assert_eq!(fs.next_ino_for_tests(), ino0, "k {k}");
        }
        fs.commit().unwrap();
        // The snapshot record goes into the metadata tree, whose root is
        // the first read once a snapshot exists.
        fs.drop_caches();
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads, 2));
        let r = fs.snapshot_create(b"s");
        fs.device_mut().faults.fail_reads = None;
        if r.is_err() {
            failed += 1;
            assert_eq!(fs.store.latest_snap_gen, snap0, "k {k}");
        }
        fs.commit().unwrap();
    }
    assert!(failed >= 4, "only {failed} failures");
}

/// A space-map free that fails part way: the range spans the first two
/// bitmap blocks (32768 blocks each), so the free changes the first bitmap
/// and then reads the second. When that read fails the map has changed
/// with nothing logged ([`Store::space_unlogged`]), and the undo must
/// rebuild it: otherwise the first half of the range stays free in memory
/// while still in use, and the next allocation hands it out again.
///
/// No current path frees such a range (data runs come from one bitmap
/// block, metadata frees are one block), so this drives [`Store::free`]
/// inside an operation directly; the guard is for crafted images and
/// future callers.
///
/// [`Store::space_unlogged`]: crate::tree::Store::space_unlogged
/// [`Store::free`]: crate::tree::Store::free
#[test]
fn free_failing_on_its_second_bitmap_block_is_undone() {
    const BIG: u64 = 33_000;
    const START: u64 = crate::space::BITS_PER_BMP - 8;
    let mut base = Filesystem::format(
        CrashDevice::new(BIG, 9),
        &opts(),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    {
        let s = &mut base.store;
        s.space.claim(&mut s.io, START, 16).unwrap();
    }
    base.mark_dirty();
    base.commit().unwrap();
    let image = base.unmount().unwrap();
    let mut hit = 0;
    for k in 0.. {
        let mut fs = remount_dev(image.clone());
        let d = fs.device_mut();
        d.faults.fail_reads = Some((d.reads + k, 2));
        let r = fs.op(|fs| fs.store.free(START, 16));
        fs.device_mut().faults.fail_reads = None;
        match r {
            Ok(()) => break,
            Err(Error::Io(_)) => {
                assert!(!fs.poisoned());
                hit += fs.unlogged_rebuilds;
                // Every block of the range is still in use: an allocation
                // from its start lands after it.
                fs.set_alloc_cursor_for_tests(START);
                let s = &mut fs.store;
                let (got, _) = s.space.alloc_run(&mut s.io, 1).unwrap();
                assert!(
                    !(START..START + 16).contains(&got),
                    "k {k}: block {got} of the range handed out again"
                );
            }
            Err(e) => panic!("k {k}: {e:?}"),
        }
    }
    assert!(hit >= 1, "no read failure landed inside the free");
}

fn remount_dev(dev: CrashDevice) -> Filesystem<CrashDevice> {
    Filesystem::mount(
        dev,
        &MountOptions::default(),
        &mut Rng::new(3),
        Box::new(ZeroClock),
    )
    .unwrap()
}
