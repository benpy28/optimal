//! Power-cut torture: run a workload, cut power at a chosen write point
//! (every one for short workloads), resolve the device's volatile cache
//! adversarially (lost, reordered and torn writes), then mount and check:
//!
//! 1. mount succeeds;
//! 2. the volume is exactly the state of the last completed commit, or of
//!    the commit that was in flight (never a mix, never anything else);
//! 3. fsck finds nothing, including stale data past EOF;
//! 4. the recovered volume keeps working: more random operations, a
//!    commit and another fsck all pass.

mod common;
use common::*;
use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;

struct Reference {
    ops: Vec<Op>,
    committed: std::collections::BTreeMap<u64, Model>,
    format_points: u64,
    total_points: u64,
    /// Generation after each op of the reference run.
    gen_after: Vec<u64>,
}

fn reference(seed: u64, enc: bool, blocks: u64, nops: usize, cfg: &GenCfg) -> Reference {
    let mut h = Harness::new(blocks, enc, seed).unwrap();
    let format_points = h.fs().device().write_points();
    let mut r = Rng::new(seed);
    let mut ops = Vec::new();
    let mut gen_after = Vec::new();
    for i in 0..=nops {
        // End with a commit so the last ops are covered too.
        let op = if i == nops {
            Op::Commit
        } else {
            gen_op(&mut r, &h.model, cfg)
        };
        h.apply(&op).unwrap();
        ops.push(op);
        gen_after.push(h.fs().generation());
    }
    let total_points = h.fs().device().write_points() - format_points;
    Reference {
        ops,
        committed: h.committed,
        format_points,
        total_points,
        gen_after,
    }
}

/// Replays the workload with power cut at write point `cut` and checks recovery.
fn crash_at(seed: u64, enc: bool, blocks: u64, rf: &Reference, cut: u64, images: u64) {
    let mut h = Harness::new(blocks, enc, seed).unwrap();
    assert_eq!(h.fs().device().write_points(), rf.format_points);
    h.fs().device_mut().cut_at = Some(rf.format_points + cut);
    let mut in_flight = None;
    for (i, op) in rf.ops.iter().enumerate() {
        if let Err(e) = h.apply(op) {
            assert_eq!(
                e,
                Error::Io(IoError::PowerLost),
                "seed {seed:#x} cut {cut}: unexpected error"
            );
            in_flight = Some(i);
            break;
        }
    }
    let Some(in_flight) = in_flight else { return };
    // Commits that completed (returned) must survive; the op that was running
    // may have completed any prefix of its own commits.
    let max_gen = rf.gen_after[in_flight];
    let last_done = *h.committed.keys().next_back().unwrap();
    let dev = h.fs.take().unwrap().into_device();
    for img_no in 0..images {
        let mut d = dev.clone();
        let img = d.crash_image();
        let crashed = CrashDevice::from_image(img, seed ^ cut ^ (img_no << 32));
        let mount_seed = seed ^ 0x5eed ^ cut;
        let fs = Filesystem::mount(
            crashed,
            &mount_opts(enc),
            &mut Rng::new(mount_seed),
            Box::new(ZeroClock),
        )
        .unwrap_or_else(|e| panic!("seed {seed:#x} cut {cut} image {img_no}: mount failed: {e:?}"));
        let g = fs.generation();
        assert!(
            g >= last_done && g <= max_gen,
            "seed {seed:#x} cut {cut}: recovered generation {g}, expected {last_done}..={max_gen}"
        );
        let want = rf
            .committed
            .get(&g)
            .unwrap_or_else(|| {
                panic!(
                    "seed {seed:#x} cut {cut}: generation {g} never committed in the reference run"
                )
            })
            .clone();
        let mut rh = Harness::from_mounted(fs, want.clone(), enc, seed ^ cut);
        rh.verify(&want);
        rh.fsck();
        // The recovered volume must keep working.
        let mut r = Rng::new(seed ^ cut ^ 0xabc);
        for _ in 0..15 {
            let op = gen_op(
                &mut r,
                &rh.model,
                &GenCfg {
                    remounts: false,
                    ..Default::default()
                },
            );
            rh.apply(&op).unwrap();
        }
        rh.apply(&Op::Commit).unwrap();
        let m = rh.model.clone();
        rh.verify(&m);
        rh.fsck();
    }
}

fn campaign(seed: u64, enc: bool, blocks: u64, nops: usize, max_cuts: u64, cfg: &GenCfg) -> u64 {
    let rf = reference(seed, enc, blocks, nops, cfg);
    let w = rf.total_points;
    let mut r = Rng::new(seed ^ 0xc0ffee);
    let cuts: Vec<u64> = if w <= max_cuts {
        (0..w).collect()
    } else {
        (0..max_cuts).map(|_| r.below(w)).collect()
    };
    for &c in &cuts {
        crash_at(seed, enc, blocks, &rf, c, 2);
    }
    cuts.len() as u64
}

#[test]
fn crash_every_write_point_plain() {
    let cfg = GenCfg {
        max_write: 30_000,
        max_offset: 200_000,
        ..Default::default()
    };
    let mut n = 0;
    for i in 0..3 * scale() {
        n += campaign(base_seed() + 10_000 + i, false, 2048, 40, 100_000, &cfg);
    }
    eprintln!("crash states checked: {}", n * 2);
}

#[test]
fn crash_every_write_point_encrypted() {
    let cfg = GenCfg {
        max_write: 30_000,
        max_offset: 200_000,
        ..Default::default()
    };
    for i in 0..3 * scale() {
        campaign(base_seed() + 20_000 + i, true, 2048, 40, 100_000, &cfg);
    }
}

#[test]
fn crash_sampled_long_workloads() {
    for i in 0..2 * scale() {
        campaign(
            base_seed() + 30_000 + i,
            i % 2 == 1,
            4096,
            300,
            150,
            &GenCfg::default(),
        );
    }
}

#[test]
fn crash_tiny_disk() {
    let cfg = GenCfg {
        max_write: 40_000,
        max_offset: 300_000,
        ..Default::default()
    };
    for i in 0..3 * scale() {
        campaign(base_seed() + 40_000 + i, i % 2 == 0, 400, 80, 400, &cfg);
    }
}

/// Nightly soak case (reported as seed 0x78c4c2f0 = seed ^ cut): after
/// recovery the 400-block volume is full, and a create must be refused with
/// `NoSpace` (the harness used to expect success because its model has no
/// space limit).
#[test]
fn tiny_disk_full_after_recovery() {
    let cfg = GenCfg {
        max_write: 40_000,
        max_offset: 300_000,
        ..Default::default()
    };
    let rf = reference(0x78c4c291, true, 400, 80, &cfg);
    crash_at(0x78c4c291, true, 400, &rf, 97, 2);
}
