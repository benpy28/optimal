//! Full consistency checker.
//!
//! Verifies every block of the newest commit (MACs, structure) and every
//! cross-structure invariant: the space map marks exactly the referenced
//! blocks, no block has two owners, every tree is well-ordered, directory
//! and link counts agree, file data never extends past EOF (and the bytes
//! past EOF in the last block are zero), and each snapshot's deadlist holds
//! exactly the blocks the snapshot before it had and it no longer has.
//!
//! The torture tests run this after every simulated crash.

use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::fs::{
    Attr, Extent, FileKind, Filesystem, K_DIRENT, K_EXTENT, K_INLINE, K_INODE, ROOT_INO,
    decode_dirents,
};
use crate::chunk::{LOGICAL, is_logical};
use crate::ptr::BlockPtr;
use crate::chunk::Entry;
use crate::superblock::Body;
use crate::snapshot::{K_DEAD, K_SNAP, decode_dead, list_records};
use crate::space::{BITS_PER_BMP, SB_RESERVED_BACK, SB_RESERVED_FRONT};
use crate::tree::{Child, Key, TreeId};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub referenced_blocks: u64,
    pub inodes: u64,
    pub snapshots: u64,
    pub data_blocks_verified: u64,
    /// Metadata copies that fail verification (each is also an error).
    pub bad_copies: u64,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    FsNode,
    Data,
    MetaNode,
    SpaceTable,
    Bitmap,
    FreeLog,
    Root,
    /// A metadata chunk's copy 0 or copy 1 run.
    ChunkCopy,
    /// A chunk-table block (either copy).
    ChunkMap,
}

struct Ctx {
    rep: Report,
    /// addr -> (role, birth, mac). Logical (metadata) and physical
    /// addresses share the map; logical ones have bit 63 set.
    owners: BTreeMap<u64, (Role, u64, [u8; 16])>,
    /// Every metadata block claimed, to check both copies.
    metas: Vec<(BlockKind, BlockPtr)>,
    total: u64,
}

impl Ctx {
    fn err(&mut self, s: String) {
        if self.rep.errors.len() < 200 {
            self.rep.errors.push(s);
        }
    }
    /// Claims logical metadata block `p.addr` (checked against the chunk
    /// table later) and remembers it for the copy check.
    fn claim_meta(&mut self, kind: BlockKind, role: Role, p: &BlockPtr) {
        if !is_logical(p.addr) {
            self.err(format!("{role:?} pointer {} is not a logical address", p.addr));
            return;
        }
        let fresh = !self.owners.contains_key(&p.addr);
        self.claim(p.addr, 1, role, p.birth, p.mac);
        if fresh {
            self.metas.push((kind, *p));
        }
    }

    fn claim(&mut self, addr: u64, len: u64, role: Role, birth: u64, mac: [u8; 16]) {
        if is_logical(addr) {
            // Resolved through the chunk table in the logical pass.
        } else if addr < SB_RESERVED_FRONT
            || addr
                .checked_add(len)
                .is_none_or(|e| e > self.total - SB_RESERVED_BACK)
        {
            self.err(format!(
                "{role:?} block {addr}+{len} outside the usable area"
            ));
            return;
        }
        for b in addr..addr + len {
            match self.owners.get(&b) {
                None => {
                    self.owners.insert(b, (role, birth, mac));
                }
                Some(&(r, bi, m)) => {
                    // Snapshots share filesystem-tree blocks; the same block must
                    // then be referenced identically. Nothing else is shared.
                    let shareable = matches!(role, Role::FsNode | Role::Data);
                    if !(shareable && r == role && bi == birth && (role == Role::Data || m == mac))
                    {
                        self.err(format!("block {b} has two owners: {r:?} (birth {bi}) and {role:?} (birth {birth})"));
                    }
                }
            }
        }
    }
}

