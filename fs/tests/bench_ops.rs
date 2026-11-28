//! In-memory cost of namespace and small-write operations, for comparing
//! two builds (ignored by default). 20000 creates in one directory, then a
//! small write to each, a rename of each and an unlink of each, on a
//! 781 MiB in-memory device, plain volume, release build; once in a single
//! transaction and once with a commit every 100 operations. Reports the
//! best of 5 runs per phase.
//!
//! `cargo test --release --test bench_ops -- --ignored --nocapture`
//! (`OFS_BENCH_LOG=0` formats without an intent log ring, to measure what
//! recording the log's records costs).

use optimalfs::sim::Rng;
use optimalfs::*;
use std::time::Instant;

fn fmt(blocks: u64) -> Filesystem<MemDevice> {
    let opts = FormatOptions {
        passphrase: None,
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
        quarantine_min_ns: 0,
        meta_chunk_blocks: 0,
        intent_log_blocks: std::env::var("OFS_BENCH_LOG")
            .ok()
            .map(|v| v.parse().unwrap()),
    };
    Filesystem::format(
        MemDevice::new(blocks),
        &opts,
        &mut Rng::new(1),
        Box::new(ZeroClock),
    )
    .unwrap()
}

fn one(commit_every: u32) -> [f64; 4] {
    let n = 20000u32;
    let mut fs = fmt(200_000);
    let d = fs.mkdir(ROOT_INO, b"d", 0o755).unwrap();
    let mut out = [0.0; 4];
    let t = Instant::now();
    let mut inos = Vec::new();
    for i in 0..n {
        inos.push(fs.create(d, format!("file-{i}").as_bytes(), 0o644).unwrap());
        if commit_every > 0 && i.is_multiple_of(commit_every) {
            fs.commit().unwrap();
        }
    }
    out[0] = t.elapsed().as_secs_f64();
    let t = Instant::now();
    for (i, &f) in inos.iter().enumerate() {
        fs.write(f, (i % 7) as u64 * 100, b"hello world").unwrap();
        if commit_every > 0 && (i as u32).is_multiple_of(commit_every) {
            fs.commit().unwrap();
        }
    }
    out[1] = t.elapsed().as_secs_f64();
    let t = Instant::now();
    for i in 0..n {
        fs.rename(
            d,
            format!("file-{i}").as_bytes(),
            d,
            format!("g-{i}").as_bytes(),
        )
        .unwrap();
        if commit_every > 0 && i.is_multiple_of(commit_every) {
            fs.commit().unwrap();
        }
    }
    out[2] = t.elapsed().as_secs_f64();
    let t = Instant::now();
    for i in 0..n {
        fs.unlink(d, format!("g-{i}").as_bytes()).unwrap();
        if commit_every > 0 && i.is_multiple_of(commit_every) {
            fs.commit().unwrap();
        }
    }
    out[3] = t.elapsed().as_secs_f64();
    out
}

#[test]
#[ignore]
fn opbench() {
    for ce in [0u32, 100] {
        let mut best = [f64::MAX; 4];
        for _ in 0..5 {
            let r = one(ce);
            for j in 0..4 {
                best[j] = best[j].min(r[j]);
            }
        }
        println!(
            "BENCH commit_every {ce}: create {:.3} s, write {:.3} s, rename {:.3} s, unlink {:.3} s (20000 each, best of 5)",
            best[0], best[1], best[2], best[3]
        );
    }
}
