//! The fsync intent log: what `fsync` makes durable without a commit, and
//! what a power cut at every write point recovers.
//!
//! Hand cases check each promise of the storage design's durability table
//! that the log carries (file fsync, directory fsync, rename over a target
//! without an fsync of the new file, a background writer's data staying out
//! of other files' batches), the fallbacks to a full commit, read-only
//! replay, and that replay reproduces inode numbers and timestamps exactly.
//! The crash campaigns run random workloads with fsyncs mixed in, cut power
//! at each write point, resolve the volatile cache adversarially (lost,
//! reordered, torn writes), and require the mounted volume to equal the
//! last durable state before the cut or the one the interrupted call was
//! making, then pass fsck and keep working.

mod common;
use common::*;
use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;
use std::collections::BTreeMap;

const BLOCKS: u64 = 4096;

/// A clock that moves 1000 ns per reading.
struct Tick(i64);
impl Clock for Tick {
    fn now_ns(&mut self) -> i64 {
        self.0 += 1000;
        self.0
    }
}

fn format(enc: bool, log: Option<u64>) -> Filesystem<CrashDevice> {
    Filesystem::format(
        CrashDevice::new(BLOCKS, 77),
        &FormatOptions {
            intent_log_blocks: log,
            ..fmt_opts(enc)
        },
        &mut Rng::new(5),
        Box::new(Tick(1_000_000)),
    )
    .unwrap()
}

fn mount_with(dev: CrashDevice, enc: bool, read_only: bool) -> Filesystem<CrashDevice> {
    Filesystem::mount(
        dev,
        &MountOptions {
            read_only,
            ..mount_opts(enc)
        },
        &mut Rng::new(6),
        Box::new(Tick(9_000_000_000)),
    )
    .unwrap()
}

/// Power cut now: volatile writes resolved adversarially, then mount.
fn crash(fs: Filesystem<CrashDevice>, enc: bool, seed: u64) -> Filesystem<CrashDevice> {
    let mut dev = fs.into_device();
    let img = dev.crash_image();
    mount_with(CrashDevice::from_image(img, seed), enc, false)
}

fn data(fs: &mut Filesystem<CrashDevice>, ino: u64) -> Vec<u8> {
    let size = fs.getattr(ino).unwrap().size as usize;
    let mut b = vec![0u8; size];
    assert_eq!(fs.read(ino, 0, &mut b).unwrap(), size);
    b
}

fn fsck_ok(fs: &mut Filesystem<CrashDevice>) {
    let r = fsck::check(fs, true).unwrap();
    assert!(r.ok(), "fsck: {:#?}", r.errors);
}

/// The live tree as a model (for comparing whole states).
fn read_live(fs: &mut Filesystem<CrashDevice>) -> BTreeMap<u64, MNode> {
    let mut out = BTreeMap::new();
    let mut todo = vec![ROOT_INO];
    while let Some(ino) = todo.pop() {
        if out.contains_key(&ino) {
            continue;
        }
        let a = fs.getattr(ino).unwrap();
        let mut n = MNode {
            kind: a.kind,
            perm: a.perm,
            uid: a.uid,
            gid: a.gid,
            nlink: a.nlink,
            parent: a.parent,
            data: vec![],
            target: vec![],
            entries: BTreeMap::new(),
        };
        match a.kind {
            FileKind::File => n.data = data(fs, ino),
            FileKind::Symlink => n.target = fs.readlink(ino).unwrap(),
            FileKind::Dir => {
                for e in fs.readdir(ino).unwrap() {
                    n.entries.insert(e.name.clone(), e.ino);
                    todo.push(e.ino);
                }
            }
        }
        out.insert(ino, n);
    }
    out
}

