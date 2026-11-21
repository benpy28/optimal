//! The chunk table: where each logical metadata chunk's two copies live.
//!
//! Every metadata block (tree nodes, space-map blocks, free-log blocks, the
//! root block) is stored twice. Pointers to metadata hold a *logical*
//! address, `bit 63 | chunk id << shift | offset`, and the MAC binds that
//! logical address, never a physical one. A chunk is `len` consecutive
//! logical blocks (`1 <= len <= 2^shift`, the volume's chunk size) backed by
//! two physical runs of `len` blocks, copy 0 at `p0` and copy 1 at `p1`,
//! normally in opposite halves of the device. Because nothing above this
//! table records a physical metadata address, a chunk can be moved (repair,
//! a failing region, balancing) by copying it and changing one entry.
//!
//! The table is a copy-on-write radix tree indexed by chunk id. Leaves hold
//! 128 entries `(p0, p1, used bitmap, len)`; interior tables hold 32 child
//! records `(pointer, free slots below, mapped chunks below)`. Its own
//! blocks cannot go through the table, so they are stored twice directly:
//! a chunk-table pointer's `addr` is copy 0's physical block (also the
//! address the MAC binds) and its `aux` is copy 1's. The depth (0 = the
//! root is a leaf) grows as chunk ids are used and is recorded with the
//! root in every superblock root set, so a fallback to an older commit
//! resolves its logical addresses through that commit's own table.
//!
//! Frees are quarantined like any block: the old physical runs of a
//! released or moved chunk and the old copies of rewritten table blocks go
//! through the space map's K-commit quarantine, and freed logical blocks
//! are quarantined before their slot is reused.

use crate::codec::{Reader, Writer};
use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Block, Io, zero_block};
use crate::ptr::BlockPtr;
use crate::space::{SB_RESERVED_BACK, SB_RESERVED_FRONT};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

/// Bit 63 marks a logical (metadata) address. Physical addresses are below
/// 2^45, so the two can never be confused.
pub const LOGICAL: u64 = 1 << 63;
pub const LEAF_ENTRIES: u64 = 128;
const ENTRY_LEN: usize = 32;
pub const TABLE_FANOUT: u64 = 32;
const KID_LEN: usize = 128;
/// 128 * 32^8 = 2^47 chunk ids.
pub const MAX_DEPTH: u32 = 8;
pub const MAX_CHUNK_SHIFT: u32 = 6;
/// Clean nodes kept in memory after a commit.
const CLEAN_CACHE: usize = 1024;

const _: () = assert!(LEAF_ENTRIES as usize * ENTRY_LEN == BLOCK_SIZE);
const _: () = assert!(TABLE_FANOUT as usize * KID_LEN == BLOCK_SIZE);

pub fn is_logical(a: u64) -> bool {
    a & LOGICAL != 0
}

/// Chunk ids covered by one node at `level` (0 = leaf).
pub fn ids_at(level: u32) -> u64 {
    LEAF_ENTRIES << (5 * level)
}

/// One chunk: two physical runs of `len` blocks and which of its logical
/// blocks are in use. `len == 0` means the id is unmapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Entry {
    pub p0: u64,
    pub p1: u64,
    pub used: u64,
    pub len: u32,
}

impl Entry {
    pub fn mapped(&self) -> bool {
        self.len != 0
    }
    pub fn free_slots(&self) -> u64 {
        self.len as u64 - self.used.count_ones() as u64
    }
    pub fn mask(len: u32) -> u64 {
        if len >= 64 { u64::MAX } else { (1u64 << len) - 1 }
    }
    pub fn is_used(&self, off: u64) -> bool {
        self.used & (1 << off) != 0
    }
}

/// A child record in an interior table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kid {
    /// Copy 0 at `addr`, copy 1 at `aux`. Null: nothing mapped below.
    /// A dirty child's pointer has address 0 until it is written.
    pub ptr: BlockPtr,
    /// Unused logical slots in mapped chunks below.
    pub free: u64,
    /// Mapped chunks below.
    pub mapped: u64,
}

