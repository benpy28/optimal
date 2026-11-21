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
}

#[derive(Debug)]
pub struct Io<D> {
    pub dev: D,
    pub sealer: Sealer,
    /// Sealed blocks waiting to be written in the current commit.
    batch: BTreeMap<u64, Block>,
    pub stats: IoStats,
}

impl<D: BlockDevice> Io<D> {
    pub fn new(dev: D, sealer: Sealer) -> Self {
        Io {
            dev,
            sealer,
            batch: BTreeMap::new(),
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

    /// Reads the block `ptr` points to and verifies (and decrypts) it.
    pub fn read_sealed(&mut self, kind: BlockKind, ptr: &BlockPtr) -> Result<Block> {
        if ptr.addr == 0 || ptr.addr >= self.num_blocks() {
            return Err(Error::Corrupt("block pointer out of range", ptr.addr));
        }
        let mut b = zero_block();
        self.read_raw(ptr.addr, &mut b[..])?;
        self.sealer
            .open(kind, ptr.addr, ptr.birth, &ptr.nonce, &ptr.mac, &mut b[..])?;
        Ok(b)
    }

    /// Seals `data` for `addr` and queues it for writing; returns its pointer.
    pub fn write_sealed(
        &mut self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        mut data: Block,
        aux: u64,
    ) -> BlockPtr {
        let (nonce, mac) = self.sealer.seal(kind, addr, birth, &mut data[..]);
        self.queue(addr, data);
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
