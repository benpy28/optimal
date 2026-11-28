//! The filesystem: inodes, directories, file data, commits.

use crate::codec::{Reader, Writer};
use crate::crypto::{self, BlockKind, Entropy, KdfParams, MAC_LEN, NONCE_LEN, Sealer, SecretKey};
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Block, CopyMode, Io, zero_block};
use crate::ptr::BlockPtr;
use crate::space::Space;
use crate::superblock::{
    self, Body, Header, KEYSLOTS, KeySlot, SUITE_PLAIN, SUITE_XCHACHA20POLY1305,
};
use crate::tree::{Key, MAX_VALUE, NodeCache, SpaceOp, Store, Tree, TreeId, Undo};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;

pub const ROOT_INO: u64 = 1;
pub const NAME_MAX: usize = 255;
/// Longest symlink target stored (kept inside one tree item).
pub const SYMLINK_MAX: usize = 1024;
pub const MAX_FILE_SIZE: u64 = 1 << 60;
pub const LINK_MAX: u32 = 65000;
/// Blocks per data extent at most.
pub const EXTENT_MAX_BLOCKS: u64 = 64;

// Item kinds in the filesystem tree.
pub(crate) const K_INODE: u8 = 1;
pub(crate) const K_DIRENT: u8 = 2;
pub(crate) const K_EXTENT: u8 = 3;
pub(crate) const K_INLINE: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File = 1,
    Dir = 2,
    Symlink = 3,
}

impl FileKind {
    pub(crate) fn from_u8(v: u8) -> Option<FileKind> {
        match v {
            1 => Some(FileKind::File),
            2 => Some(FileKind::Dir),
            3 => Some(FileKind::Symlink),
            _ => None,
        }
    }
}

/// Source of timestamps (nanoseconds since the Unix epoch).
pub trait Clock {
    fn now_ns(&mut self) -> i64;
}

/// A clock that always reads zero, for deterministic tests and tools.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZeroClock;
impl Clock for ZeroClock {
    fn now_ns(&mut self) -> i64 {
        0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub kind: FileKind,
    /// Permission bits (low 12 bits of st_mode).
    pub perm: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    /// Bytes for files and symlinks; number of entries for directories.
    pub size: u64,
    pub atime_ns: i64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub btime_ns: i64,
    /// Parent directory (directories only; 0 otherwise).
    pub parent: u64,
}

pub(crate) const INODE_LEN: usize = 80;

impl Attr {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut v = vec![0u8; INODE_LEN];
        let mut w = Writer::new(&mut v);
        w.u8(self.kind as u8);
        w.zeros(3);
        w.u32(self.perm);
        w.u32(self.uid);
        w.u32(self.gid);
        w.u32(self.nlink);
        w.zeros(4);
        w.u64(self.size);
        w.i64(self.atime_ns);
        w.i64(self.mtime_ns);
        w.i64(self.ctime_ns);
        w.i64(self.btime_ns);
        w.u64(self.parent);
        v
    }
    pub(crate) fn decode(ino: u64, v: &[u8]) -> Result<Attr> {
        let mut r = Reader::new(v, "inode", ino);
        let kind = FileKind::from_u8(r.u8()?).ok_or(Error::Corrupt("inode kind", ino))?;
        r.skip(3)?;
        let perm = r.u32()?;
        let uid = r.u32()?;
        let gid = r.u32()?;
        let nlink = r.u32()?;
        r.skip(4)?;
        let a = Attr {
            ino,
            kind,
            perm,
            uid,
            gid,
            nlink,
            size: r.u64()?,
            atime_ns: r.i64()?,
            mtime_ns: r.i64()?,
            ctime_ns: r.i64()?,
            btime_ns: r.i64()?,
            parent: r.u64()?,
        };
        r.skip(r.remaining())?;
        if perm > 0o7777 || a.size > MAX_FILE_SIZE {
            return Err(Error::Corrupt("inode fields", ino));
        }
        Ok(a)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetAttr {
    pub perm: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime_ns: Option<i64>,
    pub mtime_ns: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub kind: FileKind,
    /// Pass this to `readdir_from` to continue after this entry's bucket.
    pub cookie: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFs {
    pub total_blocks: u64,
    pub free_blocks: u64,
    /// Free blocks available to ordinary writes (excludes the reserve and quarantine).
    pub available_blocks: u64,
    pub generation: u64,
    pub encrypted: bool,
}

/// A data extent: `len` blocks at `phys` holding file blocks `[start, start+len)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Extent {
    pub phys: u64,
    pub len: u64,
    pub birth: u64,
    pub nonce: [u8; NONCE_LEN],
    pub macs: Vec<[u8; MAC_LEN]>,
}

const EXTENT_HEADER: usize = 48;

impl Extent {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut v = vec![0u8; EXTENT_HEADER + self.macs.len() * MAC_LEN];
        let mut w = Writer::new(&mut v);
        w.u64(self.phys);
        w.u32(self.len as u32);
        w.u32(0);
        w.u64(self.birth);
        w.bytes(&self.nonce);
        for m in &self.macs {
            w.bytes(m);
        }
        v
    }
    pub(crate) fn decode(v: &[u8], at: u64) -> Result<Extent> {
        let mut r = Reader::new(v, "extent", at);
        let phys = r.u64()?;
        let len = r.u32()? as u64;
        if r.u32()? != 0 || len == 0 || len > EXTENT_MAX_BLOCKS {
            return Err(r.err());
        }
        let birth = r.u64()?;
        let nonce = r.array()?;
        let mut macs = Vec::with_capacity(len as usize);
        for _ in 0..len {
            macs.push(r.array()?);
        }
        r.finish()?;
        if phys == 0 || phys.checked_add(len).is_none() {
            return Err(Error::Corrupt("extent address", at));
        }
        Ok(Extent {
            phys,
            len,
            birth,
            nonce,
            macs,
        })
    }
    /// The part of this extent covering its blocks `[from, to)` (relative).
    pub(crate) fn slice(&self, from: u64, to: u64) -> Extent {
        Extent {
            phys: self.phys + from,
            len: to - from,
            birth: self.birth,
            nonce: crypto::nonce_add(&self.nonce, from),
            macs: self.macs[from as usize..to as usize].to_vec(),
        }
    }
}

pub(crate) fn encode_dirents(entries: &[(Vec<u8>, u64, FileKind)]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (name, ino, kind) in entries {
        v.extend_from_slice(&ino.to_le_bytes());
        v.push(*kind as u8);
        v.push(name.len() as u8);
        v.extend_from_slice(name);
    }
    v
}

pub(crate) fn decode_dirents(v: &[u8], at: u64) -> Result<Vec<(Vec<u8>, u64, FileKind)>> {
    let mut r = Reader::new(v, "directory entries", at);
    let n = r.u16()? as usize;
    let mut out = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        let ino = r.u64()?;
        let kind = FileKind::from_u8(r.u8()?).ok_or(r.err())?;
        let len = r.u8()? as usize;
        let name = r.bytes(len)?.to_vec();
        if !valid_name(&name) || ino == 0 {
            return Err(r.err());
        }
        out.push((name, ino, kind));
    }
    r.finish()?;
    if n == 0 {
        return Err(r.err());
    }
    Ok(out)
}

pub(crate) fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Encrypts the volume when set.
    pub passphrase: Option<Vec<u8>>,
    pub kdf: KdfParams,
    /// Minimum age before freed blocks may be reused (see [`MountOptions`]).
    pub quarantine_min_ns: i64,
    /// Metadata chunk size in blocks: a power of two from 1 to 64, or 0 to
    /// size it to the device ([`auto_chunk_blocks`]). Fixed for the life
    /// of the volume.
    pub meta_chunk_blocks: u32,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            passphrase: None,
            kdf: KdfParams::DEFAULT,
            quarantine_min_ns: DEFAULT_QUARANTINE_NS,
            meta_chunk_blocks: 0,
        }
    }
}

/// Default metadata chunk size for a `total`-block device: 64 blocks
/// (256 KiB) from 4096 blocks (16 MiB) up, smaller below so tiny volumes
/// don't spend their space on half-empty chunks (but at least 4).
pub fn auto_chunk_blocks(total: u64) -> u32 {
    let x = (total / 64).clamp(4, 64);
    1 << (63 - x.leading_zeros())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOptions {
    pub passphrase: Option<Vec<u8>>,
    pub read_only: bool,
    /// Decoded tree nodes kept in memory.
    pub cache_nodes: usize,
    /// Dirty file data kept in memory before it is written out early
    /// (still committed atomically with everything else).
    pub dirty_limit_bytes: usize,
    /// Freed blocks are not reused until they are both
    /// [`crate::space::QUARANTINE_COMMITS`] commits and this many
    /// nanoseconds old, so a drive that silently dropped recent flushes
    /// still finds an intact older tree. See [`DEFAULT_QUARANTINE_NS`].
    pub quarantine_min_ns: i64,
    /// After an unclean shutdown, also read back file data written inside
    /// the quarantine window (metadata in the window is always checked), so
    /// mount falls back to an intact commit instead of later read errors.
    pub verify_recent_data: bool,
    /// Run a full fsck (with data) on each candidate commit; mount the
    /// newest that passes. Slow: for rescue and tests.
    pub fsck_on_mount: bool,
}

/// No extra time hold by default: the storage design promises fallback
/// within K = 8 commits, and the VFS rate-limits full commits (fsync bursts
/// go to the intent log), so 8 commits already span several seconds. A
/// non-zero value adds a minimum age on top, for drives known to cache
/// writes for longer; it costs free space while blocks wait.
pub const DEFAULT_QUARANTINE_NS: i64 = 0;

impl Default for MountOptions {
    fn default() -> Self {
        MountOptions {
            passphrase: None,
            read_only: false,
            cache_nodes: 8192,
            dirty_limit_bytes: 64 << 20,
            quarantine_min_ns: DEFAULT_QUARANTINE_NS,
            verify_recent_data: true,
            fsck_on_mount: false,
        }
    }
}

/// A mounted OptimalFS volume.
///
/// Changes are grouped into transactions. [`Filesystem::commit`] makes
/// everything since the last commit durable atomically: after a power cut
/// the volume is exactly as it was at some completed commit, never a mix.
pub struct Filesystem<D: BlockDevice> {
    pub(crate) store: Store<D>,
    pub(crate) fs_tree: Tree,
    pub(crate) meta: Tree,
    pub(crate) header: Header,
    pub(crate) body: Body,
    pub(crate) next_ino: u64,
    pub(crate) next_snap_id: u64,
    pub(crate) live_deadlist: u64,
    pub(crate) next_deadlist: u64,
    dirty_data: BTreeMap<u64, BTreeMap<u64, Block>>,
    dirty_blocks: usize,
    txn_dirty: bool,
    poisoned: bool,
    read_only: bool,
    dirty_limit_blocks: usize,
    clock: Box<dyn Clock>,
    /// The next commit's generation is above this: the newest generation
    /// seen in any verified superblock, which can exceed `body.generation`
    /// after mount fell back past commits that did not fully land.
    gen_floor: u64,
    /// Flags for the next commit ([`superblock::FLAG_CLEAN`] at unmount).
    next_flags: u64,
    /// Write the next commit to all four superblock slots.
    rewrite_all_slots: bool,
    fell_back_from: Option<u64>,
    recovery: Option<crate::recover::RecoveryStats>,
    /// The next commit raises the replay value by one.
    replay_bump: bool,
    /// Data blocks the operations of the open transaction asked
    /// [`ensure_space`](Self::ensure_space) for (written by the next commit,
    /// so they stay budgeted until then).
    txn_data: u64,
    /// The same for logical metadata blocks.
    txn_meta: u64,
    /// Per-commit overhead of recent commits: (logical metadata blocks,
    /// chunk-table node pairs) that the commit itself allocated beyond the
    /// trees, newest last, at most [`COST_RING`] entries.
    commit_costs: VecDeque<(u64, u64)>,
    /// Commits kept failing to write ([`Error::DeviceFailing`]): changes
    /// stay in memory and reads work, but new changes are refused until a
    /// [`commit`](Filesystem::commit) succeeds.
    degraded: bool,
    /// The commit in progress has started writing superblocks, so its
    /// generation may be on disk even if it fails.
    sb_started: bool,
    /// Early data flushes (dirty data over the limit) that failed since
    /// the last successful commit.
    early_flush_failures: u32,
    /// Attempts the last `commit` call made (tests).
    pub(crate) last_commit_attempts: u32,
    /// State before the operation in progress (see [`op`](Self::op)).
    op_cp: Option<OpCheckpoint>,
    /// The last operation's undo log, emptied, kept for its capacity.
    undo_spare: Vec<Undo>,
    /// Dirty-data changes of the operation in progress, made once it
    /// succeeds (see [`DataChange`]).
    op_data: Vec<DataChange>,
    /// Operations rolled back after failing part way (tests).
    pub(crate) ops_undone: u64,
    /// Superblock slots (bit per slot) a write failed on during this
    /// mount; commits write around them (see [`superblock::plan_waves`]).
    pub(crate) dead_slots: u8,
}

/// What [`Filesystem::op`] restores when an operation fails part way,
/// besides the undo log ([`Store::undo`]) the trees keep while it runs.
struct OpCheckpoint {
    /// Whole trees, for operations whose undo is not item by item
    /// (snapshots: see [`Filesystem::op_whole`]). They share their dirty
    /// nodes with the live trees, so taking them is a pointer copy (a
    /// change then copies only the nodes it touches).
    trees: Option<(Tree, Tree)>,
    log_len: usize,
    dead_len: usize,
    next_ino: u64,
    next_snap_id: u64,
    live_deadlist: u64,
    next_deadlist: u64,
    latest_snap_gen: u64,
    txn_dirty: bool,
    txn_data: u64,
    txn_meta: u64,
}

/// A change to the dirty file data that an operation makes only once it
/// has succeeded, so a failure part way leaves the dirty data as it was.
/// (Writes need none: their data goes in after their last step that can
/// fail.)
enum DataChange {
    /// Drop all of a file's dirty blocks (the file was deleted).
    DropFile(u64),
    /// Drop a file's dirty blocks from index `.1` on (it was shrunk).
    DropFrom(u64, u64),
    /// Zero block `idx` of file `ino` from byte `from` on (the new last
    /// block after a shrink). `old` is its content read beforehand when it
    /// was not dirty.
    ZeroTail {
        ino: u64,
        idx: u64,
        from: usize,
        old: Option<Block>,
    },
}

/// Most attempts one [`Filesystem::commit`] call makes (the storage
/// design's "at most 3 writeback cycles"); early data flushes that fail
/// this many times before a commit succeeds degrade the volume too.
pub const COMMIT_ATTEMPTS: u32 = 3;

/// Blocks a failed write condemns together (see
/// [`Filesystem::note_failed_writes`]): 64 blocks, 256 KiB (a small
/// flash erase block).
pub const BAD_GROUP: u64 = 64;

/// A run of file blocks queued for writing by
/// [`Filesystem::flush_data`], kept until the write is known to have
/// worked so unwritten blocks can be taken back.
struct QueuedRun {
    ino: u64,
    idx: u64,
    phys: u64,
    n: u64,
    birth: u64,
    nonce: [u8; NONCE_LEN],
    macs: Vec<[u8; MAC_LEN]>,
}

/// Smallest volume `format` accepts. Metadata is stored twice and freed
/// space is quarantined for QUARANTINE_COMMITS commits, so the space kept
/// back for the next commit and the drain after it (see
/// [`Filesystem::space_needed`]) is a fixed few hundred blocks: about 29% of a
/// 1024-block volume, 11% at 4096, 2.3% (the 2% gap) from 65536 up.
pub const MIN_BLOCKS: u64 = 1024;

/// How many recent commits' overhead [`Filesystem::commit_costs`] keeps.
const COST_RING: usize = 2 * (crate::space::QUARANTINE_COMMITS as usize + 1);

impl<D: BlockDevice> core::fmt::Debug for Filesystem<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Filesystem(generation {}, {} blocks)",
            self.body.generation,
            self.store.space.total_blocks()
        )
    }
}

