//! The two things a workload can run on: OptimalFS driven as a library on a
//! raw block device, and a mounted Linux file system reached through
//! ordinary system calls. Workloads see only [`Target`], so both run the
//! same sequence of operations.

use optimalfs::{
    BLOCK_SIZE, BlockDevice, Clock, Filesystem, FormatOptions, IoError, KdfParams, MountOptions,
    OsEntropy, ROOT_INO,
};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A file or directory handle.
pub type H = u64;

pub trait Target {
    fn root(&self) -> H;
    fn mkdir(&mut self, dir: H, name: &str) -> H;
    /// Creates a new empty regular file and opens it for reading and writing.
    fn create(&mut self, dir: H, name: &str) -> H;
    fn close(&mut self, f: H);
    fn write(&mut self, f: H, off: u64, data: &[u8]);
    fn read(&mut self, f: H, off: u64, buf: &mut [u8]);
    fn fsync(&mut self, f: H);
    fn fsync_dir(&mut self, d: H);
    fn rename(&mut self, sd: H, s: &str, dd: H, d: &str);
    fn unlink(&mut self, d: H, name: &str);
    /// Looks `name` up and returns its size (`stat`).
    fn stat(&mut self, d: H, name: &str) -> u64;
    /// Makes everything durable (`syncfs`; a full commit on OptimalFS).
    fn syncfs(&mut self);
    /// Makes everything durable and empties every cache, so the next reads
    /// go to the device. Never timed.
    fn drop_caches(&mut self);
    /// Called between operations, outside timed sections. OptimalFS runs
    /// its periodic commit here (see [`Ofs::tick`]); Linux does the same in
    /// its own threads, so this is a no-op there.
    fn tick(&mut self) {}
    /// Extra counters to publish with the result.
    fn extra(&self) -> Vec<(String, f64)> {
        Vec::new()
    }
}

// ---------------------------------------------------------------- OptimalFS

/// The raw device for OptimalFS: `O_DIRECT`, so the host page cache neither
/// helps nor hurts it (OptimalFS keeps its own caches, as it will in the
/// kernel), and `flush` is `fdatasync` on the block device, which Linux
/// turns into a device cache flush (the same request ext4's journal sends).
pub struct DirectDevice {
    file: File,
    blocks: u64,
    bounce: Vec<u8>,
}

impl std::fmt::Debug for DirectDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectDevice({} blocks)", self.blocks)
    }
}

impl DirectDevice {
    /// Opens `path`, using its first `blocks` blocks.
    pub fn open(path: &str, blocks: u64) -> DirectDevice {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)
            .unwrap_or_else(|e| panic!("open {path}: {e}"));
        let len = file.seek(SeekFrom::End(0)).unwrap();
        assert!(
            blocks * BLOCK_SIZE as u64 <= len,
            "{path} has {len} bytes, need {}",
            blocks * BLOCK_SIZE as u64
        );
        DirectDevice {
            file,
            blocks,
            bounce: Vec::new(),
        }
    }

    fn check(&self, block: u64, len: usize) -> Result<(), IoError> {
        if len == 0 || !len.is_multiple_of(BLOCK_SIZE) {
            return Err(IoError::OutOfRange);
        }
        match block.checked_add((len / BLOCK_SIZE) as u64) {
            Some(end) if end <= self.blocks => Ok(()),
            _ => Err(IoError::OutOfRange),
        }
    }
}

/// A 4096-aligned slice of `len` bytes inside `bounce` (`O_DIRECT` needs
/// aligned memory; the file system's buffers aren't).
fn aligned(bounce: &mut Vec<u8>, len: usize) -> &mut [u8] {
    if bounce.len() < len + BLOCK_SIZE {
        *bounce = vec![0; len + BLOCK_SIZE];
    }
    let p = bounce.as_ptr() as usize;
    let off = (BLOCK_SIZE - p % BLOCK_SIZE) % BLOCK_SIZE;
    &mut bounce[off..off + len]
}

/// What OptimalFS asked of the device, counted at the device (so a remount's
/// I/O is included): blocks written, write requests, blocks read, flushes.
/// They cross-check the kernel's disk counters.
pub static DEV_WRITTEN: AtomicU64 = AtomicU64::new(0);
pub static DEV_WRITES: AtomicU64 = AtomicU64::new(0);
pub static DEV_READ: AtomicU64 = AtomicU64::new(0);
pub static DEV_FLUSHES: AtomicU64 = AtomicU64::new(0);

