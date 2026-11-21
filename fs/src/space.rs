//! The space map: which blocks are in use.
//!
//! On disk it is a fixed three-level radix tree, all of it copy-on-write:
//! a root table of 64 pointers to level-1 tables, each holding 64 pointers to
//! bitmap blocks, each bitmap block covering 32768 blocks (128 MiB). That
//! reaches 2^27 blocks (512 TiB). Each bitmap pointer's `aux` field holds the
//! block's free count, so allocation can skip full regions without reading
//! them, and a mismatch between the count and the bits is detected as
//! corruption.
//!
//! The bitmap committed with generation T is exactly the set of blocks
//! referenced by T (fsck checks this). Blocks freed while building T+1 are
//! cleared from the bitmap at once but *quarantined*: they are not handed out
//! again until T+2 begins, so both superblock copies that may be needed after
//! a power cut (T and T-1) keep every block they reference intact. The blocks
//! freed by the latest commit are recorded in the free log, so the quarantine
//! survives a remount.

use crate::codec::{Reader, Writer};
use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Io, zero_block};
use crate::ptr::{BlockPtr, PTR_LEN};
use crate::rangeset::RangeSet;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

pub const BITS_PER_BMP: u64 = (BLOCK_SIZE * 8) as u64;
pub const TABLE_FANOUT: usize = BLOCK_SIZE / PTR_LEN;
pub const MAX_BLOCKS: u64 = BITS_PER_BMP * (TABLE_FANOUT * TABLE_FANOUT) as u64;
const WORDS: usize = BLOCK_SIZE / 8;

const LOG_MAGIC: u32 = 0x474f_4c46; // "FLOG"
const LOG_HEADER: usize = 8 + PTR_LEN;
pub const LOG_PER_BLOCK: usize = (BLOCK_SIZE - LOG_HEADER) / 16;

/// Number of blocks reserved for superblock copies at each end of the device.
pub const SB_RESERVED_FRONT: u64 = 2;
pub const SB_RESERVED_BACK: u64 = 2;

#[derive(Debug)]
struct Bmp {
    words: Box<[u64; WORDS]>,
    dirty: bool,
}

#[derive(Debug)]
pub struct Space {
    total: u64,
    nbmp: usize,
    nl1: usize,
    root: BlockPtr,
    l1: Vec<BlockPtr>,
    bmp: Vec<BlockPtr>,
    free_count: Vec<u32>,
    loaded: BTreeMap<usize, Bmp>,
    /// Indices of bitmap blocks / L1 tables changed since the last commit
    /// that still need a new address. Their old address was freed already.
    l1_dirty: BTreeSet<usize>,
    root_dirty: bool,
    /// Blocks freed in the transaction being built.
    pub freed_now: RangeSet,
    /// Blocks freed by the last commit (still referenced by the previous superblock).
    pub freed_prev: RangeSet,
    /// Free-log blocks of the last commit.
    log_addrs: Vec<u64>,
    /// Old copies of space tables that became garbage this transaction.
    pending_table_frees: Vec<u64>,
    cursor: u64,
}

fn bmp_index(b: u64) -> (usize, usize, u64) {
    let i = (b / BITS_PER_BMP) as usize;
    let bit = b % BITS_PER_BMP;
    (i, (bit / 64) as usize, bit % 64)
}

impl Space {
    pub fn total_blocks(&self) -> u64 {
        self.total
    }

    fn shape(total: u64) -> Result<(usize, usize)> {
        if total > MAX_BLOCKS {
            return Err(Error::Unsupported("device larger than 512 TiB"));
        }
        let nbmp = total.div_ceil(BITS_PER_BMP) as usize;
        let nl1 = nbmp.div_ceil(TABLE_FANOUT);
        Ok((nbmp, nl1))
    }