impl Kid {
    const NULL: Kid = Kid {
        ptr: BlockPtr::NULL,
        free: 0,
        mapped: 0,
    };
}

#[derive(Debug, Clone)]
enum Contents {
    Leaf(Vec<Entry>),
    Table(Vec<Kid>),
}

#[derive(Debug, Clone)]
struct CNode {
    contents: Contents,
    dirty: bool,
}

impl CNode {
    fn fresh(level: u32) -> CNode {
        CNode {
            contents: if level == 0 {
                Contents::Leaf(vec![Entry::default(); LEAF_ENTRIES as usize])
            } else {
                Contents::Table(vec![Kid::NULL; TABLE_FANOUT as usize])
            },
            dirty: false,
        }
    }

    /// (free slots, mapped chunks) in this node.
    fn sums(&self) -> (u64, u64) {
        match &self.contents {
            Contents::Leaf(es) => es.iter().fold((0, 0), |(f, m), e| {
                (f + if e.mapped() { e.free_slots() } else { 0 }, m + e.mapped() as u64)
            }),
            Contents::Table(ks) => ks
                .iter()
                .fold((0, 0), |(f, m), k| (f + k.free, m + k.mapped)),
        }
    }

    fn kids_mut(&mut self) -> &mut Vec<Kid> {
        match &mut self.contents {
            Contents::Table(k) => k,
            Contents::Leaf(_) => unreachable!("chunk table level mismatch"),
        }
    }

    fn entries(&self) -> &Vec<Entry> {
        match &self.contents {
            Contents::Leaf(e) => e,
            Contents::Table(_) => unreachable!("chunk table level mismatch"),
        }
    }

    fn encode(&self) -> Block {
        let mut b = zero_block();
        let mut w = Writer::new(&mut b[..]);
        match &self.contents {
            Contents::Leaf(es) => {
                for e in es {
                    w.u64(e.p0);
                    w.u64(e.p1);
                    w.u64(e.used);
                    w.u32(e.len);
                    w.u32(0);
                }
            }
            Contents::Table(ks) => {
                for k in ks {
                    k.ptr.encode(&mut w);
                    w.u64(k.free);
                    w.u64(k.mapped);
                    w.zeros(KID_LEN - 64 - 16);
                }
            }
        }
        b
    }
}

/// Whether `[p, p+len)` lies in the allocatable area of a `total`-block device.
fn in_usable(total: u64, p: u64, len: u64) -> bool {
    p >= SB_RESERVED_FRONT
        && p.checked_add(len)
            .is_some_and(|e| e <= total - SB_RESERVED_BACK)
}

fn overlap(a: u64, alen: u64, b: u64, blen: u64) -> bool {
    a < b + blen && b < a + alen
}

#[derive(Debug)]
pub struct ChunkTable {
    shift: u32,
    total: u64,
    depth: u32,
    /// The root's own pointer (copy 0 at `addr`, copy 1 at `aux`); address
    /// 0 while the root is dirty or was never written.
    root: BlockPtr,
    /// Loaded nodes keyed by (level, index); the root is (depth, 0).
    nodes: BTreeMap<(u32, u64), CNode>,
    free_total: u64,
    mapped_total: u64,
    /// Old copies (copy 0, copy 1) of table blocks rewritten in this
    /// transaction; the space map frees them (quarantined).
    pub pending_frees: Vec<(u64, u64)>,
}

impl ChunkTable {
    /// An empty table for a new volume.
    pub fn format(total: u64, shift: u32) -> ChunkTable {
        let mut nodes = BTreeMap::new();
        let mut root = CNode::fresh(0);
        root.dirty = true;
        nodes.insert((0, 0), root);
        ChunkTable {
            shift,
            total,
            depth: 0,
            root: BlockPtr::NULL,
            nodes,
            free_total: 0,
            mapped_total: 0,
            pending_frees: Vec::new(),
        }
    }

