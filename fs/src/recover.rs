//! Crash-recovery verification at mount.
//!
//! A drive can acknowledge a flush and still lose the writes (a volatile
//! cache that lies, or firmware that reorders). Quarantine keeps every
//! block of the commits inside the window (at least
//! [`QUARANTINE_COMMITS`](crate::space::QUARANTINE_COMMITS) commits and the
//! configured time) intact, so if the newest commit is missing blocks an
//! older one is still whole. This module finds out which: it reads every
//! block born inside the window that the commit references. Copy-on-write
//! makes that cheap, because every block a recent commit changed sits
//! under pointers with a recent birth, so older subtrees are skipped
//! without being read.

use crate::crypto::BlockKind;
use crate::device::BlockDevice;
use crate::error::Result;
use crate::fs::{Extent, Filesystem, K_EXTENT};
use crate::ptr::BlockPtr;
use crate::snapshot::{K_SNAP, SnapRecord};
use crate::tree::{Child, TreeId};
use alloc::vec;
use alloc::vec::Vec;

/// What a recovery check read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryStats {
    pub nodes: u64,
    pub data_blocks: u64,
    pub space_blocks: u64,
    /// Files with a recent extent that failed to verify: (inode, extent's
    /// first block). Reads of those blocks fail with an error; nothing
    /// wrong is ever returned. Mount does not roll the whole volume back
    /// for these, because that would discard every other file's recent
    /// changes; the newest commit whose metadata is whole is mounted.
    pub damaged: Vec<(u64, u64)>,
}

/// Reads and verifies every block of the mounted commit born at or after
/// `min_birth`: tree nodes (live tree, snapshot trees, meta tree) and
/// space-map blocks, failing on the first bad one (the commit is then
/// unusable), and with `data` also file data, recording bad extents.
pub(crate) fn verify_since<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    min_birth: u64,
    data: bool,
) -> Result<RecoveryStats> {
    let mut st = RecoveryStats::default();
    let mut stack: Vec<(TreeId, BlockPtr)> = vec![(TreeId::Fs, fs.body.fs_root)];
    if !fs.body.meta_root.is_null() {
        stack.push((TreeId::Meta, fs.body.meta_root));
    }
    while let Some((tree, p)) = stack.pop() {
        if p.is_null() || p.birth < min_birth {
            continue;
        }
        let node = fs.store.read_node(tree, &p)?;
        st.nodes += 1;
        if node.level > 0 {
            for (_, c) in &node.kids {
                if let Child::Clean(cp) = c {
                    stack.push((tree, *cp));
                }
            }
            continue;
        }
        for (k, v) in &node.items {
            match tree {
                TreeId::Fs if k.kind == K_EXTENT => {
                    let e = Extent::decode(v, k.obj)?;
                    if data && e.birth >= min_birth {
                        st.data_blocks += e.len;
                        if fs.verify_extent(k.obj, &e).is_err()
                            && !st.damaged.contains(&(k.obj, e.phys))
                        {
                            st.damaged.push((k.obj, e.phys));
                        }
                    }
                }
                TreeId::Meta if k.kind == K_SNAP => {
                    let r = SnapRecord::decode(k.obj, v)?;
                    stack.push((TreeId::Fs, r.root));
                }
                _ => {}
            }
        }
    }
    st.space_blocks = fs.store.space.verify_since(&mut fs.store.io, min_birth)?;
    // The free log is read in full by every mount already.
    let _ = BlockKind::FreeLog;
    Ok(st)
}
