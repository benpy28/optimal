//! Snapshots, deadlists and rollback.
//!
//! A snapshot is a read-only copy of the filesystem tree as of a commit. It
//! costs one record: it shares every block with the live tree.
//!
//! Space accounting uses birth generations. Snapshots `s_1 < ... < s_k` (by
//! generation) and the live tree each own a *deadlist*: `D(x)` holds the
//! blocks that were in the previous snapshot `p(x)` but are no longer in `x`
//! (equivalently: born at or before `p(x)`, dropped after it). When the live
//! tree drops a block born at or before `s_k`, `s_k` still holds it, so it
//! goes on `D(live)` instead of being freed. Blocks born after `s_k` are
//! freed at once.
//!
//! - Taking a snapshot: the live deadlist becomes the snapshot's; live starts
//!   an empty one. O(1).
//! - Deleting `s_j` (previous `p`, next `n`): entries of `D(n)` born after
//!   `p` were only in `s_j` and are freed; the rest are still in `p` and,
//!   together with `D(s_j)`, form the new `D(n)`. O(|D(n)|).
//! - Rollback to `s_j`: delete every newer snapshot, free live blocks born
//!   after `s_j` (a pruned tree walk, since a node is never younger than its
//!   children), drop `D(live)` (all still held by `s_j`) and make `s_j`'s
//!   root live again.
//!
//! fsck checks the exact invariant `blocks(p(x)) \ blocks(x) == D(x)` for
//! every snapshot and the live tree, so any accounting bug is caught.

use crate::codec::{Reader, Writer};
use crate::device::BlockDevice;
use crate::error::{Error, Result};
use crate::fs::{Extent, Filesystem, K_EXTENT};
use crate::ptr::BlockPtr;
use crate::tree::{Child, Key, MAX_VALUE, Tree, TreeId};
use alloc::vec;
use alloc::vec::Vec;

pub(crate) const K_SNAP: u8 = 10;
pub(crate) const K_DEAD: u8 = 11;
const DEAD_ENTRY: usize = 24;

/// `(addr, len, birth)` entries of one deadlist item.
pub(crate) type DeadEntries = Vec<(u64, u64, u64)>;
/// `(seq, entries)` items of a deadlist.
pub(crate) type DeadItems = Vec<(u64, DeadEntries)>;
const DEAD_PER_ITEM: usize = (MAX_VALUE - 2) / DEAD_ENTRY;
pub const SNAP_NAME_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub id: u64,
    pub generation: u64,
    pub name: Vec<u8>,
    pub created_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapRecord {
    pub id: u64,
    pub generation: u64,
    pub root: BlockPtr,
    pub deadlist: u64,
    pub next_ino: u64,
    pub created_ns: i64,
    pub name: Vec<u8>,
}

impl SnapRecord {
    fn encode(&self) -> Vec<u8> {
        let mut v = vec![0u8; 8 + 64 + 8 + 8 + 8 + 1 + self.name.len()];
        let mut w = Writer::new(&mut v);
        w.u64(self.generation);
        self.root.encode(&mut w);
        w.u64(self.deadlist);
        w.u64(self.next_ino);
        w.i64(self.created_ns);
        w.u8(self.name.len() as u8);
        w.bytes(&self.name);
        v
    }
    pub(crate) fn decode(id: u64, v: &[u8]) -> Result<SnapRecord> {
        let mut r = Reader::new(v, "snapshot record", id);
        let generation = r.u64()?;
        let root = BlockPtr::decode(&mut r)?;
        let deadlist = r.u64()?;
        let next_ino = r.u64()?;
        let created_ns = r.i64()?;
        let n = r.u8()? as usize;
        if n > SNAP_NAME_MAX {
            return Err(r.err());
        }
        let name = r.bytes(n)?.to_vec();
        r.finish()?;
        if root.is_null() {
            return Err(r.err());
        }
        Ok(SnapRecord {
            id,
            generation,
            root,
            deadlist,
            next_ino,
            created_ns,
            name,
        })
    }
}

pub(crate) fn encode_dead(entries: &[(u64, u64, u64)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + entries.len() * DEAD_ENTRY);
    v.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (a, l, b) in entries {
        v.extend_from_slice(&a.to_le_bytes());
        v.extend_from_slice(&l.to_le_bytes());
        v.extend_from_slice(&b.to_le_bytes());
    }
    v
}

