//! Lying drives: a drive that acknowledges flushes without making anything
//! durable, then loses power. Every write since the last honest flush
//! independently landed (any version) or was lost.
//!
//! The contract (FORMAT.md, "Lying drives"): if at most K = 8 commits were
//! lost this way, mount must find an intact commit no older than one before
//! the last honest one, its contents must equal that commit exactly
//! (except file data that recovery reports as damaged, which must fail to
//! read rather than return anything wrong), fsck must pass and the volume
//! must keep working. Beyond K commits that is a rescue job.

mod common;
use common::*;
use optimalfs::sim::Rng;
use optimalfs::*;
use std::collections::BTreeSet;

const K: u64 = 8;

fn cfg() -> GenCfg {
    GenCfg {
        remounts: false,
        ..Default::default()
    }
}

#[derive(Default)]
struct Tally {
    fallbacks: u64,
    damaged: u64,
    second_round: u64,
}

fn lying_case(seed: u64, enc: bool, honest_ops: usize, lies: u64, images: u64, t: &mut Tally) {
    let mut h = Harness::new(1200, enc, seed).unwrap();
    let mut r = Rng::new(seed);
    for _ in 0..honest_ops {
        let op = gen_op(&mut r, &h.model, &cfg());
        h.apply(&op).unwrap();
    }
    lie_and_check(h, seed, enc, lies, images, 1, t);
}

/// From an honest, committed state in `h`: lie for `lies` commits, lose
/// power, and check every recovered image. With `depth` > 0, one recovered
/// volume goes through another lying round, which exercises quarantine and
/// superblock numbering after a fallback.
fn lie_and_check(
    mut h: Harness,
    seed: u64,
    enc: bool,
    lies: u64,
    images: u64,
    depth: u32,
    t: &mut Tally,
) {
    let mut r = Rng::new(seed ^ 0x1e5 ^ depth as u64);
    h.apply(&Op::Commit).unwrap();
    let honest = h.fs().generation();
    h.fs().device_mut().lying_flush = true;
    // Keep working until `lies` more commits were (falsely) acknowledged.
    let mut guard = 0;
    while h.fs().generation() < honest + lies {
        let op = if guard % 7 == 6 {
            Op::Commit
        } else {
            gen_op(&mut r, &h.model, &cfg())
        };
        h.apply(&op).unwrap();
        guard += 1;
        assert!(guard < 10_000);
    }
    let newest = h.fs().generation();
    if newest > honest + K {
        // One op committed more than once and overshot; out of contract.
        return;
    }
    let committed = h.committed.clone();
    let mut dev = h.fs.take().unwrap().into_device();
    dev.lying_flush = false;
    let mut went_deeper = false;
    for img_no in 0..images {
        let mut d = dev.clone();
        // Steered images force the hardest superblock outcomes: all
        // superblock writes landed, or only the newest commit's pair did and
        // the other pair kept its pre-drop content. Either way the trees of
        // the lost commits are random, so mount must walk the history ring
        // back to the drop point (K entries deep in the second case).
        let n = d.num_blocks();
        let own = [(newest % 2), n - 1 - (newest % 2)];
        let other = [1 - (newest % 2), n - 2 + (newest % 2)];
        let img = match img_no % 3 {
            0 => d.lost_writes_image_steered(&[0, 1, n - 2, n - 1], &[]),
            1 => d.lost_writes_image_steered(&own, &other),
            _ => d.lost_writes_image(),
        };
        let crashed = optimalfs::sim::CrashDevice::from_image(img, seed ^ img_no);
        let fs = Filesystem::mount(
            crashed,
            &mount_opts(enc),
            &mut Rng::new(seed ^ 0x11e ^ img_no),
            Box::new(ZeroClock),
        )
        .unwrap_or_else(|e| {
            panic!("seed {seed:#x} lies {lies} depth {depth} image {img_no}: mount failed: {e:?}")
        });
        let g = fs.generation();
        assert!(
            g >= honest && g <= newest,
            "seed {seed:#x} lies {lies}: mounted generation {g}, honest {honest}, newest {newest}"
        );
        if g < newest {
            t.fallbacks += 1;
        }
        let damaged: BTreeSet<u64> = fs
            .recovery_stats()
            .map(|s| s.damaged.iter().map(|&(ino, _)| ino).collect())
            .unwrap_or_default();
        t.damaged += damaged.len() as u64;
        let want = committed
            .get(&g)
            .unwrap_or_else(|| panic!("seed {seed:#x}: generation {g} never committed"))
            .clone();
        let skipped = fs.fell_back_from().unwrap_or(g);
        assert!(skipped >= g && skipped <= newest);
        let mut rh = Harness::from_mounted(fs, want.clone(), enc, seed ^ img_no);
        rh.verify_skip(&want, &damaged);
        let rep = fsck::check(rh.fs(), damaged.is_empty()).unwrap();
        assert!(
            rep.ok(),
            "seed {seed:#x} lies {lies}: fsck: {:#?}",
            rep.errors
        );
        if !damaged.is_empty() {
            continue;
        }
        // The recovered volume keeps working, and its next commits outrank
        // every stale superblock copy still on the device.
        let mut r2 = Rng::new(seed ^ img_no ^ 0xabc);
        for _ in 0..20 {
            let op = gen_op(&mut r2, &rh.model, &cfg());
            rh.apply(&op).unwrap();
        }
        rh.apply(&Op::Commit).unwrap();
        assert!(rh.fs().generation() > skipped);
        let m = rh.model.clone();
        rh.verify(&m);
        rh.fsck();
        if depth > 0 && !went_deeper && g < newest {
            went_deeper = true;
            t.second_round += 1;
            lie_and_check(rh, seed ^ 0xdee9, enc, lies, images.min(3), depth - 1, t);
            continue;
        }
        // And it remounts to exactly that state.
        let dev = rh.fs.take().unwrap().into_device();
        let fs = Filesystem::mount(dev, &mount_opts(enc), &mut Rng::new(5), Box::new(ZeroClock))
            .unwrap();
        let mut rh = Harness::from_mounted(fs, m.clone(), enc, seed);
        rh.verify(&m);
    }
}