impl BlockDevice for DirectDevice {
    fn num_blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.check(block, buf.len())?;
        let a = aligned(&mut self.bounce, buf.len());
        self.file
            .read_exact_at(a, block * BLOCK_SIZE as u64)
            .map_err(|_| IoError::Failed)?;
        buf.copy_from_slice(a);
        DEV_READ.fetch_add((buf.len() / BLOCK_SIZE) as u64, Relaxed);
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        self.check(block, buf.len())?;
        let a = aligned(&mut self.bounce, buf.len());
        a.copy_from_slice(buf);
        self.file
            .write_all_at(a, block * BLOCK_SIZE as u64)
            .map_err(|_| IoError::Failed)?;
        DEV_WRITTEN.fetch_add((buf.len() / BLOCK_SIZE) as u64, Relaxed);
        DEV_WRITES.fetch_add(1, Relaxed);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        self.file.sync_data().map_err(|_| IoError::Failed)?;
        DEV_FLUSHES.fetch_add(1, Relaxed);
        Ok(())
    }
}

/// Wall-clock time, as the kernel will supply it.
#[derive(Debug, Clone, Copy)]
pub struct SysClock;
impl Clock for SysClock {
    fn now_ns(&mut self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }
}

/// How often OptimalFS commits when nothing else makes it: the same as
/// ext4's default journal commit interval (`commit=5`), so both bound the
/// work lost in a crash by about 5 seconds.
pub const PERIODIC_COMMIT: Duration = Duration::from_secs(5);

pub struct Ofs {
    fs: Option<Filesystem<DirectDevice>>,
    dev: String,
    blocks: u64,
    passphrase: Option<Vec<u8>>,
    last_commit: Instant,
    pub periodic_commits: u64,
    pub periodic_commit_ns: u64,
    /// Intent-log counters of earlier mounts (each cache drop remounts):
    /// batches, blocks, fsync commits, fsync no-ops.
    log: [u64; 4],
}

/// The KDF cost is paid once per format or mount and never inside a timed
/// section, so the benchmark uses the enforced floor (64 MiB, 4 passes)
/// to keep runs short; it does not change any I/O.
pub fn bench_kdf() -> KdfParams {
    KdfParams {
        mem_kib: KdfParams::FLOOR_MEM_KIB,
        iterations: KdfParams::FLOOR_ITERATIONS,
        lanes: 1,
    }
}

impl Ofs {
    /// Formats the first `blocks` blocks of `dev` with default options
    /// (encrypted when `passphrase` is set) and mounts it with defaults.
    pub fn format(dev: &str, blocks: u64, passphrase: Option<Vec<u8>>) -> Ofs {
        let opts = FormatOptions {
            passphrase: passphrase.clone(),
            kdf: bench_kdf(),
            ..FormatOptions::default()
        };
        let fs = Filesystem::format(
            DirectDevice::open(dev, blocks),
            &opts,
            &mut OsEntropy,
            Box::new(SysClock),
        )
        .expect("format");
        Ofs {
            fs: Some(fs),
            dev: dev.into(),
            blocks,
            passphrase,
            last_commit: Instant::now(),
            periodic_commits: 0,
            periodic_commit_ns: 0,
            log: [0; 4],
        }
    }

    pub fn fs(&mut self) -> &mut Filesystem<DirectDevice> {
        self.fs.as_mut().unwrap()
    }

    pub fn unmount(mut self) {
        self.fs.take().unwrap().unmount().expect("unmount");
    }
}