    /// A fresh map for mkfs: everything free except the superblock slots and
    /// the padding bits past the end of the device.
    pub fn format(total: u64) -> Result<Space> {
        let (nbmp, nl1) = Self::shape(total)?;
        let mut s = Space {
            total,
            nbmp,
            nl1,
            root: BlockPtr::NULL,
            l1: vec![BlockPtr::NULL; nl1],
            bmp: vec![BlockPtr::NULL; nbmp],
            free_count: vec![0; nbmp],
            loaded: BTreeMap::new(),
            l1_dirty: (0..nl1).collect(),
            root_dirty: true,
            freed_now: RangeSet::new(),
            freed_prev: RangeSet::new(),
            log_addrs: Vec::new(),
            pending_table_frees: Vec::new(),
            cursor: SB_RESERVED_FRONT,
        };
        for i in 0..nbmp {
            let mut words = Box::new([0u64; WORDS]);
            let first = i as u64 * BITS_PER_BMP;
            let mut free = 0u32;
            for bit in 0..BITS_PER_BMP {
                let b = first + bit;
                let used = b >= total || b < SB_RESERVED_FRONT || b >= total - SB_RESERVED_BACK;
                if used {
                    words[(bit / 64) as usize] |= 1 << (bit % 64);
                } else {
                    free += 1;
                }
            }
            s.free_count[i] = free;
            s.loaded.insert(i, Bmp { words, dirty: true });
        }
        Ok(s)
    }

    /// Loads the map's tables (not the bitmaps, which load on demand) and the free log.
    pub fn open<D: BlockDevice>(
        io: &mut Io<D>,
        total: u64,
        root: BlockPtr,
        log: BlockPtr,
    ) -> Result<Space> {
        let (nbmp, nl1) = Self::shape(total)?;
        let rb = io.read_sealed(BlockKind::SpaceTable, &root)?;
        let mut r = Reader::new(&rb[..], "space root table", root.addr);
        let mut l1 = Vec::with_capacity(nl1);
        for i in 0..TABLE_FANOUT {
            let p = BlockPtr::decode(&mut r)?;
            if i < nl1 {
                if p.is_null() {
                    return Err(Error::Corrupt("missing space table", root.addr));
                }
                l1.push(p);
            } else if !p.is_null() {
                return Err(Error::Corrupt("space table past end of device", root.addr));
            }
        }
        let mut bmp = Vec::with_capacity(nbmp);
        let mut free_count = Vec::with_capacity(nbmp);
        for (t, p) in l1.iter().enumerate() {
            let tb = io.read_sealed(BlockKind::SpaceTable, p)?;
            let mut r = Reader::new(&tb[..], "space table", p.addr);
            let mut sum = 0u64;
            for j in 0..TABLE_FANOUT {
                let bp = BlockPtr::decode(&mut r)?;
                let idx = t * TABLE_FANOUT + j;
                if idx < nbmp {
                    if bp.is_null() || bp.aux as u64 > BITS_PER_BMP {
                        return Err(Error::Corrupt("bad bitmap pointer", p.addr));
                    }
                    sum += bp.aux as u64;
                    free_count.push(bp.aux);
                    bmp.push(bp);
                } else if !bp.is_null() {
                    return Err(Error::Corrupt("bitmap past end of device", p.addr));
                }
            }
            if sum != p.aux as u64 {
                return Err(Error::Corrupt("space table free count mismatch", p.addr));
            }
        }
        let mut s = Space {
            total,
            nbmp,
            nl1,
            root,
            l1,
            bmp,
            free_count,
            loaded: BTreeMap::new(),
            l1_dirty: BTreeSet::new(),
            root_dirty: false,
            freed_now: RangeSet::new(),
            freed_prev: RangeSet::new(),
            log_addrs: Vec::new(),
            pending_table_frees: Vec::new(),
            cursor: SB_RESERVED_FRONT,
        };
        // The free log: blocks freed by the last commit stay quarantined.
        let mut p = log;
        let mut guard = 0u64;
        while !p.is_null() {
            guard += 1;
            if guard > total {
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
                if len == 0
                    || start.checked_add(len).is_none_or(|e| e > total)
                    || !s.freed_prev.insert(start, len)
                {
                    return Err(Error::Corrupt("bad free log entry", p.addr));
                }
            }
            s.log_addrs.push(p.addr);
            p = next;
        }
        Ok(s)
    }

