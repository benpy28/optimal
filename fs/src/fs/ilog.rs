//! The fsync intent log (storage design J22).
//!
//! A full commit rewrites the tree paths, the space map, the root block and
//! two superblocks and flushes twice. `fsync` instead appends a *batch* of
//! records to a fixed ring of blocks set aside at format (like a journal)
//! and flushes once; mount replays the batches on top of the last commit
//! and commits the result. The ring is outside the allocator: it is marked
//! used in the space map from format on and nothing else ever lands there.
//!
//! **Records.** Every successful public operation that changes the
//! namespace or attributes (create, mkdir, symlink, link, unlink, rmdir,
//! rename, setattr, truncate) appends a record of its arguments and the
//! clock readings it took to an in-memory list. A write appends nothing;
//! it marks the blocks it touched. `fsync(f)` writes the whole list plus a
//! *state record* for `f`: its full inode and the current content of every
//! block marked since the last commit. Files renamed or linked in the
//! listed records get a state record too, so a rename that becomes durable
//! carries the renamed file's data with it (the ext4 `auto_da_alloc`
//! promise in the storage design). Replay runs the operations again from
//! the committed state with the recorded clock readings, so inode numbers,
//! times and directory sizes come out identical; state records then put
//! the inode and blocks back.
//!
//! Why the namespace records all go into every batch while file data goes
//! in only for the file being synced: namespace records are small and their
//! order matters (a create in one directory and a rename out of another
//! depend on each other), while file data is large and independent between
//! files. A background writer's data therefore never rides along with an
//! fsync of another file.
//!
//! **Blocks.** Each ring block is sealed on its own: bytes 0..24 hold its
//! nonce, 24..40 its MAC, and the rest is sealed as kind [`BlockKind::Log`]
//! with the block's address and the *chain generation* (the commit the chain
//! follows) as associated data. The sealed payload starts with the block's
//! sequence number in the chain, the stream bytes it carries and a flag for
//! the last block of a batch. Each commit's superblock body names the ring
//! position where its chain starts. A block from an older chain fails its
//! MAC (other generation), so replay reads blocks in order until one fails
//! and applies only batches whose last block it reached. A batch always
//! starts on a fresh block: a block that was flushed is never written again
//! while its chain is live, so a torn write can only lose the batch in
//! flight, never one that `fsync` already returned for.
//!
//! **When fsync commits instead.** No ring (an older volume, or formatted
//! without one), a read-only or degraded volume, a commit that failed since
//! the last one that worked, a commit made in the middle of an operation,
//! a snapshot operation, a file with more marked blocks than the ring holds,
//! or a batch that does not fit in the rest of the ring: `fsync` then runs a
//! full commit, which is always correct. So does a log write or flush that
//! fails (after a failed flush, data written since the last good flush is
//! made dirty again first, as a failed commit does).

use super::*;
use alloc::collections::BTreeSet;

/// Smallest ring format makes (64 KiB).
pub const LOG_MIN_BLOCKS: u64 = 16;
/// Largest ring (64 MiB): replay holds one batch in memory, at most this.
pub const LOG_MAX_BLOCKS: u64 = 16384;

/// Ring size for a `total`-block volume: 1/64 of it, 16 to 16384 blocks
/// (64 KiB at the 1024-block minimum, 64 MiB from 4 GiB up).
pub fn auto_log_blocks(total: u64) -> u64 {
    (total / 64).clamp(LOG_MIN_BLOCKS, LOG_MAX_BLOCKS)
}

const SEAL_HDR: usize = NONCE_LEN + MAC_LEN;
const PAYLOAD_HDR: usize = 16;
/// Record stream bytes one ring block carries.
pub(crate) const STREAM_PER_BLOCK: usize = BLOCK_SIZE - SEAL_HDR - PAYLOAD_HDR;
/// Payload flag: this block ends a batch.
const LAST: u32 = 1;

const R_CREATE: u8 = 1;
const R_MKDIR: u8 = 2;
const R_SYMLINK: u8 = 3;
const R_LINK: u8 = 4;
const R_UNLINK: u8 = 5;
const R_RMDIR: u8 = 6;
const R_RENAME: u8 = 7;
const R_SETATTR: u8 = 8;
const R_TRUNCATE: u8 = 9;
const R_STATE: u8 = 10;

/// Counters for tests and the benchmarks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogStats {
    /// Batches written by `fsync`.
    pub batches: u64,
    /// Ring blocks written.
    pub blocks: u64,
    /// `fsync` calls that ran a full commit instead.
    pub fsync_commits: u64,
    /// `fsync` calls with nothing new to write.
    pub fsync_noops: u64,
    /// Batches and records applied by the last mount's replay.
    pub replayed_batches: u64,
    pub replayed_records: u64,
    /// The last mount's replay stopped at a batch it could not apply (it
    /// mounted with the batches before it).
    pub replay_stopped: bool,
    /// Ring reads that failed during replay (replay ends there).
    pub replay_read_errors: u64,
}

