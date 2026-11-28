//! Write failures: a commit (or an early data flush) whose writes fail must
//! not lose the transaction or stop the volume. Blocks that fail for good
//! get new homes; a device that keeps failing degrades the volume (changes
//! held in memory, reads working) until a commit succeeds.
//!
//! Checks, for a random workload with snapshots and remounts:
//! 1. every write the workload makes, failing for good (the blocks it
//!    covered go bad; a superblock slot cannot move, so commits write
//!    around it): every operation still succeeds and matches the model,
//!    fsck is clean, and a remount shows the committed model;
//! 2. every flush failing once: the same;
//! 3. a write failing for good, then a power cut at each of the next write
//!    points (the rebuilt commit's retry): mount gives the last commit that
//!    returned, or one of the states the interrupted operation commits;
//! 4. a device that stops taking writes: the commit fails, the volume is
//!    degraded, reads see the uncommitted changes, and once the device
//!    works again a commit succeeds and the changes survive a remount;
//! 5. a flush that fails and loses the device cache: a commit never claims
//!    success for data that did not survive.

mod common;
use common::*;
use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

const BLOCKS: u64 = 4096;

struct Reference {
    ops: Vec<Op>,
    /// Model after each op.
    after: Vec<Model>,
    format_writes: u64,
    format_flushes: u64,
    /// (first block, blocks) of every write after format.
    writes: Vec<(u64, u64)>,
    flushes: u64,
}

fn reference(seed: u64, enc: bool, nops: usize) -> Reference {
    let mut h = Harness::new(BLOCKS, enc, seed).unwrap();
    let format_writes = h.fs().device().writes;
    let format_flushes = h.fs().device().flushes;
    h.fs().device_mut().trace = Some(Vec::new());
    let mut r = Rng::new(seed);
    let mut ops = Vec::new();
    let mut after = Vec::new();
    for i in 0..=nops {
        // Remount first, so the tests' small dirty-data limit (256 KiB)
        // applies and large writes flush early, inside operations.
        let op = if i == 0 {
            Op::Remount
        } else if i == nops {
            Op::Commit
        } else {
            gen_op(&mut r, &h.model, &GenCfg::default())
        };
        h.apply(&op).unwrap();
        ops.push(op);
        after.push(h.model.clone());
    }
    let dev = h.fs().device_mut();
    Reference {
        ops,
        after,
        format_writes,
        format_flushes,
        writes: dev.trace.take().unwrap(),
        flushes: dev.flushes - format_flushes,
    }
}

fn is_slot(start: u64, n: u64) -> bool {
    [0, 1, BLOCKS - 2, BLOCKS - 1]
        .iter()
        .any(|&s| s >= start && s < start + n)
}

/// Runs the whole workload on a device set up by `arm`, which must hide
/// every fault; then checks the volume, fsck, and a remount.
fn run_hidden(
    seed: u64,
    enc: bool,
    rf: &Reference,
    what: &str,
    arm: impl FnOnce(&mut CrashDevice),
) {
    let mut h = Harness::new(BLOCKS, enc, seed).unwrap();
    arm(h.fs().device_mut());
    for (i, op) in rf.ops.iter().enumerate() {
        if let Err(e) = h.apply(op) {
            panic!("seed {seed:#x} {what}: op #{i} {} failed: {e:?}", short(op));
        }
        assert!(!h.fs().degraded(), "seed {seed:#x} {what}: degraded");
    }
    let m = h.model.clone();
    assert_eq!(&m, rf.after.last().unwrap(), "seed {seed:#x} {what}: model");
    h.verify(&m);
    h.fsck();
    h.apply(&Op::Remount).unwrap();
    h.verify(&m);
    h.fsck();
}