/// Checks the committed state of `fs`. `verify_data` also reads and
/// verifies every file data block.
pub fn check<D: BlockDevice>(fs: &mut Filesystem<D>, verify_data: bool) -> Result<Report> {
    let mut cx = Ctx {
        rep: Report::default(),
        owners: BTreeMap::new(),
        metas: Vec::new(),
        total: fs.store.space.total_blocks(),
    };
    let body = fs.body.clone();
    if fs.fs_tree.is_dirty() || fs.meta.is_dirty() {
        return Err(Error::Invalid("fsck needs a clean (committed) filesystem"));
    }

    // Snapshots and the live tree.
    let recs = list_records(fs)?;
    cx.rep.snapshots = recs.len() as u64;
    let mut roots: Vec<(String, BlockPtr, u64)> = recs
        .iter()
        .map(|r| (format!("snapshot {}", r.id), r.root, r.next_ino))
        .collect();
    roots.push((String::from("live"), body.fs_root, body.next_ino));
    let mut block_sets: Vec<BTreeMap<u64, u64>> = Vec::new();
    for (name, root, next_ino) in &roots {
        let mut blocks = BTreeMap::new();
        walk(
            fs,
            &mut cx,
            TreeId::Fs,
            root,
            Key::MIN,
            None,
            &mut blocks,
            name,
        )?;
        semantic(
            fs,
            &mut cx,
            root,
            *next_ino,
            name,
            verify_data && name == "live",
        )?;
        block_sets.push(blocks);
    }

    // Meta tree.
    let mut meta_blocks = BTreeMap::new();
    walk(
        fs,
        &mut cx,
        TreeId::Meta,
        &body.meta_root,
        Key::MIN,
        None,
        &mut meta_blocks,
        "meta",
    )?;
    check_meta(fs, &mut cx, &recs, &block_sets)?;

    // Space map, free log and root block.
    for (kind, p) in fs.store.space.all_pointers(&mut fs.store.io)? {
        let role = if kind == BlockKind::Bitmap {
            Role::Bitmap
        } else {
            Role::SpaceTable
        };
        cx.claim_meta(kind, role, &p);
    }
    let log = fs.store.space.log_ptrs(&mut fs.store.io, body.free_log)?;
    if log.iter().map(|p| p.addr).collect::<Vec<_>>() != fs.store.space.log_addrs() {
        cx.err(String::from("free log chain differs from the loaded free log"));
    }
    for p in &log {
        cx.claim_meta(BlockKind::FreeLog, Role::FreeLog, p);
    }
    cx.claim_meta(BlockKind::Root, Role::Root, &body.root_block);

    check_chunks(fs, &mut cx, &body)?;

    cx.rep.referenced_blocks = cx.owners.range(..LOGICAL).count() as u64
        + SB_RESERVED_FRONT
        + SB_RESERVED_BACK;
    let total = cx.total;
    let reserved = |b: u64| b < SB_RESERVED_FRONT || b >= total - SB_RESERVED_BACK;
    let mut mismatches = 0u64;
    // A never-written bitmap means "all free": nothing may live there.
    // Only written bitmaps are compared bit by bit, so sparse multi-terabyte
    // volumes stay fast.
    let written = fs.store.space.written_bitmaps(&mut fs.store.io)?;
    let written_set: BTreeSet<u64> = written.iter().copied().collect();
    let reserved_list = [0, 1, total - 2, total - 1];
    let in_null: Vec<u64> = cx
        .owners
        .range(..LOGICAL)
        .map(|(b, _)| b)
        .chain(reserved_list.iter())
        .copied()
        .filter(|b| !written_set.contains(&(b / BITS_PER_BMP)))
        .take(21)
        .collect();
    for b in in_null {
        mismatches += 1;
        cx.err(format!(
            "block {b}: space map says free (unwritten bitmap {}) but it is referenced",
            b / BITS_PER_BMP
        ));
    }
    for (n, i) in written.into_iter().enumerate() {
        let first = i * BITS_PER_BMP;
        let end = total.min(first + BITS_PER_BMP);
        for b in first..end {
            let used = fs.store.space.is_used(&mut fs.store.io, b)?;
            let referenced = reserved(b) || cx.owners.contains_key(&b);
            if used != referenced {
                mismatches += 1;
                if mismatches <= 20 {
                    cx.err(format!(
                        "block {b}: space map says {} but it is {}",
                        if used { "used" } else { "free" },
                        if referenced {
                            "referenced"
                        } else {
                            "unreferenced"
                        }
                    ));
                }
            }
        }
        if n % 1024 == 1023 {
            fs.store.space.evict_clean();
        }
    }
    if mismatches > 20 {
        cx.err(format!("{mismatches} space map mismatches in total"));
    }
    // Quarantined blocks must be free in the map and unreferenced.
    let mut q = fs.store.space.held_ranges();
    q.extend(fs.store.space.freed_now.to_vec());
    for (s, l) in q {
        for x in s..s + l {
            if cx.owners.contains_key(&x) {
                cx.err(format!("block {x} is in the free log but still referenced"));
            } else if fs.store.space.is_used(&mut fs.store.io, x)? {
                cx.err(format!("block {x} is in the free log but marked used"));
            }
        }
    }
    Ok(cx.rep)
}