/// Hand case: a new file in a new directory, written and fsynced, then
/// power lost. Replay must give back the same inode numbers and every
/// inode field (the times come from the recorded clock readings, not the
/// mount's clock), and the mount commits the result.
#[test]
fn fsync_survives_a_power_cut_exactly() {
    for enc in [false, true] {
        let mut fs = format(enc, None);
        let d = fs.mkdir(ROOT_INO, b"d", 0o750).unwrap();
        let f = fs.create(d, b"f", 0o640).unwrap();
        let body: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs.write(f, 0, &body).unwrap();
        fs.setattr(
            f,
            &SetAttr {
                uid: Some(42),
                mtime_ns: Some(-5),
                ..Default::default()
            },
        )
        .unwrap();
        let g0 = fs.generation();
        let flushes = fs.io_stats().flushes;
        fs.fsync(f).unwrap();
        assert_eq!(fs.generation(), g0, "fsync must not commit");
        let st = fs.intent_log_stats();
        // Records: mkdir, create, setattr (~150 bytes) + state (inode, 3
        // blocks of 4104 bytes): 12.5 KiB in 4040-byte blocks = 4 blocks.
        assert_eq!((st.batches, st.blocks, st.fsync_commits), (1, 4, 0));
        assert_eq!(fs.io_stats().flushes, flushes + 1, "one flush per fsync");
        let want = (
            fs.getattr(ROOT_INO).unwrap(),
            fs.getattr(d).unwrap(),
            fs.getattr(f).unwrap(),
        );
        let mut fs = crash(fs, enc, 1);
        let st = fs.intent_log_stats();
        assert_eq!((st.replayed_batches, st.replayed_records), (1, 4));
        assert!(fs.generation() > g0, "mount commits the replayed state");
        assert_eq!(fs.lookup(ROOT_INO, b"d"), Ok(d));
        assert_eq!(fs.lookup(d, b"f"), Ok(f));
        let got = (
            fs.getattr(ROOT_INO).unwrap(),
            fs.getattr(d).unwrap(),
            fs.getattr(f).unwrap(),
        );
        assert_eq!(got, want, "enc {enc}");
        assert_eq!(data(&mut fs, f), body);
        assert_eq!(fs.next_ino_for_tests(), f + 1);
        fsck_ok(&mut fs);
        // Nothing replays twice: a second power cut finds an empty chain.
        let mut fs = crash(fs, enc, 2);
        assert_eq!(fs.intent_log_stats().replayed_batches, 0);
        assert_eq!(fs.getattr(f).unwrap(), want.2);
        // After a clean unmount and mount, fsync logs into the chain of
        // the mounted commit.
        let mut fs = mount_with(fs.unmount().unwrap(), enc, false);
        fs.write(f, 0, b"more").unwrap();
        fs.fsync(f).unwrap();
        assert_eq!(fs.intent_log_stats().batches, 1);
        let mut fs = crash(fs, enc, 3);
        assert_eq!(fs.intent_log_stats().replayed_batches, 1);
        assert_eq!(data(&mut fs, f)[..4], *b"more");
        fsck_ok(&mut fs);
    }
}

/// fsync of a directory keeps its entries; the data of a file that was not
/// fsynced is not promised and comes back as it was committed (empty).
#[test]
fn directory_fsync_keeps_entries_not_unsynced_data() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"new", 0o644).unwrap();
    fs.write(f, 0, &[9u8; 5000]).unwrap();
    fs.mkdir(ROOT_INO, b"sub", 0o755).unwrap();
    fs.fsync(ROOT_INO).unwrap();
    assert_eq!(fs.intent_log_stats().blocks, 1, "no file data in the batch");
    let mut fs = crash(fs, false, 3);
    assert_eq!(fs.lookup(ROOT_INO, b"new"), Ok(f));
    assert!(fs.lookup(ROOT_INO, b"sub").is_ok());
    assert_eq!(fs.getattr(f).unwrap().size, 0);
    fsck_ok(&mut fs);
}

