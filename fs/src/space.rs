//! The space map: which blocks are in use.
//!
//! On disk it is a copy-on-write radix tree. Leaves are bitmap blocks, each
//! covering 32768 blocks (128 MiB). Above them are `levels` (1 to 5) levels
//! of tables of 64 pointers each; the single top table is the space root.
//! Five levels reach 2^45 blocks (128 PiB). Every pointer's `aux` holds the
//! number of free blocks below it, so allocation skips full regions without
//! reading them, and a count that disagrees with the bits is corruption.
//!
//! A **null** child pointer means "never written: entirely free". A fresh
//! volume therefore writes only the tables and bitmaps on the path to its
//! first and last blocks, so formatting is O(1) in the device size, and
//! tables and bitmaps are loaded only when touched (clean ones are evicted).
//!
//! The bitmap committed with generation T is exactly the set of blocks
//! referenced by T (fsck checks this). Blocks freed while building a commit
//! are cleared at once but *quarantined*: not handed out again until at
//! least [`QUARANTINE_COMMITS`] later commits have completed and at least
//! the configured time has passed. Every commit in that window therefore
//! stays fully intact on disk, which is what lets mount fall back to an
//! older commit when a drive lost the newest ones (for example one that
//! acknowledged flushes it never performed). The free log records the
//! quarantine, so it survives a remount.

use crate::chunk::{ChunkTable, Entry, LOGICAL, is_logical};
use crate::codec::{Reader, Writer};
use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Block, CopyOwner, Io, zero_block};
use crate::ptr::{BlockPtr, PTR_LEN};
use crate::rangeset::RangeSet;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec;
use alloc::vec::Vec;

pub const BITS_PER_BMP: u64 = (BLOCK_SIZE * 8) as u64;
pub const FANOUT: u64 = (BLOCK_SIZE / PTR_LEN) as u64;
pub const MAX_LEVELS: u32 = 5;
/// 32768 * 64^5 = 2^45 blocks.
pub const MAX_BLOCKS: u64 = BITS_PER_BMP << (6 * MAX_LEVELS);
const WORDS: usize = BLOCK_SIZE / 8;

/// Commits a freed block stays quarantined for (K in the storage design).
pub const QUARANTINE_COMMITS: u64 = 8;

const LOG_MAGIC: u32 = 0x474f_4c46; // "FLOG"
const LOG_HEADER: usize = 8 + PTR_LEN;
const LOG_ENTRY: usize = 32;
pub const LOG_PER_BLOCK: usize = (BLOCK_SIZE - LOG_HEADER) / LOG_ENTRY;

/// Number of blocks reserved for superblock copies at each end of the device.
pub const SB_RESERVED_FRONT: u64 = 2;
pub const SB_RESERVED_BACK: u64 = 2;

/// Clean tables and bitmaps kept in memory after a commit.
const CLEAN_CACHE: usize = 2048;

#[derive(Debug)]
struct Bmp {
    words: Box<[u64; WORDS]>,
    dirty: bool,
}

#[derive(Debug)]
struct Table {
    /// Child pointers. A dirty child's pointer has a stale address until commit.
    kids: Vec<BlockPtr>,
    dirty: bool,
}

/// Blocks freed by one commit, with when that commit happened.
#[derive(Debug, Clone)]
pub struct FreedBatch {
    pub generation: u64,
    pub time_ns: i64,
    /// Physical blocks.
    pub set: RangeSet,
    /// Logical metadata blocks (bit 63 set).
    pub lset: RangeSet,
}

#[derive(Debug)]
pub struct Space {
    total: u64,
    levels: u32,
    /// The root table's own pointer (address, MAC); its `aux` is `levels`.
    root_ptr: BlockPtr,
    /// Loaded tables, keyed by (level, index). Level `levels` index 0 is the root.
    tables: BTreeMap<(u32, u64), Table>,
    bmps: BTreeMap<u64, Bmp>,
    free_total: u64,
    /// Blocks freed in the transaction being built.
    pub freed_now: RangeSet,
    /// Blocks freed by recent commits, still quarantined (oldest first).
    pub held: VecDeque<FreedBatch>,
    held_blocks: u64,
    /// Logical metadata blocks allocated since this `Space` was loaded (a
    /// counter, so callers can measure what a step allocated).
    pub allocated_l: u64,
    /// The same for chunk-table node pairs.
    pub allocated_pairs: u64,
    /// See [`refresh_releasing`](Self::refresh_releasing).
    releasing_slots: u64,
    /// Free-log blocks of the last commit.
    log_addrs: Vec<u64>,
    /// Old copies of space tables that became garbage this transaction.
    pending_table_frees: Vec<u64>,
    cursor: u64,
    pub quarantine_min_ns: i64,
    /// Where each logical metadata chunk's two copies live.
    pub chunks: ChunkTable,
    /// Logical metadata blocks freed in the transaction being built.
    pub freed_now_l: RangeSet,
    /// Chunk the metadata allocator is filling.
    meta_cursor: u64,
    /// Chunks that may have become empty (checked for release at commit).
    maybe_empty: BTreeSet<u64>,
    /// Chunks to move at the start of the next commit.
    pub relocations: Vec<u64>,
    /// This commit moved a run holding live metadata blocks (see
    /// [`Body::last_move_gen`](crate::superblock::Body::last_move_gen)).
    pub moved_live_run: bool,
}

fn span(level: u32) -> u64 {
    // Blocks covered by one node at `level` (0 = bitmap).
    BITS_PER_BMP << (6 * level)
}

impl Space {
    pub fn total_blocks(&self) -> u64 {
        self.total
    }

    pub fn levels_for(total: u64) -> Result<u32> {
        if total > MAX_BLOCKS {
            return Err(Error::Unsupported("device larger than 128 PiB"));
        }
        let mut l = 1;
        while span(l) < total {
            l += 1;
        }
        Ok(l)
    }

    /// Valid (in-device) blocks inside node `(level, idx)`.
    fn blocks_in(&self, level: u32, idx: u64) -> u64 {
        let s = span(level);
        let start = idx.saturating_mul(s);
        if start >= self.total {
            0
        } else {
            (self.total - start).min(s)
        }
    }

    /// Free blocks inside node `(level, idx)` of a brand-new volume.
    fn fresh_free(&self, level: u32, idx: u64) -> u64 {
        let s = span(level);
        let start = idx * s;
        let end = start + self.blocks_in(level, idx);
        let mut reserved = 0;
        for b in [0, 1, self.total - 2, self.total - 1] {
            if b >= start && b < end {
                reserved += 1;
            }
        }
        self.blocks_in(level, idx) - reserved
    }