/// The intent log's in-memory state.
#[derive(Debug, Default)]
pub(crate) struct LogState {
    /// The ring: first block and length (0 = no ring).
    pub ring_start: u64,
    pub ring_len: u64,
    /// The live chain: the commit it follows, its first ring position, and
    /// blocks written to it.
    pub chain_gen: u64,
    pub chain_start: u64,
    pub chain_used: u64,
    /// Encoded records not yet written.
    pending: Vec<u8>,
    /// Files whose state goes into the next batch (renamed or linked).
    force: BTreeSet<u64>,
    /// File blocks changed since the last commit and not yet logged.
    unlogged: BTreeMap<u64, BTreeSet<u64>>,
    /// Files with more such blocks than the ring holds: `fsync` commits.
    too_big: BTreeSet<u64>,
    /// Logging is off until the next commit (see the module notes).
    off: bool,
    pub stats: LogStats,
}

impl LogState {
    pub fn new(ring_start: u64, ring_len: u64) -> Self {
        LogState {
            ring_start,
            ring_len,
            ..LogState::default()
        }
    }

    fn capacity_bytes(&self) -> usize {
        self.ring_len as usize * STREAM_PER_BLOCK
    }

    /// Ring position the next commit's chain starts at.
    pub fn next_start(&self) -> Option<u64> {
        (self.ring_len > 0).then(|| (self.chain_start + self.chain_used) % self.ring_len)
    }

    /// Drops everything not yet written; `fsync` commits until the next
    /// commit turns logging back on.
    pub fn disable(&mut self) {
        self.off = true;
        self.pending.clear();
        self.force.clear();
        self.unlogged.clear();
        self.too_big.clear();
    }

    /// A commit of generation `generation` landed; its chain starts at
    /// `start`.
    pub fn committed(&mut self, generation: u64, start: Option<u64>) {
        let off = self.ring_len == 0;
        self.disable();
        self.off = off;
        self.chain_gen = generation;
        self.chain_start = start.unwrap_or(0);
        self.chain_used = 0;
    }

    fn tracking(&self) -> bool {
        self.ring_len > 0 && !self.off
    }

    /// Block `idx` of file `ino` changed.
    pub fn note_dirty(&mut self, ino: u64, idx: u64) {
        if !self.tracking() || self.too_big.contains(&ino) {
            return;
        }
        let s = self.unlogged.entry(ino).or_default();
        s.insert(idx);
        if s.len() as u64 > self.ring_len {
            self.unlogged.remove(&ino);
            self.too_big.insert(ino);
        }
    }

    /// File `ino`'s blocks from `from` on are gone (all of them for 0).
    pub fn note_dropped(&mut self, ino: u64, from: u64) {
        if from == 0 {
            self.unlogged.remove(&ino);
            self.too_big.remove(&ino);
        } else if let Some(s) = self.unlogged.get_mut(&ino) {
            s.split_off(&from);
        }
    }
}

// ---- encoding ----

fn put_u32(p: &mut Vec<u8>, v: u32) {
    p.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(p: &mut Vec<u8>, v: u64) {
    p.extend_from_slice(&v.to_le_bytes());
}
fn put_bytes(p: &mut Vec<u8>, b: &[u8]) {
    put_u32(p, b.len() as u32);
    p.extend_from_slice(b);
}

/// A decoded record: the clock readings its operation took, and what it did.
#[derive(Debug)]
enum Rec {
    Create(u64, Vec<u8>, u32, u64),
    Mkdir(u64, Vec<u8>, u32, u64),
    Symlink(u64, Vec<u8>, Vec<u8>, u64),
    Link(u64, u64, Vec<u8>),
    Unlink(u64, Vec<u8>),
    Rmdir(u64, Vec<u8>),
    Rename(u64, Vec<u8>, u64, Vec<u8>),
    Setattr(u64, SetAttr),
    Truncate(u64, u64),
    State(Attr, Vec<(u64, Block)>),
}

fn get_bytes(r: &mut Reader<'_>, max: usize) -> Result<Vec<u8>> {
    let n = r.u32()? as usize;
    if n > max {
        return Err(r.err());
    }
    Ok(r.bytes(n)?.to_vec())
}

fn decode_batch(stream: &[u8], at: u64) -> Result<Vec<(Vec<i64>, Rec)>> {
    let mut r = Reader::new(stream, "intent log record", at);
    let mut out = Vec::new();
    while r.remaining() > 0 {
        let tag = r.u8()?;
        let n = r.u8()? as usize;
        let mut times = Vec::with_capacity(n);
        for _ in 0..n {
            times.push(r.i64()?);
        }
        let rec = match tag {
            R_CREATE | R_MKDIR => {
                let (dir, name, perm, ino) =
                    (r.u64()?, get_bytes(&mut r, NAME_MAX)?, r.u32()?, r.u64()?);
                if tag == R_CREATE {
                    Rec::Create(dir, name, perm, ino)
                } else {
                    Rec::Mkdir(dir, name, perm, ino)
                }
            }
            R_SYMLINK => Rec::Symlink(
                r.u64()?,
                get_bytes(&mut r, NAME_MAX)?,
                get_bytes(&mut r, SYMLINK_MAX)?,
                r.u64()?,
            ),
            R_LINK => Rec::Link(r.u64()?, r.u64()?, get_bytes(&mut r, NAME_MAX)?),
            R_UNLINK => Rec::Unlink(r.u64()?, get_bytes(&mut r, NAME_MAX)?),
            R_RMDIR => Rec::Rmdir(r.u64()?, get_bytes(&mut r, NAME_MAX)?),
            R_RENAME => Rec::Rename(
                r.u64()?,
                get_bytes(&mut r, NAME_MAX)?,
                r.u64()?,
                get_bytes(&mut r, NAME_MAX)?,
            ),
            R_SETATTR => {
                let ino = r.u64()?;
                let has = r.u8()?;
                if has & !0x3f != 0 {
                    return Err(r.err());
                }
                let mut s = SetAttr::default();
                let perm = r.u32()?;
                let uid = r.u32()?;
                let gid = r.u32()?;
                let size = r.u64()?;
                let at = r.i64()?;
                let mt = r.i64()?;
                s.perm = (has & 1 != 0).then_some(perm);
                s.uid = (has & 2 != 0).then_some(uid);
                s.gid = (has & 4 != 0).then_some(gid);
                s.size = (has & 8 != 0).then_some(size);
                s.atime_ns = (has & 16 != 0).then_some(at);
                s.mtime_ns = (has & 32 != 0).then_some(mt);
                Rec::Setattr(ino, s)
            }
            R_TRUNCATE => Rec::Truncate(r.u64()?, r.u64()?),
            R_STATE => {
                let ino = r.u64()?;
                let a = Attr::decode(ino, r.bytes(INODE_LEN)?)?;
                let n = r.u32()? as u64;
                if n > LOG_MAX_BLOCKS * BLOCK_SIZE as u64 / (BLOCK_SIZE as u64 + 8) {
                    return Err(r.err());
                }
                let mut blocks = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let idx = r.u64()?;
                    let mut b = zero_block();
                    b.copy_from_slice(r.bytes(BLOCK_SIZE)?);
                    blocks.push((idx, b));
                }
                Rec::State(a, blocks)
            }
            _ => return Err(r.err()),
        };
        out.push((times, rec));
    }
    Ok(out)
}