#[test]
fn every_write_fails_for_good() {
    let seeds = 4 * scale();
    for k in 0..seeds {
        let seed = base_seed() ^ (k * 0x9e37_79b9) ^ 0x3a17;
        let enc = k % 2 == 1;
        let rf = reference(seed, enc, 80);
        let step = (rf.writes.len() as u64 / (100 * scale())).max(1);
        let only: Option<u64> = std::env::var("OFS_WRITE").ok().and_then(|v| v.parse().ok());
        for w in (0..rf.writes.len() as u64).step_by(step as usize) {
            if only.is_some_and(|o| o != w) {
                continue;
            }
            let (start, n) = rf.writes[w as usize];
            let slot = is_slot(start, n);
            run_hidden(
                seed,
                enc,
                &rf,
                &format!(
                    "write {w} of {} ({start}+{n}, superblock slot {slot})",
                    rf.writes.len()
                ),
                |d| {
                    d.faults.fail_write_at = Some(rf.format_writes + w);
                    d.faults.fail_write_persists = true;
                },
            );
        }
    }
}

#[test]
fn every_flush_fails_once() {
    let seeds = 4 * scale();
    for k in 0..seeds {
        let seed = base_seed() ^ (k * 0x85eb_ca6b) ^ 0xf1a5;
        let enc = k % 2 == 0;
        let rf = reference(seed, enc, 80);
        for f in 0..rf.flushes {
            run_hidden(
                seed,
                enc,
                &rf,
                &format!("flush {f} of {}", rf.flushes),
                |d| {
                    d.faults.fail_flush_at = Some(rf.format_flushes + f);
                },
            );
        }
    }
}

/// Whether the mounted volume in `h` matches `m` (no panic output kept).
fn matches(h: &mut Harness, m: &Model) -> bool {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = catch_unwind(AssertUnwindSafe(|| h.verify(m))).is_ok();
    std::panic::set_hook(hook);
    ok
}

#[test]
fn power_cut_while_a_failed_commit_is_retried() {
    let seeds = scale();
    for k in 0..seeds {
        let seed = base_seed() ^ (k * 0xc2b2_ae35) ^ 0x77;
        let enc = k % 2 == 1;
        let rf = reference(seed, enc, 25);
        let step = (rf.writes.len() as u64 / 12).max(1);
        let only: Option<u64> = std::env::var("OFS_WRITE").ok().and_then(|v| v.parse().ok());
        for w in (0..rf.writes.len() as u64).step_by(step as usize) {
            if only.is_some_and(|o| o != w) {
                continue;
            }
            for after in 1..=24u64 {
                let mut h = Harness::new(BLOCKS, enc, seed).unwrap();
                let d = h.fs().device_mut();
                d.faults.fail_write_at = Some(rf.format_writes + w);
                d.faults.fail_write_persists = true;
                let mut armed = false;
                let mut cut_op = None;
                for (i, op) in rf.ops.iter().enumerate() {
                    let durable = h.committed.values().next_back().unwrap().clone();
                    let r = h.apply(op);
                    let d = h.fs().device_mut();
                    if !armed && d.writes > rf.format_writes + w {
                        // The failure has happened (possibly inside this op):
                        // cut power `after` write points later.
                        armed = true;
                        if r.is_ok() {
                            d.cut_at = Some(d.write_points() + after);
                        }
                    }
                    match r {
                        Ok(()) => {}
                        Err(Error::Io(IoError::PowerLost)) => {
                            cut_op = Some((i, durable));
                            break;
                        }
                        Err(e) => panic!("seed {seed:#x} w {w}: op #{i} failed: {e:?}"),
                    }
                    if armed && h.fs().device().cut_at.is_none() {
                        h.fs().device_mut().cut_at = Some(h.fs().device().write_points() + after);
                    }
                }
                let Some((i, durable)) = cut_op else { continue };
                let mut dev = h.fs.take().unwrap().into_device();
                let img = dev.crash_image();
                let mount = |img: Vec<u8>| {
                    let crashed = CrashDevice::from_image(img, seed ^ w ^ (after << 40));
                    let fs = Filesystem::mount(
                        crashed,
                        &mount_opts(enc),
                        &mut Rng::new(seed ^ 0x5eed ^ w),
                        Box::new(ZeroClock),
                    )
                    .unwrap_or_else(|e| panic!("seed {seed:#x} w {w} +{after}: mount: {e:?}"));
                    Harness::from_mounted(fs, durable.clone(), enc, seed)
                };
                let pre = if i == 0 {
                    Model::new()
                } else {
                    rf.after[i - 1].clone()
                };
                let cands = [durable.clone(), pre, rf.after[i].clone()];
                // A fresh mount per candidate: a failed comparison may stop
                // part way and leave the volume in an odd state.
                let hit = cands
                    .iter()
                    .position(|m| matches(&mut mount(img.clone()), m));
                assert!(
                    hit.is_some(),
                    "seed {seed:#x} write {w} cut +{after} in op #{i} {}: mounted state is none of \
                     the last returned commit, the state before the op, the state after it",
                    short(&rf.ops[i])
                );
                let mut mh = mount(img);
                mh.set_model(cands[hit.unwrap()].clone());
                let g = mh.fs().generation();
                let m = mh.model.clone();
                mh.committed.insert(g, m);
                mh.fsck();
                // The recovered volume keeps working.
                let mut r = Rng::new(seed ^ after);
                for _ in 0..10 {
                    let op = gen_op(&mut r, &mh.model, &GenCfg::default());
                    mh.apply(&op).unwrap();
                }
                mh.apply(&Op::Commit).unwrap();
                mh.fsck();
            }
        }
    }
}

