//! Operations are all or nothing: one that fails part way (here, a
//! transient read error at any point inside it) leaves the volume exactly
//! as before it, still writable, and the same operation then succeeds.
//!
//! For a random workload with snapshots and remounts frequent commits and the
//! node cache dropped before each operation (so operations read from the device), each run fails the k-th and
//! (k+1)-th device read of every operation: two in a row, so a metadata
//! read's second copy fails too. After each failure: the volume is not
//! stopped, the whole live tree and every snapshot match the model as it
//! was before the operation (a snapshot rollback may keep the deletion of
//! newer snapshots its middle commit made durable), fsck after a commit is
//! clean, and the operation, retried, succeeds and
//! matches the model. At the end fsck is clean and a remount shows the
//! committed model.

mod common;
use common::*;
use optimalfs::sim::Rng;
use optimalfs::*;

const BLOCKS: u64 = 4096;

/// Returns (operations failed, operations the volume undid).
fn run(seed: u64, enc: bool, k: u64, nops: usize) -> (u64, u64) {
    let mut h = Harness::new(BLOCKS, enc, seed).unwrap();
    h.cache_nodes = 4;
    h.pass_faults = true;
    h.apply(&Op::Remount).unwrap();
    // Odd runs damage the data read instead (a checksum failure).
    let corrupt = k % 2 == 1;
    let mut r = Rng::new(seed);
    let mut failed = 0;
    for i in 0..nops {
        let op = gen_op(&mut r, &h.model, &GenCfg::default());
        let inject = !matches!(op, Op::Commit | Op::Remount);
        if inject {
            h.fs().drop_caches();
            let d = h.fs().device_mut();
            d.faults.fail_reads = Some((d.reads + k / 2, 2));
            d.faults.fail_reads_corrupt = corrupt;
        }
        let pre = h.model.clone();
        let res = h.apply(&op);
        h.fs().device_mut().faults.fail_reads = None;
        match res {
            Ok(()) => {}
            Err(Error::Io(_) | Error::Corrupt(..)) => {
                failed += 1;
                let what = format!("seed {seed:#x} k {k} op #{i} {}", short(&op));
                assert!(!h.fs().poisoned(), "{what}: volume stopped");
                assert!(!h.fs().degraded(), "{what}: volume degraded");
                assert_eq!(h.model, pre, "{what}: model changed");
                if let Op::Rollback(id) = op {
                    // Its middle commit may have made the deletion of the
                    // newer snapshots durable: those stay deleted.
                    let ids: Vec<u64> = h
                        .fs()
                        .snapshot_list()
                        .unwrap()
                        .iter()
                        .map(|s| s.id)
                        .collect();
                    if ids != pre.snaps.keys().copied().collect::<Vec<_>>() {
                        h.model.snaps.retain(|&s, _| s <= id);
                        h.apply(&Op::Commit).unwrap();
                    }
                }
                let now = h.model.clone();
                h.verify(&now);
                // The space map is exact too: commit, then fsck.
                h.apply(&Op::Commit).unwrap();
                h.fsck();
                if let Err(e) = h.apply(&op) {
                    panic!("{what}: retry failed: {e:?}");
                }
            }
            Err(e) => panic!("seed {seed:#x} k {k} op #{i} {}: {e:?}", short(&op)),
        }
        // Commit often, so the next operation starts from clean trees and
        // has to read.
        if r.chance(1, 2) {
            h.apply(&Op::Commit).unwrap();
        }
    }
    h.apply(&Op::Commit).unwrap();
    let m = h.model.clone();
    h.verify(&m);
    h.fsck();
    h.apply(&Op::Remount).unwrap();
    h.verify(&m);
    h.fsck();
    let undone = h.fs().ops_undone_for_tests();
    (failed, undone)
}

#[test]
fn every_read_point_of_every_operation_fails_once() {
    let seeds = 2 * scale();
    let mut total_failed = 0;
    for s in 0..seeds {
        let seed = base_seed() ^ (s * 0x2545_f491) ^ 0x0b0e;
        let enc = s % 2 == 1;
        for k in 0..24 {
            let (failed, _) = run(seed, enc, k, 150);
            total_failed += failed;
        }
    }
    // The test is only worth something if operations really failed.
    assert!(
        total_failed >= 100 * seeds,
        "only {total_failed} operations failed"
    );
    eprintln!("{total_failed} operations failed part way and were undone");
}
