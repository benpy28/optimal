//! Runs one workload on one target and appends one JSON line of results.
//!
//! ```text
//! optimalfs-bench <workload> --target ofs  --dev /dev/sdX1 --blocks N [--encrypted]
//! optimalfs-bench <workload> --target ext4 --mnt /mnt/bench
//!     common: --stat-disk sdX --run R --mode plain|encrypted --scale S --out results.jsonl
//! ```
//!
//! The same workload code runs on both targets (see `target.rs`); the
//! orchestration (mkfs, mount, ordering, repeats) is in `run.sh`, and the
//! summary in `summarize.py`. What each workload does is documented on its
//! function and in `fs/docs/BENCHMARKS.md`.

mod target;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Instant;
use target::{Ofs, Posix, Target};

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

const WORKLOADS: &[&str] = &[
    "page-rewrite",
    "fsync-bg",
    "fsync-burst",
    "dpkg",
    "syncfs-1g",
    "rand-4k",
    "meta",
];

/// Block-layer counters of the physical disk (`/sys/block/<disk>/stat`).
#[derive(Clone, Copy, Default)]
struct DevStat {
    reads: u64,
    read_sectors: u64,
    writes: u64,
    write_sectors: u64,
    flushes: u64,
}

fn dev_stat(disk: &str) -> DevStat {
    if disk.is_empty() {
        return DevStat::default();
    }
    let s = std::fs::read_to_string(format!("/sys/block/{disk}/stat"))
        .unwrap_or_else(|e| panic!("/sys/block/{disk}/stat: {e}"));
    let v: Vec<u64> = s.split_whitespace().map(|x| x.parse().unwrap()).collect();
    assert!(v.len() >= 17, "kernel too old for flush counters");
    DevStat {
        reads: v[0],
        read_sectors: v[2],
        writes: v[4],
        write_sectors: v[6],
        flushes: v[15],
    }
}

/// Collects one result line.
struct Rec {
    disk: String,
    m: BTreeMap<String, f64>,
}

/// Fixed-seed xorshift64*, so both targets see the same offsets.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// A timed phase: wall time and the disk's counters across it.
struct Phase {
    name: String,
    t: Instant,
    d: DevStat,
}

impl Rec {
    fn set(&mut self, k: &str, v: f64) {
        self.m.insert(k.into(), v);
    }

    fn start(&self, name: &str) -> Phase {
        Phase {
            name: name.into(),
            d: dev_stat(&self.disk),
            t: Instant::now(),
        }
    }

    fn end(&mut self, p: Phase) -> f64 {
        let s = p.t.elapsed().as_secs_f64();
        let d = dev_stat(&self.disk);
        let n = &p.name;
        self.set(&format!("{n}_s"), s);
        self.set(
            &format!("{n}_dev_write_mib"),
            (d.write_sectors - p.d.write_sectors) as f64 * 512.0 / MIB as f64,
        );
        self.set(&format!("{n}_dev_writes"), (d.writes - p.d.writes) as f64);
        self.set(
            &format!("{n}_dev_flushes"),
            (d.flushes - p.d.flushes) as f64,
        );
        self.set(
            &format!("{n}_dev_read_mib"),
            (d.read_sectors - p.d.read_sectors) as f64 * 512.0 / MIB as f64,
        );
        self.set(&format!("{n}_dev_reads"), (d.reads - p.d.reads) as f64);
        s
    }

    /// Latency summary of `ns` (nearest-rank percentiles), in microseconds.
    fn lat(&mut self, name: &str, mut ns: Vec<u64>) {
        assert!(!ns.is_empty());
        ns.sort_unstable();
        let pct = |p: f64| {
            let r = ((p / 100.0) * ns.len() as f64).ceil() as usize;
            ns[r.clamp(1, ns.len()) - 1] as f64 / 1000.0
        };
        let mean = ns.iter().sum::<u64>() as f64 / ns.len() as f64 / 1000.0;
        self.set(&format!("{name}_n"), ns.len() as f64);
        self.set(&format!("{name}_p50_us"), pct(50.0));
        self.set(&format!("{name}_p99_us"), pct(99.0));
        self.set(&format!("{name}_p999_us"), pct(99.9));
        self.set(
            &format!("{name}_max_us"),
            *ns.last().unwrap() as f64 / 1000.0,
        );
        self.set(&format!("{name}_mean_us"), mean);
    }
}