/// Why a mount's replay stopped before the end of the chain.
pub(crate) struct ReplayStop {
    /// Batches applied before the one that failed.
    pub batches: u64,
}

impl<D: BlockDevice> Filesystem<D> {
    /// Whether operations are being recorded for the log.
    fn ilog_recording(&self) -> bool {
        self.ilog.tracking() && self.replay_times.is_none() && !self.read_only
    }

    /// Clock reading for an operation's timestamps: recorded for the log,
    /// or during replay the reading the original operation took.
    pub(super) fn op_now(&mut self) -> i64 {
        if let Some(q) = &mut self.replay_times {
            if let Some(t) = q.pop_front() {
                self.replay_last = t;
            }
            return self.replay_last;
        }
        let t = self.clock.now_ns();
        self.op_times.push(t);
        t
    }

    /// Appends a record for the operation that just succeeded: `tag`, its
    /// clock readings, then what `body` writes.
    pub(super) fn log_op(&mut self, tag: u8, body: impl FnOnce(&mut Vec<u8>)) {
        if !self.ilog_recording() {
            return;
        }
        if self.op_times.len() > u8::MAX as usize {
            self.ilog.disable();
            return;
        }
        let p = &mut self.ilog.pending;
        p.push(tag);
        p.push(self.op_times.len() as u8);
        for t in &self.op_times {
            p.extend_from_slice(&t.to_le_bytes());
        }
        body(p);
        if p.len() > self.ilog.capacity_bytes() {
            self.ilog.disable();
        }
    }

    pub(super) fn log_create(
        &mut self,
        kind: FileKind,
        dir: u64,
        name: &[u8],
        perm: u32,
        ino: u64,
    ) {
        let tag = if kind == FileKind::Dir {
            R_MKDIR
        } else {
            R_CREATE
        };
        self.log_op(tag, |p| {
            put_u64(p, dir);
            put_bytes(p, name);
            put_u32(p, perm);
            put_u64(p, ino);
        });
    }

    pub(super) fn log_symlink(&mut self, dir: u64, name: &[u8], target: &[u8], ino: u64) {
        self.log_op(R_SYMLINK, |p| {
            put_u64(p, dir);
            put_bytes(p, name);
            put_bytes(p, target);
            put_u64(p, ino);
        });
    }

    pub(super) fn log_link(&mut self, ino: u64, dir: u64, name: &[u8]) {
        self.log_op(R_LINK, |p| {
            put_u64(p, ino);
            put_u64(p, dir);
            put_bytes(p, name);
        });
        if self.ilog_recording() {
            self.ilog.force.insert(ino);
        }
    }

    pub(super) fn log_remove(&mut self, dir_kind: bool, dir: u64, name: &[u8]) {
        self.log_op(if dir_kind { R_RMDIR } else { R_UNLINK }, |p| {
            put_u64(p, dir);
            put_bytes(p, name);
        });
    }