    /// Opens a committed table; nodes load on demand.
    pub fn open<D: BlockDevice>(
        io: &mut Io<D>,
        total: u64,
        shift: u32,
        root: BlockPtr,
        depth: u32,
    ) -> Result<ChunkTable> {
        if shift > MAX_CHUNK_SHIFT || depth > MAX_DEPTH || root.is_null() {
            return Err(Error::Corrupt("chunk table root", root.addr));
        }
        let mut t = ChunkTable {
            shift,
            total,
            depth,
            root,
            nodes: BTreeMap::new(),
            free_total: 0,
            mapped_total: 0,
            pending_frees: Vec::new(),
        };
        let n = t.read_node(io, depth, 0, &root)?;
        (t.free_total, t.mapped_total) = n.sums();
        t.nodes.insert((depth, 0), n);
        Ok(t)
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }
    pub fn chunk_blocks(&self) -> u64 {
        1 << self.shift
    }
    pub fn root_ptr(&self) -> BlockPtr {
        self.root
    }
    pub fn free_slots(&self) -> u64 {
        self.free_total
    }
    pub fn mapped_chunks(&self) -> u64 {
        self.mapped_total
    }
    pub fn capacity(&self) -> u64 {
        ids_at(self.depth)
    }

    /// Logical address of block `off` of chunk `id`.
    pub fn join(&self, id: u64, off: u64) -> u64 {
        debug_assert!(off < self.chunk_blocks());
        LOGICAL | (id << self.shift) | off
    }

    /// (chunk id, offset) of a logical address.
    pub fn split(&self, l: u64) -> Result<(u64, u64)> {
        if !is_logical(l) {
            return Err(Error::Corrupt("metadata pointer is not logical", l));
        }
        let v = l & !LOGICAL;
        let id = v >> self.shift;
        if id >= ids_at(MAX_DEPTH) {
            return Err(Error::Corrupt("logical address out of range", l));
        }
        Ok((id, v & (self.chunk_blocks() - 1)))
    }

    fn read_node<D: BlockDevice>(
        &self,
        io: &mut Io<D>,
        level: u32,
        idx: u64,
        p: &BlockPtr,
    ) -> Result<CNode> {
        if !in_usable(self.total, p.addr, 1) || !in_usable(self.total, p.aux, 1) || p.addr == p.aux
        {
            return Err(Error::Corrupt("chunk table pointer", p.addr));
        }
        let kind = if level == 0 {
            BlockKind::ChunkLeaf
        } else {
            BlockKind::ChunkTable
        };
        let b = io.read_dup(kind, p, p.addr, p.aux)?;
        self.decode(&b[..], level, idx, p.addr)
    }

    fn decode(&self, b: &[u8], level: u32, idx: u64, at: u64) -> Result<CNode> {
        let mut r = Reader::new(b, "chunk table", at);
        let bad = |why| Err(Error::Corrupt(why, at));
        let contents = if level == 0 {
            let mut es = Vec::with_capacity(LEAF_ENTRIES as usize);
            for _ in 0..LEAF_ENTRIES {
                let e = Entry {
                    p0: r.u64()?,
                    p1: r.u64()?,
                    used: r.u64()?,
                    len: r.u32()?,
                };
                if r.u32()? != 0 {
                    return bad("chunk entry padding");
                }
                if !e.mapped() {
                    if e != Entry::default() {
                        return bad("unmapped chunk entry not zero");
                    }
                } else {
                    let len = e.len as u64;
                    if len > self.chunk_blocks()
                        || e.used & !Entry::mask(e.len) != 0
                        || !in_usable(self.total, e.p0, len)
                        || !in_usable(self.total, e.p1, len)
                        || overlap(e.p0, len, e.p1, len)
                    {
                        return bad("chunk entry");
                    }
                }
                es.push(e);
            }
            Contents::Leaf(es)
        } else {
            let mut ks = Vec::with_capacity(TABLE_FANOUT as usize);
            for j in 0..TABLE_FANOUT {
                let ptr = BlockPtr::decode(&mut r)?;
                let free = r.u64()?;
                let mapped = r.u64()?;
                if r.bytes(KID_LEN - 64 - 16)?.iter().any(|&x| x != 0) {
                    return bad("chunk table padding");
                }
                let k = Kid { ptr, free, mapped };
                // Ids past the table's reach can hold nothing.
                let first = (idx * TABLE_FANOUT + j) * ids_at(level - 1);
                let cap = ids_at(level - 1);
                if ptr.is_null() {
                    if k != Kid::NULL {
                        return bad("null chunk table child with contents");
                    }
                } else if mapped == 0
                    || mapped > cap
                    || free > mapped * self.chunk_blocks()
                    || first >= ids_at(MAX_DEPTH)
                {
                    return bad("chunk table child counts");
                }
                ks.push(k);
            }
            Contents::Table(ks)
        };
        r.finish()?;
        Ok(CNode {
            contents,
            dirty: false,
        })
    }