/// The chunk table: every claimed logical block resolves, each chunk's
/// used bits are exactly its claimed blocks, quarantined logical blocks
/// are free and unclaimed, both copies of every metadata block verify,
/// and the chunk runs and table blocks are claimed physically.
fn check_chunks<D: BlockDevice>(fs: &mut Filesystem<D>, cx: &mut Ctx, body: &Body) -> Result<()> {
    let chunks = fs.store.space.chunks.all_entries(&mut fs.store.io)?;
    let mut mapped: BTreeMap<u64, Entry> = BTreeMap::new();
    for (id, e) in chunks {
        let len = e.len as u64;
        cx.claim(e.p0, len, Role::ChunkCopy, body.generation, [0; 16]);
        cx.claim(e.p1, len, Role::ChunkCopy, body.generation, [1; 16]);
        for off in 0..len {
            let l = fs.store.space.chunks.join(id, off);
            let referenced = cx.owners.contains_key(&l);
            if e.is_used(off) != referenced {
                cx.err(format!(
                    "logical block {l:#x} (chunk {id} slot {off}): chunk table says {} but it is {}",
                    if e.is_used(off) { "used" } else { "free" },
                    if referenced { "referenced" } else { "unreferenced" }
                ));
            }
        }
        mapped.insert(id, e);
    }
    let (free, count) = mapped.values().fold((0, 0), |(f, c), e| (f + e.free_slots(), c + 1));
    if (free, count)
        != (
            fs.store.space.chunks.free_slots(),
            fs.store.space.chunks.mapped_chunks(),
        )
    {
        cx.err(format!("chunk table totals wrong: walked {free} free / {count} mapped"));
    }
    let logical: Vec<u64> = cx.owners.range(LOGICAL..).map(|(&l, _)| l).collect();
    for l in logical {
        let ok = fs
            .store
            .space
            .chunks
            .split(l)
            .ok()
            .and_then(|(id, off)| mapped.get(&id).map(|e| off < e.len as u64))
            .unwrap_or(false);
        if !ok {
            cx.err(format!("logical block {l:#x} is referenced but not mapped"));
        }
    }
    let mut q = fs.store.space.held_lranges();
    q.extend(fs.store.space.freed_now_l.to_vec());
    for (s, n) in q {
        for l in s..s + n {
            let used = fs
                .store
                .space
                .chunks
                .split(l)
                .ok()
                .and_then(|(id, off)| mapped.get(&id).map(|e| e.is_used(off)))
                .unwrap_or(false);
            if cx.owners.contains_key(&l) || used {
                cx.err(format!("logical block {l:#x} is in the free log but in use"));
            }
        }
    }
    // Both copies of every metadata block.
    let metas = core::mem::take(&mut cx.metas);
    for (kind, p) in &metas {
        let Ok((p0, p1)) = fs.store.space.chunks.resolve(&mut fs.store.io, p.addr) else {
            continue;
        };
        let (ok0, ok1) = fs.store.io.verify_copies(*kind, p, p0, p1);
        for (ok, c, at) in [(ok0, 0, p0), (ok1, 1, p1)] {
            if !ok {
                cx.rep.bad_copies += 1;
                cx.err(format!("{kind:?} {:#x}: copy {c} (block {at}) fails verification", p.addr));
            }
        }
    }
    // Chunk-table blocks: claimed physically, both copies verified.
    for (level, p) in fs.store.space.chunks.all_node_ptrs(&mut fs.store.io)? {
        let kind = if level == 0 {
            BlockKind::ChunkLeaf
        } else {
            BlockKind::ChunkTable
        };
        cx.claim(p.addr, 1, Role::ChunkMap, p.birth, p.mac);
        cx.claim(p.aux, 1, Role::ChunkMap, p.birth, p.mac);
        let (ok0, ok1) = fs.store.io.verify_copies(kind, &p, p.addr, p.aux);
        for (ok, c, at) in [(ok0, 0, p.addr), (ok1, 1, p.aux)] {
            if !ok {
                cx.rep.bad_copies += 1;
                cx.err(format!("{kind:?} {:#x}: copy {c} (block {at}) fails verification", p.addr));
            }
        }
    }
    Ok(())
}