impl Target for Ofs {
    fn root(&self) -> H {
        ROOT_INO
    }
    fn mkdir(&mut self, dir: H, name: &str) -> H {
        self.fs().mkdir(dir, name.as_bytes(), 0o755).expect("mkdir")
    }
    fn create(&mut self, dir: H, name: &str) -> H {
        self.fs()
            .create(dir, name.as_bytes(), 0o644)
            .expect("create")
    }
    fn close(&mut self, _f: H) {}
    fn write(&mut self, f: H, off: u64, data: &[u8]) {
        let n = self.fs().write(f, off, data).expect("write");
        assert_eq!(n, data.len());
    }
    fn read(&mut self, f: H, off: u64, buf: &mut [u8]) {
        let n = self.fs().read(f, off, buf).expect("read");
        assert_eq!(n, buf.len());
    }
    fn fsync(&mut self, f: H) {
        self.fs().fsync(f).expect("fsync");
    }
    fn fsync_dir(&mut self, d: H) {
        self.fs().fsync(d).expect("fsync dir");
    }
    fn rename(&mut self, sd: H, s: &str, dd: H, d: &str) {
        self.fs()
            .rename(sd, s.as_bytes(), dd, d.as_bytes())
            .expect("rename");
    }
    fn unlink(&mut self, d: H, name: &str) {
        self.fs().unlink(d, name.as_bytes()).expect("unlink");
    }
    fn stat(&mut self, d: H, name: &str) -> u64 {
        let fs = self.fs();
        let ino = fs.lookup(d, name.as_bytes()).expect("lookup");
        fs.getattr(ino).expect("getattr").size
    }
    fn syncfs(&mut self) {
        self.fs().commit().expect("commit");
        self.last_commit = Instant::now();
    }
    fn drop_caches(&mut self) {
        let fs = self.fs.take().unwrap();
        self.add_log_stats(&fs);
        // The unmount commits; its I/O counts (DEV_*), as the cache drop's
        // syncfs does on ext4.
        drop(fs.unmount().expect("unmount"));
        let opts = MountOptions {
            passphrase: self.passphrase.clone(),
            ..MountOptions::default()
        };
        let fs = Filesystem::mount(
            DirectDevice::open(&self.dev, self.blocks),
            &opts,
            &mut OsEntropy,
            Box::new(SysClock),
        )
        .expect("mount");
        self.fs = Some(fs);
        self.last_commit = Instant::now();
    }
    /// The periodic commit: in the kernel it runs on its own thread; here
    /// it runs between operations, is counted and timed on its own, and is
    /// included in every workload's total time.
    fn tick(&mut self) {
        if self.last_commit.elapsed() >= PERIODIC_COMMIT {
            let t = Instant::now();
            self.fs().commit().expect("periodic commit");
            self.periodic_commit_ns += t.elapsed().as_nanos() as u64;
            self.periodic_commits += 1;
            self.last_commit = Instant::now();
        }
    }
    fn extra(&self) -> Vec<(String, f64)> {
        let fs = self.fs.as_ref().unwrap();
        let l = fs.intent_log_stats();
        let c = |a: &AtomicU64| a.load(Relaxed) as f64;
        vec![
            ("ofs_periodic_commits".into(), self.periodic_commits as f64),
            (
                "ofs_periodic_commit_s".into(),
                self.periodic_commit_ns as f64 / 1e9,
            ),
            ("ofs_log_batches".into(), (self.log[0] + l.batches) as f64),
            ("ofs_log_blocks".into(), (self.log[1] + l.blocks) as f64),
            (
                "ofs_fsync_commits".into(),
                (self.log[2] + l.fsync_commits) as f64,
            ),
            (
                "ofs_fsync_noops".into(),
                (self.log[3] + l.fsync_noops) as f64,
            ),
            ("ofs_log_ring_blocks".into(), fs.intent_log_blocks() as f64),
            ("ofs_blocks_written".into(), c(&DEV_WRITTEN)),
            ("ofs_write_requests".into(), c(&DEV_WRITES)),
            ("ofs_blocks_read".into(), c(&DEV_READ)),
            ("ofs_flushes".into(), c(&DEV_FLUSHES)),
        ]
    }
}

impl Ofs {
    /// Zeroes every counter, so results cover the workload only (not the
    /// format and first mount). Call right after a mount.
    pub fn reset_counters(&mut self) {
        for c in [&DEV_WRITTEN, &DEV_WRITES, &DEV_READ, &DEV_FLUSHES] {
            c.store(0, Relaxed);
        }
        // A fresh mount's intent-log counters start at zero (replay of an
        // empty chain adds nothing).
        self.log = [0; 4];
        let l = self.fs().intent_log_stats();
        assert_eq!((l.batches, l.blocks, l.fsync_commits), (0, 0, 0));
    }

    fn add_log_stats(&mut self, fs: &Filesystem<DirectDevice>) {
        let l = fs.intent_log_stats();
        for (a, b) in self
            .log
            .iter_mut()
            .zip([l.batches, l.blocks, l.fsync_commits, l.fsync_noops])
        {
            *a += b;
        }
    }
}

// ----------------------------------------------------- mounted file system

enum Node {
    File(PathBuf, File),
    Dir(PathBuf, File),
}

/// A mounted file system (ext4 here), through ordinary system calls.
pub struct Posix {
    root: PathBuf,
    /// The block device it is mounted from (remounted to drop caches).
    dev: String,
    nodes: HashMap<H, Node>,
    next: H,
}

