//! Sealed block I/O and write batching.

use crate::crypto::{BlockKind, Sealer};
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::ptr::BlockPtr;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub type Block = Box<[u8; BLOCK_SIZE]>;

pub fn zero_block() -> Block {
    Box::new([0u8; BLOCK_SIZE])
}

/// Largest single device request the batcher issues (1 MiB).
const MAX_RUN: usize = 256;

#[derive(Debug, Default, Clone, Copy)]
pub struct IoStats {
    pub blocks_read: u64,
    pub blocks_written: u64,
    pub read_requests: u64,
    pub write_requests: u64,
    pub flushes: u64,
    /// Metadata copies that failed to verify while the other copy was good.
    pub bad_copies: u64,
    /// Bad copies repaired (moved to a fresh place by the next commit).
    pub copies_repaired: u64,
    /// Bad copies of blocks the next commit no longer references, so not
    /// repaired (older commits still have the good copy).
    pub copies_dropped: u64,
}

/// Which copy of a duplicated block to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    /// Copy 0, falling back to copy 1 (normal reads).
    Either,
    /// Read and verify both; succeed if either is good (recovery checks).
    Both,
}

/// What a duplicated block belongs to, so a bad copy can be repaired by
/// moving it: a metadata chunk, or a chunk-table node (level, index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CopyOwner {
    Chunk(u64),
    Table(u32, u64),
}

/// A bad copy found by a read: the bytes it should hold, whose it is and
/// which copy (0 or 1).
#[derive(Debug, Clone)]
pub struct Repair {
    pub good: Block,
    pub owner: CopyOwner,
    pub copy: u8,
}

#[derive(Debug)]
pub struct Io<D> {
    pub dev: D,
    pub sealer: Sealer,
    /// Sealed blocks waiting to be written in the current commit.
    batch: BTreeMap<u64, Block>,
    /// Bad metadata copies, by physical block, to repair at the next commit.
    repairs: BTreeMap<u64, Repair>,
    /// Physical blocks found bad since the last commit (counted once each).
    bad_seen: alloc::collections::BTreeSet<u64>,
    /// Off for read-only mounts and checks that must not write.
    pub repair_enabled: bool,
    /// How duplicated blocks are read ([`CopyMode::Both`] during recovery).
    pub copy_mode: CopyMode,
    pub stats: IoStats,
}

impl<D: BlockDevice> Io<D> {
    pub fn new(dev: D, sealer: Sealer) -> Self {
        Io {
            dev,
            sealer,
            batch: BTreeMap::new(),
            repairs: BTreeMap::new(),
            bad_seen: alloc::collections::BTreeSet::new(),
            repair_enabled: true,
            copy_mode: CopyMode::Either,
            stats: IoStats::default(),
        }
    }

    pub fn num_blocks(&self) -> u64 {
        self.dev.num_blocks()
    }

    pub fn read_raw(&mut self, addr: u64, buf: &mut [u8]) -> Result<()> {
        self.stats.read_requests += 1;
        self.stats.blocks_read += (buf.len() / BLOCK_SIZE) as u64;
        self.dev.read(addr, buf).map_err(Error::Io)
    }

    /// Reads physical block `phys` raw (no verification): the queued
    /// version if this commit has queued one (a chunk moved or repaired at
    /// the start of the commit lives only in the batch until it is written),
    /// otherwise the device's.
    pub fn read_copy(&mut self, phys: u64) -> Result<Block> {
        if phys == 0 || phys >= self.num_blocks() {
            return Err(Error::Corrupt("block copy out of range", phys));
        }
        if let Some(q) = self.batch.get(&phys) {
            return Ok(q.clone());
        }
        let mut b = zero_block();
        self.read_raw(phys, &mut b[..])?;
        Ok(b)
    }

    /// Reads a duplicated block whose two copies live at physical `p0` and
    /// `p1`, sealed for (`kind`, `ptr.addr`, `ptr.birth`) with the nonce
    /// and MAC in `ptr`. Copy 1 is turned back into copy 0's bytes and
    /// checked against the same MAC, so both are bound by the parent.
    ///
    /// A copy that fails while the other verifies is counted and, if
    /// repairs are enabled, recorded with `owner` so the next commit moves
    /// that copy through the chunk table. Fails only if no copy that was
    /// read verifies. Copy 1's bytes are never used unless the unmasked
    /// result matches the parent's MAC.
    pub fn read_dup(
        &mut self,
        kind: BlockKind,
        ptr: &BlockPtr,
        p0: u64,
        p1: u64,
        owner: CopyOwner,
    ) -> Result<Block> {
        let mode = self.copy_mode;
        let (a, birth, nonce, mac) = (ptr.addr, ptr.birth, ptr.nonce, ptr.mac);
        // Copy 0: the raw bytes are what copy 0 must hold.
        let r0 = self.read_copy(p0).and_then(|raw| {
            let mut b = raw.clone();
            self.sealer.open(kind, a, birth, &nonce, &mac, &mut b[..])?;
            Ok((raw, b))
        });
        if r0.is_ok() && mode == CopyMode::Either {
            return Ok(r0.unwrap().1);
        }
        let r1 = self.read_copy(p1).and_then(|mut raw| {
            self.sealer.swap_copy(kind, a, birth, &nonce, &mut raw[..]);
            let mut b = raw.clone();
            self.sealer.open(kind, a, birth, &nonce, &mac, &mut b[..])?;
            // `raw` now holds copy 0's bytes.
            Ok((raw, b))
        });
        match (r0, r1) {
            (Ok((_, b)), Ok(_)) => Ok(b),
            (Ok((raw0, b)), Err(_)) => {
                let mut c1 = raw0;
                self.sealer.swap_copy(kind, a, birth, &nonce, &mut c1[..]);
                self.note_bad_copy(p1, c1, owner, 1);
                Ok(b)
            }
            (Err(_), Ok((raw0, b))) => {
                self.note_bad_copy(p0, raw0, owner, 0);
                Ok(b)
            }
            (Err(e), Err(_)) => Err(e),
        }
    }

