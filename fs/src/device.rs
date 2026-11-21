//! The block device interface the filesystem runs on.

use alloc::vec;
use alloc::vec::Vec;

/// The filesystem block size. Fixed at 4 KiB, which matches the page size,
/// NVMe's preferred write unit and the common flash page size.
pub const BLOCK_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    /// The request was outside the device.
    OutOfRange,
    /// The device failed the request (media error, timeout, ...).
    Failed,
    /// The device lost power. Used by the simulator; nothing after this succeeds.
    PowerLost,
}

/// A device addressed in [`BLOCK_SIZE`] blocks.
///
/// Durability contract: a completed `write` may sit in a volatile cache and
/// can be lost, reordered or torn (at 512-byte sector granularity) by a power
/// cut, until a later `flush` completes. After `flush` returns `Ok`, every
/// write that completed before it was issued is durable. This is the
/// contract of NVMe (Flush), SATA (FLUSH CACHE) and SCSI (SYNCHRONIZE CACHE).
///
/// Buffers are always a whole number of blocks; multi-block requests cover
/// consecutive blocks, so the driver can issue one large command.
pub trait BlockDevice {
    fn num_blocks(&self) -> u64;
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError>;
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError>;
    fn flush(&mut self) -> Result<(), IoError>;
}

impl<D: BlockDevice + ?Sized> BlockDevice for alloc::boxed::Box<D> {
    fn num_blocks(&self) -> u64 {
        (**self).num_blocks()
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        (**self).read(block, buf)
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        (**self).write(block, buf)
    }
    fn flush(&mut self) -> Result<(), IoError> {
        (**self).flush()
    }
}

impl<D: BlockDevice + ?Sized> BlockDevice for &mut D {
    fn num_blocks(&self) -> u64 {
        (**self).num_blocks()
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        (**self).read(block, buf)
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        (**self).write(block, buf)
    }
    fn flush(&mut self) -> Result<(), IoError> {
        (**self).flush()
    }
}

pub(crate) fn check_range(num_blocks: u64, block: u64, len: usize) -> Result<u64, IoError> {
    if len == 0 || !len.is_multiple_of(BLOCK_SIZE) {
        return Err(IoError::OutOfRange);
    }
    let n = (len / BLOCK_SIZE) as u64;
    match block.checked_add(n) {
        Some(end) if end <= num_blocks => Ok(n),
        _ => Err(IoError::OutOfRange),
    }
}

/// A RAM-backed device. Every write is immediately "durable".
#[derive(Clone)]
pub struct MemDevice {
    data: Vec<u8>,
}

impl core::fmt::Debug for MemDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MemDevice({} blocks)", self.data.len() / BLOCK_SIZE)
    }
}

impl MemDevice {
    pub fn new(blocks: u64) -> Self {
        MemDevice {
            data: vec![0; blocks as usize * BLOCK_SIZE],
        }
    }
    pub fn from_image(data: Vec<u8>) -> Self {
        assert!(data.len().is_multiple_of(BLOCK_SIZE));
        MemDevice { data }
    }
    pub fn image(&self) -> &[u8] {
        &self.data
    }
    pub fn image_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
    pub fn into_image(self) -> Vec<u8> {
        self.data
    }
}

impl BlockDevice for MemDevice {
    fn num_blocks(&self) -> u64 {
        (self.data.len() / BLOCK_SIZE) as u64
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        check_range(self.num_blocks(), block, buf.len())?;
        let s = block as usize * BLOCK_SIZE;
        buf.copy_from_slice(&self.data[s..s + buf.len()]);
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        check_range(self.num_blocks(), block, buf.len())?;
        let s = block as usize * BLOCK_SIZE;
        self.data[s..s + buf.len()].copy_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

/// A device backed by a host file or raw block device node.
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct FileDevice {
    file: std::fs::File,
    blocks: u64,
}

#[cfg(feature = "std")]
impl FileDevice {
    /// Opens (or creates and sizes) `path` with `blocks` blocks. If `blocks`
    /// is `None`, the existing size is used.
    pub fn open(path: &std::path::Path, blocks: Option<u64>) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(blocks.is_some())
            .truncate(false)
            .open(path)?;
        let blocks = match blocks {
            Some(b) => {
                file.set_len(b * BLOCK_SIZE as u64)?;
                b
            }
            None => file.metadata()?.len() / BLOCK_SIZE as u64,
        };
        Ok(FileDevice { file, blocks })
    }
}

#[cfg(feature = "std")]
impl BlockDevice for FileDevice {
    fn num_blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        use std::os::unix::fs::FileExt;
        check_range(self.blocks, block, buf.len())?;
        self.file
            .read_exact_at(buf, block * BLOCK_SIZE as u64)
            .map_err(|_| IoError::Failed)
    }
    fn write(&mut self, block: u64, buf: &[u8]) -> Result<(), IoError> {
        use std::os::unix::fs::FileExt;
        check_range(self.blocks, block, buf.len())?;
        self.file
            .write_all_at(buf, block * BLOCK_SIZE as u64)
            .map_err(|_| IoError::Failed)
    }
    fn flush(&mut self) -> Result<(), IoError> {
        self.file.sync_data().map_err(|_| IoError::Failed)
    }
}