/// Minimum blocks kept back from ordinary writes, so a commit (and deletes,
/// which also need new metadata blocks) can always complete.
fn reserve_for(total: u64) -> u64 {
    (total / 50).clamp(64, 2 * 1024 * 1024)
}

impl<D: BlockDevice> Filesystem<D> {
    /// Creates a new, empty filesystem on `dev` and mounts it.
    pub fn format(
        mut dev: D,
        opts: &FormatOptions,
        rng: &mut dyn Entropy,
        clock: Box<dyn Clock>,
    ) -> Result<Self> {
        let total = dev.num_blocks();
        if total < MIN_BLOCKS {
            return Err(Error::Invalid(
                "device too small (need at least 1024 blocks, 4 MiB)",
            ));
        }
        let mut uuid = [0u8; 16];
        rng.fill(&mut uuid);
        let mut salt = [0u8; 32];
        rng.fill(&mut salt);
        let chunk_blocks = match opts.meta_chunk_blocks {
            0 => auto_chunk_blocks(total),
            c if c.is_power_of_two() && c <= 64 => c,
            _ => {
                return Err(Error::Invalid(
                    "metadata chunk size must be 1, 2, 4, .. or 64",
                ));
            }
        };
        let mut header = Header {
            uuid,
            total_blocks: total,
            suite: SUITE_PLAIN,
            compat: 0,
            ro_compat: 0,
            incompat: 0,
            chunk_shift: chunk_blocks.trailing_zeros(),
            generation: 0,
            slot: 0,
            kdf: opts.kdf,
            salt,
            keyslots: [KeySlot::default(); KEYSLOTS],
        };
        if opts.passphrase.is_some()
            && !opts.kdf.meets_floor()
            && opts.kdf != KdfParams::INSECURE_FAST_FOR_TESTS
        {
            return Err(Error::Invalid(
                "key derivation below the floor (64 MiB, 4 passes)",
            ));
        }
        let master = match &opts.passphrase {
            None => None,
            Some(p) => {
                let mut m = SecretKey([0; 32]);
                rng.fill(&mut m.0);
                header.suite = SUITE_XCHACHA20POLY1305;
                header.set_keyslot(0, p, &m, rng)?;
                Some(m)
            }
        };
        // Wipe all superblock slots so no stale volume can be found.
        let zero = zero_block();
        for slot in 0..4 {
            dev.write(superblock::slot_addr(slot, total), &zero[..])?;
        }
        let sealer = Sealer::new(uuid, master.as_ref(), rng);
        let mut space = Space::format(total, header.chunk_shift)?;
        space.quarantine_min_ns = opts.quarantine_min_ns;
        let store = Store {
            io: Io::new(dev, sealer),
            space,
            cache: NodeCache::new(8192),
            latest_snap_gen: 0,
            dead_pending: Vec::new(),
            log: Vec::new(),
            written: BTreeMap::new(),
            undo: None,
            space_unlogged: false,
        };
        let body = Body {
            generation: 0,
            chunk_root: BlockPtr::NULL,
            chunk_depth: 0,
            root_block: BlockPtr::NULL,
            fs_root: BlockPtr::NULL,
            meta_root: BlockPtr::NULL,
            space_root: BlockPtr::NULL,
            free_log: BlockPtr::NULL,
            next_ino: ROOT_INO + 1,
            next_snap_id: 1,
            live_deadlist: 1,
            next_deadlist: 2,
            latest_snap_gen: 0,
            last_move_gen: 0,
            commit_time_ns: 0,
            replay_value: 0,
            flags: 0,
            window_gen: 0,
            log_root: BlockPtr::NULL,
            history: Vec::new(),
        };
        let mut fs = Filesystem {
            store,
            fs_tree: Tree::new(TreeId::Fs, BlockPtr::NULL)?,
            meta: Tree::new(TreeId::Meta, BlockPtr::NULL)?,
            header,
            body,
            next_ino: ROOT_INO + 1,
            next_snap_id: 1,
            live_deadlist: 1,
            next_deadlist: 2,
            dirty_data: BTreeMap::new(),
            dirty_blocks: 0,
            txn_dirty: true,
            poisoned: false,
            read_only: false,
            dirty_limit_blocks: MountOptions::default().dirty_limit_bytes / BLOCK_SIZE,
            clock,
            gen_floor: 0,
            next_flags: 0,
            rewrite_all_slots: false,
            fell_back_from: None,
            recovery: None,
            replay_bump: false,
            txn_data: 0,
            txn_meta: 0,
            commit_costs: VecDeque::new(),
            degraded: false,
            sb_started: false,
            early_flush_failures: 0,
            last_commit_attempts: 0,
            op_cp: None,
            undo_spare: Vec::new(),
            op_data: Vec::new(),
            ops_undone: 0,
            dead_slots: 0,
        };
        let now = fs.clock.now_ns();
        let root = Attr {
            ino: ROOT_INO,
            kind: FileKind::Dir,
            perm: 0o755,
            uid: 0,
            gid: 0,
            nlink: 2,
            size: 0,
            atime_ns: now,
            mtime_ns: now,
            ctime_ns: now,
            btime_ns: now,
            parent: ROOT_INO,
        };
        fs.put_inode(&root)?;
        fs.commit()?;
        Ok(fs)
    }