    fn empty(total: u64, levels: u32, root_ptr: BlockPtr, chunks: ChunkTable) -> Space {
        Space {
            chunks,
            freed_now_l: RangeSet::new(),
            meta_cursor: 0,
            maybe_empty: BTreeSet::new(),
            relocations: Vec::new(),
            moved_live_run: false,
            total,
            levels,
            root_ptr,
            tables: BTreeMap::new(),
            bmps: BTreeMap::new(),
            free_total: 0,
            freed_now: RangeSet::new(),
            held: VecDeque::new(),
            held_blocks: 0,
            allocated_l: 0,
            allocated_pairs: 0,
            releasing_slots: 0,
            log_addrs: Vec::new(),
            pending_table_frees: Vec::new(),
            cursor: SB_RESERVED_FRONT,
            quarantine_min_ns: 0,
        }
    }

    /// A fresh map for mkfs: everything free except the superblock slots.
    /// Only the paths to the first and last bitmap are materialised.
    pub fn format(total: u64, chunk_shift: u32) -> Result<Space> {
        let levels = Self::levels_for(total)?;
        let chunks = ChunkTable::format(total, chunk_shift);
        let mut s = Self::empty(total, levels, BlockPtr::NULL, chunks);
        let root = s.fresh_table(levels, 0);
        s.tables.insert((levels, 0), root);
        s.free_total = s.fresh_free(levels, 0);
        for b in [0, 1, total - 2, total - 1] {
            s.mark_reserved(b);
        }
        Ok(s)
    }

    fn fresh_table(&self, level: u32, idx: u64) -> Table {
        let kids = (0..FANOUT)
            .map(|j| BlockPtr {
                aux: self.fresh_free(level - 1, idx * FANOUT + j),
                ..BlockPtr::NULL
            })
            .collect();
        Table { kids, dirty: true }
    }

    /// Used only by `format`: materialises the path to `b` and sets its bit.
    fn mark_reserved(&mut self, b: u64) {
        let i = b / BITS_PER_BMP;
        for level in (1..self.levels).rev() {
            let idx = i >> (6 * level);
            if !self.tables.contains_key(&(level, idx)) {
                let t = self.fresh_table(level, idx);
                self.tables.insert((level, idx), t);
            }
        }
        self.bmps.entry(i).or_insert_with(|| {
            let mut words = Box::new([0u64; WORDS]);
            // Bits past the end of the device are permanently "used".
            let first = i * BITS_PER_BMP;
            for x in self.total.max(first)..first + BITS_PER_BMP {
                let bit = x - first;
                words[(bit / 64) as usize] |= 1 << (bit % 64);
            }
            Bmp { words, dirty: true }
        });
        let bit = b % BITS_PER_BMP;
        self.bmps.get_mut(&i).unwrap().words[(bit / 64) as usize] |= 1 << (bit % 64);
    }

    /// Opens a committed commit's chunk table. The map itself is attached
    /// with [`Space::attach`] once the root block (read through the chunk
    /// table) has given its root and free log.
    pub fn open<D: BlockDevice>(
        io: &mut Io<D>,
        total: u64,
        chunk_shift: u32,
        chunk_root: BlockPtr,
        chunk_depth: u32,
    ) -> Result<Space> {
        let levels = Self::levels_for(total)?;
        let chunks = ChunkTable::open(io, total, chunk_shift, chunk_root, chunk_depth)?;
        Ok(Self::empty(total, levels, BlockPtr::NULL, chunks))
    }