/// Replace by rename without an fsync of the new file (the storage design's
/// `auto_da_alloc` row): once the rename is durable, the target is the
/// complete new file, never the new inode without its data.
#[test]
fn rename_over_a_target_carries_the_new_data() {
    for sync_dir in [true, false] {
        let mut fs = format(false, None);
        let old = fs.create(ROOT_INO, b"cfg", 0o644).unwrap();
        fs.write(old, 0, b"old contents").unwrap();
        fs.commit().unwrap();
        let other = fs.create(ROOT_INO, b"other", 0o644).unwrap();
        let tmp = fs.create(ROOT_INO, b"cfg.tmp", 0o644).unwrap();
        let new: Vec<u8> = (0..9000u32).map(|i| (i % 200) as u8 + 1).collect();
        fs.write(tmp, 0, &new).unwrap();
        fs.rename(ROOT_INO, b"cfg.tmp", ROOT_INO, b"cfg").unwrap();
        // The rename becomes durable through a directory fsync, or through
        // the fsync of an unrelated file (every batch carries all records).
        fs.fsync(if sync_dir { ROOT_INO } else { other }).unwrap();
        let mut fs = crash(fs, false, 4);
        assert_eq!(fs.lookup(ROOT_INO, b"cfg"), Ok(tmp));
        assert_eq!(fs.lookup(ROOT_INO, b"cfg.tmp"), Err(Error::NotFound));
        assert_eq!(data(&mut fs, tmp), new);
        assert_eq!(fs.getattr(old), Err(Error::NotFound));
        fsck_ok(&mut fs);
    }
}

/// A background writer's data does not ride along with another file's
/// fsync, even after its dirty data was flushed early, and is not promised
/// after a power cut (it comes back as committed).
#[test]
fn background_writer_data_stays_out_of_the_batch() {
    let mut fs = format(false, None);
    let big = fs.create(ROOT_INO, b"big", 0o644).unwrap();
    let small = fs.create(ROOT_INO, b"small", 0o644).unwrap();
    fs.commit().unwrap();
    let chunk = vec![0x5au8; 64 * 1024];
    for i in 0..32 {
        fs.write(big, i * chunk.len() as u64, &chunk).unwrap();
    }
    fs.write(small, 0, &[1u8; 4096]).unwrap();
    fs.fsync(small).unwrap();
    // Inode + one block: 4.2 KiB, 2 ring blocks.
    assert_eq!(fs.intent_log_stats().blocks, 2);
    let mut fs = crash(fs, false, 5);
    assert_eq!(data(&mut fs, small), vec![1u8; 4096]);
    assert_eq!(fs.getattr(big).unwrap().size, 0);
    fsck_ok(&mut fs);
}

/// Shrink, then grow past the old end with a write: the replayed file has
/// the kept head, zeros in the gap (never the old bytes) and the new tail.
/// Early-flushed blocks are read back from the device into the batch.
#[test]
fn truncate_and_regrow_replays_exactly() {
    let mut fs = format(true, None);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, &[b'a'; 8 * 4096]).unwrap();
    fs.commit().unwrap();
    fs.truncate(f, 1000).unwrap();
    fs.write(f, 6 * 4096 + 10, &[b'b'; 3000]).unwrap();
    // Enough other dirty data to force an early flush of f's blocks.
    let g = fs.create(ROOT_INO, b"g", 0o644).unwrap();
    fs.write(g, 0, &vec![b'g'; 512 * 1024]).unwrap();
    let mut want = vec![0u8; 6 * 4096 + 10 + 3000];
    want[..1000].fill(b'a');
    want[6 * 4096 + 10..].fill(b'b');
    assert_eq!(data(&mut fs, f), want);
    fs.fsync(f).unwrap();
    let mut fs = crash(fs, true, 6);
    assert_eq!(data(&mut fs, f), want);
    assert_eq!(fs.lookup(ROOT_INO, b"g"), Ok(g));
    fsck_ok(&mut fs);
}

/// A chain ends at the next commit: after a commit, an older batch is never
/// replayed again (here it would re-create a file the commit's state no
/// longer has).
#[test]
fn a_commit_ends_the_chain() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.fsync(f).unwrap();
    fs.unlink(ROOT_INO, b"f").unwrap();
    fs.commit().unwrap();
    let mut fs = crash(fs, false, 7);
    assert_eq!(fs.intent_log_stats().replayed_batches, 0);
    assert_eq!(fs.lookup(ROOT_INO, b"f"), Err(Error::NotFound));
    // The next chain starts after the old one in the ring.
    let g = fs.create(ROOT_INO, b"g", 0o644).unwrap();
    fs.fsync(g).unwrap();
    let mut fs = crash(fs, false, 8);
    assert_eq!(fs.intent_log_stats().replayed_batches, 1);
    assert_eq!(fs.lookup(ROOT_INO, b"g"), Ok(g));
    fsck_ok(&mut fs);
}