impl Posix {
    pub fn new(mnt: &str, dev: &str) -> Posix {
        let root = PathBuf::from(mnt);
        let f = File::open(&root).expect("open mount point");
        let mut nodes = HashMap::new();
        nodes.insert(1, Node::Dir(root.clone(), f));
        Posix {
            root,
            dev: dev.into(),
            nodes,
            next: 2,
        }
    }

    fn dir(&self, d: H) -> &PathBuf {
        match &self.nodes[&d] {
            Node::Dir(p, _) => p,
            Node::File(..) => panic!("not a directory"),
        }
    }

    fn file(&self, f: H) -> &File {
        match &self.nodes[&f] {
            Node::File(_, f) => f,
            Node::Dir(..) => panic!("not a file"),
        }
    }

    fn add(&mut self, n: Node) -> H {
        let h = self.next;
        self.next += 1;
        self.nodes.insert(h, n);
        h
    }
}

fn root_only(what: &str) {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        panic!("{what} needs root");
    }
}

impl Target for Posix {
    fn root(&self) -> H {
        1
    }
    fn mkdir(&mut self, dir: H, name: &str) -> H {
        let p = self.dir(dir).join(name);
        std::fs::create_dir(&p).expect("mkdir");
        let f = File::open(&p).expect("open dir");
        self.add(Node::Dir(p, f))
    }
    fn create(&mut self, dir: H, name: &str) -> H {
        let p = self.dir(dir).join(name);
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&p)
            .expect("create");
        self.add(Node::File(p, f))
    }
    fn close(&mut self, f: H) {
        self.nodes.remove(&f);
    }
    fn write(&mut self, f: H, off: u64, data: &[u8]) {
        self.file(f).write_all_at(data, off).expect("write");
    }
    fn read(&mut self, f: H, off: u64, buf: &mut [u8]) {
        self.file(f).read_exact_at(buf, off).expect("read");
    }
    fn fsync(&mut self, f: H) {
        self.file(f).sync_all().expect("fsync");
    }
    fn fsync_dir(&mut self, d: H) {
        match &self.nodes[&d] {
            Node::Dir(_, f) => f.sync_all().expect("fsync dir"),
            Node::File(..) => panic!("not a directory"),
        }
    }
    fn rename(&mut self, sd: H, s: &str, dd: H, d: &str) {
        std::fs::rename(self.dir(sd).join(s), self.dir(dd).join(d)).expect("rename");
    }
    fn unlink(&mut self, d: H, name: &str) {
        std::fs::remove_file(self.dir(d).join(name)).expect("unlink");
    }
    fn stat(&mut self, d: H, name: &str) -> u64 {
        std::fs::symlink_metadata(self.dir(d).join(name))
            .expect("stat")
            .len()
    }
    fn syncfs(&mut self) {
        let f = File::open(&self.root).unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: f is an open descriptor for the duration of the call.
        let r = unsafe { libc::syncfs(f.as_raw_fd()) };
        assert_eq!(r, 0, "syncfs: {}", std::io::Error::last_os_error());
    }
    /// Unmounts and mounts again, then drops the kernel's caches, as the
    /// OptimalFS side remounts. `drop_caches` alone is not enough: metadata
    /// buffers still held by the journal (committed, not yet checkpointed)
    /// stay in memory, so a following "cold" stat read 8 KiB instead of the
    /// directory and inode tables. Handles are reopened by path.
    fn drop_caches(&mut self) {
        root_only("dropping caches");
        self.syncfs();
        let paths: Vec<(H, bool, PathBuf)> = self
            .nodes
            .iter()
            .map(|(&h, n)| match n {
                Node::File(p, _) => (h, false, p.clone()),
                Node::Dir(p, _) => (h, true, p.clone()),
            })
            .collect();
        self.nodes.clear();
        let run = |args: &[&str]| {
            let st = std::process::Command::new(args[0])
                .args(&args[1..])
                .status()
                .expect(args[0]);
            assert!(st.success(), "{args:?} failed");
        };
        let mnt = self.root.to_str().unwrap().to_string();
        run(&["umount", &mnt]);
        std::fs::write("/proc/sys/vm/drop_caches", "3\n").expect("drop_caches");
        run(&["mount", &self.dev, &mnt]);
        for (h, dir, p) in paths {
            let f = OpenOptions::new()
                .read(true)
                .write(!dir)
                .open(&p)
                .expect("reopen");
            let n = if dir {
                Node::Dir(p, f)
            } else {
                Node::File(p, f)
            };
            self.nodes.insert(h, n);
        }
    }
}
