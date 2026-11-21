//! The filesystem: inodes, directories, file data, commits.

use crate::codec::{Reader, Writer};
use crate::crypto::{self, BlockKind, Entropy, KdfParams, MAC_LEN, NONCE_LEN, Sealer, SecretKey};
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Block, Io, zero_block};
use crate::ptr::BlockPtr;
use crate::space::Space;
use crate::superblock::{
    self, Body, Header, KEYSLOTS, KeySlot, SUITE_PLAIN, SUITE_XCHACHA20POLY1305,
};
use crate::tree::{Key, MAX_VALUE, NodeCache, Store, Tree, TreeId};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
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
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            passphrase: None,
            kdf: KdfParams::DEFAULT,
            quarantine_min_ns: DEFAULT_QUARANTINE_NS,
        }
    }
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
    /// still finds an intact older tree. Tests with a frozen clock set 0.
    pub quarantine_min_ns: i64,
    /// After an unclean shutdown, also read back file data written inside
    /// the quarantine window (metadata in the window is always checked), so
    /// mount falls back to an intact commit instead of later read errors.
    pub verify_recent_data: bool,
    /// Run a full fsck (with data) on each candidate commit; mount the
    /// newest that passes. Slow: for rescue and tests.
    pub fsck_on_mount: bool,
}