/// When the log can't take a batch, fsync commits: a volume without a ring,
/// a file with more changed blocks than the ring, a full ring.
#[test]
fn fsync_commits_when_the_log_cannot_take_it() {
    // No ring.
    let mut fs = format(false, Some(0));
    assert_eq!(fs.intent_log_blocks(), 0);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    let g0 = fs.generation();
    fs.fsync(f).unwrap();
    assert_eq!(fs.generation(), g0 + 1);
    assert_eq!(fs.intent_log_stats().fsync_commits, 1);

    // A 16-block ring.
    let mut fs = format(false, Some(16));
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, &vec![3u8; 17 * 4096]).unwrap();
    let g0 = fs.generation();
    fs.fsync(f).unwrap();
    assert_eq!(fs.generation(), g0 + 1, "17 changed blocks never fit 16");
    // 3 blocks per fsync (2 data blocks + inode): five fit (15), the sixth
    // commits and starts a new chain.
    let mut commits = 0;
    for i in 0..6u64 {
        fs.write(f, i * 8192, &[i as u8 + 1; 8192]).unwrap();
        let g = fs.generation();
        fs.fsync(f).unwrap();
        commits += (fs.generation() != g) as u32;
        if i < 5 {
            assert_eq!(fs.generation(), g, "fsync {i}");
        }
    }
    assert_eq!(commits, 1);
    let want = data(&mut fs, f);
    let mut fs = crash(fs, false, 9);
    assert_eq!(data(&mut fs, f), want);
    fsck_ok(&mut fs);
}

/// A read-only mount replays in memory and writes nothing; a later
/// read-write mount replays the same chain and commits it.
#[test]
fn read_only_mount_replays_without_writing() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, b"hello").unwrap();
    fs.fsync(f).unwrap();
    let mut dev = fs.into_device();
    let img = dev.crash_image();
    let mut fs = mount_with(CrashDevice::from_image(img, 1), false, true);
    let w0 = fs.device().writes;
    assert_eq!(data(&mut fs, f), b"hello");
    assert_eq!(fs.write(f, 0, b"x"), Err(Error::ReadOnly));
    assert_eq!(fs.fsync(f), Ok(()));
    let mut dev = fs.into_device();
    assert_eq!(dev.writes, w0, "a read-only mount wrote");
    let img = dev.crash_image();
    let mut fs = mount_with(CrashDevice::from_image(img, 2), false, false);
    assert_eq!(fs.intent_log_stats().replayed_batches, 1);
    assert_eq!(data(&mut fs, f), b"hello");
    fsck_ok(&mut fs);
}

/// A link makes the linked file's data ride along like a rename does: the
/// new name is never durable without the data.
#[test]
fn link_carries_the_file_data() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"tmp", 0o644).unwrap();
    fs.write(f, 0, &[7u8; 6000]).unwrap();
    fs.link(f, ROOT_INO, b"final").unwrap();
    fs.unlink(ROOT_INO, b"tmp").unwrap();
    fs.fsync(ROOT_INO).unwrap();
    let mut fs = crash(fs, false, 10);
    assert_eq!(fs.lookup(ROOT_INO, b"final"), Ok(f));
    assert_eq!(data(&mut fs, f), vec![7u8; 6000]);
    fsck_ok(&mut fs);
}

/// Snapshot operations change whole trees, which records don't describe:
/// the next fsync commits, so the snapshot and what came after it are
/// durable together.
#[test]
fn fsync_after_a_snapshot_commits() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    let id = fs.snapshot_create(b"s").unwrap();
    fs.write(f, 0, b"after").unwrap();
    let g = fs.generation();
    fs.fsync(f).unwrap();
    assert_eq!(fs.generation(), g + 1, "fsync must commit after a snapshot");
    let mut fs = crash(fs, false, 11);
    let ids: Vec<u64> = fs.snapshot_list().unwrap().iter().map(|s| s.id).collect();
    assert_eq!(ids, vec![id]);
    assert_eq!(data(&mut fs, f), b"after");
    // And logging resumes after that commit.
    fs.write(f, 0, b"again").unwrap();
    let g = fs.generation();
    fs.fsync(f).unwrap();
    assert_eq!(fs.generation(), g);
    let mut fs = crash(fs, false, 12);
    assert_eq!(data(&mut fs, f), b"again");
    fsck_ok(&mut fs);
}