    pub fn free_blocks(&self) -> u64 {
        self.free_count.iter().map(|&c| c as u64).sum()
    }

    /// Free blocks that can actually be allocated right now.
    pub fn allocatable_blocks(&self) -> u64 {
        self.free_blocks()
            .saturating_sub(self.freed_now.len() + self.freed_prev.len())
    }

    fn load<D: BlockDevice>(&mut self, io: &mut Io<D>, i: usize) -> Result<&mut Bmp> {
        if !self.loaded.contains_key(&i) {
            let p = self.bmp[i];
            let b = io.read_sealed(BlockKind::Bitmap, &p)?;
            let mut words = Box::new([0u64; WORDS]);
            for (w, c) in words.iter_mut().zip(b.as_chunks::<8>().0) {
                *w = u64::from_le_bytes(*c);
            }
            let ones: u64 = words.iter().map(|w| w.count_ones() as u64).sum();
            if BITS_PER_BMP - ones != p.aux as u64 || self.free_count[i] != p.aux {
                return Err(Error::Corrupt("bitmap free count mismatch", p.addr));
            }
            // Padding bits past the end of the device must read as used.
            let first = i as u64 * BITS_PER_BMP;
            for b in self.total.max(first)..first + BITS_PER_BMP {
                let bit = b - first;
                if words[(bit / 64) as usize] & (1 << (bit % 64)) == 0 {
                    return Err(Error::Corrupt(
                        "bitmap marks blocks past the device end free",
                        p.addr,
                    ));
                }
            }
            self.loaded.insert(
                i,
                Bmp {
                    words,
                    dirty: false,
                },
            );
        }
        Ok(self.loaded.get_mut(&i).unwrap())
    }

    /// Marks bitmap block `i` as changed. The first time in a transaction
    /// this frees its old copy (copy-on-write) and dirties its table.
    fn touch<D: BlockDevice>(&mut self, io: &mut Io<D>, i: usize) -> Result<()> {
        let bm = self.load(io, i)?;
        if bm.dirty {
            return Ok(());
        }
        bm.dirty = true;
        let old = self.bmp[i];
        self.bmp[i] = BlockPtr::NULL;
        self.touch_l1(i / TABLE_FANOUT);
        if !old.is_null() {
            self.free(io, old.addr, 1)?;
        }
        Ok(())
    }

    fn touch_l1(&mut self, t: usize) {
        if self.l1_dirty.insert(t) {
            let old = self.l1[t];
            self.l1[t] = BlockPtr::NULL;
            self.root_dirty = true;
            if !old.is_null() {
                self.pending_table_frees.push(old.addr);
            }
        }
    }

    /// Whether block `b` is marked used.
    pub fn is_used<D: BlockDevice>(&mut self, io: &mut Io<D>, b: u64) -> Result<bool> {
        let (i, w, bit) = bmp_index(b);
        Ok(self.load(io, i)?.words[w] & (1 << bit) != 0)
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
        let mut b = start;
        let end = start + len;
        while b < end {
            let (i, _, _) = bmp_index(b);
            let chunk_end = end.min((i as u64 + 1) * BITS_PER_BMP);
            self.touch(io, i)?;
            let bm = self.loaded.get_mut(&i).unwrap();
            let first = i as u64 * BITS_PER_BMP;
            for x in b..chunk_end {
                let bit = x - first;
                let (w, m) = ((bit / 64) as usize, 1u64 << (bit % 64));
                if bm.words[w] & m == 0 {
                    return Err(Error::Corrupt("double free", x));
                }
                bm.words[w] &= !m;
            }
            self.free_count[i] += (chunk_end - b) as u32;
            b = chunk_end;
        }
        let ok = self.freed_now.insert(start, len);
        debug_assert!(ok, "freed range already in freed_now");
        Ok(())
    }