/// Structural walk of one tree: verifies each node and key bounds, claims blocks.
#[allow(clippy::too_many_arguments)]
fn walk<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    cx: &mut Ctx,
    tree: TreeId,
    p: &BlockPtr,
    lo: Key,
    hi: Option<Key>,
    blocks: &mut BTreeMap<u64, u64>,
    name: &str,
) -> Result<()> {
    if p.is_null() {
        if tree == TreeId::Fs {
            cx.err(format!("{name}: empty filesystem tree"));
        }
        return Ok(());
    }
    let role = if tree == TreeId::Fs {
        Role::FsNode
    } else {
        Role::MetaNode
    };
    cx.claim_meta(BlockKind::Node, role, p);
    blocks.insert(p.addr, 1);
    let node = match fs.store.read_node(tree, p) {
        Ok(n) => n,
        Err(e) => {
            cx.err(format!("{name}: node {} unreadable: {e}", p.addr));
            return Ok(());
        }
    };
    let in_bounds = |k: &Key| *k >= lo && hi.is_none_or(|h| *k < h);
    if node.level == 0 {
        for (k, v) in &node.items {
            if !in_bounds(k) {
                cx.err(format!(
                    "{name}: key {k:?} outside its node's range in block {}",
                    p.addr
                ));
            }
            if tree == TreeId::Fs
                && k.kind == K_EXTENT
                && let Ok(e) = Extent::decode(v, k.obj)
            {
                cx.claim(e.phys, e.len, Role::Data, e.birth, [0; 16]);
                if e.birth > p.birth {
                    cx.err(format!(
                        "{name}: extent born after its node (block {})",
                        p.addr
                    ));
                }
                blocks.insert(e.phys, e.len);
            }
        }
    } else {
        for (i, (k, c)) in node.kids.iter().enumerate() {
            if !in_bounds(k) {
                cx.err(format!(
                    "{name}: separator {k:?} outside its node's range in block {}",
                    p.addr
                ));
            }
            let chi = node.kids.get(i + 1).map(|x| x.0).or(hi);
            if let Child::Clean(cp) = c {
                walk(fs, cx, tree, cp, *k, chi, blocks, name)?;
            }
        }
    }
    Ok(())
}