/// A ring write that fails: fsync falls back to a full commit and still
/// returns success with everything durable.
#[test]
fn failed_ring_write_falls_back_to_a_commit() {
    let mut fs = format(false, None);
    let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    fs.write(f, 0, b"payload").unwrap();
    let d = fs.device_mut();
    d.faults.fail_write_at = Some(d.writes);
    let g = fs.generation();
    fs.fsync(f).unwrap();
    assert_eq!(fs.generation(), g + 1);
    assert_eq!(fs.intent_log_stats().fsync_commits, 1);
    let mut fs = crash(fs, false, 13);
    assert_eq!(data(&mut fs, f), b"payload");
    fsck_ok(&mut fs);
}

/// The ring flush fails and loses the device cache, which held a file's
/// early-flushed blocks: before committing instead, fsync must make that
/// data dirty again, or the commit would point at blocks that never landed.
#[test]
fn failed_ring_flush_rewrites_unflushed_data() {
    for enc in [false, true] {
        let mut fs = format(enc, None);
        let big = fs.create(ROOT_INO, b"big", 0o644).unwrap();
        let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.commit().unwrap();
        let body: Vec<u8> = (0..600_000u32).map(|i| (i % 253) as u8).collect();
        fs.write(big, 0, &body).unwrap();
        assert!(
            fs.io_stats().blocks_written > 0,
            "the big write flushed early"
        );
        fs.write(f, 0, b"small").unwrap();
        let d = fs.device_mut();
        d.faults.fail_flush_at = Some(d.flushes);
        d.faults.failed_flush_loses_cache = true;
        fs.fsync(f).unwrap();
        assert_eq!(fs.intent_log_stats().fsync_commits, 1);
        let mut fs = crash(fs, enc, 14);
        assert_eq!(data(&mut fs, big), body, "enc {enc}");
        assert_eq!(data(&mut fs, f), b"small");
        fsck_ok(&mut fs);
    }
}

// ---- power cuts at every write point ----

struct Reference {
    ops: Vec<Op>,
    /// The durable state before the first op and after each op.
    durable: Vec<Model>,
    points: Vec<u64>,
}

fn gen_ops(seed: u64, h: &mut Harness, n: usize, cfg: &GenCfg) -> Reference {
    let mut r = Rng::new(seed);
    let mut ops = Vec::new();
    let mut durable = vec![h.durable.clone()];
    let mut points = vec![h.fs().device().write_points()];
    for i in 0..=n {
        let op = if i == n {
            Op::Fsync(ROOT_INO)
        } else if r.below(4) == 0 {
            // Mostly existing inodes (files more often), sometimes any.
            let inos: Vec<u64> = h.model.live.nodes.keys().copied().collect();
            let files: Vec<u64> = h
                .model
                .live
                .nodes
                .iter()
                .filter(|(_, n)| n.kind == FileKind::File)
                .map(|(i, _)| *i)
                .collect();
            Op::Fsync(match r.below(8) {
                0 => r.below(h.model.live.next_ino + 2),
                1..=5 if !files.is_empty() => files[r.below(files.len() as u64) as usize],
                _ => inos[r.below(inos.len() as u64) as usize],
            })
        } else if r.below(40) == 0 {
            Op::Commit
        } else {
            gen_op(&mut r, &h.model, cfg)
        };
        h.apply(&op).unwrap();
        ops.push(op);
        durable.push(h.durable.clone());
        points.push(h.fs().device().write_points());
    }
    Reference {
        ops,
        durable,
        points,
    }
}

fn workload_cfg() -> GenCfg {
    GenCfg {
        max_write: 20_000,
        max_offset: 120_000,
        snapshots: false,
        remounts: false,
    }
}

fn harness(seed: u64, enc: bool, blocks: u64) -> Harness {
    let mut h = Harness::new(blocks, enc, seed).unwrap();
    h.cache_nodes = 64;
    h
}