    /// Verifies each copy of a duplicated block on its own, with no side
    /// effects (fsck). Returns whether copy 0 and copy 1 are good.
    pub fn verify_copies(
        &mut self,
        kind: BlockKind,
        ptr: &BlockPtr,
        p0: u64,
        p1: u64,
    ) -> (bool, bool) {
        let (a, birth, nonce, mac) = (ptr.addr, ptr.birth, ptr.nonce, ptr.mac);
        let ok0 = self
            .read_copy(p0)
            .and_then(|mut b| self.sealer.open(kind, a, birth, &nonce, &mac, &mut b[..]))
            .is_ok();
        let ok1 = self
            .read_copy(p1)
            .and_then(|mut b| {
                self.sealer.swap_copy(kind, a, birth, &nonce, &mut b[..]);
                self.sealer.open(kind, a, birth, &nonce, &mac, &mut b[..])
            })
            .is_ok();
        (ok0, ok1)
    }

    fn note_bad_copy(&mut self, phys: u64, good: Block, owner: CopyOwner, copy: u8) {
        if self.bad_seen.insert(phys) {
            self.stats.bad_copies += 1;
        }
        if self.repair_enabled {
            self.repairs.insert(phys, Repair { good, owner, copy });
        }
    }

    /// Bad copies waiting to be rewritten.
    pub fn pending_repairs(&self) -> usize {
        self.repairs.len()
    }

    /// Whether physical block `phys` has a repair queued.
    pub fn repair_queued(&self, phys: u64) -> bool {
        self.repairs.contains_key(&phys)
    }

    /// Hands every recorded repair to the caller (the commit, which moves
    /// each bad copy to a fresh place; a live block is never overwritten in
    /// place). Those blocks may be counted again if read bad later.
    pub fn take_repairs(&mut self) -> BTreeMap<u64, Repair> {
        let r = core::mem::take(&mut self.repairs);
        for p in r.keys() {
            self.bad_seen.remove(p);
        }
        r
    }

    pub fn discard_repairs(&mut self) {
        self.repairs.clear();
        self.bad_seen.clear();
    }

    /// Seals `data` as (`kind`, `addr`, `birth`) and queues copy 0 at
    /// physical `p0` and copy 1 at `p1`; returns the pointer (whose `addr`
    /// is `addr`, the address the seal binds).
    #[allow(clippy::too_many_arguments)]
    pub fn write_dup(
        &mut self,
        kind: BlockKind,
        addr: u64,
        p0: u64,
        p1: u64,
        birth: u64,
        mut data: Block,
        aux: u64,
    ) -> BlockPtr {
        let mut c1 = data.clone();
        let (nonce, mac) = self.sealer.seal(kind, addr, birth, &mut data[..]);
        self.sealer
            .seal_copy1(kind, addr, birth, &nonce, &mut c1[..]);
        self.queue(p0, data);
        self.queue(p1, c1);
        BlockPtr {
            addr,
            birth,
            mac,
            nonce,
            aux,
        }
    }

    /// Queues an already-sealed block.
    pub fn queue(&mut self, addr: u64, data: Block) {
        let old = self.batch.insert(addr, data);
        debug_assert!(old.is_none(), "block {addr} queued twice in one commit");
    }

    pub fn discard_batch(&mut self) {
        self.batch.clear();
    }

    /// Whether physical block `a` is queued in the current batch.
    pub fn is_queued(&self, a: u64) -> bool {
        self.batch.contains_key(&a)
    }

    /// Writes every queued block, merging neighbours into large requests.
    pub fn write_batch(&mut self) -> Result<()> {
        let batch = core::mem::take(&mut self.batch);
        let mut run: Vec<u8> = Vec::new();
        let mut run_start = 0u64;
        let mut run_len = 0u64;
        for (addr, data) in batch {
            if run_len > 0 && (addr != run_start + run_len || run_len as usize >= MAX_RUN) {
                self.write_run(run_start, &run)?;
                run.clear();
                run_len = 0;
            }
            if run_len == 0 {
                run_start = addr;
            }
            run.extend_from_slice(&data[..]);
            run_len += 1;
        }
        if run_len > 0 {
            self.write_run(run_start, &run)?;
        }
        Ok(())
    }

    fn write_run(&mut self, start: u64, data: &[u8]) -> Result<()> {
        self.stats.write_requests += 1;
        self.stats.blocks_written += (data.len() / BLOCK_SIZE) as u64;
        self.dev.write(start, data).map_err(Error::Io)
    }

    pub fn write_now(&mut self, addr: u64, data: &[u8]) -> Result<()> {
        self.write_run(addr, data)
    }

    pub fn flush(&mut self) -> Result<()> {
        self.stats.flushes += 1;
        self.dev.flush().map_err(Error::Io)
    }
}