fn block(i: u64, len: usize) -> Vec<u8> {
    // Distinct, incompressible-looking contents per write.
    let mut r = Rng(i.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut v = vec![0u8; len];
    for c in v.chunks_mut(8) {
        let x = r.next().to_le_bytes();
        c.copy_from_slice(&x[..c.len()]);
    }
    v
}

fn timed<F: FnOnce()>(f: F) -> u64 {
    let t = Instant::now();
    f();
    t.elapsed().as_nanos() as u64
}

fn scaled(n: f64, scale: f64) -> u64 {
    ((n * scale).round() as u64).max(1)
}

/// The SQLite-WAL-tail pattern (storage set "page-rewrite"; also the plain
/// "fsync latency" entry): the same 4 KiB block of a 4 KiB file rewritten
/// and fsynced, 2,000 times. Latency is the write plus the fsync.
fn page_rewrite(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(2000.0, scale);
    let root = t.root();
    let f = t.create(root, "page");
    t.write(f, 0, &block(0, 4 * KIB));
    t.fsync(f);
    let mut lat = Vec::new();
    let all = r.start("total");
    for i in 0..n {
        let b = block(i + 1, 4 * KIB);
        lat.push(timed(|| {
            t.write(f, 0, &b);
            t.fsync(f);
        }));
        t.tick();
    }
    r.end(all);
    r.lat("fsync", lat);
}

/// fsync latency of a 4 KiB file while another file streams: each of 1,000
/// iterations writes 2 MiB more of a background file (two 1 MiB writes, no
/// fsync), then rewrites the 4 KiB file and fsyncs it. 2 GiB streams in
/// all, past Linux's default background-writeback threshold on the runner
/// (10% of available memory), so ext4's writeback really runs. Latency is
/// the 4 KiB write plus its fsync; the background writes are not in it.
fn fsync_bg(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(1000.0, scale);
    let root = t.root();
    let f = t.create(root, "small");
    t.write(f, 0, &block(0, 4 * KIB));
    t.fsync(f);
    let bg = t.create(root, "stream");
    let chunk = block(7, MIB);
    let mut lat = Vec::new();
    let all = r.start("total");
    let mut off = 0u64;
    for i in 0..n {
        for _ in 0..2 {
            t.write(bg, off, &chunk);
            off += MIB as u64;
        }
        let b = block(i + 1, 4 * KIB);
        lat.push(timed(|| {
            t.write(f, 0, &b);
            t.fsync(f);
        }));
        t.tick();
    }
    r.end(all);
    r.set("background_mib", off as f64 / MIB as f64);
    r.lat("fsync", lat);
}

/// The fsync burst: 5 bursts, each creating 1,000 distinct files in a new
/// directory, writing 4 KiB to each and fsyncing it. Latency is the fsync
/// alone; each burst's duration covers all three calls.
fn fsync_burst(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(1000.0, scale);
    let root = t.root();
    let mut lat = Vec::new();
    let mut bursts = Vec::new();
    let all = r.start("total");
    for b in 0..5 {
        let d = t.mkdir(root, &format!("burst-{b}"));
        let tb = Instant::now();
        for i in 0..n {
            let f = t.create(d, &format!("f-{i}"));
            t.write(f, 0, &block(b * n + i, 4 * KIB));
            lat.push(timed(|| t.fsync(f)));
            t.close(f);
        }
        bursts.push(tb.elapsed().as_nanos() as u64);
        t.tick();
    }
    r.end(all);
    r.lat("fsync", lat);
    r.lat("burst", bursts);
}

/// The dpkg-style loop: create a 4 KiB file, fsync it, rename it over one
/// of 100 existing targets, fsync the directory; 10,000 times. Latency is
/// one whole iteration.
fn dpkg(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(10000.0, scale);
    let root = t.root();
    let d = t.mkdir(root, "pkg");
    for k in 0..100 {
        let f = t.create(d, &format!("target-{k}"));
        t.write(f, 0, &block(k, 4 * KIB));
        t.close(f);
    }
    t.syncfs();
    let mut lat = Vec::new();
    let all = r.start("total");
    for i in 0..n {
        let b = block(1000 + i, 4 * KIB);
        lat.push(timed(|| {
            let f = t.create(d, "new.dpkg-new");
            t.write(f, 0, &b);
            t.fsync(f);
            t.close(f);
            t.rename(d, "new.dpkg-new", d, &format!("target-{}", i % 100));
            t.fsync_dir(d);
        }));
        t.tick();
    }
    r.end(all);
    r.lat("iter", lat);
}

/// `syncfs` after 1 GiB of new data: one file written in 1 MiB writes,
/// then `syncfs` (a full commit on OptimalFS). Both phases are timed,
/// since a file system that writes early pays in the write phase instead.
fn syncfs_1g(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(1024.0, scale);
    let root = t.root();
    let f = t.create(root, "big");
    let chunk = block(3, MIB);
    let all = r.start("total");
    let w = r.start("write");
    for i in 0..n {
        t.write(f, i * MIB as u64, &chunk);
        t.tick();
    }
    r.end(w);
    let s = r.start("syncfs");
    t.syncfs();
    r.end(s);
    let total = r.end(all);
    r.set("mib", n as f64);
    r.set("total_mib_per_s", n as f64 / total);
}

/// 4 KiB random I/O on a 256 MiB file (fixed-seed offsets, the same on
/// both): fill it sequentially and make it durable; drop caches; 20,000
/// random reads; 20,000 random overwrites then one fsync; drop caches;
/// 20,000 random reads again (now of a file whose blocks copy-on-write
/// moved).
fn rand_4k(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let size = 256 * MIB as u64;
    let blocks = size / 4096;
    let n = scaled(20000.0, scale);
    let root = t.root();
    let f = t.create(root, "data");
    let chunk = block(5, MIB);
    for i in 0..size / MIB as u64 {
        t.write(f, i * MIB as u64, &chunk);
    }
    t.syncfs();
    t.drop_caches();
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let mut buf = vec![0u8; 4096];
    let mut lat = Vec::new();
    let p = r.start("read_fresh");
    for _ in 0..n {
        let off = rng.next() % blocks * 4096;
        lat.push(timed(|| t.read(f, off, &mut buf)));
        t.tick();
    }
    r.end(p);
    r.lat("read_fresh", lat);
    let b = block(9, 4 * KIB);
    let p = r.start("overwrite");
    for _ in 0..n {
        let off = rng.next() % blocks * 4096;
        t.write(f, off, &b);
        t.tick();
    }
    t.fsync(f);
    let s = r.end(p);
    r.set("overwrite_iops", n as f64 / s);
    t.drop_caches();
    let mut lat = Vec::new();
    let p = r.start("read_after");
    for _ in 0..n {
        let off = rng.next() % blocks * 4096;
        lat.push(timed(|| t.read(f, off, &mut buf)));
        t.tick();
    }
    r.end(p);
    r.lat("read_after", lat);
}

/// Metadata: create 100,000 empty files in one directory, `syncfs`; drop
/// caches; stat each; unlink each, `syncfs`.
fn meta(t: &mut dyn Target, r: &mut Rec, scale: f64) {
    let n = scaled(100_000.0, scale);
    let root = t.root();
    let d = t.mkdir(root, "many");
    let p = r.start("create");
    for i in 0..n {
        let f = t.create(d, &format!("file-{i:06}"));
        t.close(f);
        t.tick();
    }
    t.syncfs();
    r.end(p);
    t.drop_caches();
    let p = r.start("stat");
    for i in 0..n {
        let s = t.stat(d, &format!("file-{i:06}"));
        assert_eq!(s, 0);
        t.tick();
    }
    r.end(p);
    let p = r.start("unlink");
    for i in 0..n {
        t.unlink(d, &format!("file-{i:06}"));
        t.tick();
    }
    t.syncfs();
    r.end(p);
    r.set("files", n as f64);
}

fn run(name: &str, t: &mut dyn Target, r: &mut Rec, scale: f64) {
    // Everything from the workload's start until all of it is durable:
    // the device writes and flushes each file system needed for the same
    // work (ext4 writes back and checkpoints after the timed part; the
    // final syncfs makes it pay that here).
    let all = r.start("work");
    match name {
        "page-rewrite" => page_rewrite(t, r, scale),
        "fsync-bg" => fsync_bg(t, r, scale),
        "fsync-burst" => fsync_burst(t, r, scale),
        "dpkg" => dpkg(t, r, scale),
        "syncfs-1g" => syncfs_1g(t, r, scale),
        "rand-4k" => rand_4k(t, r, scale),
        "meta" => meta(t, r, scale),
        _ => panic!("unknown workload {name}; one of {WORKLOADS:?}"),
    }
    t.syncfs();
    r.end(all);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--list") {
        for w in WORKLOADS {
            println!("{w}");
        }
        return;
    }
    let workload = args.first().expect("workload").clone();
    let mut opt = BTreeMap::new();
    let mut flags = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let k = args[i].trim_start_matches("--").to_string();
        if k == "encrypted" {
            flags.push(k);
            i += 1;
        } else {
            opt.insert(k, args[i + 1].clone());
            i += 2;
        }
    }
    let get = |k: &str| opt.get(k).cloned().unwrap_or_default();
    let scale: f64 = opt.get("scale").map_or(1.0, |s| s.parse().unwrap());
    let target = get("target");
    let mut r = Rec {
        disk: get("stat-disk"),
        m: BTreeMap::new(),
    };
    match target.as_str() {
        "ofs" => {
            let blocks: u64 = get("blocks").parse().expect("--blocks");
            let pass = flags
                .iter()
                .any(|f| f == "encrypted")
                .then(|| b"benchmark passphrase".to_vec());
            let mut t = Ofs::format(&get("dev"), blocks, pass);
            // Start from a mounted, clean volume, as ext4 does after mkfs.
            t.drop_caches();
            t.reset_counters();
            run(&workload, &mut t, &mut r, scale);
            for (k, v) in t.extra() {
                r.set(&k, v);
            }
            t.unmount();
        }
        "ext4" => {
            let mut t = Posix::new(&get("mnt"));
            run(&workload, &mut t, &mut r, scale);
        }
        _ => panic!("--target ofs|ext4"),
    }
    let mut line = format!(
        "{{\"workload\":\"{workload}\",\"target\":\"{target}\",\"mode\":\"{}\",\"run\":{},\"scale\":{scale},\"metrics\":{{",
        get("mode"),
        opt.get("run").map_or("0", String::as_str)
    );
    let parts: Vec<String> =
        r.m.iter()
            .map(|(k, v)| format!("\"{k}\":{}", if v.is_finite() { *v } else { -1.0 }))
            .collect();
    line.push_str(&parts.join(","));
    line.push_str("}}");
    println!("{line}");
    if let Some(out) = opt.get("out") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }
}