    pub(super) fn log_rename(&mut self, sdir: u64, sname: &[u8], ddir: u64, dname: &[u8]) {
        self.log_op(R_RENAME, |p| {
            put_u64(p, sdir);
            put_bytes(p, sname);
            put_u64(p, ddir);
            put_bytes(p, dname);
        });
        if self.ilog_recording() {
            match self.lookup(ddir, dname) {
                Ok(ino) => {
                    self.ilog.force.insert(ino);
                }
                // Without the inode its data can't ride along: commit.
                Err(_) => self.ilog.disable(),
            }
        }
    }

    pub(super) fn log_setattr(&mut self, ino: u64, s: &SetAttr) {
        self.log_op(R_SETATTR, |p| {
            put_u64(p, ino);
            let has = s.perm.is_some() as u8
                | (s.uid.is_some() as u8) << 1
                | (s.gid.is_some() as u8) << 2
                | (s.size.is_some() as u8) << 3
                | (s.atime_ns.is_some() as u8) << 4
                | (s.mtime_ns.is_some() as u8) << 5;
            p.push(has);
            put_u32(p, s.perm.unwrap_or(0));
            put_u32(p, s.uid.unwrap_or(0));
            put_u32(p, s.gid.unwrap_or(0));
            put_u64(p, s.size.unwrap_or(0));
            p.extend_from_slice(&s.atime_ns.unwrap_or(0).to_le_bytes());
            p.extend_from_slice(&s.mtime_ns.unwrap_or(0).to_le_bytes());
        });
    }

    pub(super) fn log_truncate(&mut self, ino: u64, size: u64) {
        self.log_op(R_TRUNCATE, |p| {
            put_u64(p, ino);
            put_u64(p, size);
        });
    }

    /// Takes up the chain of the mounted commit (its own superblock's, not
    /// one from a history ring, which records no chain): new batches go
    /// after whatever replay finds. Without one, `fsync` commits until the
    /// first commit starts a chain.
    pub(super) fn attach_chain(&mut self, from_history: bool) {
        match self.body.log_start {
            Some(s) if !from_history && self.ilog.ring_len > 0 => {
                self.ilog.committed(self.body.generation, Some(s));
            }
            _ => self.ilog.disable(),
        }
    }

    /// Intent-log counters.
    pub fn intent_log_stats(&self) -> LogStats {
        self.ilog.stats
    }

    /// Blocks in the intent log ring (0: none, `fsync` always commits).
    pub fn intent_log_blocks(&self) -> u64 {
        self.ilog.ring_len
    }

