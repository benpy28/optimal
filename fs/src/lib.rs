//! OptimalFS: the Optimal OS filesystem.
//!
//! A copy-on-write filesystem where every block (data and metadata) is
//! checksummed or authenticated-encrypted, commits are atomic, and whole-
//! filesystem snapshots are instant. The on-disk format is specified in
//! `docs/FORMAT.md`; the design reasoning is in `docs/DESIGN.md`.
//!
//! The crate is `no_std` + `alloc` so the kernel can link it directly. All
//! I/O goes through the [`BlockDevice`] trait.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

extern crate alloc;

pub mod chunk;
mod codec;
pub mod crypto;
pub mod device;
#[cfg(test)]
mod dup_tests;
mod error;
mod fs;
pub mod fsck;
mod io;
mod ptr;
mod rangeset;
mod recover;
pub mod sim;
mod snapshot;
mod space;
mod superblock;
#[cfg(test)]
mod tests;
mod tree;

#[cfg(feature = "std")]
pub use crypto::OsEntropy;
pub use crypto::{Entropy, KdfParams};
#[cfg(feature = "std")]
pub use device::FileDevice;
pub use device::{BLOCK_SIZE, BlockDevice, IoError, MemDevice};
pub use error::{Error, Result};
pub use fs::{
    Attr, Clock, DirEntry, FileKind, Filesystem, FormatOptions, MountOptions, ROOT_INO, SetAttr,
    StatFs, ZeroClock, auto_chunk_blocks,
};
pub use io::IoStats;
pub use recover::RecoveryStats;
pub use snapshot::SnapshotInfo;