    fn quarantined_until(&self, b: u64) -> Option<u64> {
        let mut end = None;
        for set in [&self.freed_now, &self.freed_prev] {
            if let Some((s, e)) = set.next_range_from(b)
                && s == b
            {
                end = Some(end.map_or(e, |x: u64| x.max(e)));
            }
        }
        end
    }

    /// Allocates a run of up to `want` contiguous blocks (at least 1), as
    /// long as possible, searching forward from the allocation cursor.
    pub fn alloc_run<D: BlockDevice>(&mut self, io: &mut Io<D>, want: u64) -> Result<(u64, u64)> {
        debug_assert!(want > 0);
        let start_bmp = (self.cursor / BITS_PER_BMP) as usize;
        for step in 0..=self.nbmp {
            let i = (start_bmp + step) % self.nbmp;
            if self.free_count[i] == 0 {
                continue;
            }
            let first = i as u64 * BITS_PER_BMP;
            let from = if step == 0 { self.cursor } else { first };
            if let Some((s, l)) = self.find_in(io, i, from, want)? {
                self.mark_used(io, s, l)?;
                self.cursor = s + l;
                if self.cursor >= self.total {
                    self.cursor = SB_RESERVED_FRONT;
                }
                return Ok((s, l));
            }
        }
        Err(Error::NoSpace)
    }