    /// Makes file or directory `ino` durable: for a file its data and
    /// inode, and for both every namespace change made so far (creates,
    /// renames, links, unlinks, attribute changes) anywhere on the volume.
    /// Writes one intent-log batch and flushes once when it can, and runs a
    /// full [`commit`](Self::commit) when it can't (see the module notes).
    /// `fdatasync` is the same call: the inode is small and always logged.
    pub fn fsync(&mut self, ino: u64) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let a = self.get_inode(ino)?;
        if self.read_only {
            return Ok(());
        }
        let mut inos = self.ilog.force.clone();
        if a.kind == FileKind::File {
            inos.insert(ino);
        }
        if !self.ilog_recording()
            || self.degraded
            || inos.iter().any(|i| self.ilog.too_big.contains(i))
        {
            return self.fsync_by_commit();
        }
        match self.log_batch(&inos) {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.ilog.stats.fsync_noops += 1;
                Ok(())
            }
            Err(_) => {
                // The batch did not land: a full commit makes everything
                // durable another way. After a failed flush the data
                // written since the last good one may be lost: make it
                // dirty again first, as a failed commit does.
                if self.store.io.flush_failed()
                    && let Err(e) = self.redirty_unflushed()
                {
                    self.poisoned = true;
                    return Err(e);
                }
                self.ilog.disable();
                self.fsync_by_commit()
            }
        }
    }

    fn fsync_by_commit(&mut self) -> Result<()> {
        self.ilog.stats.fsync_commits += 1;
        self.commit()
    }

    /// Writes the pending records plus a state record for each file in
    /// `inos`, then flushes. `Ok(false)`: nothing to write.
    fn log_batch(&mut self, inos: &BTreeSet<u64>) -> Result<bool> {
        let bs = BLOCK_SIZE as u64;
        // Inodes and the blocks to log, checked against the room left
        // before anything is read.
        let mut states = Vec::new();
        let mut bytes = self.ilog.pending.len();
        for &i in inos {
            let a = match self.get_inode(i) {
                Ok(a) if a.kind == FileKind::File => a,
                Ok(_) | Err(Error::NotFound) => continue,
                Err(e) => return Err(e),
            };
            // A file with no changed blocks has its data committed or in
            // an earlier batch already (attribute changes are records).
            let Some(marked) = self.ilog.unlogged.get(&i) else {
                continue;
            };
            let blocks: Vec<u64> = marked.range(..a.size.div_ceil(bs)).copied().collect();
            bytes += 2 + 8 + INODE_LEN + 4 + blocks.len() * (8 + BLOCK_SIZE);
            states.push((a, blocks));
        }
        if bytes == 0 {
            return Ok(false);
        }
        let n = bytes.div_ceil(STREAM_PER_BLOCK) as u64;
        if self.ilog.chain_used + n > self.ilog.ring_len {
            return Err(Error::NoSpace);
        }
        let mut stream = Vec::with_capacity(bytes);
        stream.extend_from_slice(&self.ilog.pending);
        let mut buf = zero_block();
        for (a, blocks) in &states {
            stream.push(R_STATE);
            stream.push(0);
            put_u64(&mut stream, a.ino);
            stream.extend_from_slice(&a.encode());
            put_u32(&mut stream, blocks.len() as u32);
            for &idx in blocks {
                put_u64(&mut stream, idx);
                match self.dirty_data.get(&a.ino).and_then(|m| m.get(&idx)) {
                    Some(b) => stream.extend_from_slice(&b[..]),
                    None => {
                        self.read_disk_blocks(a.ino, idx, 1, &mut buf[..])?;
                        stream.extend_from_slice(&buf[..]);
                    }
                }
            }
        }
        debug_assert_eq!(stream.len(), bytes);
        self.write_ring(&stream)?;
        self.ilog.chain_used += n;
        self.ilog.stats.batches += 1;
        self.ilog.stats.blocks += n;
        self.ilog.pending.clear();
        self.ilog.force.clear();
        for (a, _) in &states {
            self.ilog.unlogged.remove(&a.ino);
        }
        Ok(true)
    }

    fn ring_addr(&self, seq: u64) -> u64 {
        self.ilog.ring_start + (self.ilog.chain_start + seq) % self.ilog.ring_len
    }

    /// Seals `stream` into ring blocks after the chain's last one, writes
    /// them (one request per contiguous run) and flushes.
    fn write_ring(&mut self, stream: &[u8]) -> Result<()> {
        let chunks: Vec<&[u8]> = stream.chunks(STREAM_PER_BLOCK).collect();
        let mut run: Vec<u8> = Vec::new();
        let mut run_start = 0;
        for (k, c) in chunks.iter().enumerate() {
            let seq = self.ilog.chain_used + k as u64;
            let addr = self.ring_addr(seq);
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[SEAL_HDR..SEAL_HDR + PAYLOAD_HDR]);
            w.u64(seq);
            w.u32(c.len() as u32);
            w.u32(if k + 1 == chunks.len() { LAST } else { 0 });
            b[SEAL_HDR + PAYLOAD_HDR..SEAL_HDR + PAYLOAD_HDR + c.len()].copy_from_slice(c);
            let (nonce, mac) = self.store.io.sealer.seal(
                BlockKind::Log,
                addr,
                self.ilog.chain_gen,
                &mut b[SEAL_HDR..],
            );
            b[..NONCE_LEN].copy_from_slice(&nonce);
            b[NONCE_LEN..SEAL_HDR].copy_from_slice(&mac);
            if !run.is_empty() && addr != run_start + (run.len() / BLOCK_SIZE) as u64 {
                self.store.io.write_now(run_start, &run)?;
                run.clear();
            }
            if run.is_empty() {
                run_start = addr;
            }
            run.extend_from_slice(&b[..]);
        }
        self.store.io.write_now(run_start, &run)?;
        self.store.io.flush()
    }

    /// Replays the chain that follows the mounted commit (see the module
    /// notes), applying at most `limit` batches. Stops quietly at the
    /// first block that does not verify. A batch that fails to apply stops
    /// the replay with [`ReplayStop`]: the caller mounts again with the
    /// batches before it.
    pub(super) fn replay_log(
        &mut self,
        limit: Option<u64>,
    ) -> core::result::Result<(), ReplayStop> {
        let Some(start) = self.body.log_start else {
            return Ok(());
        };
        let len = self.ilog.ring_len;
        self.ilog.chain_gen = self.body.generation;
        self.ilog.chain_start = start;
        self.ilog.chain_used = 0;
        let mut stream: Vec<u8> = Vec::new();
        let mut applied = 0u64;
        let mut b = zero_block();
        for seq in 0..len {
            let addr = self.ring_addr(seq);
            if self.store.io.read_raw(addr, &mut b[..]).is_err() {
                self.ilog.stats.replay_read_errors += 1;
                break;
            }
            let nonce: [u8; NONCE_LEN] = b[..NONCE_LEN].try_into().unwrap();
            let mac: [u8; MAC_LEN] = b[NONCE_LEN..SEAL_HDR].try_into().unwrap();
            if self
                .store
                .io
                .sealer
                .open(
                    BlockKind::Log,
                    addr,
                    self.ilog.chain_gen,
                    &nonce,
                    &mac,
                    &mut b[SEAL_HDR..],
                )
                .is_err()
            {
                break;
            }
            let mut r = Reader::new(&b[SEAL_HDR..], "intent log block", addr);
            let (Ok(s), Ok(used), Ok(flags)) = (r.u64(), r.u32(), r.u32()) else {
                break;
            };
            if s != seq || used as usize > STREAM_PER_BLOCK || flags & !LAST != 0 {
                break;
            }
            self.ilog.chain_used = seq + 1;
            let from = SEAL_HDR + PAYLOAD_HDR;
            stream.extend_from_slice(&b[from..from + used as usize]);
            if flags & LAST == 0 || limit.is_some_and(|l| applied >= l) {
                if flags & LAST != 0 {
                    stream.clear();
                }
                continue;
            }
            let recs = match decode_batch(&stream, addr) {
                Ok(r) => r,
                Err(_) => {
                    self.ilog.stats.replay_stopped = true;
                    break;
                }
            };
            stream.clear();
            // Replayed data stays dirty in memory (state records never
            // flush early), so a read-only mount writes nothing.
            let n = recs.len() as u64;
            if self.apply_batch(recs).is_err() {
                return Err(ReplayStop { batches: applied });
            }
            applied += 1;
            self.ilog.stats.replayed_batches += 1;
            self.ilog.stats.replayed_records += n;
        }
        if limit.is_some() {
            self.ilog.stats.replay_stopped = true;
        }
        Ok(())
    }

    fn apply_batch(&mut self, recs: Vec<(Vec<i64>, Rec)>) -> Result<()> {
        let diverged = Error::Corrupt("intent log replay diverged", self.body.generation);
        for (times, rec) in recs {
            self.replay_times = Some(times.into());
            let r = match rec {
                Rec::Create(dir, name, perm, ino) => self
                    .create(dir, &name, perm)
                    .and_then(|i| check_ino(i, ino, &diverged)),
                Rec::Mkdir(dir, name, perm, ino) => self
                    .mkdir(dir, &name, perm)
                    .and_then(|i| check_ino(i, ino, &diverged)),
                Rec::Symlink(dir, name, target, ino) => self
                    .symlink(dir, &name, &target)
                    .and_then(|i| check_ino(i, ino, &diverged)),
                Rec::Link(ino, dir, name) => self.link(ino, dir, &name),
                Rec::Unlink(dir, name) => self.unlink(dir, &name),
                Rec::Rmdir(dir, name) => self.rmdir(dir, &name),
                Rec::Rename(sd, sn, dd, dn) => self.rename(sd, &sn, dd, &dn),
                Rec::Setattr(ino, s) => self.setattr(ino, &s).map(|_| ()),
                Rec::Truncate(ino, size) => self.truncate(ino, size),
                Rec::State(a, blocks) => self.op(|fs| fs.apply_state(a, blocks)),
            };
            self.replay_times = None;
            r?;
        }
        Ok(())
    }

    /// Puts back a logged file state: the inode as it was at the `fsync`
    /// and the blocks logged with it.
    fn apply_state(&mut self, a: Attr, blocks: Vec<(u64, Block)>) -> Result<()> {
        self.check_writable()?;
        let cur = self.get_inode(a.ino)?;
        if cur.kind != FileKind::File || a.kind != FileKind::File {
            return Err(Error::Corrupt("intent log state of a non-file", a.ino));
        }
        let n = blocks.len() as u64;
        self.ensure_space(n, n / 32 + 8, false)?;
        if cur.size > a.size {
            self.truncate_inner(a.ino, a.size)?;
        }
        self.put_inode(&a)?;
        for (idx, b) in blocks {
            self.put_dirty(a.ino, idx, b);
        }
        Ok(())
    }
}

