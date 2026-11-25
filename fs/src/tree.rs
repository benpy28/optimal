//! Copy-on-write B+tree.
//!
//! Keys are `(object, kind, offset)` triples; values are byte strings of up
//! to [`MAX_VALUE`] bytes. Nodes are 4 KiB blocks. A node that is changed is
//! first copied into memory ("dirtied"), and its old block is released to
//! the space map or a snapshot's deadlist (see `snapshot.rs`). At commit,
//! dirty nodes are written bottom-up to freshly allocated blocks, each
//! sealed and referenced by a pointer carrying its MAC, birth generation and
//! level. Nothing on disk is ever overwritten in place.
//!
//! Invariants (checked by fsck, and by decode where cheap):
//! - all leaves are at level 0; a child's level is its parent's minus one;
//! - keys are strictly increasing within a node and across leaves;
//! - in an internal node, `kids[i].0` is <= every key under child `i`, and
//!   every key under child `i` is < `kids[i+1].0`;
//! - a child's birth generation is <= its parent's.

use crate::codec::{Reader, Writer};
use crate::crypto::BlockKind;
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Block, Io, zero_block};
use crate::ptr::{BlockPtr, PTR_LEN};
use crate::space::Space;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const NODE_HEADER: usize = 16;
pub const KEY_LEN: usize = 17;
pub const NODE_CAP: usize = BLOCK_SIZE - NODE_HEADER;
/// Largest value. Small enough that any two items fit in one node, so a
/// split always succeeds.
pub const MAX_VALUE: usize = 1900;
const LEAF_OVERHEAD: usize = KEY_LEN + 2;
const INTERNAL_ENTRY: usize = KEY_LEN + PTR_LEN;
pub const MAX_LEVEL: u8 = 16;
const NODE_MAGIC: u16 = 0x4e4f;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub obj: u64,
    pub kind: u8,
    pub off: u64,
}