/// A new volume, mounted with the tests' small dirty-data limit (256 KiB).
fn fresh(enc: bool) -> Filesystem<CrashDevice> {
    let fs = Filesystem::format(
        CrashDevice::new(BLOCKS, 3),
        &fmt_opts(enc),
        &mut Rng::new(11),
        Box::new(ZeroClock),
    )
    .unwrap();
    let dev = fs.unmount().unwrap();
    Filesystem::mount(
        dev,
        &mount_opts(enc),
        &mut Rng::new(12),
        Box::new(ZeroClock),
    )
    .unwrap()
}

#[test]
fn dead_device_degrades_then_recovers() {
    for enc in [false, true] {
        let mut fs = fresh(enc);
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.write(a, 0, &content(1, 40_000)).unwrap();
        fs.commit().unwrap();
        let g = fs.generation();

        fs.device_mut().faults.all_writes_fail = true;
        fs.write(a, 100, b"changed").unwrap();
        let b = fs.create(ROOT_INO, b"b", 0o644).unwrap();
        // Past the dirty limit, so the write flushes early and that fails:
        // the write itself is done (in memory).
        let big = content(2, 3 << 20);
        fs.write(b, 0, &big).unwrap();
        assert!(!fs.degraded(), "enc {enc}: one failed early flush degrades");
        // Two more failed early flushes (three in all) degrade the volume.
        fs.write(b, big.len() as u64, &big[..1 << 19]).unwrap();
        assert!(!fs.degraded());
        fs.write(b, big.len() as u64 + (1 << 19), &big[..1 << 19])
            .unwrap();
        assert!(fs.degraded(), "enc {enc}: three failed early flushes");
        assert_eq!(fs.write(b, 0, b"x"), Err(Error::DeviceFailing));
        fs.device_mut().faults.all_writes_fail = true;
        assert!(matches!(fs.commit(), Err(Error::Io(_))), "enc {enc}");
        // Not one block could be written: a failing device, not bad blocks.
        assert_eq!(fs.bad_blocks(), 0);
        assert!(fs.degraded());
        assert_eq!(fs.generation(), g);
        assert_eq!(fs.create(ROOT_INO, b"c", 0o644), Err(Error::DeviceFailing));
        assert_eq!(fs.write(a, 0, b"x"), Err(Error::DeviceFailing));
        // Reads see the transaction, which is still in memory.
        let mut want = big.clone();
        want.extend_from_slice(&big[..1 << 19]);
        want.extend_from_slice(&big[..1 << 19]);
        let mut buf = vec![0u8; want.len()];
        assert_eq!(fs.read(b, 0, &mut buf).unwrap(), want.len());
        assert!(buf == want, "enc {enc}: uncommitted data");
        let mut buf = [0u8; 7];
        fs.read(a, 100, &mut buf).unwrap();
        assert_eq!(&buf, b"changed");
        // Trying again while the device is still dead changes nothing on disk.
        assert!(matches!(fs.commit(), Err(Error::Io(_))));

        // A crash now gives the last good commit.
        let mut d = fs.device().clone();
        let img = d.crash_image();
        let mut old = Filesystem::mount(
            CrashDevice::from_image(img, 5),
            &mount_opts(enc),
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap();
        assert_eq!(old.generation(), g);
        assert_eq!(old.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        assert!(fsck::check(&mut old, true).unwrap().ok());

        // The device recovers: the commit goes through and survives a remount.
        fs.device_mut().faults.all_writes_fail = false;
        fs.commit().unwrap();
        assert!(!fs.degraded());
        fs.create(ROOT_INO, b"c", 0o644).unwrap();
        fs.commit().unwrap();
        assert!(fsck::check(&mut fs, true).unwrap().ok());
        let dev = fs.unmount().unwrap();
        let mut fs =
            Filesystem::mount(dev, &mount_opts(enc), &mut Rng::new(2), Box::new(ZeroClock))
                .unwrap();
        let mut buf = vec![0u8; want.len()];
        assert_eq!(fs.read(b, 0, &mut buf).unwrap(), want.len());
        assert!(buf == want, "enc {enc}: data after recovery");
        fs.lookup(ROOT_INO, b"c").unwrap();
        assert!(fsck::check(&mut fs, true).unwrap().ok());
    }
}

/// A failed flush that also loses the device cache: the commit retries and
/// the data written before the flush is gone. The commit must fail (the
/// volume stops) rather than report success for lost data; whatever it
/// returns, a remount shows a state that matches what it reported.
#[test]
fn failed_flush_that_loses_the_cache_never_claims_lost_data() {
    for enc in [false, true] {
        for at in 0..6u64 {
            let mut fs = fresh(enc);
            let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
            fs.write(a, 0, &content(1, 20_000)).unwrap();
            fs.commit().unwrap();
            let b = fs.create(ROOT_INO, b"b", 0o644).unwrap();
            let data = content(9, 3 << 20);
            fs.write(b, 0, &data).unwrap();
            let f = fs.device().flushes;
            let d = fs.device_mut();
            d.faults.fail_flush_at = Some(f + at);
            d.faults.failed_flush_loses_cache = true;
            let r = fs.commit();
            fs.device_mut().faults.fail_flush_at = None;
            let dev = fs.into_device();
            let mut fs = Filesystem::mount(
                dev,
                &mount_opts(enc),
                &mut Rng::new(at),
                Box::new(ZeroClock),
            )
            .unwrap();
            assert!(
                fsck::check(&mut fs, true).unwrap().ok(),
                "enc {enc} at {at}"
            );
            match r {
                Ok(()) => {
                    let mut buf = vec![0u8; data.len()];
                    let ino = fs.lookup(ROOT_INO, b"b").unwrap();
                    assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), data.len());
                    assert!(
                        buf == data,
                        "enc {enc} at {at}: commit succeeded with lost data"
                    );
                }
                Err(_) => {
                    // The transaction may or may not have reached the disk
                    // (a lost cache can also lose the error's own cause), but
                    // nothing half-done: either no `b`, or `b` intact.
                    if let Ok(ino) = fs.lookup(ROOT_INO, b"b") {
                        let mut buf = vec![0u8; data.len()];
                        assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), data.len());
                        assert!(buf == data, "enc {enc} at {at}: half a transaction");
                    }
                }
            }
        }
    }
}