#[test]
fn lost_commits_one_to_k() {
    let scale = scale();
    let base = base_seed();
    let mut t = Tally::default();
    for lies in 1..=K {
        for i in 0..scale.max(1) * 2 {
            let seed = base ^ (lies << 40) ^ (i * 0x9e37_79b9);
            lying_case(seed, i % 2 == 1, 40, lies, 6, &mut t);
        }
    }
    eprintln!(
        "lying drives: {} fallbacks, {} damaged-file reports, {} second rounds",
        t.fallbacks, t.damaged, t.second_round
    );
    // The adversary must actually have forced fallbacks, or this tested nothing.
    assert!(t.fallbacks > 10 && t.second_round > 2, "too few fallbacks");
}

#[test]
fn clean_unmount_skips_recovery_and_unclean_runs_it() {
    let mut h = Harness::new(1200, false, 7).unwrap();
    let mut r = Rng::new(7);
    let cfg = GenCfg {
        remounts: false,
        ..Default::default()
    };
    for _ in 0..30 {
        let op = gen_op(&mut r, &h.model, &cfg);
        h.apply(&op).unwrap();
    }
    h.apply(&Op::Commit).unwrap();
    let m = h.model.clone();
    let fs = h.fs.take().unwrap();
    // Unclean: the recovery check runs and reads the recent blocks.
    let dev = fs.into_device();
    let fs = Filesystem::mount(
        dev.clone(),
        &mount_opts(false),
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap();
    let st = fs
        .recovery_stats()
        .expect("unclean mount must run recovery")
        .clone();
    assert!(st.nodes > 0 && st.space_blocks > 0, "{st:?}");
    // Clean: unmount writes a clean commit; the next mount skips the check.
    let dev = fs.unmount().unwrap();
    let fs = Filesystem::mount(
        dev,
        &mount_opts(false),
        &mut Rng::new(2),
        Box::new(ZeroClock),
    )
    .unwrap();
    assert!(fs.recovery_stats().is_none());
    let mut rh = Harness::from_mounted(fs, m.clone(), false, 7);
    rh.verify(&m);
    // A commit after a clean mount clears the flag again.
    rh.apply(&Op::Commit).unwrap();
    let f = rh.fs().create(ROOT_INO, b"after-clean", 0o644).unwrap();
    let _ = f;
    rh.fs().commit().unwrap();
    let dev = rh.fs.take().unwrap().into_device();
    let fs = Filesystem::mount(
        dev,
        &mount_opts(false),
        &mut Rng::new(3),
        Box::new(ZeroClock),
    )
    .unwrap();
    assert!(fs.recovery_stats().is_some());
}