/// 30 seconds: covers drives that hold writes in a volatile cache across
/// several flush requests.
pub const DEFAULT_QUARANTINE_NS: i64 = 30_000_000_000;

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
}

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
        if total < 64 {
            return Err(Error::Invalid("device too small (need at least 64 blocks)"));
        }
        let mut uuid = [0u8; 16];
        rng.fill(&mut uuid);
        let mut salt = [0u8; 32];
        rng.fill(&mut salt);
        let mut header = Header {
            uuid,
            total_blocks: total,
            suite: SUITE_PLAIN,
            generation: 0,
            slot: 0,
            kdf: opts.kdf,
            salt,
            keyslots: [KeySlot::default(); KEYSLOTS],
        };
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
        let mut space = Space::format(total)?;
        space.quarantine_min_ns = opts.quarantine_min_ns;
        let store = Store {
            io: Io::new(dev, sealer),
            space,
            cache: NodeCache::new(8192),
            latest_snap_gen: 0,
            dead_pending: Vec::new(),
        };
        let body = Body {
            generation: 0,
            fs_root: BlockPtr::NULL,
            meta_root: BlockPtr::NULL,
            space_root: BlockPtr::NULL,
            free_log: BlockPtr::NULL,
            next_ino: ROOT_INO + 1,
            next_snap_id: 1,
            live_deadlist: 1,
            next_deadlist: 2,
            latest_snap_gen: 0,
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

        let mut dev = Some(dev);
        for (body, header, master, from_history) in commits {
            let sealer = Sealer::new(header.uuid, master.as_ref(), rng);
            let mut io = Io::new(dev.take().unwrap(), sealer);
            let (mut space, fs_tree, meta) = match Self::open_commit(&mut io, &header, &body) {
                Ok(x) => x,
                Err(e) => {
                    last_err = e;
                    dev = Some(io.dev);
                    continue;
                }
            };
            space.quarantine_min_ns = opts.quarantine_min_ns;
            let generation = body.generation;
            let clean = body.flags & superblock::FLAG_CLEAN != 0 && !from_history;
            let mut header = header;
            header.generation = generation;
            let mut fs = Filesystem {
                store: Store {
                    io,
                    space,
                    cache: NodeCache::new(opts.cache_nodes),
                    latest_snap_gen: body.latest_snap_gen,
                    dead_pending: Vec::new(),
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
            };
            let check = if clean {
                Ok(())
            } else {
                let min_birth = fs.body.window_gen;
                crate::recover::verify_since(&mut fs, min_birth, opts.verify_recent_data)
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

    fn open_commit(io: &mut Io<D>, h: &Header, body: &Body) -> Result<(Space, Tree, Tree)> {
        let total = io.num_blocks();
        if body.fs_root.is_null()
            || body.space_root.is_null()
            || body.next_ino <= ROOT_INO
            || body.live_deadlist >= body.next_deadlist
        {
            return Err(Error::Corrupt("superblock fields", 0));
        }
        let _ = h;
        let space = Space::open(io, total, body.space_root, body.free_log)?;
        let fs_tree = Tree::new(TreeId::Fs, body.fs_root)?;
        let meta = Tree::new(TreeId::Meta, body.meta_root)?;
        // Make sure the roots themselves are readable now, so a damaged
        // newest commit falls back to the previous one instead of failing later.
        let mut b = io.read_sealed(BlockKind::Node, &body.fs_root)?;
        crate::tree::Node::decode(&b[..], TreeId::Fs, &body.fs_root)?;
        if !body.meta_root.is_null() {
            b = io.read_sealed(BlockKind::Node, &body.meta_root)?;
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

    /// Makes the allocator continue its search at block `b` (tests use it
    /// to put data in far regions of huge volumes).
    #[doc(hidden)]
    pub fn set_alloc_cursor_for_tests(&mut self, b: u64) {
        self.store.space.set_cursor(b);
    }

    #[doc(hidden)]
    pub fn next_ino_for_tests(&self) -> u64 {
        self.next_ino
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
            .saturating_sub(reserve_for(total) + self.dirty_blocks as u64);
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
        Ok(())
    }

    /// Fails with `NoSpace` unless `blocks` more can be allocated while
    /// keeping the commit reserve. Deletes pass `use_reserve = true`.
    fn ensure_space(&mut self, blocks: u64, use_reserve: bool) -> Result<()> {
        let total = self.store.space.total_blocks();
        let reserve = if use_reserve {
            reserve_for(total) / 2
        } else {
            reserve_for(total)
        };
        let need = |fs: &Self| blocks + fs.dirty_blocks as u64 + reserve;
        if self.store.space.allocatable_blocks() >= need(self) {
            return Ok(());
        }
        // Quarantined blocks become usable once they are QUARANTINE_COMMITS
        // commits and the minimum time old. Cycle commits while that could
        // still help; past the time window the caller must wait and retry.
        if self.txn_dirty {
            self.commit()?;
        }
        let mut cycles = 0;
        while self.store.space.allocatable_blocks() < need(self) {
            let could =
                self.store.space.allocatable_blocks() + self.store.space.quarantined_blocks();
            if could < need(self) || cycles > crate::space::QUARANTINE_COMMITS {
                return Err(Error::NoSpace);
            }
            self.commit_forced()?;
            cycles += 1;
        }
        Ok(())
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
        self.ensure_space(8, false)?;
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
        self.ensure_space(8, false)?;
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
        self.ensure_space(8, true)?;
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
        self.ensure_space(8, true)?;
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
        if let Some(m) = self.dirty_data.remove(&a.ino) {
            self.dirty_blocks -= m.len();
        }
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
        self.ensure_space(16, true)?;
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
        let r = self.put_inode(&a);
        self.guard(r)?;
        Ok(a)
    }

    // Public mutating operations: see `guard`.
    pub fn create(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        let r = self.create_inner(dir, name, perm);
        self.guard(r)
    }

    pub fn mkdir(&mut self, dir: u64, name: &[u8], perm: u32) -> Result<u64> {
        let r = self.mkdir_inner(dir, name, perm);
        self.guard(r)
    }

    pub fn symlink(&mut self, dir: u64, name: &[u8], target: &[u8]) -> Result<u64> {
        let r = self.symlink_inner(dir, name, target);
        self.guard(r)
    }

    pub fn link(&mut self, ino: u64, dir: u64, name: &[u8]) -> Result<()> {
        let r = self.link_inner(ino, dir, name);
        self.guard(r)
    }

    pub fn unlink(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        let r = self.unlink_inner(dir, name);
        self.guard(r)
    }

    pub fn rmdir(&mut self, dir: u64, name: &[u8]) -> Result<()> {
        let r = self.rmdir_inner(dir, name);
        self.guard(r)
    }

    pub fn rename(&mut self, sdir: u64, sname: &[u8], ddir: u64, dname: &[u8]) -> Result<()> {
        let r = self.rename_inner(sdir, sname, ddir, dname);
        self.guard(r)
    }

    pub fn write(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<usize> {
        let r = self.write_inner(ino, off, data);
        self.guard(r)
    }

    pub fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        let r = self.truncate_inner(ino, size);
        self.guard(r)
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

    /// Errors that may leave a half-applied change in memory (I/O failures,
    /// corruption found mid-operation) stop all further writes, so the
    /// half-applied change can never be committed.
    pub(crate) fn guard<T>(&mut self, r: Result<T>) -> Result<T> {
        if let Err(Error::Io(_) | Error::Corrupt(..)) = &r {
            self.poisoned = true;
        }
        r
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

    fn dirty_block(&mut self, ino: u64, idx: u64, need_old: bool) -> Result<&mut Block> {
        let present = self
            .dirty_data
            .get(&ino)
            .is_some_and(|m| m.contains_key(&idx));
        if !present {
            let mut b = zero_block();
            if need_old {
                self.read_disk_blocks(ino, idx, 1, &mut b[..])?;
            }
            self.dirty_data.entry(ino).or_default().insert(idx, b);
            self.dirty_blocks += 1;
        }
        Ok(self
            .dirty_data
            .get_mut(&ino)
            .unwrap()
            .get_mut(&idx)
            .unwrap())
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
        self.ensure_space(nblocks + nblocks / 32 + 8, false)?;
        self.txn_dirty = true;
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
            // Only blocks that existed before need their old content.
            let need_old = (from != 0 || to != BLOCK_SIZE) && bstart < a.size;
            let b = self.dirty_block(ino, idx, need_old)?;
            b[from..to].copy_from_slice(&data[pos..pos + (to - from)]);
            pos += to - from;
        }
        let now = self.clock.now_ns();
        if end > a.size {
            a.size = end;
        }
        a.mtime_ns = now;
        a.ctime_ns = now;
        self.put_inode(&a)?;
        if self.dirty_blocks > self.dirty_limit_blocks {
            self.flush_data()?;
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
        self.ensure_space(8, true)?;
        self.txn_dirty = true;
        let bs = BLOCK_SIZE as u64;
        if size < a.size {
            let keep = size.div_ceil(bs);
            if let Some(m) = self.dirty_data.get_mut(&ino) {
                let dropped = m.split_off(&keep);
                self.dirty_blocks -= dropped.len();
            }
            self.punch(ino, keep, u64::MAX)?;
            if !size.is_multiple_of(bs) {
                let b = self.dirty_block(ino, size / bs, true)?;
                b[(size % bs) as usize..].fill(0);
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
            self.store.release(
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
    fn flush_data(&mut self) -> Result<()> {
        let data = core::mem::take(&mut self.dirty_data);
        self.dirty_blocks = 0;
        let generation = self.txn_generation();
        for (ino, blocks) in data {
            let mut it = blocks.into_iter().peekable();
            while let Some((idx, b)) = it.next() {
                let mut run = vec![b];
                while run.len() < EXTENT_MAX_BLOCKS as usize {
                    match it.peek() {
                        Some((j, _)) if *j == idx + run.len() as u64 => {
                            run.push(it.next().unwrap().1)
                        }
                        _ => break,
                    }
                }
                let mut done = 0u64;
                while (done as usize) < run.len() {
                    let want = run.len() as u64 - done;
                    let (phys, n) = self.store.space.alloc_run(&mut self.store.io, want)?;
                    let fidx = idx + done;
                    self.punch(ino, fidx, fidx + n)?;
                    let nonce = self.store.io.sealer.reserve_nonces(n);
                    let mut macs = Vec::with_capacity(n as usize);
                    for j in 0..n {
                        let mut blk =
                            core::mem::replace(&mut run[(done + j) as usize], zero_block());
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
                    let e = Extent {
                        phys,
                        len: n,
                        birth: generation,
                        nonce,
                        macs,
                    };
                    self.fs_tree.insert(
                        &mut self.store,
                        Key::new(ino, K_EXTENT, fidx),
                        e.encode(),
                    )?;
                    done += n;
                }
            }
        }
        // New blocks can go to the device now; they only become reachable at commit.
        self.store.io.write_batch()
    }

    // ---- commit ----

    /// Makes every change since the last commit durable, atomically.
    pub fn commit(&mut self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if !self.txn_dirty {
            return Ok(());
        }
        self.check_writable()?;
        match self.commit_inner() {
            Ok(()) => Ok(()),
            Err(e) => {
                // The on-disk state is still the previous commit, but our
                // in-memory state is half-written. Refuse further work.
                self.poisoned = true;
                self.store.io.discard_batch();
                Err(e)
            }
        }
    }

    /// Commits even with no changes (used to cycle quarantined space back).
    fn commit_forced(&mut self) -> Result<()> {
        self.txn_dirty = true;
        self.commit()
    }

    fn commit_inner(&mut self) -> Result<()> {
        let generation = self.txn_generation();
        self.flush_data()?;
        let fs_root = self.fs_tree.write(&mut self.store, generation)?;
        crate::snapshot::flush_deadlist(self)?;
        let meta_root = self.meta.write(&mut self.store, generation)?;
        let now = self.clock.now_ns();
        let (space_root, free_log) = self
            .store
            .space
            .write(&mut self.store.io, generation, now)?;
        self.store.io.write_batch()?;
        let body = Body {
            generation,
            fs_root,
            meta_root,
            space_root,
            free_log,
            next_ino: self.next_ino,
            next_snap_id: self.next_snap_id,
            live_deadlist: self.live_deadlist,
            next_deadlist: self.next_deadlist,
            latest_snap_gen: self.store.latest_snap_gen,
            commit_time_ns: now,
            replay_value: self.body.replay_value,
            flags: self.next_flags,
            window_gen: self
                .store
                .space
                .oldest_held()
                .unwrap_or(generation)
                .min(generation.saturating_sub(crate::space::QUARANTINE_COMMITS - 1))
                .max(1),
            log_root: BlockPtr::NULL,
            history: core::iter::once(self.body.core())
                .chain(self.body.history.iter().cloned())
                .take(superblock::HISTORY)
                .collect(),
        };
        superblock::commit(
            &mut self.store.io,
            &self.header,
            &body,
            self.rewrite_all_slots,
        )?;
        self.rewrite_all_slots = false;
        self.header.generation = generation;
        self.body = body;
        self.store.space.committed(generation, now);
        self.txn_dirty = false;
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
