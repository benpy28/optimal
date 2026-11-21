//! Model-based property tests: long random workloads run against the real
//! filesystem and a simple in-memory reference model, compared after every
//! operation (results) and regularly in full (state), with fsck throughout.

mod common;
use common::*;
use optimalfs::sim::Rng;

fn run(seed: u64, enc: bool, blocks: u64, ops: u64, cfg: &GenCfg) {
    let mut h = Harness::new(blocks, enc, seed).unwrap();
    let mut r = Rng::new(seed);
    for i in 0..ops {
        let op = gen_op(&mut r, &h.model, cfg);
        h.apply(&op).unwrap();
        if i % 50 == 49 {
            let m = h.model.clone();
            h.verify(&m);
        }
        if matches!(
            op,
            Op::Commit | Op::Snapshot | Op::SnapDelete(_) | Op::Rollback(_)
        ) && r.chance(1, 3)
        {
            h.fsck();
        }
    }
    h.fs().commit().unwrap();
    let m = h.model.clone();
    h.verify(&m);
    h.fsck();
}

#[test]
fn model_plain() {
    for i in 0..4 * scale() {
        run(base_seed() + i, false, 8192, 400, &GenCfg::default());
    }
}

#[test]
fn model_encrypted() {
    for i in 0..4 * scale() {
        run(base_seed() + 1000 + i, true, 8192, 400, &GenCfg::default());
    }
}

#[test]
fn model_tiny_disk() {
    // Constant space pressure: ENOSPC paths, quarantine recycling, reserve.
    for i in 0..4 * scale() {
        run(
            base_seed() + 2000 + i,
            i % 2 == 1,
            600,
            500,
            &GenCfg {
                max_write: 60_000,
                max_offset: 400_000,
                ..Default::default()
            },
        );
    }
}

#[test]
fn model_many_small_files() {
    let cfg = GenCfg {
        max_write: 3000,
        max_offset: 20_000,
        snapshots: true,
        remounts: true,
    };
    for i in 0..2 * scale() {
        run(base_seed() + 3000 + i, false, 16384, 2000, &cfg);
    }
}

#[test]
#[ignore]
fn debug_one() {
    let seed: u64 = std::env::var("OFS_SEED").unwrap().parse().unwrap();
    let enc = std::env::var("OFS_ENC").is_ok();
    let blocks: u64 = std::env::var("OFS_BLOCKS")
        .map(|v| v.parse().unwrap())
        .unwrap_or(8192);
    run(seed, enc, blocks, 2000, &GenCfg::default());
}