fn check_cut(seed: u64, enc: bool, blocks: u64, rf: &Reference, cut: u64, images: u64) {
    let mut h = harness(seed, enc, blocks);
    let base = rf.points[0];
    assert_eq!(h.fs().device().write_points(), base);
    h.fs().device_mut().cut_at = Some(base + cut);
    let mut in_flight = None;
    for (i, op) in rf.ops.iter().enumerate() {
        if let Err(e) = h.apply(op) {
            assert_eq!(e, Error::Io(IoError::PowerLost), "seed {seed:#x} cut {cut}");
            in_flight = Some(i);
            break;
        }
    }
    let Some(i) = in_flight else { return };
    let allowed = [&rf.durable[i].live, &rf.durable[i + 1].live];
    let dev = h.fs.take().unwrap().into_device();
    for img_no in 0..images {
        let mut d = dev.clone();
        let img = d.crash_image();
        let crashed = CrashDevice::from_image(img, seed ^ cut ^ (img_no << 32));
        let mut fs = Filesystem::mount(
            crashed,
            &mount_opts(enc),
            &mut Rng::new(seed ^ cut ^ 0x5eed),
            Box::new(ZeroClock),
        )
        .unwrap_or_else(|e| panic!("seed {seed:#x} cut {cut} image {img_no}: mount: {e:?}"));
        let got = read_live(&mut fs);
        let next = fs.next_ino_for_tests();
        let which = allowed
            .iter()
            .position(|w| w.nodes == got && w.next_ino == next);
        assert!(
            which.is_some(),
            "seed {seed:#x} cut {cut} image {img_no} (in op #{i} {}): recovered state is neither \
             the durable state before the op nor after it",
            short(&rf.ops[i])
        );
        let want = if which == Some(0) {
            rf.durable[i].clone()
        } else {
            rf.durable[i + 1].clone()
        };
        assert_eq!(
            fs.next_ino_for_tests(),
            want.live.next_ino,
            "seed {seed:#x} cut {cut}: next inode number (op #{i} {}, state {which:?})",
            short(&rf.ops[i])
        );
        let mut rh = Harness::from_mounted(fs, want, enc, seed ^ cut);
        rh.fsck();
        // The recovered volume keeps working, fsyncs included.
        let mut r = Rng::new(seed ^ cut ^ 0xabc);
        for k in 0..12 {
            let op = if k % 4 == 3 {
                Op::Fsync(ROOT_INO)
            } else {
                gen_op(&mut r, &rh.model, &workload_cfg())
            };
            rh.apply(&op).unwrap();
        }
        rh.apply(&Op::Commit).unwrap();
        let m = rh.model.clone();
        rh.verify(&m);
        rh.fsck();
    }
}

fn campaign(seed: u64, enc: bool, blocks: u64, nops: usize, max_cuts: u64) -> u64 {
    let mut h = harness(seed, enc, blocks);
    let rf = gen_ops(seed, &mut h, nops, &workload_cfg());
    // The reference run itself must recover its final durable state.
    h.apply(&Op::Remount).unwrap();
    let got = read_live(h.fs());
    assert!(got == h.durable.live.nodes, "seed {seed:#x}: remount state");
    let w = rf.points.last().unwrap() - rf.points[0];
    let mut r = Rng::new(seed ^ 0xc0ffee);
    let cuts: Vec<u64> = if w <= max_cuts {
        (0..w).collect()
    } else {
        (0..max_cuts).map(|_| r.below(w)).collect()
    };
    for &c in &cuts {
        check_cut(seed, enc, blocks, &rf, c, 2);
    }
    cuts.len() as u64
}

#[test]
fn power_cut_at_every_write_point_plain() {
    let mut n = 0;
    for i in 0..3 * scale() {
        n += campaign(base_seed() + 70_000 + i, false, 2048, 40, 100_000);
    }
    eprintln!("crash states checked: {}", n * 2);
}

#[test]
fn power_cut_at_every_write_point_encrypted() {
    for i in 0..3 * scale() {
        campaign(base_seed() + 71_000 + i, true, 2048, 40, 100_000);
    }
}

#[test]
fn power_cut_sampled_long_workloads() {
    for i in 0..2 * scale() {
        campaign(base_seed() + 72_000 + i, i % 2 == 1, 4096, 250, 200);
    }
}