    /// Mounts the newest commit that fully verifies.
    ///
    /// Candidates are every superblock copy that verifies plus the commits
    /// in their history rings, newest first. A candidate is accepted when
    /// its roots read back and, unless it is a cleanly unmounted commit,
    /// every metadata block born inside its quarantine window does too (and
    /// file data, with [`MountOptions::verify_recent_data`]). If mount had
    /// to skip newer commits, new commits are numbered above all of them
    /// and the first one overwrites every superblock slot.
    pub fn mount(
        mut dev: D,
        opts: &MountOptions,
        rng: &mut dyn Entropy,
        mut clock: Box<dyn Clock>,
    ) -> Result<Self> {
        let cands = superblock::candidates(&mut dev)?;
        if cands.is_empty() {
            return Err(Error::NoFilesystem);
        }
        let mut last_err = Error::NoFilesystem;
        let mut unlocked: Vec<([u8; 32], [KeySlot; KEYSLOTS], SecretKey)> = Vec::new();
        // (commit, its superblock's header, master key, from a history ring)
        let mut commits: Vec<(Body, Header, Option<SecretKey>, bool)> = Vec::new();
        let mut floor = 0u64;
        for c in &cands {
            if c.header.ro_compat != 0 && !opts.read_only {
                last_err =
                    Error::Unsupported("unknown read-only-compatible features: mount read-only");
                continue;
            }
            let master = if c.header.suite == SUITE_PLAIN {
                None
            } else {
                let Some(pass) = &opts.passphrase else {
                    return Err(Error::KeyRequired);
                };
                if let Some((_, _, m)) = unlocked
                    .iter()
                    .find(|(s, k, _)| *s == c.header.salt && *k == c.header.keyslots)
                {
                    Some(m.clone())
                } else {
                    match c.header.unlock(pass) {
                        Ok(m) => {
                            unlocked.push((c.header.salt, c.header.keyslots, m.clone()));
                            Some(m)
                        }
                        Err(e) => {
                            last_err = e;
                            continue;
                        }
                    }
                }
            };
            let sealer = Sealer::new(c.header.uuid, master.as_ref(), rng);
            let body = match superblock::open_body(c, &sealer) {
                Ok(b) => b,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            floor = floor.max(body.generation);
            for h in &body.history {
                commits.push((h.clone(), c.header.clone(), master.clone(), true));
            }
            commits.push((body, c.header.clone(), master, false));
        }
        // Newest first; for one generation prefer a superblock's own copy.
        commits.sort_by_key(|(b, _, _, hist)| (core::cmp::Reverse(b.generation), *hist));
        commits.dedup_by_key(|(b, _, _, _)| b.generation);

        // Pass 0 needs every block born in a candidate's window, data
        // included, to verify: recent blocks that fail after an unclean
        // shutdown are lost writes, so an older commit is the right answer.
        // If no candidate passes, pass 1 accepts the newest one whose
        // metadata verifies and reports the files with lost data.
        let mut dev = Some(dev);
        let passes: &[bool] = if opts.verify_recent_data {
            &[true, false]
        } else {
            &[false]
        };
        for (body, header, master, from_history, strict) in passes.iter().flat_map(|&p| {
            commits
                .iter()
                .cloned()
                .map(move |(b, h, m, f)| (b, h, m, f, p))
        }) {
            let sealer = Sealer::new(header.uuid, master.as_ref(), rng);
            let mut io = Io::new(dev.take().unwrap(), sealer);
            let clean = body.flags & superblock::FLAG_CLEAN != 0 && !from_history;
            // After an unclean shutdown both copies of every block read
            // during the check must be good (a bad one is repaired).
            io.copy_mode = if clean {
                CopyMode::Either
            } else {
                CopyMode::Both
            };
            io.repair_enabled = !opts.read_only;
            let mut body = body;
            let (mut space, fs_tree, meta) = match Self::open_commit(&mut io, &header, &mut body) {
                Ok(x) => x,
                Err(e) => {
                    last_err = e;
                    dev = Some(io.dev);
                    continue;
                }
            };
            space.quarantine_min_ns = opts.quarantine_min_ns;
            let generation = body.generation;
            let mut header = header;
            header.generation = generation;
            let mut fs = Filesystem {
                store: Store {
                    io,
                    space,
                    cache: NodeCache::new(opts.cache_nodes),
                    latest_snap_gen: body.latest_snap_gen,
                    dead_pending: Vec::new(),
                    log: Vec::new(),
                    written: BTreeMap::new(),
                    undo: None,
                    space_unlogged: false,
                },
                fs_tree,
                meta,
                header,
                next_ino: body.next_ino,
                next_snap_id: body.next_snap_id,
                live_deadlist: body.live_deadlist,
                next_deadlist: body.next_deadlist,
                body,
                dirty_data: BTreeMap::new(),
                dirty_blocks: 0,
                txn_dirty: false,
                poisoned: false,
                read_only: opts.read_only,
                dirty_limit_blocks: (opts.dirty_limit_bytes / BLOCK_SIZE).max(16),
                clock,
                gen_floor: 0,
                next_flags: 0,
                rewrite_all_slots: false,
                fell_back_from: None,
                recovery: None,
                replay_bump: false,
                txn_data: 0,
                txn_meta: 0,
                commit_costs: VecDeque::new(),
                degraded: false,
                sb_started: false,
                early_flush_failures: 0,
                last_commit_attempts: 0,
                op_cp: None,
                undo_spare: Vec::new(),
                op_data: Vec::new(),
                ops_undone: 0,
                dead_slots: 0,
            };
            let check = if clean {
                Ok(())
            } else {
                let min_birth = fs.body.window_gen;
                crate::recover::verify_since(&mut fs, min_birth, opts.verify_recent_data, strict)
                    .map(|st| fs.recovery = Some(st))
            };
            let check = check.and_then(|()| {
                if opts.fsck_on_mount {
                    let r = crate::fsck::check(&mut fs, true)?;
                    if !r.ok() {
                        return Err(Error::Corrupt("fsck at mount found errors", generation));
                    }
                }
                Ok(())
            });
            fs.store.io.copy_mode = CopyMode::Either;
            match check {
                Ok(()) => {
                    if generation < floor {
                        // Newer commits exist on disk but did not fully land.
                        // Number new commits above them so they never outrank
                        // ours. Their blocks are free in this commit's map and
                        // will be reused; that is safe because every block they
                        // could still need was born inside their window, which
                        // mount verifies (a reused block fails its MAC). As
                        // defence in depth the first commit also overwrites
                        // every superblock slot, so no stale copy lingers.
                        fs.gen_floor = floor;
                        fs.rewrite_all_slots = true;
                        fs.fell_back_from = Some(floor);
                        // The first commit runs even if nothing changes, so
                        // the skipped copies are replaced as soon as possible.
                        if !fs.read_only {
                            fs.txn_dirty = true;
                        }
                    }
                    return Ok(fs);
                }
                Err(e) => {
                    last_err = e;
                    clock = fs.clock_take();
                    dev = Some(fs.into_device());
                }
            }
        }
        Err(last_err)
    }

    fn clock_take(&mut self) -> Box<dyn Clock> {
        core::mem::replace(&mut self.clock, Box::new(ZeroClock))
    }

    /// The newer generation mount had to skip, if it fell back.
    pub fn fell_back_from(&self) -> Option<u64> {
        self.fell_back_from
    }

    /// What mount's recovery check read (None after a clean unmount).
    pub fn recovery_stats(&self) -> Option<&crate::recover::RecoveryStats> {
        self.recovery.as_ref()
    }

    /// The anti-rollback counter of the mounted commit.
    pub fn replay_value(&self) -> u64 {
        self.body.replay_value
    }

    /// Raises the anti-rollback counter by one in a commit written to all
    /// four superblock slots (both pairs, each flushed), so every copy that
    /// mount could pick carries the new value. Only after this returns may
    /// the caller advance its TPM monotonic counter; a crash before that
    /// leaves the old value, which the TPM still accepts. Returns the new value.
    pub fn bump_replay(&mut self) -> Result<u64> {
        self.check_writable()?;
        self.replay_bump = true;
        self.txn_dirty = true;
        let r = self.commit();
        self.replay_bump = false;
        r?;
        Ok(self.body.replay_value)
    }

    /// Commits any pending changes and marks the volume cleanly unmounted,
    /// which lets the next mount skip the recovery check. Returns the device.
    pub fn unmount(mut self) -> Result<D> {
        if !self.read_only && !self.poisoned {
            self.next_flags = superblock::FLAG_CLEAN;
            self.txn_dirty = true;
            self.commit()?;
        }
        Ok(self.store.io.dev)
    }

    /// Opens commit `body` (a core from a superblock or its history):
    /// its chunk table, its root block (filling the rest of `body`), its
    /// space map, and checks its tree roots read back.
    fn open_commit(io: &mut Io<D>, h: &Header, body: &mut Body) -> Result<(Space, Tree, Tree)> {
        let total = io.num_blocks();
        let mut space = Space::open(io, total, h.chunk_shift, body.chunk_root, body.chunk_depth)?;
        let rb = space.read_meta(io, BlockKind::Root, &body.root_block)?;
        body.apply_root(&rb[..])?;
        if body.fs_root.is_null()
            || body.space_root.is_null()
            || body.next_ino <= ROOT_INO
            || body.live_deadlist >= body.next_deadlist
        {
            return Err(Error::Corrupt("root block fields", body.generation));
        }
        space.attach(io, body.space_root, body.free_log)?;
        let fs_tree = Tree::new(TreeId::Fs, body.fs_root)?;
        let meta = Tree::new(TreeId::Meta, body.meta_root)?;
        // Make sure the roots themselves are readable now, so a damaged
        // newest commit falls back to the previous one instead of failing later.
        let mut b = space.read_meta(io, BlockKind::Node, &body.fs_root)?;
        crate::tree::Node::decode(&b[..], TreeId::Fs, &body.fs_root)?;
        if !body.meta_root.is_null() {
            b = space.read_meta(io, BlockKind::Node, &body.meta_root)?;
            crate::tree::Node::decode(&b[..], TreeId::Meta, &body.meta_root)?;
        }
        Ok((space, fs_tree, meta))
    }

    /// Unmounts, returning the device. Uncommitted changes are discarded;
    /// call [`Filesystem::commit`] first to keep them.
    pub fn into_device(self) -> D {
        self.store.io.dev
    }

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.store.io.dev
    }

    pub fn device(&self) -> &D {
        &self.store.io.dev
    }

    /// Writes the dirty file data now (tests).
    #[doc(hidden)]
    pub fn flush_data_for_tests(&mut self) -> Result<()> {
        self.flush_data()
    }

    /// Makes the allocator continue its search at block `b` (tests use it
    /// to put data in far regions of huge volumes).
    #[doc(hidden)]
    pub fn set_alloc_cursor_for_tests(&mut self, b: u64) {
        self.store.space.set_cursor(b);
    }

    /// Moves metadata chunk `id` to new physical blocks in the next commit
    /// (both copies; pointers are logical, so nothing else changes). For
    /// moving metadata off a failing region, balancing, and tests.
    pub fn relocate_metadata_chunk(&mut self, id: u64) -> Result<()> {
        self.check_writable()?;
        if !self
            .store
            .space
            .chunks
            .entry(&mut self.store.io, id)?
            .mapped()
        {
            return Err(Error::NotFound);
        }
        if !self.store.space.relocations.contains(&id) {
            self.store.space.relocations.push(id);
        }
        self.txn_dirty = true;
        Ok(())
    }

    /// Every mapped metadata chunk of the current state: (id, entry).
    pub fn metadata_chunks(&mut self) -> Result<Vec<(u64, crate::chunk::Entry)>> {
        self.store.space.chunks.all_entries(&mut self.store.io)
    }

    /// The volume's metadata chunk size in blocks.
    pub fn metadata_chunk_blocks(&self) -> u64 {
        self.store.space.chunks.chunk_blocks()
    }

    /// The logical address of slot `off` of metadata chunk `id`.
    pub fn metadata_address(&self, id: u64, off: u64) -> u64 {
        self.store.space.chunks.join(id, off)
    }

    #[doc(hidden)]
    pub fn commit_forced_for_tests(&mut self) -> Result<()> {
        self.commit_forced()
    }

    /// Physical blocks of copies 0 and 1 of logical metadata block `l`.
    pub fn resolve_metadata(&mut self, l: u64) -> Result<(u64, u64)> {
        self.store.space.chunks.resolve(&mut self.store.io, l)
    }

    /// Both copies of every chunk-table block (tests damage them).
    #[doc(hidden)]
    pub fn chunk_table_blocks_for_tests(&mut self) -> Result<Vec<(u64, u64)>> {
        Ok(self
            .store
            .space
            .chunks
            .all_node_ptrs(&mut self.store.io)?
            .iter()
            .map(|(_, _, p)| (p.addr, p.aux))
            .collect())
    }

    /// The committed root block and filesystem-tree root pointers (tests
    /// use them to damage specific copies).
    #[doc(hidden)]
    pub fn roots_for_tests(&self) -> (u64, u64) {
        (self.body.root_block.addr, self.body.fs_root.addr)
    }

    #[doc(hidden)]
    /// Drops the decoded-node cache, so the next reads of committed tree
    /// nodes go to the device (cold-cache benchmarks, fault tests).
    pub fn drop_caches(&mut self) {
        self.store.cache.clear();
    }

    /// Operations undone after failing part way, since mount (tests).
    pub fn ops_undone_for_tests(&self) -> u64 {
        self.ops_undone
    }

    /// Whether an error stopped all writes until remount ([`Error::Poisoned`]).
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn next_ino_for_tests(&self) -> u64 {
        self.next_ino
    }

    /// The oldest generation the current commit's recovery check covers.
    #[doc(hidden)]
    pub fn window_gen_for_tests(&self) -> u64 {
        self.body.window_gen
    }

    /// The newest commit that moved a run of live metadata blocks.
    #[doc(hidden)]
    pub fn last_move_gen_for_tests(&self) -> u64 {
        self.body.last_move_gen
    }

    pub fn generation(&self) -> u64 {
        self.body.generation
    }

    pub fn io_stats(&self) -> crate::io::IoStats {
        self.store.io.stats
    }

    pub fn encrypted(&self) -> bool {
        self.store.io.sealer.encrypted()
    }

    pub fn statfs(&self) -> StatFs {
        let total = self.store.space.total_blocks();
        let avail = self
            .store
            .space
            .allocatable_blocks()
            .saturating_sub(self.space_needed(0, 0) + self.space_gap(false));
        StatFs {
            total_blocks: total,
            free_blocks: self.store.space.free_blocks(),
            available_blocks: avail,
            generation: self.body.generation,
            encrypted: self.encrypted(),
        }
    }

    pub(crate) fn txn_generation(&self) -> u64 {
        self.body.generation.max(self.gen_floor) + 1
    }

    fn check_writable(&self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if self.degraded {
            return Err(Error::DeviceFailing);
        }
        Ok(())
    }

    /// Whether commits kept failing to write, so new changes are refused
    /// (see [`Error::DeviceFailing`]).
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    /// Blocks this mount has found it cannot write to.
    pub fn bad_blocks(&self) -> u64 {
        self.store.space.bad.len()
    }

    /// Most bad blocks a mount accepts before it stops looking for new
    /// homes for failed writes: 1/64 of the volume, and at least 256 (one
    /// whole failed 1 MiB request).
    fn bad_limit(&self) -> u64 {
        (self.store.space.total_blocks() / 64).max(256)
    }

    /// Upper bounds for one commit's own overhead: (logical metadata blocks,
    /// chunk-table node pairs). The larger of what recent commits used and a
    /// structural floor: the root block, a free-log block, two bitmaps and
    /// one table per space-map level; one pair per chunk-table level plus
    /// one for growth.
    fn commit_overhead(&self) -> (u64, u64) {
        let s = &self.store.space;
        let l_floor = 4 + s.levels() as u64;
        let p_floor = s.chunks.depth() as u64 + 2;
        self.commit_costs
            .iter()
            .fold((l_floor, p_floor), |(l, p), &(cl, cp)| {
                (l.max(cl), p.max(cp))
            })
    }

