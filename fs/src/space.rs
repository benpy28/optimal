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

use crate::codec::{Reader, Writer};
use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Io, zero_block};
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
    pub set: RangeSet,
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
    /// Free-log blocks of the last commit.
    log_addrs: Vec<u64>,
    /// Old copies of space tables that became garbage this transaction.
    pending_table_frees: Vec<u64>,
    cursor: u64,
    pub quarantine_min_ns: i64,
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

    fn empty(total: u64, levels: u32, root_ptr: BlockPtr) -> Space {
        Space {
            total,
            levels,
            root_ptr,
            tables: BTreeMap::new(),
            bmps: BTreeMap::new(),
            free_total: 0,
            freed_now: RangeSet::new(),
            held: VecDeque::new(),
            held_blocks: 0,
            log_addrs: Vec::new(),
            pending_table_frees: Vec::new(),
            cursor: SB_RESERVED_FRONT,
            quarantine_min_ns: 30_000_000_000,
        }
    }

    /// A fresh map for mkfs: everything free except the superblock slots.
    /// Only the paths to the first and last bitmap are materialised.
    pub fn format(total: u64) -> Result<Space> {
        let levels = Self::levels_for(total)?;
        let mut s = Self::empty(total, levels, BlockPtr::NULL);
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

    /// Opens the committed map. Tables and bitmaps load on demand.
    pub fn open<D: BlockDevice>(
        io: &mut Io<D>,
        total: u64,
        root: BlockPtr,
        log: BlockPtr,
    ) -> Result<Space> {
        let levels = Self::levels_for(total)?;
        if root.aux != levels as u64 || root.is_null() {
            return Err(Error::Corrupt("space map depth", root.addr));
        }
        let mut s = Self::empty(total, levels, root);
        let rb = io.read_sealed(BlockKind::SpaceTable, &root)?;
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
            let b = io.read_sealed(BlockKind::FreeLog, &p)?;
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
                let bad = len == 0 || start.checked_add(len).is_none_or(|e| e > total);
                let batch = match s.held.iter_mut().find(|h| h.generation == generation) {
                    Some(h) => h,
                    None => {
                        s.held.push_back(FreedBatch {
                            generation,
                            time_ns,
                            set: RangeSet::new(),
                        });
                        s.held.back_mut().unwrap()
                    }
                };
                if bad || batch.time_ns != time_ns || !batch.set.insert(start, len) {
                    return Err(Error::Corrupt("bad free log entry", p.addr));
                }
                s.held_blocks += len;
            }
            s.log_addrs.push(p.addr);
            p = next;
        }
        s.held.make_contiguous().sort_by_key(|h| h.generation);
        Ok(s)
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

    /// Free blocks that can actually be allocated right now.
    pub fn allocatable_blocks(&self) -> u64 {
        self.free_total
            .saturating_sub(self.freed_now.len() + self.held_blocks)
    }

    pub fn quarantined_blocks(&self) -> u64 {
        self.freed_now.len() + self.held_blocks
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
                let b = io.read_sealed(BlockKind::SpaceTable, &p)?;
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
                let b = io.read_sealed(BlockKind::Bitmap, &p)?;
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
        if let Some(a) = old_bmp {
            if a != 0 {
                self.free(io, a, 1)?;
            }
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
    pub fn free<D: BlockDevice>(&mut self, io: &mut Io<D>, start: u64, len: u64) -> Result<()> {
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

    /// End of the quarantined range starting exactly at `b`, if any.
    fn quarantined_until(&self, b: u64) -> Option<u64> {
        let mut end: Option<u64> = None;
        for set in core::iter::once(&self.freed_now).chain(self.held.iter().map(|h| &h.set)) {
            if let Some((s, e)) = set.next_range_from(b) {
                if s == b {
                    end = Some(end.map_or(e, |x| x.max(e)));
                }
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
            if bit % 64 == 0 && w == u64::MAX {
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

    /// Allocates exactly `n` blocks, possibly as several runs.
    pub fn alloc_many<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        n: u64,
    ) -> Result<Vec<(u64, u64)>> {
        let mut out = Vec::new();
        let mut left = n;
        while left > 0 {
            let (s, l) = self.alloc_run(io, left)?;
            out.push((s, l));
            left -= l;
        }
        Ok(out)
    }

    /// Writes the space map and free log for commit `generation` into the
    /// write batch. Returns the new (space root, free log head) pointers.
    ///
    /// Allocating new homes for the map's own blocks changes the map, so this
    /// loops until nothing new needs a home. It converges because each round
    /// only relocates blocks that were clean in the previous round.
    pub fn write<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        generation: u64,
        now_ns: i64,
    ) -> Result<(BlockPtr, BlockPtr)> {
        for a in core::mem::take(&mut self.log_addrs) {
            self.free(io, a, 1)?;
        }
        let mut bmp_addr: BTreeMap<u64, u64> = BTreeMap::new();
        let mut table_addr: BTreeMap<(u32, u64), u64> = BTreeMap::new();
        let mut log_blocks: Vec<u64> = Vec::new();
        loop {
            let mut changed = false;
            for a in core::mem::take(&mut self.pending_table_frees) {
                self.free(io, a, 1)?;
                changed = true;
            }
            let entries = self.freed_now.range_count()
                + self.held.iter().map(|h| h.set.range_count()).sum::<usize>();
            while log_blocks.len() < entries.div_ceil(LOG_PER_BLOCK) {
                log_blocks.push(self.alloc_run(io, 1)?.0);
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
                let a = self.alloc_run(io, 1)?.0;
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
                let a = self.alloc_run(io, 1)?.0;
                table_addr.insert(k, a);
                changed = true;
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
            let p = io.write_sealed(BlockKind::Bitmap, a, generation, b, free);
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
                self.root_ptr =
                    io.write_sealed(BlockKind::SpaceTable, a, generation, b, self.levels as u64);
                debug_assert_eq!(sum, self.free_total);
            } else {
                let p = io.write_sealed(BlockKind::SpaceTable, a, generation, b, sum);
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
            entries.extend(h.set.ranges().map(|(s, l)| (s, l, h.generation, h.time_ns)));
        }
        entries.extend(
            self.freed_now
                .ranges()
                .map(|(s, l)| (s, l, generation, now_ns)),
        );
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
            next = io.write_sealed(BlockKind::FreeLog, a, generation, b, 0);
        }
        self.log_addrs = log_blocks;
        Ok((self.root_ptr, next))
    }

    /// Called once commit `generation` is durable. This commit's frees join
    /// the quarantine; batches that are both at least
    /// [`QUARANTINE_COMMITS`] commits and `quarantine_min_ns` old leave it.
    pub fn committed(&mut self, generation: u64, now_ns: i64) {
        let set = core::mem::take(&mut self.freed_now);
        self.held_blocks += set.len();
        if set.len() > 0 {
            self.held.push_back(FreedBatch {
                generation,
                time_ns: now_ns,
                set,
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
                self.held.pop_front();
            } else {
                break;
            }
        }
        self.evict_clean();
    }

    pub(crate) fn evict_clean(&mut self) {
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

    #[cfg(test)]
    pub(crate) fn levels(&self) -> u32 {
        self.levels
    }

    /// (loaded tables, loaded bitmaps, the root table's free counts).
    #[cfg(test)]
    pub(crate) fn shape(&self) -> (usize, usize, Vec<u64>) {
        let root = &self.tables[&(self.levels, 0)];
        (self.tables.len(), self.bmps.len(), root.kids.iter().map(|k| k.aux).collect())
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

    pub fn log_addrs(&self) -> &[u64] {
        &self.log_addrs
    }

    /// All quarantined blocks from earlier commits (for fsck).
    pub fn held_ranges(&self) -> Vec<(u64, u64)> {
        self.held.iter().flat_map(|h| h.set.to_vec()).collect()
    }

    /// Next used/free transition search for fsck: whether any block in
    /// `[start, start+len)` is used, without loading untouched regions.
    pub fn bitmap_is_null<D: BlockDevice>(&mut self, io: &mut Io<D>, i: u64) -> Result<bool> {
        Ok(!self.bmps.contains_key(&i) && self.bmp_ptr(io, i)?.is_null())
    }
}

#[allow(dead_code)]
fn _assert_sets(_: BTreeSet<u64>) {}