pub(crate) fn decode_dead(v: &[u8], at: u64) -> Result<Vec<(u64, u64, u64)>> {
    let mut r = Reader::new(v, "deadlist", at);
    let n = r.u16()? as usize;
    if n == 0 || n > DEAD_PER_ITEM {
        return Err(r.err());
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let a = r.u64()?;
        let l = r.u64()?;
        let b = r.u64()?;
        if l == 0 || a.checked_add(l).is_none() {
            return Err(r.err());
        }
        out.push((a, l, b));
    }
    r.finish()?;
    Ok(out)
}

/// All `(seq, entries)` items of deadlist `dl`.
pub(crate) fn read_deadlist<D: BlockDevice>(fs: &mut Filesystem<D>, dl: u64) -> Result<DeadItems> {
    let mut items = Vec::new();
    let mut err = None;
    fs.meta.scan(
        &mut fs.store,
        &Key::new(dl, K_DEAD, 0),
        &Key::new(dl, K_DEAD + 1, 0),
        &mut |k, v| {
            match decode_dead(v, dl) {
                Ok(e) => items.push((k.off, e)),
                Err(e) => {
                    err = Some(e);
                    return Ok(false);
                }
            }
            Ok(true)
        },
    )?;
    match err {
        Some(e) => Err(e),
        None => Ok(items),
    }
}

fn append_deadlist<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    dl: u64,
    entries: &[(u64, u64, u64)],
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let seq = match fs.meta.last(
        &mut fs.store,
        &Key::new(dl, K_DEAD, 0),
        &Key::new(dl, K_DEAD + 1, 0),
    )? {
        Some((k, _)) => k.off + 1,
        None => 0,
    };
    for (i, chunk) in entries.chunks(DEAD_PER_ITEM).enumerate() {
        let k = Key::new(dl, K_DEAD, seq + i as u64);
        fs.meta.insert(&mut fs.store, k, encode_dead(chunk))?;
    }
    Ok(())
}

fn delete_deadlist<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    dl: u64,
    items: &[(u64, DeadEntries)],
) -> Result<()> {
    for (seq, _) in items {
        fs.meta.remove(&mut fs.store, &Key::new(dl, K_DEAD, *seq))?;
    }
    Ok(())
}

/// Moves blocks the live tree dropped (but a snapshot holds) onto the live deadlist.
/// The pending list is cleared only once all of it is on the deadlist.
pub(crate) fn flush_deadlist<D: BlockDevice>(fs: &mut Filesystem<D>) -> Result<()> {
    let pending = core::mem::take(&mut fs.store.dead_pending);
    let dl = fs.live_deadlist;
    let r = append_deadlist(fs, dl, &pending);
    if r.is_err() {
        fs.store.dead_pending = pending;
    }
    r
}