    /// Physical blocks that must stay allocatable so that, after an
    /// operation needing `data` data blocks and `meta` logical metadata
    /// blocks, the next commit and then QUARANTINE_COMMITS + 1 empty commits
    /// (which drain freed space back into use) can all complete. Metadata
    /// slots come from mapped chunks first; each further chunk costs two
    /// copies of `chunk_blocks`; each chunk-table pair costs two blocks.
    /// Slots freed during the drain are not counted (they are quarantined).
    fn space_needed(&self, data: u64, meta: u64) -> u64 {
        let (l, p) = self.commit_overhead();
        let k = crate::space::QUARANTINE_COMMITS;
        let cb = self.store.space.chunks.chunk_blocks();
        let mut slots = self.store.space.usable_meta_slots();
        let mut chunks = 0;
        for want in [self.txn_meta + meta + l, (k + 1) * l] {
            let short = want.saturating_sub(slots);
            let new = short.div_ceil(cb);
            chunks += new;
            slots = slots + new * cb - want;
        }
        data + self.dirty_blocks as u64 + self.txn_data + 2 * cb * chunks + 2 * (k + 2) * p
    }

    /// Extra allocatable blocks an operation must leave on top of
    /// [`space_needed`](Self::space_needed). Deletes keep 1% of the volume
    /// (at least 32 blocks) so the allocator never runs completely dry.
    /// Ordinary operations keep enough more that a delete (16 metadata
    /// blocks) can still be committed and drained when they are refused:
    /// using up mapped metadata slots raises `space_needed` a whole chunk at
    /// a time, so the difference covers the chunks such a delete can map,
    /// one more for rounding, and a chunk-table path; and at least 2%.
    fn space_gap(&self, use_reserve: bool) -> u64 {
        let r = reserve_for(self.store.space.total_blocks());
        if use_reserve {
            return r / 2;
        }
        let (l, p) = self.commit_overhead();
        let cb = self.store.space.chunks.chunk_blocks();
        r.max(r / 2 + 2 * cb * ((16 + l).div_ceil(cb) + 1) + 2 * p)
    }

    /// Whether an operation needing `data_blocks` data blocks and
    /// `meta_blocks` logical metadata blocks passes the space check right
    /// now without committing (a create or mkdir needs 0 and 8; a delete
    /// passes `delete = true`).
    pub fn has_room_for(
        &mut self,
        data_blocks: u64,
        meta_blocks: u64,
        delete: bool,
    ) -> Result<bool> {
        self.store.space.refresh_releasing(&mut self.store.io)?;
        Ok(self.store.space.allocatable_blocks()
            >= self.space_needed(data_blocks, meta_blocks) + self.space_gap(delete))
    }

    /// How many empty commits make `need` blocks allocatable, counting only
    /// blocks already quarantined (by generation and age) and charging each
    /// commit its overhead (slot-aware, as in `space_needed`). `None` if no
    /// number up to QUARANTINE_COMMITS + 1 can, or a commit can't be afforded.
    fn drain_plan(&self, need: u64, now: i64) -> Option<u64> {
        let s = &self.store.space;
        let k = crate::space::QUARANTINE_COMMITS;
        let (l, p) = self.commit_overhead();
        let cb = s.chunks.chunk_blocks();
        let mut slots = s.usable_meta_slots();
        let mut avail = s.allocatable_blocks();
        let mut released = 0;
        let first = self.txn_generation();
        for j in 0..=k {
            let new = l.saturating_sub(slots).div_ceil(cb);
            let cost = 2 * cb * new + 2 * p;
            if avail < cost {
                return None;
            }
            avail -= cost;
            slots = slots + new * cb - l;
            let generation = first + j;
            while let Some(h) = s.held.get(released) {
                let old_enough = generation.saturating_sub(h.generation) >= k;
                let long_enough = now.saturating_sub(h.time_ns) >= s.quarantine_min_ns;
                if !(old_enough && long_enough) {
                    break;
                }
                avail += h.set.len();
                released += 1;
            }
            if avail >= need {
                return Some(j + 1);
            }
        }
        None
    }

    /// Fails with `NoSpace` unless an operation needing `data` data blocks
    /// and `meta` logical metadata blocks leaves enough allocatable space
    /// ([`space_needed`](Self::space_needed) plus [`space_gap`](Self::space_gap)).
    /// Deletes pass `use_reserve = true` and keep only half the gap. May
    /// commit, and cycle empty commits to drain quarantined space, but only
    /// after checking that the drain ends with enough space: a refusal writes
    /// nothing, and no commit is started that the volume can't afford.
    fn ensure_space(&mut self, data: u64, meta: u64, use_reserve: bool) -> Result<()> {
        self.store.space.refresh_releasing(&mut self.store.io)?;
        let need = |fs: &Self| fs.space_needed(data, meta) + fs.space_gap(use_reserve);
        if self.store.space.allocatable_blocks() < need(self) {
            // The open transaction's commit was budgeted by the operations
            // that made it. Committing it frees that budget; then
            // quarantined blocks become usable once they are
            // QUARANTINE_COMMITS commits and the minimum time old. Past the
            // time window the caller must wait and retry.
            if self.txn_dirty || self.dirty_blocks > 0 {
                self.commit_for_space()?;
                self.store.space.refresh_releasing(&mut self.store.io)?;
            }
            if self.store.space.allocatable_blocks() < need(self) {
                let now = self.clock.now_ns();
                let Some(cycles) = self.drain_plan(need(self), now) else {
                    return Err(Error::NoSpace);
                };
                for _ in 0..cycles {
                    if self.store.space.allocatable_blocks() >= need(self) {
                        break;
                    }
                    self.commit_for_space()?;
                    self.store.space.refresh_releasing(&mut self.store.io)?;
                }
                if self.store.space.allocatable_blocks() < need(self) {
                    return Err(Error::NoSpace);
                }
            }
        }
        self.txn_data += data;
        self.txn_meta += meta;
        Ok(())
    }

    /// A forced commit made by an operation to free space. If it fails to
    /// write, the volume is rebuilt and degraded, and the operation (which
    /// has changed nothing yet) is refused with [`Error::DeviceFailing`]
    /// rather than an I/O error that would stop the volume.
    fn commit_for_space(&mut self) -> Result<()> {
        match self.commit_forced() {
            Err(Error::Io(_) | Error::Corrupt(..)) if !self.poisoned && self.degraded => {
                Err(Error::DeviceFailing)
            }
            r => r,
        }
    }

    // ---- inode helpers ----

    pub(crate) fn get_inode(&mut self, ino: u64) -> Result<Attr> {
        if ino == 0 {
            return Err(Error::NotFound);
        }
        match self
            .fs_tree
            .get(&mut self.store, &Key::new(ino, K_INODE, 0))?
        {
            Some(v) => Attr::decode(ino, &v),
            None => Err(Error::NotFound),
        }
    }

    fn put_inode(&mut self, a: &Attr) -> Result<()> {
        self.txn_dirty = true;
        self.fs_tree
            .insert(&mut self.store, Key::new(a.ino, K_INODE, 0), a.encode())
    }

    pub fn getattr(&mut self, ino: u64) -> Result<Attr> {
        self.get_inode(ino)
    }

    fn get_bucket(&mut self, dir: u64, h: u64) -> Result<Vec<(Vec<u8>, u64, FileKind)>> {
        match self
            .fs_tree
            .get(&mut self.store, &Key::new(dir, K_DIRENT, h))?
        {
            Some(v) => decode_dirents(&v, dir),
            None => Ok(Vec::new()),
        }
    }

    fn put_bucket(&mut self, dir: u64, h: u64, b: &[(Vec<u8>, u64, FileKind)]) -> Result<()> {
        let k = Key::new(dir, K_DIRENT, h);
        self.txn_dirty = true;
        if b.is_empty() {
            self.fs_tree.remove(&mut self.store, &k)?;
            Ok(())
        } else {
            let v = encode_dirents(b);
            if v.len() > MAX_VALUE {
                return Err(Error::NoSpace);
            }
            self.fs_tree.insert(&mut self.store, k, v)
        }
    }

    fn dir_inode(&mut self, dir: u64) -> Result<Attr> {
        let a = self.get_inode(dir)?;
        if a.kind != FileKind::Dir {
            return Err(Error::NotDir);
        }
        Ok(a)
    }