fn check_ino(got: u64, want: u64, diverged: &Error) -> Result<()> {
    if got == want {
        Ok(())
    } else {
        Err(diverged.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{FormatOptions, MountOptions, ROOT_INO, ZeroClock};
    use crate::sim::{CrashDevice, Rng};

    fn fresh() -> Filesystem<CrashDevice> {
        Filesystem::format(
            CrashDevice::new(4096, 3),
            &FormatOptions {
                passphrase: None,
                kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
                quarantine_min_ns: 0,
                meta_chunk_blocks: 0,
                intent_log_blocks: None,
            },
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap()
    }

    fn crash_mount(fs: Filesystem<CrashDevice>) -> Filesystem<CrashDevice> {
        let mut dev = fs.into_device();
        let img = dev.crash_image();
        Filesystem::mount(
            CrashDevice::from_image(img, 9),
            &MountOptions::default(),
            &mut Rng::new(2),
            Box::new(ZeroClock),
        )
        .unwrap()
    }

    fn fsck_ok(fs: &mut Filesystem<CrashDevice>) {
        let r = crate::fsck::check(fs, true).unwrap();
        assert!(r.ok(), "{:#?}", r.errors);
    }

    /// A batch whose replay fails (a record the committed state can't
    /// take, which only a bug could write) is not applied half way: the
    /// mount starts over with the batches before it.
    #[test]
    fn a_batch_that_fails_to_replay_mounts_the_ones_before_it() {
        let mut fs = fresh();
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.write(a, 0, b"first").unwrap();
        fs.fsync(a).unwrap();
        let b = fs.create(ROOT_INO, b"b", 0o644).unwrap();
        fs.log_remove(false, ROOT_INO, b"ghost");
        let c = fs.create(ROOT_INO, b"c", 0o644).unwrap();
        fs.fsync(c).unwrap();
        assert_eq!(fs.intent_log_stats().batches, 2);
        let mut fs = crash_mount(fs);
        let st = fs.intent_log_stats();
        assert!(st.replay_stopped);
        assert_eq!(st.replayed_batches, 1);
        assert_eq!(fs.lookup(ROOT_INO, b"a"), Ok(a));
        let mut buf = [0u8; 5];
        assert_eq!(fs.read(a, 0, &mut buf), Ok(5));
        assert_eq!(&buf, b"first");
        // Nothing of the second batch, not even the create before the bad
        // record.
        assert_eq!(fs.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        assert_eq!(fs.next_ino_for_tests(), b);
        fsck_ok(&mut fs);
    }

    /// A batch that does not decode (a valid MAC over records this code
    /// can't read) ends the replay before it.
    #[test]
    fn an_undecodable_batch_ends_the_replay() {
        let mut fs = fresh();
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.fsync(a).unwrap();
        fs.create(ROOT_INO, b"b", 0o644).unwrap();
        fs.ilog.pending.push(99);
        fs.fsync(ROOT_INO).unwrap();
        let mut fs = crash_mount(fs);
        assert!(fs.intent_log_stats().replay_stopped);
        assert_eq!(fs.intent_log_stats().replayed_batches, 1);
        assert_eq!(fs.lookup(ROOT_INO, b"a"), Ok(a));
        assert_eq!(fs.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        fsck_ok(&mut fs);
    }

    /// A damaged ring block (one flipped bit) fails its MAC: replay stops
    /// there and keeps the batches before it.
    #[test]
    fn a_damaged_ring_block_ends_the_replay() {
        let mut fs = fresh();
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.fsync(a).unwrap();
        fs.create(ROOT_INO, b"b", 0o644).unwrap();
        fs.fsync(ROOT_INO).unwrap();
        let addr = fs.ring_addr(1);
        let mut dev = fs.into_device();
        let mut img = dev.crash_image();
        img[addr as usize * BLOCK_SIZE + 2000] ^= 4;
        let mut fs = Filesystem::mount(
            CrashDevice::from_image(img, 9),
            &MountOptions::default(),
            &mut Rng::new(2),
            Box::new(ZeroClock),
        )
        .unwrap();
        assert_eq!(fs.intent_log_stats().replayed_batches, 1);
        assert_eq!(fs.lookup(ROOT_INO, b"a"), Ok(a));
        assert_eq!(fs.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        fsck_ok(&mut fs);
    }

    /// Hand-worked block layout: payload header (sequence, stream bytes,
    /// last flag) right after the 40-byte seal header, sealed as kind 10
    /// at the block's address with the chain generation.
    #[test]
    fn ring_block_layout_hand_worked() {
        let mut fs = fresh();
        assert_eq!((fs.ilog.ring_start, fs.ilog.ring_len), (2, 64));
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.fsync(a).unwrap();
        // One create record: tag 1 + reading count 1 + 2 readings x 8 (the
        // new inode's times, then the parent's) + dir 8 + name (4 + 1) +
        // perm 4 + ino 8 = 43 bytes. "a" has no blocks marked, so no state
        // record: one ring block.
        assert_eq!(fs.intent_log_stats().blocks, 1);
        let addr = fs.ring_addr(0);
        assert_eq!(addr, 2 + fs.ilog.chain_start);
        let mut b = zero_block();
        fs.store.io.read_raw(addr, &mut b[..]).unwrap();
        let nonce: [u8; NONCE_LEN] = b[..24].try_into().unwrap();
        let mac: [u8; MAC_LEN] = b[24..40].try_into().unwrap();
        fs.store
            .io
            .sealer
            .open(
                BlockKind::Log,
                addr,
                fs.body.generation,
                &nonce,
                &mac,
                &mut b[40..],
            )
            .unwrap();
        assert_eq!(b[40..48], 0u64.to_le_bytes(), "sequence 0");
        let used = u32::from_le_bytes(b[48..52].try_into().unwrap());
        assert_eq!(used, 43);
        assert_eq!(b[52..56], LAST.to_le_bytes());
        assert_eq!(b[56], R_CREATE);
        assert_eq!(b[57], 2, "two clock readings");
        assert!(b[56 + used as usize..].iter().all(|&x| x == 0));
        // The wrong generation (an older chain) fails.
        let mut c = zero_block();
        fs.store.io.read_raw(addr, &mut c[..]).unwrap();
        assert!(
            fs.store
                .io
                .sealer
                .open(
                    BlockKind::Log,
                    addr,
                    fs.body.generation - 1,
                    &nonce,
                    &mac,
                    &mut c[40..]
                )
                .is_err()
        );
    }

    /// The blocks waiting to be logged follow each file: a shrink drops
    /// those past the new end, a deleted file drops all of them, and a file
    /// with more than the ring holds keeps none (fsync commits it).
    #[test]
    fn marked_blocks_follow_the_file() {
        let mut fs = fresh();
        let f = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.write(f, 0, &[1u8; 8 * BLOCK_SIZE]).unwrap();
        assert_eq!(fs.ilog.unlogged[&f].len(), 8);
        fs.truncate(f, 100).unwrap();
        assert_eq!(
            fs.ilog.unlogged[&f].iter().copied().collect::<Vec<_>>(),
            [0]
        );
        fs.unlink(ROOT_INO, b"f").unwrap();
        assert!(!fs.ilog.unlogged.contains_key(&f));
        let ring = fs.ilog.ring_len as usize;
        let g = fs.create(ROOT_INO, b"g", 0o644).unwrap();
        fs.write(g, 0, &vec![2u8; ring * BLOCK_SIZE]).unwrap();
        assert_eq!(fs.ilog.unlogged[&g].len(), ring);
        fs.write(g, (ring * BLOCK_SIZE) as u64, &[3u8; 1]).unwrap();
        assert!(!fs.ilog.unlogged.contains_key(&g), "one past the ring");
        assert!(fs.ilog.too_big.contains(&g));
        fs.unlink(ROOT_INO, b"g").unwrap();
        assert!(!fs.ilog.too_big.contains(&g));
        assert!(fs.ilog.unlogged.is_empty());
    }

    /// Records that would not fit the ring are not kept piling up in
    /// memory: past the ring's capacity they are dropped and fsync commits.
    #[test]
    fn pending_records_never_outgrow_the_ring() {
        let mut fs = Filesystem::format(
            CrashDevice::new(4096, 3),
            &FormatOptions {
                passphrase: None,
                kdf: crate::KdfParams::INSECURE_FAST_FOR_TESTS,
                quarantine_min_ns: 0,
                meta_chunk_blocks: 0,
                intent_log_blocks: Some(LOG_MIN_BLOCKS),
            },
            &mut Rng::new(1),
            Box::new(ZeroClock),
        )
        .unwrap();
        fs.mark_dirty();
        fs.commit().unwrap();
        let cap = fs.ilog.capacity_bytes();
        let mut most = 0;
        let mut i = 0;
        while fs.ilog.tracking() {
            let name = alloc::format!("{i:0>200}");
            fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap();
            most = most.max(fs.ilog.pending.len());
            i += 1;
            assert!(i < 1000, "records never filled a 16-block ring");
        }
        assert!(most <= cap, "{most} bytes held for a {cap}-byte ring");
        assert!(fs.ilog.pending.is_empty());
        let g = fs.generation();
        fs.fsync(ROOT_INO).unwrap();
        assert_eq!(fs.generation(), g + 1);
        let mut fs = crash_mount(fs);
        assert!(
            fs.lookup(ROOT_INO, alloc::format!("{:0>200}", i - 1).as_bytes())
                .is_ok()
        );
        fsck_ok(&mut fs);
    }

    /// Replay never commits part way (the chain must stay whole until the
    /// commit after it): a space check that would commit refuses instead.
    #[test]
    fn replay_refuses_a_commit_for_space() {
        let mut fs = fresh();
        fs.create(ROOT_INO, b"a", 0o644).unwrap();
        let g = fs.generation();
        fs.replay_times = Some(VecDeque::new());
        assert_eq!(fs.commit_for_space(), Err(Error::NoSpace));
        assert_eq!(fs.generation(), g);
        fs.replay_times = None;
        assert_eq!(fs.commit_for_space(), Ok(()));
        assert_eq!(fs.generation(), g + 1);
    }

    /// A superblock body whose chain start lies outside the ring is not
    /// mounted (a plain volume's MAC key is no secret, so a crafted image
    /// can carry one): mount falls back to the commit before it.
    #[test]
    fn a_chain_start_outside_the_ring_is_not_mounted() {
        let mut fs = fresh();
        let a = fs.create(ROOT_INO, b"a", 0o644).unwrap();
        fs.commit().unwrap();
        let prev = fs.generation();
        fs.create(ROOT_INO, b"b", 0o644).unwrap();
        let ring = fs.ilog.ring_len;
        fs.ilog.ring_len = 1 << 20;
        fs.ilog.chain_start = ring + 5;
        fs.commit().unwrap();
        assert_eq!(fs.body.log_start, Some(ring + 5));
        let fs = crash_mount(fs);
        assert_eq!(fs.generation(), prev);
        let mut fs = fs;
        assert_eq!(fs.lookup(ROOT_INO, b"a"), Ok(a));
        assert_eq!(fs.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
        fsck_ok(&mut fs);
    }

    /// A header whose ring overlaps a superblock slot, is shorter than the
    /// minimum, or runs past the end is refused.
    #[test]
    fn a_header_with_a_bad_ring_is_refused() {
        for (start, blocks) in [(1, 64), (2, LOG_MIN_BLOCKS - 1), (4096 - 2 - 63, 64)] {
            let mut fs = fresh();
            fs.header.log_start = start;
            fs.header.log_blocks = blocks;
            for _ in 0..2 {
                fs.mark_dirty();
                fs.commit().unwrap();
            }
            let mut dev = fs.into_device();
            let img = dev.crash_image();
            let r = Filesystem::mount(
                CrashDevice::from_image(img, 9),
                &MountOptions::default(),
                &mut Rng::new(2),
                Box::new(ZeroClock),
            );
            // No slot decodes, so no superblock is found at all.
            assert!(
                matches!(r, Err(Error::NoFilesystem)),
                "({start}, {blocks}): {:?}",
                r.err()
            );
        }
    }
}