impl Key {
    pub const MIN: Key = Key {
        obj: 0,
        kind: 0,
        off: 0,
    };
    pub const MAX: Key = Key {
        obj: u64::MAX,
        kind: u8::MAX,
        off: u64::MAX,
    };
    pub const fn new(obj: u64, kind: u8, off: u64) -> Key {
        Key { obj, kind, off }
    }
    fn encode(&self, w: &mut Writer) {
        w.u64(self.obj);
        w.u8(self.kind);
        w.u64(self.off);
    }
    fn decode(r: &mut Reader) -> Result<Key> {
        Ok(Key {
            obj: r.u64()?,
            kind: r.u8()?,
            off: r.u64()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeId {
    /// The filesystem tree (inodes, directories, extents). Snapshotted.
    Fs = 1,
    /// Volume metadata (snapshot records, deadlists). Never snapshotted.
    Meta = 2,
}

#[derive(Debug, Clone)]
pub enum Child {
    Clean(BlockPtr),
    Dirty(Box<Node>),
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub level: u8,
    pub items: Vec<(Key, Vec<u8>)>,
    pub kids: Vec<(Key, Child)>,
}

impl Node {
    fn size(&self) -> usize {
        if self.level == 0 {
            self.items
                .iter()
                .map(|(_, v)| LEAF_OVERHEAD + v.len())
                .sum()
        } else {
            self.kids.len() * INTERNAL_ENTRY
        }
    }
    fn is_empty(&self) -> bool {
        if self.level == 0 {
            self.items.is_empty()
        } else {
            self.kids.is_empty()
        }
    }
    fn min_key(&self) -> Key {
        if self.level == 0 {
            self.items[0].0
        } else {
            self.kids[0].0
        }
    }
    /// Index of the child whose range holds `key`.
    fn child_index(&self, key: &Key) -> usize {
        match self.kids.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    /// Splits off the upper part into a new node. Leaves split by bytes so
    /// both halves fit; internal nodes split by count.
    fn split(&mut self) -> Node {
        if self.level == 0 {
            let total = self.size();
            let mut acc = 0;
            let mut k = 0;
            for (i, (_, v)) in self.items.iter().enumerate() {
                let s = LEAF_OVERHEAD + v.len();
                if i > 0
                    && acc + s > total / 2
                    && (acc + s > NODE_CAP || acc * 2 >= total.saturating_sub(s))
                {
                    k = i;
                    break;
                }
                acc += s;
                k = i + 1;
            }
            let k = k.clamp(1, self.items.len() - 1);
            Node {
                level: 0,
                items: self.items.split_off(k),
                kids: Vec::new(),
            }
        } else {
            let k = self.kids.len() / 2;
            Node {
                level: self.level,
                items: Vec::new(),
                kids: self.kids.split_off(k),
            }
        }
    }

    pub(crate) fn encode(&self, tree: TreeId, birth: u64) -> Block {
        let mut b = zero_block();
        let mut w = Writer::new(&mut b[..]);
        w.u16(NODE_MAGIC);
        w.u8(self.level);
        w.u8(tree as u8);
        let n = if self.level == 0 {
            self.items.len()
        } else {
            self.kids.len()
        };
        w.u16(n as u16);
        w.u16(0);
        w.u64(birth);
        if self.level == 0 {
            for (k, v) in &self.items {
                k.encode(&mut w);
                w.u16(v.len() as u16);
                w.bytes(v);
            }
        } else {
            for (k, c) in &self.kids {
                k.encode(&mut w);
                match c {
                    Child::Clean(p) => p.encode(&mut w),
                    Child::Dirty(_) => unreachable!("encoding a node with dirty children"),
                }
            }
        }
        b
    }

    pub fn decode(b: &[u8], tree: TreeId, ptr: &BlockPtr) -> Result<Node> {
        let at = ptr.addr;
        let mut r = Reader::new(b, "tree node", at);
        let bad = || Error::Corrupt("malformed tree node", at);
        if r.u16()? != NODE_MAGIC {
            return Err(bad());
        }
        let level = r.u8()?;
        if level > MAX_LEVEL || level as u64 != ptr.aux || r.u8()? != tree as u8 {
            return Err(bad());
        }
        let n = r.u16()? as usize;
        if r.u16()? != 0 || r.u64()? != ptr.birth || n == 0 {
            return Err(bad());
        }
        let mut node = Node {
            level,
            items: Vec::new(),
            kids: Vec::new(),
        };
        let mut prev: Option<Key> = None;
        for _ in 0..n {
            let k = Key::decode(&mut r)?;
            if prev.is_some_and(|p| p >= k) {
                return Err(bad());
            }
            prev = Some(k);
            if level == 0 {
                let len = r.u16()? as usize;
                if len > MAX_VALUE {
                    return Err(bad());
                }
                node.items.push((k, r.bytes(len)?.to_vec()));
            } else {
                let p = BlockPtr::decode(&mut r)?;
                if p.is_null() || p.aux != level as u64 - 1 || p.birth > ptr.birth {
                    return Err(bad());
                }
                node.kids.push((k, Child::Clean(p)));
            }
        }
        if b[r.pos()..].iter().any(|&x| x != 0) {
            return Err(bad());
        }
        Ok(node)
    }
}

/// Decoded clean nodes, keyed by address and checked against the MAC on use.
#[derive(Debug)]
pub struct NodeCache {
    map: BTreeMap<u64, (BlockPtr, Arc<Node>)>,
    order: VecDeque<u64>,
    cap: usize,
}

impl NodeCache {
    pub fn new(cap: usize) -> Self {
        NodeCache {
            map: BTreeMap::new(),
            order: VecDeque::new(),
            cap: cap.max(16),
        }
    }
    fn get(&self, p: &BlockPtr) -> Option<Arc<Node>> {
        match self.map.get(&p.addr) {
            Some((q, n)) if q == p => Some(n.clone()),
            _ => None,
        }
    }
    fn put(&mut self, p: BlockPtr, n: Arc<Node>) {
        if self.map.insert(p.addr, (p, n)).is_none() {
            self.order.push_back(p.addr);
        }
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(a) => {
                    self.map.remove(&a);
                }
                None => break,
            }
        }
    }
}

/// Everything the trees need below them: I/O, the space map, a node cache,
/// and the snapshot rule for where released blocks go.
#[derive(Debug)]
pub struct Store<D> {
    pub io: Io<D>,
    pub space: Space,
    pub cache: NodeCache,
    /// Generation of the newest snapshot; blocks born at or before it are
    /// still referenced by that snapshot when the live tree drops them.
    pub latest_snap_gen: u64,
    /// Blocks the live tree dropped that a snapshot still holds:
    /// `(addr, len, birth)`, appended to the live deadlist at commit.
    pub dead_pending: Vec<(u64, u64, u64)>,
}

impl<D: BlockDevice> Store<D> {
    pub fn read_node(&mut self, tree: TreeId, p: &BlockPtr) -> Result<Arc<Node>> {
        if let Some(n) = self.cache.get(p) {
            return Ok(n);
        }
        let b = self.space.read_meta(&mut self.io, BlockKind::Node, p)?;
        let n = Arc::new(Node::decode(&b[..], tree, p)?);
        self.cache.put(*p, n.clone());
        Ok(n)
    }

    /// Drops a reference from `tree` to `[addr, addr+len)` born at `birth`.
    pub fn release(&mut self, tree: TreeId, addr: u64, len: u64, birth: u64) -> Result<()> {
        if tree == TreeId::Fs && birth <= self.latest_snap_gen {
            self.dead_pending.push((addr, len, birth));
            Ok(())
        } else {
            self.space.free(&mut self.io, addr, len)
        }
    }
}

fn make_dirty<'a, D: BlockDevice>(
    c: &'a mut Child,
    s: &mut Store<D>,
    tree: TreeId,
) -> Result<&'a mut Node> {
    if let Child::Clean(p) = c {
        let p = *p;
        let n = s.read_node(tree, &p)?;
        let node = (*n).clone();
        s.release(tree, p.addr, 1, p.birth)?;
        *c = Child::Dirty(Box::new(node));
    }
    match c {
        Child::Dirty(n) => Ok(n),
        Child::Clean(_) => unreachable!(),
    }
}

fn dirty_mut(c: &mut Child) -> &mut Node {
    match c {
        Child::Dirty(n) => n,
        Child::Clean(_) => unreachable!("expected a dirty node"),
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub id: TreeId,
    pub root: Option<Child>,
}

impl Tree {
    pub fn new(id: TreeId, root: BlockPtr) -> Result<Tree> {
        if !root.is_null() && root.aux > MAX_LEVEL as u64 {
            return Err(Error::Corrupt("tree root level", root.addr));
        }
        Ok(Tree {
            id,
            root: if root.is_null() {
                None
            } else {
                Some(Child::Clean(root))
            },
        })
    }

    pub fn is_dirty(&self) -> bool {
        matches!(self.root, Some(Child::Dirty(_)))
    }

    /// The committed root pointer. Only valid when the tree is clean.
    pub fn root_ptr(&self) -> BlockPtr {
        match &self.root {
            None => BlockPtr::NULL,
            Some(Child::Clean(p)) => *p,
            Some(Child::Dirty(_)) => panic!("root_ptr on a dirty tree"),
        }
    }

    pub fn get<D: BlockDevice>(&self, s: &mut Store<D>, key: &Key) -> Result<Option<Vec<u8>>> {
        let mut out = None;
        self.scan(s, key, &Key::MAX, &mut |k, v| {
            if k == *key {
                out = Some(v.to_vec());
            }
            Ok(false)
        })?;
        Ok(out)
    }

    /// Calls `f` on every item with `start <= key < end`, in order, until it returns false.
    pub fn scan<D: BlockDevice>(
        &self,
        s: &mut Store<D>,
        start: &Key,
        end: &Key,
        f: &mut dyn FnMut(Key, &[u8]) -> Result<bool>,
    ) -> Result<()> {
        if let Some(root) = &self.root {
            scan_rec(root, s, self.id, start, end, f)?;
        }
        Ok(())
    }

    /// Last item with `start <= key < end`.
    pub fn last<D: BlockDevice>(
        &self,
        s: &mut Store<D>,
        start: &Key,
        end: &Key,
    ) -> Result<Option<(Key, Vec<u8>)>> {
        match &self.root {
            None => Ok(None),
            Some(root) => last_rec(root, s, self.id, start, end),
        }
    }

    pub fn insert<D: BlockDevice>(
        &mut self,
        s: &mut Store<D>,
        key: Key,
        val: Vec<u8>,
    ) -> Result<()> {
        if val.len() > MAX_VALUE {
            return Err(Error::Invalid("tree value too large"));
        }
        let id = self.id;
        match &mut self.root {
            None => {
                self.root = Some(Child::Dirty(Box::new(Node {
                    level: 0,
                    items: alloc::vec![(key, val)],
                    kids: Vec::new(),
                })));
            }
            Some(root) => {
                insert_rec(root, s, id, key, val)?;
                let r = dirty_mut(root);
                if r.size() > NODE_CAP {
                    if r.level >= MAX_LEVEL {
                        return Err(Error::NoSpace);
                    }
                    let right = r.split();
                    let level = r.level + 1;
                    let left = core::mem::replace(root, Child::Clean(BlockPtr::NULL));
                    let lk = match &left {
                        Child::Dirty(n) => n.min_key(),
                        Child::Clean(_) => unreachable!(),
                    };
                    *root = Child::Dirty(Box::new(Node {
                        level,
                        items: Vec::new(),
                        kids: alloc::vec![
                            (lk, left),
                            (right.min_key(), Child::Dirty(Box::new(right)))
                        ],
                    }));
                }
            }
        }
        Ok(())
    }

    /// Removes `key`; returns whether it was present. A miss dirties nothing.
    pub fn remove<D: BlockDevice>(&mut self, s: &mut Store<D>, key: &Key) -> Result<bool> {
        if self.get(s, key)?.is_none() {
            return Ok(false);
        }
        let id = self.id;
        let root = self.root.as_mut().unwrap();
        remove_rec(root, s, id, key)?;
        loop {
            let r = dirty_mut(self.root.as_mut().unwrap());
            if r.is_empty() {
                self.root = None;
                break;
            }
            if r.level > 0 && r.kids.len() == 1 {
                let only = r.kids.pop().unwrap().1;
                self.root = Some(only);
                if matches!(self.root, Some(Child::Clean(_))) {
                    break;
                }
                continue;
            }
            break;
        }
        Ok(true)
    }

    /// Writes every dirty node and returns the new root pointer.
    pub fn write<D: BlockDevice>(&mut self, s: &mut Store<D>, generation: u64) -> Result<BlockPtr> {
        let id = self.id;
        match &mut self.root {
            None => Ok(BlockPtr::NULL),
            Some(Child::Clean(p)) => Ok(*p),
            Some(root) => {
                let n = count_dirty(root);
                let runs = s.space.alloc_meta(&mut s.io, n)?;
                let mut addrs = runs.into_iter().flat_map(|(a, l)| a..a + l);
                write_rec(root, s, id, generation, &mut addrs)?;
                debug_assert!(addrs.next().is_none());
                Ok(match root {
                    Child::Clean(p) => *p,
                    Child::Dirty(_) => unreachable!(),
                })
            }
        }
    }
}

fn with_node<D: BlockDevice, R>(
    c: &Child,
    s: &mut Store<D>,
    tree: TreeId,
    f: impl FnOnce(&Node, &mut Store<D>) -> Result<R>,
) -> Result<R> {
    match c {
        Child::Dirty(n) => f(n, s),
        Child::Clean(p) => {
            let n = s.read_node(tree, p)?;
            f(&n, s)
        }
    }
}

fn scan_rec<D: BlockDevice>(
    c: &Child,
    s: &mut Store<D>,
    tree: TreeId,
    start: &Key,
    end: &Key,
    f: &mut dyn FnMut(Key, &[u8]) -> Result<bool>,
) -> Result<bool> {
    with_node(c, s, tree, |node, s| {
        if node.level == 0 {
            let i0 = node.items.partition_point(|(k, _)| k < start);
            for (k, v) in &node.items[i0..] {
                if k >= end {
                    return Ok(false);
                }
                if !f(*k, v)? {
                    return Ok(false);
                }
            }
            Ok(true)
        } else {
            let i0 = node.child_index(start);
            for (i, (k, child)) in node.kids.iter().enumerate().skip(i0) {
                if i > i0 && k >= end {
                    return Ok(false);
                }
                if !scan_rec(child, s, tree, start, end, f)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    })
}

fn last_rec<D: BlockDevice>(
    c: &Child,
    s: &mut Store<D>,
    tree: TreeId,
    start: &Key,
    end: &Key,
) -> Result<Option<(Key, Vec<u8>)>> {
    with_node(c, s, tree, |node, s| {
        if node.level == 0 {
            let i = node.items.partition_point(|(k, _)| k < end);
            if i > 0 && node.items[i - 1].0 >= *start {
                return Ok(Some((node.items[i - 1].0, node.items[i - 1].1.clone())));
            }
            Ok(None)
        } else {
            // Children whose lower bound is < end, newest first.
            let mut i = node.kids.partition_point(|(k, _)| k < end);
            while i > 0 {
                i -= 1;
                if let Some(r) = last_rec(&node.kids[i].1, s, tree, start, end)? {
                    return Ok(Some(r));
                }
                if node.kids[i].0 <= *start {
                    break;
                }
            }
            Ok(None)
        }
    })
}

fn insert_rec<D: BlockDevice>(
    c: &mut Child,
    s: &mut Store<D>,
    tree: TreeId,
    key: Key,
    val: Vec<u8>,
) -> Result<()> {
    let node = make_dirty(c, s, tree)?;
    if node.level == 0 {
        match node.items.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(i) => node.items[i].1 = val,
            Err(i) => node.items.insert(i, (key, val)),
        }
        return Ok(());
    }
    let i = node.child_index(&key);
    if key < node.kids[i].0 {
        node.kids[i].0 = key;
    }
    insert_rec(&mut node.kids[i].1, s, tree, key, val)?;
    let child = dirty_mut(&mut node.kids[i].1);
    if child.size() > NODE_CAP {
        let right = child.split();
        node.kids
            .insert(i + 1, (right.min_key(), Child::Dirty(Box::new(right))));
    }
    Ok(())
}

fn remove_rec<D: BlockDevice>(
    c: &mut Child,
    s: &mut Store<D>,
    tree: TreeId,
    key: &Key,
) -> Result<()> {
    let node = make_dirty(c, s, tree)?;
    if node.level == 0 {
        if let Ok(i) = node.items.binary_search_by(|(k, _)| k.cmp(key)) {
            node.items.remove(i);
        }
        return Ok(());
    }
    let i = node.child_index(key);
    remove_rec(&mut node.kids[i].1, s, tree, key)?;
    let child = dirty_mut(&mut node.kids[i].1);
    if child.is_empty() {
        node.kids.remove(i);
    } else if child.size() < NODE_CAP / 4 && node.kids.len() > 1 {
        rebalance(node, i, s, tree)?;
    }
    Ok(())
}

/// Merges child `i` with a neighbour, splitting again if the result is too big.
fn rebalance<D: BlockDevice>(
    node: &mut Node,
    i: usize,
    s: &mut Store<D>,
    tree: TreeId,
) -> Result<()> {
    let (l, r) = if i + 1 < node.kids.len() {
        (i, i + 1)
    } else {
        (i - 1, i)
    };
    make_dirty(&mut node.kids[r].1, s, tree)?;
    let right = match node.kids.remove(r).1 {
        Child::Dirty(n) => n,
        Child::Clean(_) => unreachable!(),
    };
    let left = make_dirty(&mut node.kids[l].1, s, tree)?;
    let right = *right;
    left.items.extend(right.items);
    left.kids.extend(right.kids);
    if left.size() > NODE_CAP {
        let nr = left.split();
        node.kids
            .insert(l + 1, (nr.min_key(), Child::Dirty(Box::new(nr))));
    }
    Ok(())
}

fn count_dirty(c: &Child) -> u64 {
    match c {
        Child::Clean(_) => 0,
        Child::Dirty(n) => 1 + n.kids.iter().map(|(_, k)| count_dirty(k)).sum::<u64>(),
    }
}

fn write_rec<D: BlockDevice>(
    c: &mut Child,
    s: &mut Store<D>,
    tree: TreeId,
    generation: u64,
    addrs: &mut dyn Iterator<Item = u64>,
) -> Result<()> {
    let Child::Dirty(node) = c else { return Ok(()) };
    for (_, k) in node.kids.iter_mut() {
        write_rec(k, s, tree, generation, addrs)?;
    }
    let addr = addrs.next().expect("allocated too few node blocks");
    let b = node.encode(tree, generation);
    let p = s.space.write_meta(
        &mut s.io,
        BlockKind::Node,
        addr,
        generation,
        b,
        node.level as u64,
    )?;
    let node = core::mem::take(&mut **node);
    s.cache.put(p, Arc::new(node));
    *c = Child::Clean(p);
    Ok(())
}