    fn find_in<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        i: usize,
        from: u64,
        want: u64,
    ) -> Result<Option<(u64, u64)>> {
        let first = i as u64 * BITS_PER_BMP;
        let end = (first + BITS_PER_BMP).min(self.total);
        self.load(io, i)?;
        let mut b = from.max(first);
        while b < end {
            let words = &self.loaded[&i].words;
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
            // Free, allocatable block at b: extend the run.
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

    fn mark_used<D: BlockDevice>(&mut self, io: &mut Io<D>, start: u64, len: u64) -> Result<()> {
        let mut b = start;
        let end = start + len;
        while b < end {
            let (i, _, _) = bmp_index(b);
            let chunk_end = end.min((i as u64 + 1) * BITS_PER_BMP);
            self.touch(io, i)?;
            let bm = self.loaded.get_mut(&i).unwrap();
            let first = i as u64 * BITS_PER_BMP;
            for x in b..chunk_end {
                let bit = x - first;
                let (w, m) = ((bit / 64) as usize, 1u64 << (bit % 64));
                if bm.words[w] & m != 0 {
                    return Err(Error::Corrupt("allocating a used block", x));
                }
                bm.words[w] |= m;
            }
            self.free_count[i] -= (chunk_end - b) as u32;
            b = chunk_end;
        }
        Ok(())
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
            match self.alloc_run(io, left) {
                Ok((s, l)) => {
                    out.push((s, l));
                    left -= l;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Writes the space map and free log for commit `generation` into the write
    /// batch. Returns the new (space root, free log head) pointers.
    ///
    /// Allocating new homes for the map's own blocks changes the map, so this
    /// loops until nothing new needs a home. It converges because each round
    /// only relocates blocks that were clean in the previous round.
    pub fn write<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        generation: u64,
    ) -> Result<(BlockPtr, BlockPtr)> {
        // The previous commit's free log is now garbage.
        for a in core::mem::take(&mut self.log_addrs) {
            self.free(io, a, 1)?;
        }
        let mut bmp_addr: BTreeMap<usize, u64> = BTreeMap::new();
        let mut l1_addr: BTreeMap<usize, u64> = BTreeMap::new();
        let mut root_addr: Option<u64> = None;
        let mut log_blocks: Vec<u64> = Vec::new();
        loop {
            let mut changed = false;
            for a in core::mem::take(&mut self.pending_table_frees) {
                self.free(io, a, 1)?;
                changed = true;
            }
            let need = self.freed_now.range_count().div_ceil(LOG_PER_BLOCK);
            while log_blocks.len() < need {
                log_blocks.push(self.alloc_run(io, 1)?.0);
                changed = true;
            }
            let dirty: Vec<usize> = self
                .loaded
                .iter()
                .filter(|(_, b)| b.dirty)
                .map(|(&i, _)| i)
                .collect();
            for i in dirty {
                if let alloc::collections::btree_map::Entry::Vacant(v) = bmp_addr.entry(i) {
                    v.insert(0);
                    let a = self.alloc_run(io, 1)?.0;
                    bmp_addr.insert(i, a);
                    changed = true;
                }
            }
            let l1d: Vec<usize> = self.l1_dirty.iter().copied().collect();
            for t in l1d {
                if let alloc::collections::btree_map::Entry::Vacant(v) = l1_addr.entry(t) {
                    v.insert(0);
                    let a = self.alloc_run(io, 1)?.0;
                    l1_addr.insert(t, a);
                    changed = true;
                }
            }
            if self.root_dirty && root_addr.is_none() {
                if !self.root.is_null() {
                    let old = self.root.addr;
                    self.root = BlockPtr::NULL;
                    self.free(io, old, 1)?;
                }
                root_addr = Some(self.alloc_run(io, 1)?.0);
                changed = true;
            }
            if !changed {
                break;
            }
        }

        // Everything has a home and no more bits change: serialize.
        for (&i, &a) in &bmp_addr {
            let bm = self.loaded.get_mut(&i).unwrap();
            let mut b = zero_block();
            for (c, w) in b.as_chunks_mut::<8>().0.iter_mut().zip(bm.words.iter()) {
                c.copy_from_slice(&w.to_le_bytes());
            }
            bm.dirty = false;
            self.bmp[i] = io.write_sealed(BlockKind::Bitmap, a, generation, b, self.free_count[i]);
        }
        for (&t, &a) in &l1_addr {
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[..]);
            let mut sum = 0u64;
            for j in 0..TABLE_FANOUT {
                let idx = t * TABLE_FANOUT + j;
                if idx < self.nbmp {
                    debug_assert!(!self.bmp[idx].is_null());
                    sum += self.free_count[idx] as u64;
                    self.bmp[idx].encode(&mut w);
                } else {
                    BlockPtr::NULL.encode(&mut w);
                }
            }
            self.l1[t] = io.write_sealed(BlockKind::SpaceTable, a, generation, b, sum as u32);
        }
        self.l1_dirty.clear();
        if let Some(a) = root_addr {
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[..]);
            for t in 0..TABLE_FANOUT {
                if t < self.nl1 {
                    self.l1[t].encode(&mut w)
                } else {
                    BlockPtr::NULL.encode(&mut w)
                }
            }
            self.root = io.write_sealed(BlockKind::SpaceTable, a, generation, b, 0);
            self.root_dirty = false;
        }

        // Free log, written back to front so each block can point at the next.
        let ranges = self.freed_now.to_vec();
        let mut next = BlockPtr::NULL;
        for (k, &a) in log_blocks.iter().enumerate().rev() {
            let chunk = ranges.chunks(LOG_PER_BLOCK).nth(k).unwrap_or(&[]);
            let mut b = zero_block();
            let mut w = Writer::new(&mut b[..]);
            w.u32(LOG_MAGIC);
            w.u32(chunk.len() as u32);
            next.encode(&mut w);
            for &(s, l) in chunk {
                w.u64(s);
                w.u64(l);
            }
            next = io.write_sealed(BlockKind::FreeLog, a, generation, b, 0);
        }
        self.log_addrs = log_blocks;
        Ok((self.root, next))
    }

    /// Called once commit `generation` is durable: the previous commit's frees leave quarantine.
    pub fn committed(&mut self) {
        self.freed_prev = core::mem::take(&mut self.freed_now);
    }

    /// Every bitmap block pointer (for fsck).
    pub fn tables(&self) -> (BlockPtr, &[BlockPtr], &[BlockPtr]) {
        (self.root, &self.l1, &self.bmp)
    }

    pub fn log_addrs(&self) -> &[u64] {
        &self.log_addrs
    }
}