fn semantic<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    cx: &mut Ctx,
    root: &BlockPtr,
    next_ino: u64,
    name: &str,
    verify_data: bool,
) -> Result<()> {
    let t = crate::tree::Tree::new(TreeId::Fs, *root)?;
    let mut items: Vec<(Key, Vec<u8>)> = Vec::new();
    if let Err(e) = t.scan(&mut fs.store, &Key::MIN, &Key::MAX, &mut |k, v| {
        items.push((k, v.to_vec()));
        Ok(true)
    }) {
        cx.err(format!("{name}: tree scan failed: {e}"));
        return Ok(());
    }
    let mut inodes: BTreeMap<u64, Attr> = BTreeMap::new();
    let mut entries: Vec<(u64, Vec<u8>, u64, FileKind)> = Vec::new();
    let mut extents: BTreeMap<u64, Vec<(u64, Extent)>> = BTreeMap::new();
    let mut inline: BTreeMap<u64, usize> = BTreeMap::new();
    for (k, v) in &items {
        if k.kind != K_INODE && !inodes.contains_key(&k.obj) {
            cx.err(format!("{name}: item {k:?} has no inode"));
            continue;
        }
        match k.kind {
            K_INODE => match Attr::decode(k.obj, v) {
                Ok(a) => {
                    if k.off != 0 {
                        cx.err(format!("{name}: inode key offset {k:?}"));
                    }
                    if k.obj >= next_ino {
                        cx.err(format!("{name}: inode {} >= next_ino {next_ino}", k.obj));
                    }
                    inodes.insert(k.obj, a);
                }
                Err(e) => cx.err(format!("{name}: inode {}: {e}", k.obj)),
            },
            K_DIRENT => match decode_dirents(v, k.obj) {
                Ok(b) => {
                    if inodes[&k.obj].kind != FileKind::Dir {
                        cx.err(format!("{name}: entries in non-directory {}", k.obj));
                    }
                    for (n, ino, kind) in b {
                        if fs.store.io.sealer.name_hash(&n) != k.off {
                            cx.err(format!(
                                "{name}: entry {:?} in dir {} has the wrong hash",
                                String::from_utf8_lossy(&n),
                                k.obj
                            ));
                        }
                        entries.push((k.obj, n, ino, kind));
                    }
                }
                Err(e) => cx.err(format!("{name}: entries of {}: {e}", k.obj)),
            },
            K_EXTENT => match Extent::decode(v, k.obj) {
                Ok(e) => extents.entry(k.obj).or_default().push((k.off, e)),
                Err(e) => cx.err(format!("{name}: extent of {}: {e}", k.obj)),
            },
            K_INLINE => {
                if k.off != 0 {
                    cx.err(format!("{name}: inline key offset {k:?}"));
                }
                inline.insert(k.obj, v.len());
            }
            _ => cx.err(format!("{name}: unknown item kind {k:?}")),
        }
    }
    cx.rep.inodes += inodes.len() as u64;
    match inodes.get(&ROOT_INO) {
        Some(r) if r.kind == FileKind::Dir && r.parent == ROOT_INO => {}
        _ => cx.err(format!("{name}: bad or missing root directory")),
    }
    // Link counts, directory sizes and parents.
    let mut links: BTreeMap<u64, u32> = BTreeMap::new();
    let mut dir_parent: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut subdirs: BTreeMap<u64, u32> = BTreeMap::new();
    let mut nentries: BTreeMap<u64, u64> = BTreeMap::new();
    let mut seen_names: BTreeSet<(u64, Vec<u8>)> = BTreeSet::new();
    for (dir, n, ino, kind) in &entries {
        if !seen_names.insert((*dir, n.clone())) {
            cx.err(format!(
                "{name}: duplicate name {:?} in dir {dir}",
                String::from_utf8_lossy(n)
            ));
        }
        *nentries.entry(*dir).or_default() += 1;
        match inodes.get(ino) {
            None => cx.err(format!(
                "{name}: entry {:?} in dir {dir} points to missing inode {ino}",
                String::from_utf8_lossy(n)
            )),
            Some(a) => {
                if a.kind != *kind {
                    cx.err(format!("{name}: entry kind mismatch for inode {ino}"));
                }
                if a.kind == FileKind::Dir {
                    dir_parent.entry(*ino).or_default().push(*dir);
                    *subdirs.entry(*dir).or_default() += 1;
                } else {
                    *links.entry(*ino).or_default() += 1;
                }
            }
        }
    }
    for (ino, a) in &inodes {
        match a.kind {
            FileKind::Dir => {
                let ps = dir_parent.get(ino).cloned().unwrap_or_default();
                if *ino == ROOT_INO {
                    if !ps.is_empty() {
                        cx.err(format!("{name}: root directory has a parent entry"));
                    }
                } else if ps.len() != 1 || ps[0] != a.parent {
                    cx.err(format!(
                        "{name}: directory {ino} has parents {ps:?}, records {}",
                        a.parent
                    ));
                }
                let want = 2 + subdirs.get(ino).copied().unwrap_or(0);
                if a.nlink != want {
                    cx.err(format!(
                        "{name}: directory {ino} nlink {} but {want} expected",
                        a.nlink
                    ));
                }
                let n = nentries.get(ino).copied().unwrap_or(0);
                if a.size != n {
                    cx.err(format!(
                        "{name}: directory {ino} size {} but {n} entries",
                        a.size
                    ));
                }
            }
            _ => {
                let n = links.get(ino).copied().unwrap_or(0);
                if a.nlink != n || n == 0 {
                    cx.err(format!(
                        "{name}: inode {ino} nlink {} but {n} entries",
                        a.nlink
                    ));
                }
                if a.parent != 0 {
                    cx.err(format!("{name}: non-directory {ino} records a parent"));
                }
            }
        }
        match (a.kind, inline.get(ino)) {
            (FileKind::Symlink, Some(&l)) if l as u64 == a.size && l > 0 => {}
            (FileKind::Symlink, _) => cx.err(format!(
                "{name}: symlink {ino} target missing or wrong size"
            )),
            (_, Some(_)) => cx.err(format!("{name}: inline data on non-symlink {ino}")),
            _ => {}
        }
        if a.kind != FileKind::File && extents.contains_key(ino) {
            cx.err(format!("{name}: extents on non-file {ino}"));
        }
    }
    // Every inode must be reachable from the root.
    let mut reach = BTreeSet::new();
    let mut stack = vec![ROOT_INO];
    let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (dir, _, ino, _) in &entries {
        children.entry(*dir).or_default().push(*ino);
    }
    while let Some(i) = stack.pop() {
        if reach.insert(i)
            && let Some(c) = children.get(&i)
        {
            stack.extend(c.iter().copied());
        }
    }
    for ino in inodes.keys() {
        if !reach.contains(ino) {
            cx.err(format!(
                "{name}: inode {ino} is not reachable from the root"
            ));
        }
    }
    // Extents: ordered, disjoint, within the file size; data verifies.
    for (ino, list) in &extents {
        let Some(a) = inodes.get(ino).cloned() else {
            continue;
        };
        let limit = a.size.div_ceil(BLOCK_SIZE as u64);
        let mut prev_end = 0u64;
        for (start, e) in list {
            let end = start + e.len;
            if *start < prev_end {
                cx.err(format!("{name}: overlapping extents in inode {ino}"));
            }
            if end > limit {
                cx.err(format!(
                    "{name}: inode {ino} has data past EOF (blocks {start}..{end}, size {})",
                    a.size
                ));
            }
            prev_end = end;
            if verify_data {
                let mut buf = vec![0u8; BLOCK_SIZE];
                for r in 0..e.len {
                    let addr = e.phys + r;
                    if fs.store.io.read_raw(addr, &mut buf).is_err() {
                        cx.err(format!("{name}: data block {addr} unreadable"));
                        continue;
                    }
                    let nonce = crate::crypto::nonce_add(&e.nonce, r);
                    if fs
                        .store
                        .io
                        .sealer
                        .open(
                            crate::crypto::BlockKind::Data,
                            addr,
                            e.birth,
                            &nonce,
                            &e.macs[r as usize],
                            &mut buf,
                        )
                        .is_err()
                    {
                        cx.err(format!(
                            "{name}: data block {addr} of inode {ino} fails verification"
                        ));
                        continue;
                    }
                    cx.rep.data_blocks_verified += 1;
                    let fb = start + r;
                    if fb + 1 == limit && a.size % BLOCK_SIZE as u64 != 0 {
                        let tail = (a.size % BLOCK_SIZE as u64) as usize;
                        if buf[tail..].iter().any(|&x| x != 0) {
                            cx.err(format!("{name}: stale bytes past EOF in inode {ino}"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn expand(set: &BTreeMap<u64, u64>) -> BTreeSet<u64> {
    let mut s = BTreeSet::new();
    for (&a, &l) in set {
        for b in a..a + l {
            s.insert(b);
        }
    }
    s
}

fn check_meta<D: BlockDevice>(
    fs: &mut Filesystem<D>,
    cx: &mut Ctx,
    recs: &[crate::snapshot::SnapRecord],
    block_sets: &[BTreeMap<u64, u64>],
) -> Result<()> {
    let body = fs.body.clone();
    let mut items: Vec<(Key, Vec<u8>)> = Vec::new();
    fs.meta
        .scan(&mut fs.store, &Key::MIN, &Key::MAX, &mut |k, v| {
            items.push((k, v.to_vec()));
            Ok(true)
        })?;
    let mut dls: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    let mut owners: BTreeSet<u64> = recs.iter().map(|r| r.deadlist).collect();
    if owners.len() != recs.len() || owners.contains(&body.live_deadlist) {
        cx.err(String::from("meta: deadlist ids are not unique"));
    }
    owners.insert(body.live_deadlist);
    for (k, v) in &items {
        match k.kind {
            K_SNAP => {
                if k.obj >= body.next_snap_id {
                    cx.err(format!(
                        "meta: snapshot id {} >= next {}",
                        k.obj, body.next_snap_id
                    ));
                }
            }
            K_DEAD => {
                if !owners.contains(&k.obj) {
                    cx.err(format!("meta: deadlist {} has no owner", k.obj));
                }
                if k.obj >= body.next_deadlist {
                    cx.err(format!(
                        "meta: deadlist id {} >= next {}",
                        k.obj, body.next_deadlist
                    ));
                }
                match decode_dead(v, k.obj) {
                    Ok(es) => {
                        let set = dls.entry(k.obj).or_default();
                        for (a, l, _) in es {
                            for b in a..a + l {
                                if !set.insert(b) {
                                    cx.err(format!("meta: block {b} twice in deadlist {}", k.obj));
                                }
                            }
                        }
                    }
                    Err(e) => cx.err(format!("meta: deadlist item {k:?}: {e}")),
                }
            }
            _ => cx.err(format!("meta: unknown item {k:?}")),
        }
    }
    let mut prev_gen = 0;
    for r in recs {
        if r.generation < prev_gen || r.generation > body.generation {
            cx.err(format!(
                "meta: snapshot {} generation {} out of order",
                r.id, r.generation
            ));
        }
        prev_gen = r.generation;
    }
    let latest = recs.last().map_or(0, |r| r.generation);
    if latest != body.latest_snap_gen {
        cx.err(format!(
            "meta: latest snapshot generation {} but superblock says {}",
            latest, body.latest_snap_gen
        ));
    }
    // The exact accounting invariant: D(x) == blocks(prev(x)) \ blocks(x).
    let mut dl_ids: Vec<u64> = recs.iter().map(|r| r.deadlist).collect();
    dl_ids.push(body.live_deadlist);
    let expanded: Vec<BTreeSet<u64>> = block_sets.iter().map(expand).collect();
    for (i, dl) in dl_ids.iter().enumerate() {
        let have = dls.get(dl).cloned().unwrap_or_default();
        let want: BTreeSet<u64> = if i == 0 {
            BTreeSet::new()
        } else {
            expanded[i - 1].difference(&expanded[i]).copied().collect()
        };
        if have != want {
            let extra = have.difference(&want).count();
            let missing = want.difference(&have).count();
            cx.err(format!(
                "meta: deadlist of {} wrong: {extra} extra blocks, {missing} missing",
                if i < recs.len() {
                    format!("snapshot {}", recs[i].id)
                } else {
                    String::from("live")
                }
            ));
        }
    }
    Ok(())
}
