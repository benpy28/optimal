//! Fault-injecting block device used by the torture tests.
//!
//! [`CrashDevice`] models a disk with a volatile write cache. Writes since
//! the last flush are "in flight". When the simulated power is cut, every
//! block written since the last flush independently ends up with one of:
//! its old durable content, any one of the versions written to it since the
//! flush (so writes can land out of order), or a sector-by-sector mix of
//! those (a torn write). This is the worst behaviour the flush contract in
//! [`crate::BlockDevice`] allows, so if the filesystem survives it, it
//! survives any real disk that honours flushes.
//!
//! It can also inject read/write errors and silent bit rot, and can lose
//! power after an exact number of operations so tests can cut power at
//! every single write point of a workload.

use crate::device::{BLOCK_SIZE, BlockDevice, IoError, check_range};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

const SECTOR: usize = 512;

/// Small deterministic PRNG (xoshiro256**), so every test run is reproducible from its seed.
#[derive(Debug, Clone)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
        }
    }
    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }
    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

impl crate::crypto::Entropy for Rng {
    fn fill(&mut self, buf: &mut [u8]) {
        Rng::fill(self, buf)
    }
}

/// Fault settings. Probabilities are "1 in N"; 0 disables.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    pub read_error_one_in: u64,
    pub write_error_one_in: u64,
    pub flush_error_one_in: u64,
}

#[derive(Debug, Clone)]
pub struct CrashDevice {
    durable: Vec<u8>,
    /// Versions written to each block since the last flush, oldest first.
    volatile: BTreeMap<u64, Vec<Vec<u8>>>,
    blocks: u64,
    /// Operations (read, write, flush) completed so far.
    pub ops: u64,
    /// Writes completed so far.
    pub writes: u64,
    pub flushes: u64,
    /// Lose power when `writes + flushes` reaches this value.
    pub cut_at: Option<u64>,
    powered: bool,
    pub faults: Faults,
    /// A lying drive: flushes report success but make nothing durable, so
    /// writes from many "flushed" commits pile up in the volatile cache.
    pub lying_flush: bool,
    rng: Rng,
}

impl CrashDevice {
    pub fn new(blocks: u64, seed: u64) -> Self {
        CrashDevice {
            durable: vec![0; blocks as usize * BLOCK_SIZE],
            volatile: BTreeMap::new(),
            blocks,
            ops: 0,
            writes: 0,
            flushes: 0,
            cut_at: None,
            powered: true,
            faults: Faults::default(),
            lying_flush: false,
            rng: Rng::new(seed),
        }
    }
    pub fn from_image(image: Vec<u8>, seed: u64) -> Self {
        let blocks = (image.len() / BLOCK_SIZE) as u64;
        let mut d = CrashDevice::new(0, seed);
        d.durable = image;
        d.blocks = blocks;
        d
    }
    pub fn powered(&self) -> bool {
        self.powered
    }
    /// Writes and flushes issued so far: the "write points" a test can cut power at.
    pub fn write_points(&self) -> u64 {
        self.writes + self.flushes
    }
    /// Cuts power now. Further operations fail with `PowerLost`.
    pub fn cut_power(&mut self) {
        self.powered = false;
    }
    /// What the platters hold after the power cut: resolves every in-flight
    /// write using the adversarial model described in the module docs.
    pub fn crash_image(&mut self) -> Vec<u8> {
        let mut img = self.durable.clone();
        let volatile = core::mem::take(&mut self.volatile);
        for (block, versions) in volatile {
            let off = block as usize * BLOCK_SIZE;
            let mut candidates: Vec<&[u8]> = Vec::with_capacity(versions.len() + 1);
            let old = self.durable[off..off + BLOCK_SIZE].to_vec();
            candidates.push(&old);
            for v in &versions {
                candidates.push(v);
            }
            match self.rng.below(4) {
                // Whole-block outcome: old content or any written version.
                0..=2 => {
                    let pick = self.rng.below(candidates.len() as u64) as usize;
                    img[off..off + BLOCK_SIZE].copy_from_slice(candidates[pick]);
                }
                // Torn: every sector independently from any candidate.
                _ => {
                    for s in 0..BLOCK_SIZE / SECTOR {
                        let pick = self.rng.below(candidates.len() as u64) as usize;
                        let r = s * SECTOR..(s + 1) * SECTOR;
                        img[off + r.start..off + r.end].copy_from_slice(&candidates[pick][r]);
                    }
                }
            }
        }
        img
    }
    /// What the platters hold after a lying drive loses power: every write
    /// it claimed to have flushed or still had cached independently either
    /// landed (any version written to that block) or was lost (the block's
    /// durable content). Nothing is torn: a lying cache loses whole writes.
    /// Tearing is covered by [`CrashDevice::crash_image`].
    pub fn lost_writes_image(&mut self) -> Vec<u8> {
        self.lost_writes_image_steered(&[], &[])
    }