    /// The node at (level, idx), loading it if needed.
    fn node<D: BlockDevice>(&mut self, io: &mut Io<D>, level: u32, idx: u64) -> Result<&mut CNode> {
        if !self.nodes.contains_key(&(level, idx)) {
            debug_assert!(level < self.depth);
            let k = self.node(io, level + 1, idx / TABLE_FANOUT)?.kids_mut()
                [(idx % TABLE_FANOUT) as usize];
            let n = if k.ptr.is_null() {
                CNode::fresh(level)
            } else {
                let n = self.read_node(io, level, idx, &k.ptr)?;
                if n.sums() != (k.free, k.mapped) {
                    return Err(Error::Corrupt("chunk table counts mismatch", k.ptr.addr));
                }
                n
            };
            self.nodes.insert((level, idx), n);
        }
        Ok(self.nodes.get_mut(&(level, idx)).unwrap())
    }

    /// The entry for chunk `id` (unmapped if beyond the table).
    pub fn entry<D: BlockDevice>(&mut self, io: &mut Io<D>, id: u64) -> Result<Entry> {
        if id >= self.capacity() {
            return Ok(Entry::default());
        }
        let leaf = self.node(io, 0, id / LEAF_ENTRIES)?;
        Ok(leaf.entries()[(id % LEAF_ENTRIES) as usize])
    }

    /// Physical blocks of copies 0 and 1 of logical block `l`.
    pub fn resolve<D: BlockDevice>(&mut self, io: &mut Io<D>, l: u64) -> Result<(u64, u64)> {
        let (id, off) = self.split(l)?;
        let e = self.entry(io, id)?;
        if off >= e.len as u64 {
            return Err(Error::Corrupt("logical address not mapped", l));
        }
        Ok((e.p0 + off, e.p1 + off))
    }

    /// Replaces chunk `id`'s entry, updating the counts on its path and
    /// marking the path dirty (the first change frees each old copy).
    pub fn set_entry<D: BlockDevice>(&mut self, io: &mut Io<D>, id: u64, e: Entry) -> Result<()> {
        debug_assert!(id < self.capacity());
        let old = self.entry(io, id)?;
        let f = |x: &Entry| if x.mapped() { x.free_slots() as i64 } else { 0 };
        let df = f(&e) - f(&old);
        let dm = e.mapped() as i64 - old.mapped() as i64;
        let leaf_idx = id / LEAF_ENTRIES;
        match &mut self.node(io, 0, leaf_idx)?.contents {
            Contents::Leaf(es) => es[(id % LEAF_ENTRIES) as usize] = e,
            Contents::Table(_) => unreachable!(),
        }
        let mut idx = leaf_idx;
        for level in 0..=self.depth {
            let was_dirty = self.node(io, level, idx)?.dirty;
            self.nodes.get_mut(&(level, idx)).unwrap().dirty = true;
            if level == self.depth {
                if !was_dirty && !self.root.is_null() {
                    self.pending_frees.push((self.root.addr, self.root.aux));
                }
                if !was_dirty {
                    self.root = BlockPtr::NULL;
                }
                break;
            }
            let parent = idx / TABLE_FANOUT;
            let k = &mut self.node(io, level + 1, parent)?.kids_mut()[(idx % TABLE_FANOUT) as usize];
            k.free = (k.free as i64 + df) as u64;
            k.mapped = (k.mapped as i64 + dm) as u64;
            if !was_dirty && k.ptr.addr != 0 {
                self.pending_frees.push((k.ptr.addr, k.ptr.aux));
            }
            if !was_dirty {
                // Homeless until written; counts stay meaningful.
                k.ptr = BlockPtr::NULL;
            }
            idx = parent;
        }
        self.free_total = (self.free_total as i64 + df) as u64;
        self.mapped_total = (self.mapped_total as i64 + dm) as u64;
        Ok(())
    }