    /// Loads the committed map's root and free log. Tables and bitmaps
    /// load on demand.
    pub fn attach<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        root: BlockPtr,
        log: BlockPtr,
    ) -> Result<()> {
        let levels = self.levels;
        let total = self.total;
        if root.aux != levels as u64 || root.is_null() {
            return Err(Error::Corrupt("space map depth", root.addr));
        }
        self.root_ptr = root;
        let s = self;
        let rb = s.read_meta(io, BlockKind::SpaceTable, &root)?;
        let t = s.decode_table(&rb[..], levels, 0, root.addr)?;
        s.free_total = t.kids.iter().map(|k| k.aux).sum();
        s.tables.insert((levels, 0), t);
        // The free log: recent commits' frees stay quarantined.
        let mut p = log;
        let mut guard = 0u64;
        while !p.is_null() {
            guard += 1;
            if guard > 1 << 24 {
                return Err(Error::Corrupt("free log loops", p.addr));
            }
            let b = s.read_meta(io, BlockKind::FreeLog, &p)?;
            let mut r = Reader::new(&b[..], "free log", p.addr);
            if r.u32()? != LOG_MAGIC {
                return Err(r.err());
            }
            let n = r.u32()? as usize;
            if n > LOG_PER_BLOCK {
                return Err(r.err());
            }
            let next = BlockPtr::decode(&mut r)?;
            for _ in 0..n {
                let start = r.u64()?;
                let len = r.u64()?;
                let generation = r.u64()?;
                let time_ns = r.i64()?;
                // Physical ranges lie in the device; logical ones (bit 63)
                // stay logical.
                let logical = is_logical(start);
                let bad = len == 0
                    || start
                        .checked_add(len)
                        .is_none_or(|e| if logical { e - 1 < LOGICAL } else { e > total });
                let batch = match s.held.iter_mut().find(|h| h.generation == generation) {
                    Some(h) => h,
                    None => {
                        s.held.push_back(FreedBatch {
                            generation,
                            time_ns,
                            set: RangeSet::new(),
                            lset: RangeSet::new(),
                        });
                        s.held.back_mut().unwrap()
                    }
                };
                let set = if logical {
                    &mut batch.lset
                } else {
                    &mut batch.set
                };
                if bad || batch.time_ns != time_ns || !set.insert(start, len) {
                    return Err(Error::Corrupt("bad free log entry", p.addr));
                }
                if !logical {
                    s.held_blocks += len;
                }
            }
            s.log_addrs.push(p.addr);
            p = next;
        }
        s.held.make_contiguous().sort_by_key(|h| h.generation);
        Ok(())
    }

    fn decode_table(&self, b: &[u8], level: u32, idx: u64, at: u64) -> Result<Table> {
        let mut r = Reader::new(b, "space table", at);
        let mut kids = Vec::with_capacity(FANOUT as usize);
        for j in 0..FANOUT {
            let p = BlockPtr::decode(&mut r)?;
            let c = idx * FANOUT + j;
            let cap = self.blocks_in(level - 1, c);
            if p.aux > cap || (cap == 0 && p != BlockPtr::NULL) {
                return Err(Error::Corrupt("space table entry", at));
            }
            // A never-written child must be entirely free (minus superblock slots).
            if p.is_null() && p.aux != self.fresh_free(level - 1, c) {
                return Err(Error::Corrupt("implicit space entry count", at));
            }
            kids.push(p);
        }
        Ok(Table { kids, dirty: false })
    }

    pub fn free_blocks(&self) -> u64 {
        self.free_total
    }

    /// Metadata slots a commit can take without mapping a new chunk: free
    /// slots of mapped chunks that are neither quarantined nor in a chunk
    /// that may be released at the next commit.
    pub fn usable_meta_slots(&self) -> u64 {
        let quarantined =
            self.freed_now_l.len() + self.held.iter().map(|h| h.lset.len()).sum::<u64>();
        self.chunks
            .free_slots()
            .saturating_sub(quarantined + self.releasing_slots)
    }

    /// Counts the slots of chunks the next commit may release (empty ones
    /// among those that may have become empty), for
    /// [`usable_meta_slots`](Self::usable_meta_slots).
    pub fn refresh_releasing<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<()> {
        let mut n = 0;
        for &id in &self.maybe_empty {
            let e = self.chunks.entry(io, id)?;
            if e.mapped() && e.used == 0 {
                n += e.len as u64;
            }
        }
        self.releasing_slots = n;
        Ok(())
    }

    /// Free blocks that can actually be allocated right now.
    pub fn allocatable_blocks(&self) -> u64 {
        self.free_total
            .saturating_sub(self.freed_now.len() + self.held_blocks)
    }

    /// The table at `(level, idx)`, loading (or synthesising) it if needed.
    fn table<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        level: u32,
        idx: u64,
    ) -> Result<&mut Table> {
        if !self.tables.contains_key(&(level, idx)) {
            debug_assert!(level < self.levels);
            let p = self.table(io, level + 1, idx / FANOUT)?.kids[(idx % FANOUT) as usize];
            let t = if p.is_null() {
                let mut t = self.fresh_table(level, idx);
                t.dirty = false;
                t
            } else {
                let b = self.read_meta(io, BlockKind::SpaceTable, &p)?;
                let t = self.decode_table(&b[..], level, idx, p.addr)?;
                let sum: u64 = t.kids.iter().map(|k| k.aux).sum();
                if sum != p.aux {
                    return Err(Error::Corrupt("space table free count mismatch", p.addr));
                }
                t
            };
            self.tables.insert((level, idx), t);
        }
        Ok(self.tables.get_mut(&(level, idx)).unwrap())
    }

    fn bmp_ptr<D: BlockDevice>(&mut self, io: &mut Io<D>, i: u64) -> Result<BlockPtr> {
        Ok(self.table(io, 1, i / FANOUT)?.kids[(i % FANOUT) as usize])
    }

    fn load<D: BlockDevice>(&mut self, io: &mut Io<D>, i: u64) -> Result<&mut Bmp> {
        if !self.bmps.contains_key(&i) {
            let p = self.bmp_ptr(io, i)?;
            let first = i * BITS_PER_BMP;
            let words = if p.is_null() {
                Box::new([0u64; WORDS])
            } else {
                let b = self.read_meta(io, BlockKind::Bitmap, &p)?;
                let mut words = Box::new([0u64; WORDS]);
                for (w, c) in words.iter_mut().zip(b.as_chunks::<8>().0) {
                    *w = u64::from_le_bytes(*c);
                }
                // Padding bits past the end of the device must read as used.
                for x in self.total.max(first)..first + BITS_PER_BMP {
                    let bit = x - first;
                    if words[(bit / 64) as usize] & (1 << (bit % 64)) == 0 {
                        return Err(Error::Corrupt(
                            "bitmap marks blocks past the device end free",
                            p.addr,
                        ));
                    }
                }
                words
            };
            let ones: u64 = words.iter().map(|w| w.count_ones() as u64).sum();
            if BITS_PER_BMP - ones != p.aux {
                return Err(Error::Corrupt("bitmap free count mismatch", p.addr));
            }
            self.bmps.insert(
                i,
                Bmp {
                    words,
                    dirty: false,
                },
            );
        }
        Ok(self.bmps.get_mut(&i).unwrap())
    }

    /// Marks bitmap `i` changed and adds `delta` to the free counts on its
    /// path. The first change in a transaction frees the old copies
    /// (copy-on-write) of the bitmap and of every table above it.
    fn touch<D: BlockDevice>(&mut self, io: &mut Io<D>, i: u64, delta: i64) -> Result<()> {
        self.load(io, i)?;
        let first_touch = !self.bmps[&i].dirty;
        let mut old_bmp = None;
        let mut idx = i;
        for level in 1..=self.levels {
            let parent = idx / FANOUT;
            let slot = (idx % FANOUT) as usize;
            let t = self.table(io, level, parent)?;
            let k = &mut t.kids[slot];
            k.aux = (k.aux as i64 + delta) as u64;
            if level == 1 && first_touch {
                old_bmp = Some(k.addr);
                k.addr = 0;
            }
            let was_dirty = t.dirty;
            t.dirty = true;
            if !was_dirty {
                // Free this table's old copy, whose address is in its parent.
                let old = if level == self.levels {
                    core::mem::replace(&mut self.root_ptr.addr, 0)
                } else {
                    let gp = self.table(io, level + 1, parent / FANOUT)?;
                    core::mem::replace(&mut gp.kids[(parent % FANOUT) as usize].addr, 0)
                };
                if old != 0 {
                    self.pending_table_frees.push(old);
                }
            }
            idx = parent;
        }
        self.free_total = (self.free_total as i64 + delta) as u64;
        self.bmps.get_mut(&i).unwrap().dirty = true;
        if let Some(a) = old_bmp
            && a != 0
        {
            self.free(io, a, 1)?;
        }
        Ok(())
    }

    /// Whether block `b` is marked used.
    pub fn is_used<D: BlockDevice>(&mut self, io: &mut Io<D>, b: u64) -> Result<bool> {
        let i = b / BITS_PER_BMP;
        let bit = b % BITS_PER_BMP;
        Ok(self.load(io, i)?.words[(bit / 64) as usize] & (1 << (bit % 64)) != 0)
    }

    /// Changes bits `[start, start+len)` to `used`, failing if any already is.
    fn set_range<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        start: u64,
        len: u64,
        used: bool,
    ) -> Result<()> {
        let mut b = start;
        let end = start + len;
        while b < end {
            let i = b / BITS_PER_BMP;
            let chunk_end = end.min((i + 1) * BITS_PER_BMP);
            let first = i * BITS_PER_BMP;
            let bm = self.load(io, i)?;
            for x in b..chunk_end {
                let bit = x - first;
                let (w, m) = ((bit / 64) as usize, 1u64 << (bit % 64));
                if (bm.words[w] & m != 0) == used {
                    return Err(Error::Corrupt(
                        if used {
                            "allocating a used block"
                        } else {
                            "double free"
                        },
                        x,
                    ));
                }
            }
            for x in b..chunk_end {
                let bit = x - first;
                bm.words[(bit / 64) as usize] ^= 1u64 << (bit % 64);
            }
            let n = (chunk_end - b) as i64;
            self.touch(io, i, if used { -n } else { n })?;
            b = chunk_end;
        }
        Ok(())
    }

    /// Frees `[start, start+len)`. Every block must currently be used.
    /// Logical (metadata) addresses free chunk slots; physical ones free
    /// bitmap bits. Both are quarantined.
    pub fn free<D: BlockDevice>(&mut self, io: &mut Io<D>, start: u64, len: u64) -> Result<()> {
        if is_logical(start) {
            return self.free_meta(io, start, len);
        }
        if len == 0
            || start < SB_RESERVED_FRONT
            || start
                .checked_add(len)
                .is_none_or(|e| e > self.total - SB_RESERVED_BACK)
        {
            return Err(Error::Corrupt(
                "free of reserved or out-of-range block",
                start,
            ));
        }
        self.set_range(io, start, len, false)?;
        let ok = self.freed_now.insert(start, len);
        debug_assert!(ok, "freed range already in freed_now");
        Ok(())
    }

    /// Frees logical metadata blocks `[start, start+len)` (may span chunks).
    fn free_meta<D: BlockDevice>(&mut self, io: &mut Io<D>, start: u64, len: u64) -> Result<()> {
        if len == 0 || start.checked_add(len).is_none() {
            return Err(Error::Corrupt("free of a bad logical range", start));
        }
        let mut l = start;
        while l < start + len {
            let (id, off) = self.chunks.split(l)?;
            let mut e = self.chunks.entry(io, id)?;
            let n = (e.len as u64).saturating_sub(off).min(start + len - l);
            if n == 0 {
                return Err(Error::Corrupt("free of an unmapped logical block", l));
            }
            let bits = Entry::mask(n as u32) << off;
            if e.used & bits != bits {
                return Err(Error::Corrupt("double free", l));
            }
            e.used &= !bits;
            self.chunks.set_entry(io, id, e)?;
            if e.used == 0 {
                self.maybe_empty.insert(id);
            }
            l += n;
        }
        let ok = self.freed_now_l.insert(start, len);
        debug_assert!(ok, "freed logical range already in freed_now_l");
        Ok(())
    }

    /// Whether logical block `l` is quarantined (freed by this transaction
    /// or a recent commit).
    fn lquarantined(&self, l: u64) -> bool {
        core::iter::once(&self.freed_now_l)
            .chain(self.held.iter().map(|h| &h.lset))
            .any(|set| set.overlaps(l, 1))
    }

    /// Reads a metadata block through the chunk table (copy 0, falling back
    /// to copy 1).
    pub fn read_meta<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        kind: BlockKind,
        p: &BlockPtr,
    ) -> Result<Block> {
        let (p0, p1) = self.chunks.resolve(io, p.addr)?;
        let id = self.chunks.split(p.addr)?.0;
        io.read_dup(kind, p, p0, p1, CopyOwner::Chunk(id))
    }

    /// Seals metadata block `l` and queues both copies.
    pub fn write_meta<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        kind: BlockKind,
        l: u64,
        birth: u64,
        data: Block,
        aux: u64,
    ) -> Result<BlockPtr> {
        let (p0, p1) = self.chunks.resolve(io, l)?;
        Ok(io.write_dup(kind, l, p0, p1, birth, data, aux))
    }

    /// Allocates `n` logical metadata blocks, as runs of consecutive
    /// addresses (each run inside one chunk).
    pub fn alloc_meta<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        n: u64,
    ) -> Result<Vec<(u64, u64)>> {
        self.allocated_l += n;
        let mut out = Vec::new();
        let mut left = n;
        while left > 0 {
            let (l, k) = self.alloc_meta_run(io, left)?;
            out.push((l, k));
            left -= k;
        }
        Ok(out)
    }

    fn alloc_meta_run<D: BlockDevice>(&mut self, io: &mut Io<D>, want: u64) -> Result<(u64, u64)> {
        // Fill existing chunks first (from the one being filled), so
        // metadata stays compact; map a new chunk only when none has room.
        for pass in 0..2 {
            let mut from = if pass == 0 { self.meta_cursor } else { 0 };
            while let Some(id) = self.chunks.find_free(io, from)? {
                if let Some(r) = self.take_slots(io, id, want)? {
                    return Ok(r);
                }
                from = id + 1;
            }
        }
        let id = self.new_chunk(io)?;
        self.take_slots(io, id, want)?
            .ok_or(Error::Corrupt("new chunk has no free slot", id))
    }

    /// Takes the first run (up to `want`) of free, unquarantined slots of
    /// chunk `id`, if it has one.
    fn take_slots<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        id: u64,
        want: u64,
    ) -> Result<Option<(u64, u64)>> {
        let mut e = self.chunks.entry(io, id)?;
        let len = e.len as u64;
        let mut off = 0;
        while off < len {
            if e.is_used(off) || self.lquarantined(self.chunks.join(id, off)) {
                off += 1;
                continue;
            }
            let mut end = off + 1;
            while end < len
                && end - off < want
                && !e.is_used(end)
                && !self.lquarantined(self.chunks.join(id, end))
            {
                end += 1;
            }
            e.used |= Entry::mask((end - off) as u32) << off;
            self.chunks.set_entry(io, id, e)?;
            self.meta_cursor = id;
            return Ok(Some((self.chunks.join(id, off), end - off)));
        }
        Ok(None)
    }

    /// Maps the lowest free chunk id to two fresh physical runs.
    fn new_chunk<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<u64> {
        let id = loop {
            match self.chunks.find_unmapped(io)? {
                Some(id) => break id,
                None => self.chunks.grow()?,
            }
        };
        let (p0, p1, len) = self.alloc_chunk_runs(io, self.chunks.chunk_blocks(), true)?;
        self.chunks.set_entry(
            io,
            id,
            Entry {
                p0,
                p1,
                used: 0,
                len: len as u32,
            },
        )?;
        Ok(id)
    }

    /// Allocates two runs of `len` blocks for a chunk's copies, copy 1 in
    /// the other half of the device where possible. With `shrink`, a
    /// fragmented device gets shorter runs (down to one block) rather
    /// than failing while any two blocks are free.
    fn alloc_chunk_runs<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        len: u64,
        shrink: bool,
    ) -> Result<(u64, u64, u64)> {
        let mut len = len;
        loop {
            if let Some(p0) = self.find_exact(io, self.cursor, len)? {
                self.set_range(io, p0, len, true)?;
                let far = self.far_from(p0);
                if let Some(p1) = self.find_exact(io, far, len)? {
                    self.set_range(io, p1, len, true)?;
                    self.cursor = if p0 + len >= self.total - SB_RESERVED_BACK {
                        SB_RESERVED_FRONT
                    } else {
                        p0 + len
                    };
                    return Ok((p0, p1, len));
                }
                // Never written, so it can be handed back at once.
                self.set_range(io, p0, len, false)?;
            }
            if !shrink || len == 1 {
                return Err(Error::NoSpace);
            }
            len /= 2;
        }
    }

    /// Two single blocks for a chunk-table node's copies.
    fn alloc_pair<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<(u64, u64)> {
        self.allocated_pairs += 1;
        let (p0, p1, _) = self.alloc_chunk_runs(io, 1, false)?;
        Ok((p0, p1))
    }

    /// The block half a device away from `b` (wrapping inside the usable area).
    fn far_from(&self, b: u64) -> u64 {
        let usable = self.total - SB_RESERVED_FRONT - SB_RESERVED_BACK;
        SB_RESERVED_FRONT + (b - SB_RESERVED_FRONT + usable / 2) % usable
    }

    /// The first run of exactly `len` free, unquarantined blocks at or
    /// after `from` (wrapping around once). Does not mark it.
    fn find_exact<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        from: u64,
        len: u64,
    ) -> Result<Option<u64>> {
        if self.allocatable_blocks() < len {
            return Ok(None);
        }
        for (lo, hi) in [(from, self.total), (SB_RESERVED_FRONT, from)] {
            let mut b = lo;
            while b < hi {
                let Some(i) = self.next_free_bmp(io, b / BITS_PER_BMP)? else {
                    break;
                };
                let end = ((i + 1) * BITS_PER_BMP).min(hi);
                let mut x = b.max(i * BITS_PER_BMP);
                while x < end {
                    match self.find_in(io, i, x, len)? {
                        Some((s, l)) if s < end => {
                            if l == len && s + len <= hi {
                                return Ok(Some(s));
                            }
                            x = s + l;
                        }
                        _ => break,
                    }
                }
                b = (i + 1) * BITS_PER_BMP;
            }
        }
        Ok(None)
    }

    /// Releases chunks that are empty and have no quarantined slot (so no
    /// commit in the window still reads them through this mapping). Their
    /// physical runs go through the normal quarantine.
    fn release_empty_chunks<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<()> {
        for id in core::mem::take(&mut self.maybe_empty) {
            let e = self.chunks.entry(io, id)?;
            if !e.mapped() || e.used != 0 {
                continue;
            }
            let first = self.chunks.join(id, 0);
            let held = core::iter::once(&self.freed_now_l)
                .chain(self.held.iter().map(|h| &h.lset))
                .any(|set| set.overlaps(first, e.len as u64));
            if held || id == self.meta_cursor {
                // Look again once its quarantine ends (see `committed`).
                if id != self.meta_cursor {
                    self.maybe_empty.insert(id);
                }
                continue;
            }
            self.chunks.set_entry(io, id, Entry::default())?;
            self.free(io, e.p0, e.len as u64)?;
            self.free(io, e.p1, e.len as u64)?;
        }
        Ok(())
    }

    /// Moves chunk `id` to fresh physical runs: its used blocks are copied
    /// byte for byte (sealed bytes don't depend on where they sit), the
    /// entry is changed, and the old runs are freed into quarantine, so
    /// commits that still map the chunk the old way read it intact.
    /// Must run before anything else is queued in the transaction.
    pub fn relocate_chunk<D: BlockDevice>(&mut self, io: &mut Io<D>, id: u64) -> Result<()> {
        let e = self.chunks.entry(io, id)?;
        if !e.mapped() {
            return Err(Error::Invalid("chunk is not mapped"));
        }
        let len = e.len as u64;
        // Read everything first, so a failed read changes nothing.
        let mut copies = Vec::new();
        for off in (0..len).filter(|&o| e.is_used(o)) {
            for p in [e.p0, e.p1] {
                copies.push((off, p == e.p0, io.read_copy(p + off)?));
            }
        }
        let (q0, q1, _) = self.alloc_chunk_runs(io, len, false)?;
        self.moved_live_run |= e.used != 0;
        for (off, first, b) in copies {
            io.queue(if first { q0 } else { q1 } + off, b);
        }
        // Re-read: allocating may have rewritten a bitmap and freed its old
        // logical block, which can be in this very chunk.
        let cur = self.chunks.entry(io, id)?;
        debug_assert_eq!((cur.p0, cur.p1, cur.len), (e.p0, e.p1, e.len));
        self.chunks.set_entry(
            io,
            id,
            Entry {
                p0: q0,
                p1: q1,
                ..cur
            },
        )?;
        self.free(io, e.p0, len)?;
        self.free(io, e.p1, len)
    }

    /// Repairs every bad copy recorded so far by moving it: a chunk's bad
    /// run is rebuilt at a fresh place from its good blocks and the
    /// recorded good bytes, the entry is changed, and the old run goes into
    /// quarantine (older commits in the window still map it, and their
    /// other copy is intact); a chunk-table node with a bad copy is simply
    /// rewritten by this commit. A live block is never overwritten in place,
    /// so a torn or misdirected repair can't damage anything a commit
    /// references. Must run before anything else is queued in the commit.
    pub fn repair_copies<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<()> {
        let mut runs: BTreeMap<(u64, u8), BTreeMap<u64, Block>> = BTreeMap::new();
        let mut nodes = BTreeMap::new();
        for (phys, r) in io.take_repairs() {
            match r.owner {
                CopyOwner::Chunk(id) => {
                    runs.entry((id, r.copy)).or_default().insert(phys, r.good);
                }
                CopyOwner::Table(level, idx) => *nodes.entry((level, idx)).or_insert(0u64) += 1,
            }
        }
        for ((id, copy), good) in runs {
            self.repair_run(io, id, copy, good)?;
        }
        for ((level, idx), n) in nodes {
            if self.chunks.redirty(io, level, idx)? {
                io.stats.copies_repaired += n;
            } else {
                io.stats.copies_dropped += n;
            }
        }
        Ok(())
    }

    fn repair_run<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        id: u64,
        copy: u8,
        mut good: BTreeMap<u64, Block>,
    ) -> Result<()> {
        let e = if id < self.chunks.capacity() {
            self.chunks.entry(io, id)?
        } else {
            Entry::default()
        };
        if !e.mapped() {
            // Released since: nothing this commit references is in it.
            io.stats.copies_dropped += good.len() as u64;
            return Ok(());
        }
        let (run, other) = if copy == 0 {
            (e.p0, e.p1)
        } else {
            (e.p1, e.p0)
        };
        let len = e.len as u64;
        let mut fixed = 0;
        let mut blocks = Vec::new();
        for off in (0..len).filter(|&o| e.is_used(o)) {
            let b = match good.remove(&(run + off)) {
                Some(b) => {
                    fixed += 1;
                    b
                }
                None => io.read_copy(run + off)?,
            };
            blocks.push((off, b));
        }
        // Bad copies of blocks this commit no longer references (freed
        // since they were read, for instance a bitmap the repair of an
        // earlier run rewrote) need no repair: older commits still map them
        // through their own tables, with the other copy good.
        io.stats.copies_dropped += good.len() as u64;
        if fixed == 0 {
            // The bad blocks are no longer in use (or the chunk moved).
            return Ok(());
        }
        let far = self.far_from(other);
        let Some(q) = self.find_exact(io, far, len)? else {
            return Err(Error::NoSpace);
        };
        self.set_range(io, q, len, true)?;
        for (off, b) in blocks {
            io.queue(q + off, b);
        }
        // Re-read: marking the run may have rewritten a bitmap, freeing
        // its old logical block, which can be in this very chunk.
        let cur = self.chunks.entry(io, id)?;
        debug_assert_eq!((cur.p0, cur.p1, cur.len), (e.p0, e.p1, e.len));
        let moved = if copy == 0 {
            Entry { p0: q, ..cur }
        } else {
            Entry { p1: q, ..cur }
        };
        self.chunks.set_entry(io, id, moved)?;
        self.moved_live_run = true;
        self.free(io, run, len)?;
        io.stats.copies_repaired += fixed;
        Ok(())
    }

    /// End of the quarantined range starting exactly at `b`, if any.
    fn quarantined_until(&self, b: u64) -> Option<u64> {
        let mut end: Option<u64> = None;
        for set in core::iter::once(&self.freed_now).chain(self.held.iter().map(|h| &h.set)) {
            if let Some((s, e)) = set.next_range_from(b)
                && s == b
            {
                end = Some(end.map_or(e, |x| x.max(e)));
            }
        }
        end
    }

    /// Next bitmap index >= `from` whose subtree has free blocks.
    fn next_free_bmp<D: BlockDevice>(&mut self, io: &mut Io<D>, from: u64) -> Result<Option<u64>> {
        self.search(io, self.levels, 0, from)
    }

    fn search<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        level: u32,
        idx: u64,
        from: u64,
    ) -> Result<Option<u64>> {
        // Children of (level, idx) cover bitmaps [c << 6(level-1), (c+1) << 6(level-1)).
        let shift = 6 * (level - 1);
        let first_child = idx * FANOUT;
        let start = (from >> shift).max(first_child);
        for c in start..first_child + FANOUT {
            let k = self.table(io, level, idx)?.kids[(c - first_child) as usize];
            if k.aux == 0 {
                continue;
            }
            if level == 1 {
                return Ok(Some(c));
            }
            if let Some(r) = self.search(io, level - 1, c, from.max(c << shift))? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Allocates a run of up to `want` contiguous blocks (at least 1),
    /// next-fit from the allocation cursor.
    pub fn alloc_run<D: BlockDevice>(&mut self, io: &mut Io<D>, want: u64) -> Result<(u64, u64)> {
        debug_assert!(want > 0);
        if self.allocatable_blocks() == 0 {
            return Err(Error::NoSpace);
        }
        for pass in 0..2 {
            let mut from_block = if pass == 0 { self.cursor } else { 0 };
            while let Some(i) = self.next_free_bmp(io, from_block / BITS_PER_BMP)? {
                if let Some((s, l)) = self.find_in(io, i, from_block, want)? {
                    self.set_range(io, s, l, true)?;
                    self.cursor = if s + l >= self.total {
                        SB_RESERVED_FRONT
                    } else {
                        s + l
                    };
                    return Ok((s, l));
                }
                from_block = (i + 1) * BITS_PER_BMP;
                if from_block >= self.total {
                    break;
                }
            }
        }
        Err(Error::NoSpace)
    }

    fn find_in<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        i: u64,
        from: u64,
        want: u64,
    ) -> Result<Option<(u64, u64)>> {
        let first = i * BITS_PER_BMP;
        let end = (first + BITS_PER_BMP).min(self.total);
        self.load(io, i)?;
        let mut b = from.max(first);
        while b < end {
            let words = &self.bmps[&i].words;
            let bit = b - first;
            let w = words[(bit / 64) as usize];
            if bit.is_multiple_of(64) && w == u64::MAX {
                b += 64;
                continue;
            }
            if w & (1 << (bit % 64)) != 0 {
                b += 1;
                continue;
            }
            if let Some(qe) = self.quarantined_until(b) {
                b = qe;
                continue;
            }
            let mut e = b + 1;
            while e < end && e - b < want {
                let bit = e - first;
                if words[(bit / 64) as usize] & (1 << (bit % 64)) != 0
                    || self.quarantined_until(e).is_some()
                {
                    break;
                }
                e += 1;
            }
            return Ok(Some((b, e - b)));
        }
        Ok(None)
    }

    /// Writes the space map, the chunk table and the free log for commit
    /// `generation` into the write batch. Returns the new (space root, free
    /// log head, chunk table root) pointers.
    ///
    /// Allocating new homes for these blocks changes the map and the chunk
    /// table, so this loops until nothing new needs a home. It converges
    /// because each block gets a home at most once per commit.
    pub fn write<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        generation: u64,
        now_ns: i64,
    ) -> Result<(BlockPtr, BlockPtr, BlockPtr)> {
        self.release_empty_chunks(io)?;
        for a in core::mem::take(&mut self.log_addrs) {
            self.free(io, a, 1)?;
        }
        let mut bmp_addr: BTreeMap<u64, u64> = BTreeMap::new();
        let mut table_addr: BTreeMap<(u32, u64), u64> = BTreeMap::new();
        let mut chunk_homes: BTreeMap<(u32, u64), (u64, u64)> = BTreeMap::new();
        let mut log_blocks: Vec<u64> = Vec::new();
        loop {
            let mut changed = false;
            for a in core::mem::take(&mut self.pending_table_frees) {
                self.free(io, a, 1)?;
                changed = true;
            }
            for (a, b) in core::mem::take(&mut self.chunks.pending_frees) {
                self.free(io, a, 1)?;
                self.free(io, b, 1)?;
                changed = true;
            }
            let entries = self.freed_now.range_count()
                + self.freed_now_l.range_count()
                + self
                    .held
                    .iter()
                    .map(|h| h.set.range_count() + h.lset.range_count())
                    .sum::<usize>();
            while log_blocks.len() < entries.div_ceil(LOG_PER_BLOCK) {
                log_blocks.push(self.alloc_meta(io, 1)?[0].0);
                changed = true;
            }
            let dirty: Vec<u64> = self
                .bmps
                .iter()
                .filter(|(i, b)| b.dirty && !bmp_addr.contains_key(i))
                .map(|(&i, _)| i)
                .collect();
            for i in dirty {
                bmp_addr.insert(i, u64::MAX);
                let a = self.alloc_meta(io, 1)?[0].0;
                bmp_addr.insert(i, a);
                changed = true;
            }
            let dirty: Vec<(u32, u64)> = self
                .tables
                .iter()
                .filter(|(k, t)| t.dirty && !table_addr.contains_key(k))
                .map(|(&k, _)| k)
                .collect();
            for k in dirty {
                table_addr.insert(k, u64::MAX);
                let a = self.alloc_meta(io, 1)?[0].0;
                table_addr.insert(k, a);
                changed = true;
            }
            for k in self.chunks.dirty_nodes() {
                if let alloc::collections::btree_map::Entry::Vacant(v) = chunk_homes.entry(k) {
                    v.insert(self.alloc_pair(io)?);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Everything has a home and no bit changes any more: serialize,
        // bitmaps first, then tables bottom-up so each parent sees final pointers.
        for (&i, &a) in &bmp_addr {
            let mut b = zero_block();
            for (c, w) in b
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(self.bmps[&i].words.iter())
            {
                *c = w.to_le_bytes();
            }
            let free = self.bmp_ptr(io, i)?.aux;
            let p = self.write_meta(io, BlockKind::Bitmap, a, generation, b, free)?;
            self.table(io, 1, i / FANOUT)?.kids[(i % FANOUT) as usize] = p;
            self.bmps.get_mut(&i).unwrap().dirty = false;
        }
        let mut keys: Vec<(u32, u64)> = table_addr.keys().copied().collect();
        keys.sort();
        for (level, idx) in keys {
            let a = table_addr[&(level, idx)];
            let t = &self.tables[&(level, idx)];
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[..]);
            let mut sum = 0;
            for k in &t.kids {
                debug_assert!(
                    k.addr != 0
                        || *k
                            == BlockPtr {
                                aux: k.aux,
                                ..BlockPtr::NULL
                            }
                );
                sum += k.aux;
                k.encode(&mut w);
            }
            if level == self.levels {
                let levels = self.levels as u64;
                self.root_ptr =
                    self.write_meta(io, BlockKind::SpaceTable, a, generation, b, levels)?;
                debug_assert_eq!(sum, self.free_total);
            } else {
                let p = self.write_meta(io, BlockKind::SpaceTable, a, generation, b, sum)?;
                self.tables
                    .get_mut(&(level + 1, idx / FANOUT))
                    .unwrap()
                    .kids[(idx % FANOUT) as usize] = p;
            }
            self.tables.get_mut(&(level, idx)).unwrap().dirty = false;
        }

        // Free log, written back to front so each block can point at the next.
        let mut entries: Vec<(u64, u64, u64, i64)> = Vec::new();
        for h in &self.held {
            for set in [&h.set, &h.lset] {
                entries.extend(set.ranges().map(|(s, l)| (s, l, h.generation, h.time_ns)));
            }
        }
        for set in [&self.freed_now, &self.freed_now_l] {
            entries.extend(set.ranges().map(|(s, l)| (s, l, generation, now_ns)));
        }
        let mut next = BlockPtr::NULL;
        for (k, &a) in log_blocks.iter().enumerate().rev() {
            let chunk = entries.chunks(LOG_PER_BLOCK).nth(k).unwrap_or(&[]);
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[..]);
            w.u32(LOG_MAGIC);
            w.u32(chunk.len() as u32);
            next.encode(&mut w);
            for &(s, l, g, t) in chunk {
                w.u64(s);
                w.u64(l);
                w.u64(g);
                w.i64(t);
            }
            next = self.write_meta(io, BlockKind::FreeLog, a, generation, b, 0)?;
        }
        self.log_addrs = log_blocks;
        // The chunk table last: everything above resolved through it, and
        // nothing after this changes an entry.
        let chunk_root = self.chunks.write(io, generation, &chunk_homes)?;
        debug_assert!(self.chunks.dirty_nodes().is_empty());
        Ok((self.root_ptr, next, chunk_root))
    }

    /// Called once commit `generation` is durable. This commit's frees join
    /// the quarantine; batches that are both at least
    /// [`QUARANTINE_COMMITS`] commits and `quarantine_min_ns` old leave it.
    pub fn committed(&mut self, generation: u64, now_ns: i64) {
        let set = core::mem::take(&mut self.freed_now);
        let lset = core::mem::take(&mut self.freed_now_l);
        self.held_blocks += set.len();
        if set.len() > 0 || lset.len() > 0 {
            self.held.push_back(FreedBatch {
                generation,
                time_ns: now_ns,
                set,
                lset,
            });
        }
        // A clock that went backwards proves nothing about elapsed time:
        // restart those batches' windows from now (bounded, never forever).
        for h in self.held.iter_mut() {
            if h.time_ns > now_ns {
                h.time_ns = now_ns;
            }
        }
        while let Some(h) = self.held.front() {
            let old_enough = generation.saturating_sub(h.generation) >= QUARANTINE_COMMITS;
            let long_enough = now_ns - h.time_ns >= self.quarantine_min_ns;
            if old_enough && long_enough {
                self.held_blocks -= h.set.len();
                // Chunks whose last slots leave quarantine may now be released.
                for (l, n) in h.lset.ranges() {
                    if let (Ok((a, _)), Ok((b, _))) =
                        (self.chunks.split(l), self.chunks.split(l + n - 1))
                    {
                        self.maybe_empty.extend(a..=b);
                    }
                }
                self.held.pop_front();
            } else {
                break;
            }
        }
        self.evict_clean();
    }

    pub(crate) fn evict_clean(&mut self) {
        self.chunks.evict_clean();
        if self.bmps.len() > CLEAN_CACHE {
            let clean: Vec<u64> = self
                .bmps
                .iter()
                .filter(|(_, b)| !b.dirty)
                .map(|(&i, _)| i)
                .collect();
            for i in clean.into_iter().take(self.bmps.len() - CLEAN_CACHE / 2) {
                self.bmps.remove(&i);
            }
        }
        if self.tables.len() > CLEAN_CACHE {
            let top = self.levels;
            let clean: Vec<(u32, u64)> = self
                .tables
                .iter()
                .filter(|(k, t)| !t.dirty && k.0 != top)
                .map(|(&k, _)| k)
                .collect();
            let n = self.tables.len() - CLEAN_CACHE / 2;
            for k in clean.into_iter().take(n) {
                self.tables.remove(&k);
            }
        }
    }

    pub(crate) fn levels(&self) -> u32 {
        self.levels
    }

    /// (loaded tables, loaded bitmaps, the root table's free counts).
    #[cfg(test)]
    pub(crate) fn shape(&self) -> (usize, usize, Vec<u64>) {
        let root = &self.tables[&(self.levels, 0)];
        (
            self.tables.len(),
            self.bmps.len(),
            root.kids.iter().map(|k| k.aux).collect(),
        )
    }

    /// Every on-disk table and bitmap pointer (for fsck). Loads them all.
    pub fn all_pointers<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
    ) -> Result<Vec<(BlockKind, BlockPtr)>> {
        let mut out = vec![(BlockKind::SpaceTable, self.root_ptr)];
        let mut stack = vec![(self.levels, 0u64)];
        while let Some((level, idx)) = stack.pop() {
            let kids = self.table(io, level, idx)?.kids.clone();
            for (j, k) in kids.iter().enumerate() {
                let c = idx * FANOUT + j as u64;
                if k.is_null() {
                    continue;
                }
                if level == 1 {
                    out.push((BlockKind::Bitmap, *k));
                } else {
                    out.push((BlockKind::SpaceTable, *k));
                    stack.push((level - 1, c));
                }
            }
        }
        Ok(out)
    }

    /// Loads (verifying) every table and bitmap born at or after
    /// `min_birth`, skipping older subtrees. Returns how many it read.
    pub fn verify_since<D: BlockDevice>(&mut self, io: &mut Io<D>, min_birth: u64) -> Result<u64> {
        let mut n = 1;
        let mut stack = vec![(self.levels, 0u64)];
        while let Some((level, idx)) = stack.pop() {
            let kids = self.table(io, level, idx)?.kids.clone();
            for (j, k) in kids.iter().enumerate() {
                if k.is_null() || k.birth < min_birth {
                    continue;
                }
                let c = idx * FANOUT + j as u64;
                n += 1;
                if level == 1 {
                    self.load(io, c)?;
                } else {
                    stack.push((level - 1, c));
                }
            }
        }
        self.evict_clean();
        Ok(n)
    }

    /// Indices of every bitmap that has been written (non-null), found by
    /// walking only written tables, so a sparse huge map is cheap.
    pub fn written_bitmaps<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut stack = vec![(self.levels, 0u64)];
        while let Some((level, idx)) = stack.pop() {
            let kids = self.table(io, level, idx)?.kids.clone();
            for (j, k) in kids.iter().enumerate().rev() {
                if k.is_null() {
                    continue;
                }
                let c = idx * FANOUT + j as u64;
                if level == 1 {
                    out.push(c);
                } else {
                    stack.push((level - 1, c));
                }
            }
        }
        Ok(out)
    }

    /// Moves the next-fit allocation cursor (tests reach far regions).
    pub fn set_cursor(&mut self, b: u64) {
        self.cursor = b.clamp(SB_RESERVED_FRONT, self.total - 1);
    }

    /// The oldest generation whose frees are still quarantined, if any.
    pub fn oldest_held(&self) -> Option<u64> {
        self.held.front().map(|h| h.generation)
    }

    pub fn log_addrs(&self) -> &[u64] {
        &self.log_addrs
    }

    /// All quarantined blocks from earlier commits (for fsck).
    pub fn held_ranges(&self) -> Vec<(u64, u64)> {
        self.held.iter().flat_map(|h| h.set.to_vec()).collect()
    }

    /// The pointers of every block of the free log starting at `head`.
    pub fn log_ptrs<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        head: BlockPtr,
    ) -> Result<Vec<BlockPtr>> {
        let mut out = Vec::new();
        let mut p = head;
        while !p.is_null() {
            if out.len() > 1 << 24 {
                return Err(Error::Corrupt("free log loops", p.addr));
            }
            out.push(p);
            let b = self.read_meta(io, BlockKind::FreeLog, &p)?;
            let mut r = Reader::new(&b[..], "free log", p.addr);
            r.skip(8)?;
            p = BlockPtr::decode(&mut r)?;
        }
        Ok(out)
    }

    /// All quarantined logical blocks from earlier commits (for fsck).
    pub fn held_lranges(&self) -> Vec<(u64, u64)> {
        self.held.iter().flat_map(|h| h.lset.to_vec()).collect()
    }
}