    /// [`CrashDevice::lost_writes_image`], except that the newest version
    /// written to each block in `land` always landed and every write to a
    /// block in `lose` was lost (superblock slots, to force the hardest
    /// fallback cases).
    pub fn lost_writes_image_steered(&mut self, land: &[u64], lose: &[u64]) -> Vec<u8> {
        let mut img = self.durable.clone();
        let volatile = core::mem::take(&mut self.volatile);
        for (block, versions) in volatile {
            let off = block as usize * BLOCK_SIZE;
            let pick = if land.contains(&block) {
                versions.len()
            } else if lose.contains(&block) {
                0
            } else {
                self.rng.below(versions.len() as u64 + 1) as usize
            };
            if pick > 0 {
                img[off..off + BLOCK_SIZE].copy_from_slice(&versions[pick - 1]);
            }
        }
        img
    }

    /// The image if every in-flight write had landed in order (no crash).
    pub fn clean_image(&self) -> Vec<u8> {
        let mut img = self.durable.clone();
        for (block, versions) in &self.volatile {
            let off = *block as usize * BLOCK_SIZE;
            img[off..off + BLOCK_SIZE].copy_from_slice(versions.last().unwrap());
        }
        img
    }

    fn step(&mut self, counts_as_point: bool) -> Result<(), IoError> {
        if !self.powered {
            return Err(IoError::PowerLost);
        }
        if counts_as_point
            && let Some(cut) = self.cut_at
            && self.writes + self.flushes >= cut
        {
            self.powered = false;
            return Err(IoError::PowerLost);
        }
        self.ops += 1;
        Ok(())
    }
}

impl BlockDevice for CrashDevice {
    fn num_blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.step(false)?;
        let n = check_range(self.blocks, block, buf.len())?;
        if self.faults.read_error_one_in > 0 && self.rng.chance(1, self.faults.read_error_one_in) {
            return Err(IoError::Failed);
        }
        for i in 0..n {
            let b = block + i;
            let dst = &mut buf[i as usize * BLOCK_SIZE..(i as usize + 1) * BLOCK_SIZE];
            match self.volatile.get(&b) {
                Some(v) => dst.copy_from_slice(v.last().unwrap()),
                None => {
                    let off = b as usize * BLOCK_SIZE;
                    dst.copy_from_slice(&self.durable[off..off + BLOCK_SIZE]);
                }
            }
        }
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        self.step(true)?;
        let n = check_range(self.blocks, block, buf.len())?;
        self.writes += 1;
        if self.faults.write_error_one_in > 0 && self.rng.chance(1, self.faults.write_error_one_in)
        {
            // A failed write may still have partially reached the media.
            if self.rng.chance(1, 2) {
                let b = block + self.rng.below(n);
                let i = (b - block) as usize;
                self.volatile
                    .entry(b)
                    .or_default()
                    .push(buf[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE].to_vec());
            }
            return Err(IoError::Failed);
        }
        for i in 0..n {
            let src = &buf[i as usize * BLOCK_SIZE..(i as usize + 1) * BLOCK_SIZE];
            self.volatile
                .entry(block + i)
                .or_default()
                .push(src.to_vec());
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        self.step(true)?;
        self.flushes += 1;
        if self.faults.flush_error_one_in > 0 && self.rng.chance(1, self.faults.flush_error_one_in)
        {
            return Err(IoError::Failed);
        }
        if self.lying_flush {
            return Ok(());
        }
        let volatile = core::mem::take(&mut self.volatile);
        for (block, versions) in volatile {
            let off = block as usize * BLOCK_SIZE;
            self.durable[off..off + BLOCK_SIZE].copy_from_slice(versions.last().unwrap());
        }
        Ok(())
    }
}

/// A huge device that stores only blocks written with non-zero content
/// (unwritten blocks read as zeros), for testing multi-terabyte volumes.
#[derive(Debug, Clone, Default)]
pub struct SparseDevice {
    blocks: u64,
    data: BTreeMap<u64, Vec<u8>>,
    pub reads: u64,
    pub writes: u64,
}

impl SparseDevice {
    pub fn new(blocks: u64) -> Self {
        SparseDevice {
            blocks,
            ..Default::default()
        }
    }
    /// Blocks actually holding data.
    pub fn stored_blocks(&self) -> usize {
        self.data.len()
    }
}

impl BlockDevice for SparseDevice {
    fn num_blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let n = check_range(self.blocks, block, buf.len())?;
        self.reads += n;
        for i in 0..n {
            let dst = &mut buf[i as usize * BLOCK_SIZE..(i as usize + 1) * BLOCK_SIZE];
            match self.data.get(&(block + i)) {
                Some(v) => dst.copy_from_slice(v),
                None => dst.fill(0),
            }
        }
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        let n = check_range(self.blocks, block, buf.len())?;
        self.writes += n;
        for i in 0..n {
            let src = &buf[i as usize * BLOCK_SIZE..(i as usize + 1) * BLOCK_SIZE];
            if src.iter().all(|&x| x == 0) {
                self.data.remove(&(block + i));
            } else {
                self.data.insert(block + i, src.to_vec());
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}