    /// Adds a level above the root (room for 32 times more chunk ids).
    pub fn grow(&mut self) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::NoSpace);
        }
        let mut t = CNode::fresh(self.depth + 1);
        t.kids_mut()[0] = Kid {
            ptr: self.root,
            free: self.free_total,
            mapped: self.mapped_total,
        };
        t.dirty = true;
        self.depth += 1;
        self.nodes.insert((self.depth, 0), t);
        // The old root's copy stays live: it is now child 0.
        self.root = BlockPtr::NULL;
        Ok(())
    }

    /// The lowest unmapped chunk id that fits the current depth.
    pub fn find_unmapped<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<Option<u64>> {
        self.find_unmapped_in(io, self.depth, 0)
    }

    fn find_unmapped_in<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        level: u32,
        idx: u64,
    ) -> Result<Option<u64>> {
        if level == 0 {
            let es = self.node(io, 0, idx)?.entries();
            return Ok(es
                .iter()
                .position(|e| !e.mapped())
                .map(|i| idx * LEAF_ENTRIES + i as u64));
        }
        let cap = ids_at(level - 1);
        for j in 0..TABLE_FANOUT {
            let k = self.node(io, level, idx)?.kids_mut()[j as usize];
            let c = idx * TABLE_FANOUT + j;
            if k.mapped < cap
                && let Some(id) = self.find_unmapped_in(io, level - 1, c)?
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// The first chunk with id `>= from` that has an unused slot.
    pub fn find_free<D: BlockDevice>(&mut self, io: &mut Io<D>, from: u64) -> Result<Option<u64>> {
        if from >= self.capacity() {
            return Ok(None);
        }
        self.find_free_in(io, self.depth, 0, from)
    }

    fn find_free_in<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        level: u32,
        idx: u64,
        from: u64,
    ) -> Result<Option<u64>> {
        if level == 0 {
            let es = self.node(io, 0, idx)?.entries();
            let first = idx * LEAF_ENTRIES;
            let start = from.saturating_sub(first) as usize;
            return Ok(es
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, e)| e.mapped() && e.free_slots() > 0)
                .map(|(i, _)| first + i as u64));
        }
        let span = ids_at(level - 1);
        let first_child = idx * TABLE_FANOUT;
        let start = (from / span).max(first_child);
        for c in start..first_child + TABLE_FANOUT {
            let k = self.node(io, level, idx)?.kids_mut()[(c - first_child) as usize];
            if k.free == 0 {
                continue;
            }
            if let Some(id) = self.find_free_in(io, level - 1, c, from.max(c * span))? {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Dirty nodes, children before parents (the order they must be written).
    pub fn dirty_nodes(&self) -> Vec<(u32, u64)> {
        let mut v: Vec<(u32, u64)> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.dirty)
            .map(|(&k, _)| k)
            .collect();
        v.sort();
        v
    }

    /// Seals every dirty node into `io`'s batch at its home (copy 0, copy 1)
    /// and returns the new root pointer.
    pub fn write<D: BlockDevice>(
        &mut self,
        io: &mut Io<D>,
        generation: u64,
        homes: &BTreeMap<(u32, u64), (u64, u64)>,
    ) -> Result<BlockPtr> {
        for (level, idx) in self.dirty_nodes() {
            let (p0, p1) = homes[&(level, idx)];
            let n = self.nodes.get_mut(&(level, idx)).unwrap();
            n.dirty = false;
            let kind = if level == 0 {
                BlockKind::ChunkLeaf
            } else {
                BlockKind::ChunkTable
            };
            let p = io.write_dup(kind, p0, p0, p1, generation, n.encode(), p1);
            if level == self.depth {
                self.root = p;
            } else {
                let parent = self.node(io, level + 1, idx / TABLE_FANOUT)?;
                let k = &mut parent.kids_mut()[(idx % TABLE_FANOUT) as usize];
                k.ptr = p;
            }
        }
        debug_assert!(!self.root.is_null());
        Ok(self.root)
    }

    /// Drops clean nodes (except the root) when the cache is large.
    pub fn evict_clean(&mut self) {
        if self.nodes.len() <= CLEAN_CACHE {
            return;
        }
        let top = (self.depth, 0);
        let clean: Vec<(u32, u64)> = self
            .nodes
            .iter()
            .filter(|(k, n)| !n.dirty && **k != top)
            .map(|(&k, _)| k)
            .collect();
        let n = self.nodes.len() - CLEAN_CACHE / 2;
        // Evict leaves before tables, so a loaded leaf's parent stays.
        for k in clean.into_iter().take(n) {
            self.nodes.remove(&k);
        }
    }

    /// Every written node pointer with its level (for fsck).
    pub fn all_node_ptrs<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<Vec<(u32, BlockPtr)>> {
        let mut out = vec![(self.depth, self.root)];
        let mut stack = vec![(self.depth, 0u64)];
        while let Some((level, idx)) = stack.pop() {
            if level == 0 {
                continue;
            }
            let kids = self.node(io, level, idx)?.kids_mut().clone();
            for (j, k) in kids.iter().enumerate() {
                if k.ptr.is_null() {
                    continue;
                }
                out.push((level - 1, k.ptr));
                stack.push((level - 1, idx * TABLE_FANOUT + j as u64));
            }
        }
        Ok(out)
    }

    /// Every mapped chunk (for fsck and tests). Loads the whole table.
    pub fn all_entries<D: BlockDevice>(&mut self, io: &mut Io<D>) -> Result<Vec<(u64, Entry)>> {
        let mut out = Vec::new();
        let mut stack = vec![(self.depth, 0u64)];
        while let Some((level, idx)) = stack.pop() {
            if level == 0 {
                let es = self.node(io, 0, idx)?.entries().clone();
                for (i, e) in es.iter().enumerate() {
                    if e.mapped() {
                        out.push((idx * LEAF_ENTRIES + i as u64, *e));
                    }
                }
                continue;
            }
            let kids = self.node(io, level, idx)?.kids_mut().clone();
            for (j, k) in kids.iter().enumerate().rev() {
                if k.mapped > 0 {
                    stack.push((level - 1, idx * TABLE_FANOUT + j as u64));
                }
            }
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Reads (verifying, re-reading from the device) every node born at or
    /// after `min_birth`, skipping older subtrees. Returns how many.
    pub fn verify_since<D: BlockDevice>(&mut self, io: &mut Io<D>, min_birth: u64) -> Result<u64> {
        let mut n = 0;
        let mut stack = vec![(self.depth, 0u64, self.root)];
        while let Some((level, idx, p)) = stack.pop() {
            if p.is_null() || p.birth < min_birth {
                continue;
            }
            let node = self.read_node(io, level, idx, &p)?;
            n += 1;
            if let Contents::Table(ks) = &node.contents {
                for (j, k) in ks.iter().enumerate() {
                    stack.push((level - 1, idx * TABLE_FANOUT + j as u64, k.ptr));
                }
            }
        }
        Ok(n)
    }
}