pub(crate) fn list_records<D: BlockDevice>(fs: &mut Filesystem<D>) -> Result<Vec<SnapRecord>> {
    let mut out = Vec::new();
    let mut err = None;
    fs.meta
        .scan(&mut fs.store, &Key::MIN, &Key::MAX, &mut |k, v| {
            if k.kind == K_SNAP {
                match SnapRecord::decode(k.obj, v) {
                    Ok(r) => out.push(r),
                    Err(e) => {
                        err = Some(e);
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })?;
    if let Some(e) = err {
        return Err(e);
    }
    out.sort_by_key(|r| r.id);
    Ok(out)
}

impl<D: BlockDevice> Filesystem<D> {
    /// Takes a read-only snapshot of the filesystem as it is now (committing
    /// first). Returns its id.
    pub fn snapshot_create(&mut self, name: &[u8]) -> Result<u64> {
        if name.len() > SNAP_NAME_MAX {
            return Err(Error::NameTooLong);
        }
        self.commit()?;
        self.op_whole(|fs| fs.snapshot_create_inner(name))
    }

    fn snapshot_create_inner(&mut self, name: &[u8]) -> Result<u64> {
        let id = self.next_snap_id;
        let rec = SnapRecord {
            id,
            generation: self.body.generation,
            root: self.body.fs_root,
            deadlist: self.live_deadlist,
            next_ino: self.next_ino,
            created_ns: self.body.commit_time_ns,
            name: name.to_vec(),
        };
        self.next_snap_id += 1;
        self.live_deadlist = self.next_deadlist;
        self.next_deadlist += 1;
        self.store.latest_snap_gen = rec.generation;
        self.meta
            .insert(&mut self.store, Key::new(id, K_SNAP, 0), rec.encode())?;
        self.mark_dirty();
        self.commit()?;
        Ok(id)
    }

    pub fn snapshot_list(&mut self) -> Result<Vec<SnapshotInfo>> {
        Ok(list_records(self)?
            .into_iter()
            .map(|r| SnapshotInfo {
                id: r.id,
                generation: r.generation,
                name: r.name,
                created_ns: r.created_ns,
            })
            .collect())
    }

    /// Deletes a snapshot and frees the blocks only it held.
    pub fn snapshot_delete(&mut self, id: u64) -> Result<()> {
        self.commit()?;
        self.op_whole(|fs| {
            fs.snapshot_delete_inner(id)?;
            fs.mark_dirty();
            fs.commit()
        })
    }

    fn snapshot_delete_inner(&mut self, id: u64) -> Result<()> {
        let recs = list_records(self)?;
        let j = recs
            .iter()
            .position(|r| r.id == id)
            .ok_or(Error::NotFound)?;
        let s = &recs[j];
        let p_gen = if j > 0 { recs[j - 1].generation } else { 0 };
        let (n_dl, n_rec) = match recs.get(j + 1) {
            Some(n) => (n.deadlist, Some(n.clone())),
            None => (self.live_deadlist, None),
        };
        let items = read_deadlist(self, n_dl)?;
        let mut keep = Vec::new();
        for (_, entries) in &items {
            for &(a, l, b) in entries {
                if b > p_gen {
                    self.store.free(a, l)?;
                } else {
                    keep.push((a, l, b));
                }
            }
        }
        delete_deadlist(self, n_dl, &items)?;
        append_deadlist(self, s.deadlist, &keep)?;
        match n_rec {
            Some(mut n) => {
                n.deadlist = s.deadlist;
                self.meta
                    .insert(&mut self.store, Key::new(n.id, K_SNAP, 0), n.encode())?;
            }
            None => {
                self.live_deadlist = s.deadlist;
                self.store.latest_snap_gen = p_gen;
            }
        }
        self.meta
            .remove(&mut self.store, &Key::new(id, K_SNAP, 0))?;
        Ok(())
    }

    /// Returns the whole filesystem to snapshot `id`. Every newer snapshot
    /// and every change since is discarded; snapshot `id` itself is kept.
    ///
    /// All or nothing up to each of its commits: if it fails part way,
    /// what its last completed commit made durable stays (newer snapshots
    /// already deleted, say) and everything after is undone.
    pub fn rollback(&mut self, id: u64) -> Result<()> {
        self.commit()?;
        self.op_whole(|fs| fs.rollback_inner(id))
    }

    fn rollback_inner(&mut self, id: u64) -> Result<()> {
        let recs = list_records(self)?;
        let j = recs
            .iter()
            .position(|r| r.id == id)
            .ok_or(Error::NotFound)?;
        for newer in recs[j + 1..].iter().rev() {
            self.snapshot_delete_inner(newer.id)?;
        }
        // Commit so the live tree is clean, then free what is younger than the snapshot.
        self.mark_dirty();
        self.commit()?;
        let s = recs[j].clone();
        debug_assert_eq!(self.store.latest_snap_gen, s.generation);
        let live_root = self.fs_tree.root_ptr();
        free_younger(self, &live_root, s.generation)?;
        let dl = self.live_deadlist;
        let items = read_deadlist(self, dl)?;
        delete_deadlist(self, dl, &items)?;
        self.fs_tree = Tree {
            id: TreeId::Fs,
            root: Some(Child::Clean(s.root)),
        };
        self.next_ino = self.next_ino.max(s.next_ino);
        self.mark_dirty();
        self.commit()
    }
}

/// Frees every block reachable from `p` that was born after `generation`.
fn free_younger<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    p: &BlockPtr,
    generation: u64,
) -> Result<()> {
    if p.is_null() || p.birth <= generation {
        return Ok(());
    }
    let node = fs.store.read_node(TreeId::Fs, p)?;
    if node.level == 0 {
        for (k, v) in &node.items {
            if k.kind == K_EXTENT {
                let e = Extent::decode(v, k.obj)?;
                if e.birth > generation {
                    fs.store.free(e.phys, e.len)?;
                }
            }
        }
    } else {
        for (_, c) in &node.kids {
            if let Child::Clean(cp) = c {
                free_younger(fs, cp, generation)?;
            }
        }
    }
    fs.store.free(p.addr, 1)
}