    pub fn lookup(&mut self, dir: u64, name: &[u8]) -> Result<u64> {
        self.dir_inode(dir)?;
        if name.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        let h = self.store.io.sealer.name_hash(name);
        self.get_bucket(dir, h)?
            .into_iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, i, _)| i)
            .ok_or(Error::NotFound)
    }

    fn check_new_name(&mut self, dir: u64, name: &[u8]) -> Result<Attr> {
        if name.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        let d = self.dir_inode(dir)?;
        match self.lookup(dir, name) {
            Ok(_) => Err(Error::Exists),
            Err(Error::NotFound) => Ok(d),
            Err(e) => Err(e),
        }
    }

    fn add_entry(&mut self, dir: u64, name: &[u8], ino: u64, kind: FileKind) -> Result<()> {
        let h = self.store.io.sealer.name_hash(name);
        let mut b = self.get_bucket(dir, h)?;
        b.push((name.to_vec(), ino, kind));
        self.put_bucket(dir, h, &b)
    }

    fn remove_entry(&mut self, dir: u64, name: &[u8]) -> Result<(u64, FileKind)> {
        let h = self.store.io.sealer.name_hash(name);
        let mut b = self.get_bucket(dir, h)?;
        let i = b
            .iter()
            .position(|(n, _, _)| n == name)
            .ok_or(Error::NotFound)?;
        let (_, ino, kind) = b.remove(i);
        self.put_bucket(dir, h, &b)?;
        Ok((ino, kind))
    }

    fn new_inode(&mut self, kind: FileKind, perm: u32, parent: u64) -> Attr {
        let now = self.clock.now_ns();
        let ino = self.next_ino;
        self.next_ino += 1;
        Attr {
            ino,
            kind,
            perm: perm & 0o7777,
            uid: 0,
            gid: 0,
            nlink: if kind == FileKind::Dir { 2 } else { 1 },
            size: 0,
            atime_ns: now,
            mtime_ns: now,
            ctime_ns: now,
            btime_ns: now,
            parent: if kind == FileKind::Dir { parent } else { 0 },
        }
    }

    fn create_common(&mut self, dir: u64, name: &[u8], kind: FileKind, perm: u32) -> Result<Attr> {
        self.check_writable()?;
        let mut d = self.check_new_name(dir, name)?;
        self.ensure_space(0, 8, false)?;
        if kind == FileKind::Dir && d.nlink >= LINK_MAX {
            return Err(Error::TooManyLinks);
        }
        let a = self.new_inode(kind, perm, dir);
        self.put_inode(&a)?;
        self.add_entry(dir, name, a.ino, kind)?;
        let now = self.clock.now_ns();
        d.size += 1;
        if kind == FileKind::Dir {
            d.nlink += 1;
        }
        d.mtime_ns = now;
        d.ctime_ns = now;
        self.put_inode(&d)?;
        Ok(a)
    }

    /// Creates an empty regular file.
    fn create_inner(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        Ok(self.create_common(dir, name, FileKind::File, perm)?.ino)
    }

    fn mkdir_inner(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        Ok(self.create_common(dir, name, FileKind::Dir, perm)?.ino)
    }

    fn symlink_inner(&mut self, dir: u64, name: &[u8], target: &[u8]) -> Result<u64> {
        if target.is_empty() || target.len() > SYMLINK_MAX || target.contains(&0) {
            return Err(Error::Invalid("symlink target"));
        }
        let mut a = self.create_common(dir, name, FileKind::Symlink, 0o777)?;
        a.size = target.len() as u64;
        self.put_inode(&a)?;
        self.fs_tree.insert(
            &mut self.store,
            Key::new(a.ino, K_INLINE, 0),
            target.to_vec(),
        )?;
        Ok(a.ino)
    }

    pub fn readlink(&mut self, ino: u64) -> Result<Vec<u8>> {
        let a = self.get_inode(ino)?;
        if a.kind != FileKind::Symlink {
            return Err(Error::Invalid("not a symlink"));
        }
        self.fs_tree
            .get(&mut self.store, &Key::new(ino, K_INLINE, 0))?
            .ok_or(Error::Corrupt("symlink without target", ino))
    }

    /// Adds a hard link to an existing non-directory inode.
    fn link_inner(&mut self, ino: u64, dir: u64, name: &[u8]) -> Result<()> {
        self.check_writable()?;
        let mut a = self.get_inode(ino)?;
        if a.kind == FileKind::Dir {
            return Err(Error::IsDir);
        }
        if a.nlink >= LINK_MAX {
            return Err(Error::TooManyLinks);
        }
        let mut d = self.check_new_name(dir, name)?;
        self.ensure_space(0, 8, false)?;
        self.add_entry(dir, name, ino, a.kind)?;
        let now = self.clock.now_ns();
        a.nlink += 1;
        a.ctime_ns = now;
        self.put_inode(&a)?;
        d.size += 1;
        d.mtime_ns = now;
        d.ctime_ns = now;
        self.put_inode(&d)
    }

    /// Removes a non-directory entry. The inode and its data are freed when
    /// its last link goes.
    fn unlink_inner(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        self.check_writable()?;
        let ino = self.lookup(dir, name)?;
        let mut a = self.get_inode(ino)?;
        if a.kind == FileKind::Dir {
            return Err(Error::IsDir);
        }
        self.ensure_space(0, 8, true)?;
        self.remove_entry(dir, name)?;
        let now = self.clock.now_ns();
        let mut d = self.get_inode(dir)?;
        d.size -= 1;
        d.mtime_ns = now;
        d.ctime_ns = now;
        self.put_inode(&d)?;
        a.nlink -= 1;
        a.ctime_ns = now;
        if a.nlink == 0 {
            self.destroy_inode(&a)
        } else {
            self.put_inode(&a)
        }
    }

    fn rmdir_inner(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        self.check_writable()?;
        let ino = self.lookup(dir, name)?;
        let a = self.get_inode(ino)?;
        if a.kind != FileKind::Dir {
            return Err(Error::NotDir);
        }
        if a.size != 0 {
            return Err(Error::NotEmpty);
        }
        self.ensure_space(0, 8, true)?;
        self.remove_entry(dir, name)?;
        let now = self.clock.now_ns();
        let mut d = self.get_inode(dir)?;
        d.size -= 1;
        d.nlink -= 1;
        d.mtime_ns = now;
        d.ctime_ns = now;
        self.put_inode(&d)?;
        self.destroy_inode(&a)
    }

    /// Deletes an inode and everything it owns.
    fn destroy_inode(&mut self, a: &Attr) -> Result<()> {
        self.txn_dirty = true;
        self.defer(DataChange::DropFile(a.ino));
        self.punch(a.ino, 0, u64::MAX)?;
        self.fs_tree
            .remove(&mut self.store, &Key::new(a.ino, K_INLINE, 0))?;
        self.fs_tree
            .remove(&mut self.store, &Key::new(a.ino, K_INODE, 0))?;
        Ok(())
    }

    /// Whether `anc` is `ino` or one of its ancestors.
    fn is_ancestor(&mut self, anc: u64, mut ino: u64) -> Result<bool> {
        for _ in 0..1_000_000 {
            if ino == anc {
                return Ok(true);
            }
            if ino == ROOT_INO {
                return Ok(false);
            }
            ino = self.get_inode(ino)?.parent;
        }
        Err(Error::Corrupt("directory parent loop", ino))
    }

    /// POSIX rename: atomically moves `sdir/sname` to `ddir/dname`,
    /// replacing a compatible target if one exists.
    fn rename_inner(&mut self, sdir: u64, sname: &[u8], ddir: u64, dname: &[u8]) -> Result<()> {
        self.check_writable()?;
        let ino = self.lookup(sdir, sname)?;
        self.dir_inode(ddir)?;
        if dname.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(dname) {
            return Err(Error::InvalidName);
        }
        let mut a = self.get_inode(ino)?;
        let target = match self.lookup(ddir, dname) {
            Ok(t) => Some(t),
            Err(Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        if target == Some(ino) {
            return Ok(()); // same file: POSIX says do nothing
        }
        if a.kind == FileKind::Dir && sdir != ddir && self.is_ancestor(ino, ddir)? {
            return Err(Error::Invalid("cannot move a directory inside itself"));
        }
        if let Some(t) = target {
            let ta = self.get_inode(t)?;
            match (a.kind == FileKind::Dir, ta.kind == FileKind::Dir) {
                (true, false) => return Err(Error::NotDir),
                (false, true) => return Err(Error::IsDir),
                (true, true) if ta.size != 0 => return Err(Error::NotEmpty),
                _ => {}
            }
        }
        if a.kind == FileKind::Dir
            && sdir != ddir
            && target.is_none()
            && self.get_inode(ddir)?.nlink >= LINK_MAX
        {
            return Err(Error::TooManyLinks);
        }
        self.ensure_space(0, 16, true)?;
        let now = self.clock.now_ns();
        // Drop the replaced target.
        if let Some(t) = target {
            let mut ta = self.get_inode(t)?;
            self.remove_entry(ddir, dname)?;
            let mut d = self.get_inode(ddir)?;
            d.size -= 1;
            if ta.kind == FileKind::Dir {
                d.nlink -= 1;
            }
            self.put_inode(&d)?;
            if ta.kind == FileKind::Dir {
                self.destroy_inode(&ta)?;
            } else {
                ta.nlink -= 1;
                ta.ctime_ns = now;
                if ta.nlink == 0 {
                    self.destroy_inode(&ta)?
                } else {
                    self.put_inode(&ta)?
                }
            }
        }
        self.remove_entry(sdir, sname)?;
        self.add_entry(ddir, dname, ino, a.kind)?;
        let mut s = self.get_inode(sdir)?;
        s.size -= 1;
        if a.kind == FileKind::Dir && sdir != ddir {
            s.nlink -= 1;
        }
        s.mtime_ns = now;
        s.ctime_ns = now;
        self.put_inode(&s)?;
        let mut d = self.get_inode(ddir)?;
        d.size += 1;
        if a.kind == FileKind::Dir && sdir != ddir {
            d.nlink += 1;
        }
        d.mtime_ns = now;
        d.ctime_ns = now;
        self.put_inode(&d)?;
        if a.kind == FileKind::Dir {
            a.parent = ddir;
        }
        a.ctime_ns = now;
        self.put_inode(&a)
    }

    /// Directory entries with name hash >= `cookie`, at most about `max`
    /// (whole hash buckets are never split). Order is by name hash.
    pub fn readdir_from(&mut self, dir: u64, cookie: u64, max: usize) -> Result<Vec<DirEntry>> {
        self.dir_inode(dir)?;
        let mut out = Vec::new();
        let mut err = None;
        let max = max.max(1);
        self.fs_tree.scan(
            &mut self.store,
            &Key::new(dir, K_DIRENT, cookie),
            &Key::new(dir, K_DIRENT + 1, 0),
            &mut |k, v| {
                match decode_dirents(v, dir) {
                    Ok(b) => {
                        for (name, ino, kind) in b {
                            out.push(DirEntry {
                                name,
                                ino,
                                kind,
                                cookie: k.off.saturating_add(1),
                            });
                        }
                    }
                    Err(e) => {
                        err = Some(e);
                        return Ok(false);
                    }
                }
                Ok(out.len() < max && k.off != u64::MAX)
            },
        )?;
        if let Some(e) = err {
            return Err(e);
        }
        Ok(out)
    }

    pub fn readdir(&mut self, dir: u64) -> Result<Vec<DirEntry>> {
        self.readdir_from(dir, 0, usize::MAX)
    }

    pub fn setattr(&mut self, ino: u64, s: &SetAttr) -> Result<Attr> {
        self.op(|fs| fs.setattr_inner(ino, s))
    }

    fn setattr_inner(&mut self, ino: u64, s: &SetAttr) -> Result<Attr> {
        self.check_writable()?;
        let mut a = self.get_inode(ino)?;
        if s.perm.is_some_and(|p| p > 0o7777) {
            return Err(Error::Invalid("permission bits"));
        }
        let now = self.clock.now_ns();
        if let Some(size) = s.size {
            if a.kind != FileKind::File {
                return Err(if a.kind == FileKind::Dir {
                    Error::IsDir
                } else {
                    Error::Invalid("truncate of a non-file")
                });
            }
            self.truncate(ino, size)?;
            a = self.get_inode(ino)?;
        }
        if let Some(p) = s.perm {
            a.perm = p;
        }
        if let Some(u) = s.uid {
            a.uid = u;
        }
        if let Some(g) = s.gid {
            a.gid = g;
        }
        if let Some(t) = s.atime_ns {
            a.atime_ns = t;
        }
        if let Some(t) = s.mtime_ns {
            a.mtime_ns = t;
        }
        a.ctime_ns = now;
        self.txn_dirty = true;
        self.put_inode(&a)?;
        Ok(a)
    }

    // Public mutating operations: see `op`.
    pub fn create(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        self.op(|fs| fs.create_inner(dir, name, perm))
    }

    pub fn mkdir(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        self.op(|fs| fs.mkdir_inner(dir, name, perm))
    }

    pub fn symlink(&mut self, dir: u64, name: &[u8], target: &[u8]) -> Result<u64> {
        self.op(|fs| fs.symlink_inner(dir, name, target))
    }

    pub fn link(&mut self, ino: u64, dir: u64, name: &[u8]) -> Result<()> {
        self.op(|fs| fs.link_inner(ino, dir, name))
    }

    pub fn unlink(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        self.op(|fs| fs.unlink_inner(dir, name))
    }

    pub fn rmdir(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        self.op(|fs| fs.rmdir_inner(dir, name))
    }

    pub fn rename(&mut self, sdir: u64, sname: &[u8], ddir: u64, dname: &[u8]) -> Result<()> {
        self.op(|fs| fs.rename_inner(sdir, sname, ddir, dname))
    }

    pub fn write(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<usize> {
        let n = self.op(|fs| fs.write_inner(ino, off, data))?;
        // After the operation: the write has happened whatever the flush
        // does (see `flush_early`).
        if self.dirty_blocks > self.dirty_limit_blocks {
            self.flush_early()?;
        }
        Ok(n)
    }

    pub fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        self.op(|fs| fs.truncate_inner(ino, size))
    }

    /// Runs `f` against snapshot `id`, read-only. Uncommitted live changes
    /// are set aside meanwhile (not committed).
    pub fn with_snapshot<R>(
        &mut self,
        id: u64,
        f: impl FnOnce(&mut Self) -> Result<R>,
    ) -> Result<R> {
        let rec = crate::snapshot::list_records(self)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or(Error::NotFound)?;
        let live = core::mem::replace(&mut self.fs_tree, Tree::new(TreeId::Fs, rec.root)?);
        let data = core::mem::take(&mut self.dirty_data);
        let blocks = core::mem::take(&mut self.dirty_blocks);
        let ro = core::mem::replace(&mut self.read_only, true);
        let r = f(self);
        self.fs_tree = live;
        self.dirty_data = data;
        self.dirty_blocks = blocks;
        self.read_only = ro;
        r
    }

    /// Runs one public operation all or nothing: if `f` fails, everything
    /// it changed in memory is put back (tree items, counters, the space
    /// map, the dirty data), so a half-applied change can never be
    /// committed and the volume stays writable. Only a failure while
    /// putting things back (a read from a failing device) stops the volume
    /// ([`Error::Poisoned`] from then on).
    ///
    /// While `f` runs, the trees log each item they change with its old
    /// value ([`Store::undo`], moved, not copied), and file data an item
    /// stops pointing at is logged as released. Undo puts the items back
    /// newest first and takes those releases out of the space-map log,
    /// rebuilding the map only if any were made. Nodes made dirty on the
    /// way stay dirty: they hold the same items, and the free of their old
    /// copy stays. Dirty file data changes only once `f` succeeded
    /// ([`DataChange`]).
    ///
    /// A commit inside the operation (to free space, or a snapshot's) moves
    /// the checkpoint to the state after that commit: what a commit made
    /// durable is not undone. (A whole-tree operation's commit that fails
    /// keeps the checkpoint: the operation is undone.) Operations nest;
    /// the outermost one owns the checkpoint.
    pub(crate) fn op<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.run_op(false, f)
    }

    /// [`op`](Self::op) for operations that change the trees and the space
    /// map other than item by item (snapshots replace a whole tree and free
    /// blocks it still points at): the checkpoint keeps the whole trees,
    /// and undo restores them and replays the space-map log up to it. Each
    /// such operation commits first, so the trees it keeps are clean.
    pub(crate) fn op_whole<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.run_op(true, f)
    }

    fn run_op<T>(&mut self, whole: bool, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.op_cp.is_some() {
            return f(self);
        }
        self.op_cp = Some(self.checkpoint(whole));
        let r = f(self);
        let cp = self.op_cp.take().expect("operation checkpoint");
        let undo = self.store.undo.take();
        match r {
            Ok(v) => {
                if let Some(mut u) = undo {
                    u.clear();
                    self.undo_spare = u;
                }
                self.apply_data_changes();
                Ok(v)
            }
            Err(e) => {
                self.op_data.clear();
                if self.poisoned {
                    return Err(e);
                }
                if let Err(re) = self.undo_op(cp, undo.unwrap_or_default()) {
                    // Report why recovery stopped (a lost device, say).
                    self.poisoned = true;
                    return Err(re);
                }
                Err(e)
            }
        }
    }

    fn checkpoint(&mut self, whole: bool) -> OpCheckpoint {
        self.store.undo = (!whole).then(|| core::mem::take(&mut self.undo_spare));
        OpCheckpoint {
            trees: whole.then(|| (self.fs_tree.clone(), self.meta.clone())),
            log_len: self.store.log.len(),
            dead_len: self.store.dead_pending.len(),
            next_ino: self.next_ino,
            next_snap_id: self.next_snap_id,
            live_deadlist: self.live_deadlist,
            next_deadlist: self.next_deadlist,
            latest_snap_gen: self.store.latest_snap_gen,
            txn_dirty: self.txn_dirty,
            txn_data: self.txn_data,
            txn_meta: self.txn_meta,
        }
    }

    /// Puts back the state of `cp` (see [`op`](Self::op)).
    fn undo_op(&mut self, cp: OpCheckpoint, undo: Vec<Undo>) -> Result<()> {
        self.ops_undone += 1;
        self.next_ino = cp.next_ino;
        self.next_snap_id = cp.next_snap_id;
        self.live_deadlist = cp.live_deadlist;
        self.next_deadlist = cp.next_deadlist;
        self.store.latest_snap_gen = cp.latest_snap_gen;
        self.txn_data = cp.txn_data;
        self.txn_meta = cp.txn_meta;
        let mut rebuild = core::mem::take(&mut self.store.space_unlogged);
        if let Some((fs_tree, meta)) = cp.trees {
            self.fs_tree = fs_tree;
            self.meta = meta;
            self.txn_dirty = cp.txn_dirty;
            rebuild |= self.store.log.len() != cp.log_len;
            self.store.log.truncate(cp.log_len);
            self.store.dead_pending.truncate(cp.dead_len);
        } else {
            // `txn_dirty` stays set: nodes made dirty stay dirty.
            let log_frees = self.undo_items(undo)?;
            rebuild |= !log_frees.is_empty();
            for i in log_frees {
                self.store.log.remove(i);
            }
        }
        if rebuild {
            let relocs = self.store.space.relocations.clone();
            self.rebuild_space(relocs)?;
        }
        Ok(())
    }

    /// Puts back the tree items in `undo`, newest change first, and takes
    /// the releases of file data they point at out of
    /// [`Store::dead_pending`]. Returns the [`Store::log`] entries that
    /// freed such data, descending, for the caller to remove (the space
    /// map must then be rebuilt). Nodes this dirties free their old copies
    /// as usual (logged after every returned index).
    fn undo_items(&mut self, undo: Vec<Undo>) -> Result<Vec<usize>> {
        let mut log_frees = Vec::new();
        for u in undo.into_iter().rev() {
            match u {
                Undo::Item(t, key, old) => {
                    let tree = match t {
                        TreeId::Meta => &mut self.meta,
                        _ => &mut self.fs_tree,
                    };
                    match old {
                        Some(v) => tree.insert(&mut self.store, key, v)?,
                        None => {
                            tree.remove(&mut self.store, &key)?;
                        }
                    }
                }
                Undo::LogFree(i) => log_frees.push(i),
                // Recorded oldest first, undone newest first: descending,
                // so each removal leaves the indices still to come valid.
                Undo::DeadPending(i) => {
                    self.store.dead_pending.remove(i);
                }
            }
        }
        Ok(log_frees)
    }

    /// Queues a dirty-data change for when the operation succeeds.
    fn defer(&mut self, c: DataChange) {
        debug_assert!(self.op_cp.is_some(), "data change outside an operation");
        self.op_data.push(c);
    }

    fn apply_data_changes(&mut self) {
        if self.op_data.is_empty() {
            return;
        }
        let mut changes = core::mem::take(&mut self.op_data);
        for c in changes.drain(..) {
            match c {
                DataChange::DropFile(ino) => {
                    if let Some(m) = self.dirty_data.remove(&ino) {
                        self.dirty_blocks -= m.len();
                    }
                }
                DataChange::DropFrom(ino, keep) => {
                    if let Some(m) = self.dirty_data.get_mut(&ino) {
                        let dropped = m.split_off(&keep);
                        self.dirty_blocks -= dropped.len();
                        if m.is_empty() {
                            self.dirty_data.remove(&ino);
                        }
                    }
                }
                DataChange::ZeroTail {
                    ino,
                    idx,
                    from,
                    old,
                } => {
                    if let Some(b) = self.dirty_data.get_mut(&ino).and_then(|m| m.get_mut(&idx)) {
                        b[from..].fill(0);
                    } else if let Some(mut b) = old {
                        b[from..].fill(0);
                        self.put_dirty(ino, idx, b);
                    } else {
                        debug_assert!(false, "tail block neither dirty nor read");
                    }
                }
            }
        }
        self.op_data = changes;
    }

    // ---- file data ----

    fn extent_at(&mut self, ino: u64, idx: u64) -> Result<Option<(u64, Extent)>> {
        let found = self.fs_tree.last(
            &mut self.store,
            &Key::new(ino, K_EXTENT, 0),
            &Key::new(ino, K_EXTENT, idx.saturating_add(1)),
        )?;
        match found {
            Some((k, v)) => {
                let e = Extent::decode(&v, ino)?;
                if idx >= k.off && idx - k.off < e.len {
                    Ok(Some((k.off, e)))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Reads and verifies every block of an extent (recovery checks).
    pub(crate) fn verify_extent(&mut self, ino: u64, e: &Extent) -> Result<()> {
        if e.phys
            .checked_add(e.len)
            .is_none_or(|end| end > self.store.space.total_blocks())
        {
            return Err(Error::Corrupt("extent out of range", ino));
        }
        let mut buf = vec![0u8; BLOCK_SIZE];
        for r in 0..e.len {
            self.store.io.read_raw(e.phys + r, &mut buf)?;
            let nonce = crypto::nonce_add(&e.nonce, r);
            self.store.io.sealer.open(
                BlockKind::Data,
                e.phys + r,
                e.birth,
                &nonce,
                &e.macs[r as usize],
                &mut buf,
            )?;
        }
        Ok(())
    }

    /// Reads committed (on-disk) data for file blocks `[idx, idx+n)` into `out`.
    fn read_disk_blocks(&mut self, ino: u64, idx: u64, n: u64, out: &mut [u8]) -> Result<()> {
        let mut i = 0;
        while i < n {
            let cur = idx + i;
            match self.extent_at(ino, cur)? {
                None => {
                    out[i as usize * BLOCK_SIZE..(i as usize + 1) * BLOCK_SIZE].fill(0);
                    i += 1;
                }
                Some((start, e)) => {
                    let rel = cur - start;
                    let take = (e.len - rel).min(n - i);
                    let dst = &mut out[i as usize * BLOCK_SIZE..(i + take) as usize * BLOCK_SIZE];
                    if e.phys
                        .checked_add(e.len)
                        .is_none_or(|end| end > self.store.space.total_blocks())
                    {
                        return Err(Error::Corrupt("extent out of range", ino));
                    }
                    self.store.io.read_raw(e.phys + rel, dst)?;
                    for j in 0..take {
                        let r = rel + j;
                        let nonce = crypto::nonce_add(&e.nonce, r);
                        let b = &mut dst[j as usize * BLOCK_SIZE..(j as usize + 1) * BLOCK_SIZE];
                        self.store.io.sealer.open(
                            BlockKind::Data,
                            e.phys + r,
                            e.birth,
                            &nonce,
                            &e.macs[r as usize],
                            b,
                        )?;
                    }
                    i += take;
                }
            }
        }
        Ok(())
    }

    /// Reads up to `buf.len()` bytes at `off`; returns bytes read (0 at EOF).
    pub fn read(&mut self, ino: u64, off: u64, buf: &mut [u8]) -> Result<usize> {
        let a = self.get_inode(ino)?;
        match a.kind {
            FileKind::File => {}
            FileKind::Dir => return Err(Error::IsDir),
            FileKind::Symlink => return Err(Error::Invalid("read of a symlink")),
        }
        if off >= a.size || buf.is_empty() {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(a.size - off) as usize;
        let first = off / BLOCK_SIZE as u64;
        let last = (off + len as u64 - 1) / BLOCK_SIZE as u64;
        let mut tmp = vec![0u8; (last - first + 1) as usize * BLOCK_SIZE];
        // Runs of blocks not in the dirty cache are read from disk together.
        let mut i = first;
        while i <= last {
            let dirty = self.dirty_data.get(&ino).and_then(|m| m.get(&i)).cloned();
            if let Some(b) = dirty {
                let o = (i - first) as usize * BLOCK_SIZE;
                tmp[o..o + BLOCK_SIZE].copy_from_slice(&b[..]);
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j <= last
                && !self
                    .dirty_data
                    .get(&ino)
                    .is_some_and(|m| m.contains_key(&j))
            {
                j += 1;
            }
            let o = (i - first) as usize * BLOCK_SIZE;
            let e = (j - first) as usize * BLOCK_SIZE;
            self.read_disk_blocks(ino, i, j - i, &mut tmp[o..e])?;
            i = j;
        }
        let s = (off % BLOCK_SIZE as u64) as usize;
        buf[..len].copy_from_slice(&tmp[s..s + len]);
        Ok(len)
    }

    /// Block `idx` of file `ino` as committed, unless it is dirty (then
    /// `None`: the dirty copy is newer).
    fn old_block_unless_dirty(&mut self, ino: u64, idx: u64) -> Result<Option<Block>> {
        if self
            .dirty_data
            .get(&ino)
            .is_some_and(|m| m.contains_key(&idx))
        {
            return Ok(None);
        }
        let mut b = zero_block();
        self.read_disk_blocks(ino, idx, 1, &mut b[..])?;
        Ok(Some(b))
    }

    /// The dirty copy of block `idx` of file `ino`, made from `old` (or
    /// zeros) if it is not dirty yet.
    fn dirty_block(&mut self, ino: u64, idx: u64, old: Option<Block>) -> &mut Block {
        let m = self.dirty_data.entry(ino).or_default();
        let mut added = false;
        let b = m.entry(idx).or_insert_with(|| {
            added = true;
            old.unwrap_or_else(zero_block)
        });
        if added {
            self.dirty_blocks += 1;
        }
        b
    }

    /// Writes `data` at `off`, extending the file if needed.
    fn write_inner(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<usize> {
        self.check_writable()?;
        let mut a = self.get_inode(ino)?;
        match a.kind {
            FileKind::File => {}
            FileKind::Dir => return Err(Error::IsDir),
            FileKind::Symlink => return Err(Error::Invalid("write to a symlink")),
        }
        if data.is_empty() {
            return Ok(0);
        }
        let end = off
            .checked_add(data.len() as u64)
            .ok_or(Error::FileTooBig)?;
        if end > MAX_FILE_SIZE {
            return Err(Error::FileTooBig);
        }
        let bs = BLOCK_SIZE as u64;
        let first = off / bs;
        let last = (end - 1) / bs;
        let nblocks = last - first + 1;
        // Data blocks plus about one metadata node per extent.
        self.ensure_space(nblocks, nblocks / 32 + 8, false)?;
        self.txn_dirty = true;
        // Everything that can fail comes before the data goes in: the old
        // content of partly written blocks, then the inode.
        let partial = |idx: u64| {
            let bstart = idx * bs;
            let from = if idx == first { off - bstart } else { 0 };
            let to = if idx == last { end - bstart } else { bs };
            (from != 0 || to != bs) && bstart < a.size
        };
        let mut old = [None, None];
        for (slot, idx) in [first, last].into_iter().enumerate() {
            if (slot == 0 || last != first) && partial(idx) {
                old[slot] = self.old_block_unless_dirty(ino, idx)?;
            }
        }
        let now = self.clock.now_ns();
        if end > a.size {
            a.size = end;
        }
        a.mtime_ns = now;
        a.ctime_ns = now;
        self.put_inode(&a)?;
        let mut pos = 0usize;
        for idx in first..=last {
            let bstart = idx * bs;
            let from = if idx == first {
                (off - bstart) as usize
            } else {
                0
            };
            let to = if idx == last {
                (end - bstart) as usize
            } else {
                BLOCK_SIZE
            };
            let prior = if idx == first {
                old[0].take()
            } else if idx == last {
                old[1].take()
            } else {
                None
            };
            let b = self.dirty_block(ino, idx, prior);
            b[from..to].copy_from_slice(&data[pos..pos + (to - from)]);
            pos += to - from;
        }
        Ok(data.len())
    }

    /// Changes a file's size. Shrinking frees data past the end and zeroes
    /// the tail of the last block, so growing again never exposes old bytes.
    fn truncate_inner(&mut self, ino: u64, size: u64) -> Result<()> {
        self.check_writable()?;
        let mut a = self.get_inode(ino)?;
        if a.kind != FileKind::File {
            return Err(Error::Invalid("truncate of a non-file"));
        }
        if size > MAX_FILE_SIZE {
            return Err(Error::FileTooBig);
        }
        self.ensure_space(0, 8, true)?;
        self.txn_dirty = true;
        let bs = BLOCK_SIZE as u64;
        if size < a.size {
            let keep = size.div_ceil(bs);
            // Read the new last block first (it is below `keep`, so the
            // punch leaves it); the dirty data changes once all succeeded.
            let tail = if size.is_multiple_of(bs) {
                None
            } else {
                let idx = size / bs;
                Some((idx, self.old_block_unless_dirty(ino, idx)?))
            };
            self.punch(ino, keep, u64::MAX)?;
            self.defer(DataChange::DropFrom(ino, keep));
            if let Some((idx, old)) = tail {
                self.defer(DataChange::ZeroTail {
                    ino,
                    idx,
                    from: (size % bs) as usize,
                    old,
                });
            }
        }
        let now = self.clock.now_ns();
        a.size = size;
        a.mtime_ns = now;
        a.ctime_ns = now;
        self.put_inode(&a)
    }

    /// Removes committed extents covering file blocks `[from, to)`,
    /// releasing their blocks and keeping the parts outside the range.
    fn punch(&mut self, ino: u64, from: u64, to: u64) -> Result<()> {
        if from >= to {
            return Ok(());
        }
        // Extents that start before `from` may overlap it.
        let mut hits: Vec<(u64, Extent)> = Vec::new();
        if let Some((start, e)) = self.extent_at(ino, from)?
            && start < from
        {
            hits.push((start, e));
        }
        let mut err = None;
        self.fs_tree.scan(
            &mut self.store,
            &Key::new(ino, K_EXTENT, from),
            &Key::new(ino, K_EXTENT, to),
            &mut |k, v| {
                match Extent::decode(v, ino) {
                    Ok(e) => hits.push((k.off, e)),
                    Err(e) => {
                        err = Some(e);
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )?;
        if let Some(e) = err {
            return Err(e);
        }
        for (start, e) in hits {
            let end = start
                .checked_add(e.len)
                .ok_or(Error::Corrupt("extent offset", ino))?;
            let cut_from = from.max(start);
            let cut_to = to.min(end);
            self.fs_tree
                .remove(&mut self.store, &Key::new(ino, K_EXTENT, start))?;
            self.store.release_data(
                TreeId::Fs,
                e.phys + (cut_from - start),
                cut_to - cut_from,
                e.birth,
            )?;
            if start < cut_from {
                let left = e.slice(0, cut_from - start);
                self.fs_tree.insert(
                    &mut self.store,
                    Key::new(ino, K_EXTENT, start),
                    left.encode(),
                )?;
            }
            if cut_to < end {
                let right = e.slice(cut_to - start, e.len);
                self.fs_tree.insert(
                    &mut self.store,
                    Key::new(ino, K_EXTENT, cut_to),
                    right.encode(),
                )?;
            }
        }
        Ok(())
    }

    /// Writes dirty file data to new extents (not yet committed).
    ///
    /// Never loses data. If building the extents fails (no space, or a
    /// tree read failing), the flush is undone as a whole: tree items back
    /// (see [`undo_items`](Self::undo_items)), its allocations and data
    /// releases out of the space-map log and the map rebuilt, every block
    /// back in `dirty_data`. If the write itself fails, every block that
    /// did not reach the device is back in `dirty_data` (decrypted from the
    /// write queue if it was already sealed), while extents already in the
    /// tree keep pointing at the unwritten blocks until the next flush
    /// replaces them; blocks whose write failed join the bad set. Only a
    /// failure while undoing stops the volume.
    fn flush_data(&mut self) -> Result<()> {
        let mut data = core::mem::take(&mut self.dirty_data);
        self.dirty_blocks = 0;
        let mut queued = Vec::new();
        let log0 = self.store.log.len();
        debug_assert!(self.store.undo.is_none(), "flush inside an operation");
        self.store.undo = Some(Vec::new());
        let q = self.queue_data(&mut data, &mut queued);
        let undo = self.store.undo.take().unwrap_or_default();
        if let Err(e) = q {
            if let Err(re) = self.undo_flush(undo, log0, data, queued) {
                self.poisoned = true;
                return Err(re);
            }
            return Err(e);
        }
        let r = self.store.io.write_batch();
        if r.is_err() {
            self.note_failed_writes();
            if let Err(e) = self.restore_data(data, queued) {
                self.poisoned = true;
                return Err(e);
            }
        }
        r
    }

    /// Undoes a flush whose extents could not all be built (see
    /// [`flush_data`](Self::flush_data)); `log0` is the log length before it.
    fn undo_flush(
        &mut self,
        undo: Vec<Undo>,
        log0: usize,
        data: BTreeMap<u64, BTreeMap<u64, Block>>,
        queued: Vec<QueuedRun>,
    ) -> Result<()> {
        let mut drop = self.undo_items(undo)?;
        drop.extend(
            (log0..self.store.log.len())
                .filter(|&i| matches!(self.store.log[i], SpaceOp::Data { .. })),
        );
        drop.sort_unstable();
        drop.dedup();
        for i in drop.into_iter().rev() {
            self.store.log.remove(i);
        }
        self.restore_data(data, queued)?;
        let relocs = self.store.space.relocations.clone();
        self.rebuild_space(relocs)
    }

    /// Seals and queues `data` into new extents. Blocks leave `data` only
    /// as they are queued (and recorded in `queued`).
    fn queue_data(
        &mut self,
        data: &mut BTreeMap<u64, BTreeMap<u64, Block>>,
        queued: &mut Vec<QueuedRun>,
    ) -> Result<()> {
        let generation = self.txn_generation();
        while let Some(mut entry) = data.first_entry() {
            let ino = *entry.key();
            while let Some((&idx, _)) = entry.get().first_key_value() {
                let mut want = 1;
                while want < EXTENT_MAX_BLOCKS && entry.get().contains_key(&(idx + want)) {
                    want += 1;
                }
                let (phys, n) = self.store.space.alloc_run(&mut self.store.io, want)?;
                self.store.log.push(SpaceOp::Data {
                    addr: phys,
                    len: n,
                    ino,
                    idx,
                });
                self.punch(ino, idx, idx + n)?;
                let nonce = self.store.io.sealer.reserve_nonces(n);
                let mut macs = Vec::with_capacity(n as usize);
                for j in 0..n {
                    let mut blk = entry.get_mut().remove(&(idx + j)).unwrap();
                    let mac = self.store.io.sealer.seal_with(
                        BlockKind::Data,
                        phys + j,
                        generation,
                        &crypto::nonce_add(&nonce, j),
                        &mut blk[..],
                    );
                    macs.push(mac);
                    self.store.io.queue(phys + j, blk);
                }
                queued.push(QueuedRun {
                    ino,
                    idx,
                    phys,
                    n,
                    birth: generation,
                    nonce,
                    macs: macs.clone(),
                });
                let e = Extent {
                    phys,
                    len: n,
                    birth: generation,
                    nonce,
                    macs,
                };
                self.fs_tree
                    .insert(&mut self.store, Key::new(ino, K_EXTENT, idx), e.encode())?;
            }
            entry.remove();
        }
        Ok(())
    }

    /// Puts file blocks a failed flush did not write back into `dirty_data`.
    fn restore_data(
        &mut self,
        data: BTreeMap<u64, BTreeMap<u64, Block>>,
        queued: Vec<QueuedRun>,
    ) -> Result<()> {
        for (ino, blocks) in data {
            for (idx, b) in blocks {
                self.put_dirty(ino, idx, b);
            }
        }
        for q in queued {
            for j in 0..q.n {
                let Some(mut b) = self.store.io.unqueue(q.phys + j) else {
                    continue; // written
                };
                self.store.io.sealer.open(
                    BlockKind::Data,
                    q.phys + j,
                    q.birth,
                    &crypto::nonce_add(&q.nonce, j),
                    &q.macs[j as usize],
                    &mut b[..],
                )?;
                self.put_dirty(q.ino, q.idx + j, b);
            }
        }
        Ok(())
    }

    fn put_dirty(&mut self, ino: u64, idx: u64, b: Block) {
        if self
            .dirty_data
            .entry(ino)
            .or_default()
            .insert(idx, b)
            .is_none()
        {
            self.dirty_blocks += 1;
        }
    }

    /// Moves blocks whose write failed into the bad set, each with the
    /// rest of its aligned group of [`BAD_GROUP`] blocks: failing blocks
    /// come in regions (a worn flash erase block, a damaged track), and
    /// finding them one failed commit at a time would take an attempt per
    /// block.
    fn note_failed_writes(&mut self) {
        let failed = self.store.io.take_failed();
        let total = self.store.space.total_blocks();
        let (lo, hi) = (
            crate::space::SB_RESERVED_FRONT,
            total - crate::space::SB_RESERVED_BACK,
        );
        for (s, l) in failed.ranges() {
            let g0 = (s / BAD_GROUP * BAD_GROUP).max(lo);
            let g1 = (s + l)
                .div_ceil(BAD_GROUP)
                .saturating_mul(BAD_GROUP)
                .min(hi);
            self.store.space.bad.add(g0, g1 - g0);
        }
    }

    /// The flush a write makes when dirty data passes the limit. Its data
    /// is already in the open transaction, so a failure to write it is not
    /// the write's failure: the data stays in memory and the space map is
    /// rebuilt; the commit tries again (and reports). Repeated failures
    /// degrade the volume so memory does not grow without bound.
    fn flush_early(&mut self) -> Result<()> {
        let Err(e) = self.flush_data() else {
            return Ok(());
        };
        if self.poisoned {
            return Err(e);
        }
        let relocs = self.store.space.relocations.clone();
        if let Err(r) = self.rebuild(self.txn_generation(), false, relocs) {
            self.poisoned = true;
            return Err(r);
        }
        self.early_flush_failures += 1;
        if self.early_flush_failures >= COMMIT_ATTEMPTS
            || self.store.space.bad.len() > self.bad_limit()
        {
            self.degraded = true;
        }
        Ok(())
    }

    /// Rebuilds the in-memory state of the open transaction on top of the
    /// last committed state after a failed write: the space map is opened
    /// again from the last commit and the transaction's own changes
    /// ([`Store::log`]) are replayed onto it; nodes a failed commit wrote
    /// (birth `attempted`) become dirty again; chunks with a copy on a bad
    /// block are queued to move; and if a flush failed, file data whose
    /// durability is now unknown is read back, checked and made dirty
    /// again so the next commit writes it afresh. The trees, the inode and
    /// snapshot counters and the dirty data are the transaction's and stay.
    /// `sb_started`: the failed commit may have reached a superblock, so
    /// later commits are numbered above it and rewrite every slot.
    pub(crate) fn rebuild(
        &mut self,
        attempted: u64,
        sb_started: bool,
        relocs: Vec<u64>,
    ) -> Result<()> {
        self.note_failed_writes();
        self.rebuild_space(relocs)?;

        let mut written = core::mem::take(&mut self.store.written);
        for a in written.keys() {
            self.store.cache.forget(*a);
        }
        self.fs_tree.unwrite(&mut written, attempted)?;
        self.meta.unwrite(&mut written, attempted)?;
        if let Some((&a, _)) = written.first_key_value() {
            return Err(Error::Corrupt("failed commit wrote an unreachable node", a));
        }
        if self.store.io.flush_failed() {
            self.redirty_unflushed()?;
        }
        if sb_started {
            self.gen_floor = self.gen_floor.max(attempted);
            self.rewrite_all_slots = true;
        }
        Ok(())
    }

    /// Rebuilds the space map: the last commit's, with the open
    /// transaction's logged changes ([`Store::log`]) replayed onto it, the
    /// mount's bad set kept, and chunks `relocs` (plus any with a copy on a
    /// bad block) queued to move.
    fn rebuild_space(&mut self, relocs: Vec<u64>) -> Result<()> {
        let b = &self.body;
        let mut space = self.store.space.reopen(
            &mut self.store.io,
            self.header.chunk_shift,
            (b.chunk_root, b.chunk_depth),
            (b.space_root, b.free_log),
            b.generation,
            b.commit_time_ns,
        )?;
        for op in &self.store.log {
            match *op {
                SpaceOp::Free(a, l) => space.free(&mut self.store.io, a, l)?,
                SpaceOp::Data { addr, len, .. } => space.claim(&mut self.store.io, addr, len)?,
            }
        }
        let bad = space.bad.clone();
        let mut relocs = relocs;
        for id in space.chunks_touching(&mut self.store.io, &bad)? {
            if !relocs.contains(&id) {
                relocs.push(id);
            }
        }
        space.relocations = relocs;
        space.refresh_releasing(&mut self.store.io)?;
        self.store.space = space;
        Ok(())
    }

    /// After a failed flush: file blocks of this transaction written since
    /// the last good flush may not be durable. Each one still in use is
    /// read back, checked against its MAC and put back into the dirty
    /// data, so the next commit writes it to a new home and flushes it.
    /// One that no longer reads back intact is lost: that is an error.
    fn redirty_unflushed(&mut self) -> Result<()> {
        let runs: Vec<(u64, u64, u64, u64)> = self
            .store
            .log
            .iter()
            .filter_map(|op| match *op {
                SpaceOp::Data {
                    addr,
                    len,
                    ino,
                    idx,
                } => Some((addr, len, ino, idx)),
                SpaceOp::Free(..) => None,
            })
            .collect();
        let mut buf = zero_block();
        for (addr, len, ino, idx) in runs {
            for j in 0..len {
                let (a, i) = (addr + j, idx + j);
                if !self.store.io.unflushed(a)
                    || self
                        .dirty_data
                        .get(&ino)
                        .is_some_and(|m| m.contains_key(&i))
                {
                    continue;
                }
                let Some((start, e)) = self.extent_at(ino, i)? else {
                    continue;
                };
                let r = i - start;
                if e.phys + r != a {
                    continue; // replaced since
                }
                self.store.io.read_raw(a, &mut buf[..])?;
                self.store.io.sealer.open(
                    BlockKind::Data,
                    a,
                    e.birth,
                    &crypto::nonce_add(&e.nonce, r),
                    &e.macs[r as usize],
                    &mut buf[..],
                )?;
                self.put_dirty(ino, i, core::mem::replace(&mut buf, zero_block()));
            }
        }
        Ok(())
    }

    // ---- commit ----

    /// Makes every change since the last commit durable, atomically.
    ///
    /// A commit that fails to write does not lose the transaction. Blocks
    /// whose write failed are never used again on this mount, the
    /// in-memory state is rebuilt on top of the last commit (see
    /// [`rebuild`](Self::rebuild)), and the commit runs again with new
    /// homes for everything. It gives up after [`COMMIT_ATTEMPTS`] attempts,
    /// or once the bad set passes 1/64 of the volume: it then returns the I/O error and the volume is
    /// degraded ([`Error::DeviceFailing`]) until a later `commit` succeeds.
    /// A commit refused for space ([`Error::NoSpace`]) is rebuilt the same
    /// way and the volume stays writable. A read the commit needed that
    /// failed its checksum ([`Error::Corrupt`]) is retried the same way,
    /// since the damage may be in transit; if it persists, the volume
    /// degrades. With fewer than two superblock slots still taking writes
    /// it degrades at once ([`Error::DeviceFailing`]). On disk the volume is always the
    /// last completed commit.
    pub fn commit(&mut self) -> Result<()> {
        // Inside an operation (a commit to free space, or a snapshot's):
        // drop the checkpoint so the trees are not shared while they are
        // written, and take a new one at the state the commit leaves.
        let held = self.op_cp.take();
        // Operations commit only before they change anything.
        debug_assert!(self.op_data.is_empty());
        debug_assert!(
            self.store
                .undo
                .as_ref()
                .is_none_or(|u| u.iter().all(|x| !matches!(x, Undo::Item(..))))
        );
        self.store.undo = None;
        let r = self.commit_attempts();
        match held {
            // A whole-tree operation whose commit failed is undone back to
            // before its changes (the failed commit left them in memory).
            Some(cp) if cp.trees.is_some() && r.is_err() => self.op_cp = Some(cp),
            Some(cp) => self.op_cp = Some(self.checkpoint(cp.trees.is_some())),
            None => {}
        }
        r
    }

    fn commit_attempts(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        // A degraded volume tries every superblock slot again: one that
        // failed may work now.
        if self.degraded {
            self.dead_slots = 0;
        }
        // Queued repairs of bad metadata copies are written by the next
        // commit even when nothing else changed.
        if !self.txn_dirty && self.store.io.pending_repairs() == 0 {
            return Ok(());
        }
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let relocs = self.store.space.relocations.clone();
        for attempt in 1.. {
            self.last_commit_attempts = attempt;
            let attempted = self.txn_generation();
            self.sb_started = false;
            let e = match self.commit_inner() {
                Ok(()) => {
                    self.degraded = false;
                    self.early_flush_failures = 0;
                    self.store.log.clear();
                    self.store.written.clear();
                    return Ok(());
                }
                Err(e) => e,
            };
            // The on-disk state is still the previous commit (or the failed
            // one, if its superblock landed).
            self.store.io.discard_batch();
            self.store.io.discard_repairs();
            if self.poisoned
                || !matches!(
                    e,
                    Error::Io(_) | Error::Corrupt(..) | Error::NoSpace | Error::DeviceFailing
                )
            {
                self.poisoned = true;
                return Err(e);
            }
            if let Err(re) = self.rebuild(attempted, self.sb_started, relocs.clone()) {
                // Report why recovery stopped (a lost device, say), not
                // the write failure that started it.
                self.poisoned = true;
                return Err(re);
            }
            if e == Error::NoSpace {
                return Err(e);
            }
            if e == Error::DeviceFailing {
                // Fewer than two superblock slots still take writes.
                self.degraded = true;
                return Err(e);
            }
            if attempt >= COMMIT_ATTEMPTS || self.store.space.bad.len() > self.bad_limit() {
                self.degraded = true;
                return Err(e);
            }
        }
        unreachable!()
    }

    /// Commits even with no changes (used to cycle quarantined space back).
    fn commit_forced(&mut self) -> Result<()> {
        self.txn_dirty = true;
        self.commit()
    }

    fn commit_inner(&mut self) -> Result<()> {
        let generation = self.txn_generation();
        // Repairs and chunk moves first, while nothing else is queued.
        if self.store.io.repair_enabled {
            self.store.space.repair_copies(&mut self.store.io)?;
        }
        for id in core::mem::take(&mut self.store.space.relocations) {
            self.store.space.relocate_chunk(&mut self.store.io, id)?;
        }
        self.flush_data()?;
        let fs_root = self.fs_tree.write(&mut self.store, generation)?;
        // Undone item by item if it fails part way (a rebuild does not
        // undo tree changes).
        self.store.undo = Some(Vec::new());
        let r = crate::snapshot::flush_deadlist(self);
        let undo = self.store.undo.take().unwrap_or_default();
        if let Err(e) = r {
            if let Err(re) = self.undo_items(undo) {
                self.poisoned = true;
                return Err(re);
            }
            return Err(e);
        }
        let meta_root = self.meta.write(&mut self.store, generation)?;
        let cost_from = (
            self.store.space.allocated_l,
            self.store.space.allocated_pairs,
        );
        let now = self.clock.now_ns();
        // A new root block every commit; the old one is quarantined.
        if !self.body.root_block.is_null() {
            let old = self.body.root_block.addr;
            self.store.space.free(&mut self.store.io, old, 1)?;
        }
        let root_addr = self.store.space.alloc_meta(&mut self.store.io, 1)?[0].0;
        let (space_root, free_log, chunk_root) =
            self.store
                .space
                .write(&mut self.store.io, generation, now)?;
        let mut body = Body {
            generation,
            chunk_root,
            chunk_depth: self.store.space.chunks.depth(),
            root_block: BlockPtr::NULL,
            fs_root,
            meta_root,
            space_root,
            free_log,
            next_ino: self.next_ino,
            next_snap_id: self.next_snap_id,
            live_deadlist: self.live_deadlist,
            next_deadlist: self.next_deadlist,
            latest_snap_gen: self.store.latest_snap_gen,
            last_move_gen: if core::mem::take(&mut self.store.space.moved_live_run) {
                generation
            } else {
                self.body.last_move_gen
            },
            commit_time_ns: now,
            replay_value: self.body.replay_value + self.replay_bump as u64,
            flags: self.next_flags,
            window_gen: self
                .store
                .space
                .oldest_held()
                .unwrap_or(generation)
                .min(generation.saturating_sub(crate::space::QUARANTINE_COMMITS - 1))
                .max(1),
            log_root: BlockPtr::NULL,
            // Generation 0 is format's starting state, never a commit.
            history: core::iter::once(self.body.core())
                .filter(|c| c.generation > 0)
                .chain(self.body.history.iter().cloned())
                .take(superblock::HISTORY)
                .collect(),
        };
        body.root_block = self.store.space.write_meta(
            &mut self.store.io,
            BlockKind::Root,
            root_addr,
            generation,
            body.encode_root(),
            0,
        )?;
        let cost = (
            self.store.space.allocated_l - cost_from.0,
            self.store.space.allocated_pairs - cost_from.1,
        );
        self.store.io.write_batch()?;
        self.sb_started = true;
        superblock::commit(
            &mut self.store.io,
            &self.header,
            &body,
            self.rewrite_all_slots || self.replay_bump,
            &mut self.dead_slots,
        )?;
        self.rewrite_all_slots = false;
        self.replay_bump = false;
        self.header.generation = generation;
        self.body = body;
        self.store.space.committed(generation, now);
        self.txn_dirty = false;
        self.txn_data = 0;
        self.txn_meta = 0;
        if self.commit_costs.len() == COST_RING {
            self.commit_costs.pop_front();
        }
        self.commit_costs.push_back(cost);
        Ok(())
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.txn_dirty = true;
    }

    // ---- keys ----

    /// Adds (or replaces) key slot `slot` so `new_pass` also unlocks the
    /// volume. Takes effect at the next commit, which this performs.
    pub fn set_passphrase(
        &mut self,
        slot: usize,
        current: &[u8],
        new_pass: &[u8],
        rng: &mut dyn Entropy,
    ) -> Result<()> {
        self.check_writable()?;
        if self.header.suite == SUITE_PLAIN {
            return Err(Error::Unsupported("volume is not encrypted"));
        }
        if slot >= KEYSLOTS {
            return Err(Error::Invalid("key slot"));
        }
        let master = self.header.unlock(current)?;
        self.header.set_keyslot(slot, new_pass, &master, rng)?;
        self.commit_forced()
    }

    /// Removes key slot `slot`. Refuses to remove the last one.
    pub fn remove_keyslot(&mut self, slot: usize) -> Result<()> {
        self.check_writable()?;
        if slot >= KEYSLOTS || !self.header.keyslots[slot].used {
            return Err(Error::Invalid("key slot"));
        }
        if self.header.keyslots.iter().filter(|k| k.used).count() == 1 {
            return Err(Error::Invalid("cannot remove the last key slot"));
        }
        self.header.keyslots[slot] = KeySlot::default();
        self.commit_forced()
    }
}
