//! Shared torture-test machinery: a reference model of the filesystem,
//! a random operation generator, and a harness that runs each operation
//! against both and compares results and full state.
#![allow(dead_code)]

use optimalfs::sim::{CrashDevice, Rng};
use optimalfs::*;
use std::collections::BTreeMap;

pub const LINK_MAX: u32 = 65000;
pub const NAME_MAX: usize = 255;
pub const MAX_FILE_SIZE: u64 = 1 << 60;
pub const BS: u64 = 4096;

/// How hard to push: `OFS_TORTURE_SCALE` multiplies iteration counts (CI soak sets it high).
pub fn scale() -> u64 {
    std::env::var("OFS_TORTURE_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

/// Base seed: `OFS_SEED` reproduces a failing run.
pub fn base_seed() -> u64 {
    std::env::var("OFS_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x0f5_2026)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MNode {
    pub kind: FileKind,
    pub perm: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub parent: u64,
    pub data: Vec<u8>,
    pub target: Vec<u8>,
    pub entries: BTreeMap<Vec<u8>, u64>,
}

impl MNode {
    pub fn size(&self) -> u64 {
        match self.kind {
            FileKind::File => self.data.len() as u64,
            FileKind::Dir => self.entries.len() as u64,
            FileKind::Symlink => self.target.len() as u64,
        }
    }
}

/// Model of one filesystem tree (live or a snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MFs {
    pub nodes: BTreeMap<u64, MNode>,
    pub next_ino: u64,
}

/// Model of the whole volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub live: MFs,
    pub snaps: BTreeMap<u64, MFs>,
    pub next_snap: u64,
}

fn valid_name(n: &[u8]) -> bool {
    !n.is_empty()
        && n.len() <= NAME_MAX
        && n != b"."
        && n != b".."
        && !n.contains(&b'/')
        && !n.contains(&0)
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Model {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT_INO,
            MNode {
                kind: FileKind::Dir,
                perm: 0o755,
                uid: 0,
                gid: 0,
                nlink: 2,
                parent: ROOT_INO,
                data: vec![],
                target: vec![],
                entries: BTreeMap::new(),
            },
        );
        Model {
            live: MFs { nodes, next_ino: 2 },
            snaps: BTreeMap::new(),
            next_snap: 1,
        }
    }
}

impl MFs {
    fn dir(&self, d: u64) -> Result<&MNode> {
        match self.nodes.get(&d) {
            None => Err(Error::NotFound),
            Some(n) if n.kind != FileKind::Dir => Err(Error::NotDir),
            Some(n) => Ok(n),
        }
    }
    pub fn lookup(&self, d: u64, name: &[u8]) -> Result<u64> {
        let dn = self.dir(d)?;
        if name.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        dn.entries.get(name).copied().ok_or(Error::NotFound)
    }
    fn check_new(&self, d: u64, name: &[u8]) -> Result<()> {
        if name.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        self.dir(d)?;
        match self.lookup(d, name) {
            Ok(_) => Err(Error::Exists),
            Err(Error::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn create(&mut self, d: u64, name: &[u8], kind: FileKind, perm: u32) -> Result<u64> {
        self.check_new(d, name)?;
        if kind == FileKind::Dir && self.nodes[&d].nlink >= LINK_MAX {
            return Err(Error::TooManyLinks);
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.nodes.insert(
            ino,
            MNode {
                kind,
                perm: perm & 0o7777,
                uid: 0,
                gid: 0,
                nlink: if kind == FileKind::Dir { 2 } else { 1 },
                parent: if kind == FileKind::Dir { d } else { 0 },
                data: vec![],
                target: vec![],
                entries: BTreeMap::new(),
            },
        );
        let dn = self.nodes.get_mut(&d).unwrap();
        dn.entries.insert(name.to_vec(), ino);
        if kind == FileKind::Dir {
            dn.nlink += 1;
        }
        Ok(ino)
    }
    fn symlink(&mut self, d: u64, name: &[u8], target: &[u8]) -> Result<u64> {
        if target.is_empty() || target.len() > 1024 || target.contains(&0) {
            return Err(Error::Invalid("symlink target"));
        }
        let ino = self.create(d, name, FileKind::Symlink, 0o777)?;
        self.nodes.get_mut(&ino).unwrap().target = target.to_vec();
        Ok(ino)
    }
    fn link(&mut self, ino: u64, d: u64, name: &[u8]) -> Result<()> {
        let n = self.nodes.get(&ino).ok_or(Error::NotFound)?;
        if n.kind == FileKind::Dir {
            return Err(Error::IsDir);
        }
        if n.nlink >= LINK_MAX {
            return Err(Error::TooManyLinks);
        }
        self.check_new(d, name)?;
        self.nodes
            .get_mut(&d)
            .unwrap()
            .entries
            .insert(name.to_vec(), ino);
        self.nodes.get_mut(&ino).unwrap().nlink += 1;
        Ok(())
    }
    fn unlink(&mut self, d: u64, name: &[u8]) -> Result<()> {
        let ino = self.lookup(d, name)?;
        if self.nodes[&ino].kind == FileKind::Dir {
            return Err(Error::IsDir);
        }
        self.nodes.get_mut(&d).unwrap().entries.remove(name);
        self.drop_link(ino);
        Ok(())
    }
    fn drop_link(&mut self, ino: u64) {
        let n = self.nodes.get_mut(&ino).unwrap();
        n.nlink -= 1;
        if n.nlink == 0 {
            self.nodes.remove(&ino);
        }
    }
    fn rmdir(&mut self, d: u64, name: &[u8]) -> Result<()> {
        let ino = self.lookup(d, name)?;
        let n = &self.nodes[&ino];
        if n.kind != FileKind::Dir {
            return Err(Error::NotDir);
        }
        if !n.entries.is_empty() {
            return Err(Error::NotEmpty);
        }
        self.nodes.remove(&ino);
        let dn = self.nodes.get_mut(&d).unwrap();
        dn.entries.remove(name);
        dn.nlink -= 1;
        Ok(())
    }
    fn is_ancestor(&self, anc: u64, mut ino: u64) -> bool {
        loop {
            if ino == anc {
                return true;
            }
            if ino == ROOT_INO {
                return false;
            }
            ino = self.nodes[&ino].parent;
        }
    }
    fn rename(&mut self, sd: u64, sn: &[u8], dd: u64, dn: &[u8]) -> Result<()> {
        let ino = self.lookup(sd, sn)?;
        self.dir(dd)?;
        if dn.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        if !valid_name(dn) {
            return Err(Error::InvalidName);
        }
        let is_dir = self.nodes[&ino].kind == FileKind::Dir;
        let target = match self.lookup(dd, dn) {
            Ok(t) => Some(t),
            Err(Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        if target == Some(ino) {
            return Ok(());
        }
        if is_dir && sd != dd && self.is_ancestor(ino, dd) {
            return Err(Error::Invalid("cannot move a directory inside itself"));
        }
        if let Some(t) = target {
            let tn = &self.nodes[&t];
            match (is_dir, tn.kind == FileKind::Dir) {
                (true, false) => return Err(Error::NotDir),
                (false, true) => return Err(Error::IsDir),
                (true, true) if !tn.entries.is_empty() => return Err(Error::NotEmpty),
                _ => {}
            }
        }
        if is_dir && sd != dd && target.is_none() && self.nodes[&dd].nlink >= LINK_MAX {
            return Err(Error::TooManyLinks);
        }
        if let Some(t) = target {
            self.nodes.get_mut(&dd).unwrap().entries.remove(dn);
            if self.nodes[&t].kind == FileKind::Dir {
                self.nodes.remove(&t);
                self.nodes.get_mut(&dd).unwrap().nlink -= 1;
            } else {
                self.drop_link(t);
            }
        }
        self.nodes.get_mut(&sd).unwrap().entries.remove(sn);
        self.nodes
            .get_mut(&dd)
            .unwrap()
            .entries
            .insert(dn.to_vec(), ino);
        if is_dir && sd != dd {
            self.nodes.get_mut(&sd).unwrap().nlink -= 1;
            self.nodes.get_mut(&dd).unwrap().nlink += 1;
        }
        if is_dir {
            self.nodes.get_mut(&ino).unwrap().parent = dd;
        }
        Ok(())
    }
    fn file(&mut self, ino: u64, what: &'static str) -> Result<&mut MNode> {
        match self.nodes.get_mut(&ino) {
            None => Err(Error::NotFound),
            Some(n) if n.kind == FileKind::Dir => Err(Error::IsDir),
            Some(n) if n.kind == FileKind::Symlink => Err(Error::Invalid(what)),
            Some(n) => Ok(n),
        }
    }
    fn write(&mut self, ino: u64, off: u64, data: &[u8]) -> Result<usize> {
        let n = self.file(ino, "write to a symlink")?;
        if data.is_empty() {
            return Ok(0);
        }
        let end = off
            .checked_add(data.len() as u64)
            .ok_or(Error::FileTooBig)?;
        if end > MAX_FILE_SIZE {
            return Err(Error::FileTooBig);
        }
        if n.data.len() < end as usize {
            n.data.resize(end as usize, 0);
        }
        n.data[off as usize..end as usize].copy_from_slice(data);
        Ok(data.len())
    }
    fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        let n = match self.nodes.get_mut(&ino) {
            None => return Err(Error::NotFound),
            Some(n) if n.kind != FileKind::File => {
                return Err(Error::Invalid("truncate of a non-file"));
            }
            Some(n) => n,
        };
        if size > MAX_FILE_SIZE {
            return Err(Error::FileTooBig);
        }
        n.data.resize(size as usize, 0);
        Ok(())
    }
    fn setattr(&mut self, ino: u64, s: &SetAttr) -> Result<()> {
        let kind = self.nodes.get(&ino).ok_or(Error::NotFound)?.kind;
        if s.perm.is_some_and(|p| p > 0o7777) {
            return Err(Error::Invalid("permission bits"));
        }
        if let Some(sz) = s.size {
            if kind != FileKind::File {
                return Err(if kind == FileKind::Dir {
                    Error::IsDir
                } else {
                    Error::Invalid("truncate of a non-file")
                });
            }
            self.truncate(ino, sz)?;
        }
        let n = self.nodes.get_mut(&ino).unwrap();
        if let Some(p) = s.perm {
            n.perm = p;
        }
        if let Some(u) = s.uid {
            n.uid = u;
        }
        if let Some(g) = s.gid {
            n.gid = g;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Op {
    Create(u64, Vec<u8>, u32),
    Mkdir(u64, Vec<u8>, u32),
    Symlink(u64, Vec<u8>, Vec<u8>),
    Link(u64, u64, Vec<u8>),
    Unlink(u64, Vec<u8>),
    Rmdir(u64, Vec<u8>),
    Rename(u64, Vec<u8>, u64, Vec<u8>),
    /// ino, offset, length, content seed
    Write(u64, u64, u64, u64),
    Truncate(u64, u64),
    SetAttr(u64, SetAttr),
    Commit,
    Snapshot,
    SnapDelete(u64),
    Rollback(u64),
    /// Unmount without committing, then mount again.
    Remount,
}

pub fn content(seed: u64, len: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut v = vec![0u8; len as usize];
    // Mix of compressible and random content.
    if seed.is_multiple_of(3) {
        v.fill((seed >> 8) as u8 | 1);
    } else {
        r.fill(&mut v);
    }
    v
}

/// Tunables for the generator.
#[derive(Debug, Clone)]
pub struct GenCfg {
    pub max_write: u64,
    pub max_offset: u64,
    pub snapshots: bool,
    pub remounts: bool,
}

impl Default for GenCfg {
    fn default() -> Self {
        GenCfg {
            max_write: 200_000,
            max_offset: 2_000_000,
            snapshots: true,
            remounts: true,
        }
    }
}

fn pick<T: Copy>(r: &mut Rng, v: &[T]) -> Option<T> {
    if v.is_empty() {
        None
    } else {
        Some(v[r.below(v.len() as u64) as usize])
    }
}

fn name(r: &mut Rng) -> Vec<u8> {
    match r.below(40) {
        0 => vec![b'x'; 256],
        1 => b".".to_vec(),
        2 => b"..".to_vec(),
        3 => b"a/b".to_vec(),
        4 => vec![],
        5 => vec![b'n'; 255],
        6 => vec![0xff, 0xfe, b'u'],
        _ => {
            const N: [&str; 10] = [
                "a",
                "b",
                "c",
                "d",
                "e",
                "file",
                "dir",
                "x1",
                "x2",
                "long-name-for-testing",
            ];
            N[r.below(N.len() as u64) as usize].as_bytes().to_vec()
        }
    }
}

/// A random operation, biased towards ones that are valid in `m`.
pub fn gen_op(r: &mut Rng, m: &Model, cfg: &GenCfg) -> Op {
    let nodes = &m.live.nodes;
    let dirs: Vec<u64> = nodes
        .iter()
        .filter(|(_, n)| n.kind == FileKind::Dir)
        .map(|(&i, _)| i)
        .collect();
    let files: Vec<u64> = nodes
        .iter()
        .filter(|(_, n)| n.kind == FileKind::File)
        .map(|(&i, _)| i)
        .collect();
    let all: Vec<u64> = nodes.keys().copied().collect();
    let any_ino = |r: &mut Rng| {
        if r.chance(1, 30) {
            r.below(m.live.next_ino + 3)
        } else {
            pick(r, &all).unwrap()
        }
    };
    let dir = |r: &mut Rng| {
        if r.chance(1, 30) {
            any_ino(r)
        } else {
            pick(r, &dirs).unwrap()
        }
    };
    // A name that exists in `d`, usually.
    let existing = |r: &mut Rng, d: u64| -> Vec<u8> {
        if let Some(n) = nodes.get(&d) {
            let names: Vec<&Vec<u8>> = n.entries.keys().collect();
            if !names.is_empty() && !r.chance(1, 8) {
                return names[r.below(names.len() as u64) as usize].clone();
            }
        }
        name(r)
    };
    let snaps: Vec<u64> = m.snaps.keys().copied().collect();
    loop {
        let k = r.below(100);
        let op = match k {
            0..=11 => Op::Create(dir(r), name(r), r.below(0o1000) as u32),
            12..=17 => Op::Mkdir(dir(r), name(r), 0o755),
            18..=19 => {
                let t = if r.chance(1, 20) {
                    vec![]
                } else {
                    content(r.next_u64(), 1 + r.below(200))
                        .iter()
                        .map(|b| b | 1)
                        .collect()
                };
                Op::Symlink(dir(r), name(r), t)
            }
            20..=22 => Op::Link(
                if files.is_empty() {
                    any_ino(r)
                } else {
                    pick(r, &files).unwrap()
                },
                dir(r),
                name(r),
            ),
            23..=29 => {
                let d = dir(r);
                Op::Unlink(d, existing(r, d))
            }
            30..=32 => {
                let d = dir(r);
                Op::Rmdir(d, existing(r, d))
            }
            33..=39 => {
                let s = dir(r);
                let sn = existing(r, s);
                let d = dir(r);
                let dn = if r.chance(1, 3) {
                    existing(r, d)
                } else {
                    name(r)
                };
                Op::Rename(s, sn, d, dn)
            }
            40..=64 => {
                let f = if files.is_empty() || r.chance(1, 40) {
                    any_ino(r)
                } else {
                    pick(r, &files).unwrap()
                };
                let cur = nodes.get(&f).map_or(0, |n| n.data.len() as u64);
                let off = match r.below(6) {
                    0 => 0,
                    1 => cur,
                    2 => r.below(cur + 1),
                    3 => r.below(cfg.max_offset),
                    4 => (r.below(cfg.max_offset / BS + 1)) * BS,
                    _ => cur.saturating_sub(r.below(5000)),
                };
                let len = match r.below(5) {
                    0 => 1 + r.below(100),
                    1 => 1 + r.below(BS * 2),
                    2 => BS * (1 + r.below(8)),
                    3 => 1 + r.below(cfg.max_write),
                    _ => 0,
                };
                Op::Write(f, off, len, r.next_u64())
            }
            65..=71 => {
                let f = if files.is_empty() {
                    any_ino(r)
                } else {
                    pick(r, &files).unwrap()
                };
                let cur = nodes.get(&f).map_or(0, |n| n.data.len() as u64);
                let size = match r.below(4) {
                    0 => 0,
                    1 => r.below(cur + 1),
                    2 => cur + r.below(3 * BS),
                    _ => r.below(cfg.max_offset),
                };
                Op::Truncate(f, size)
            }
            72..=74 => {
                let i = any_ino(r);
                let s = SetAttr {
                    perm: r.chance(1, 2).then(|| r.below(0o20000) as u32),
                    uid: r.chance(1, 3).then(|| r.below(5) as u32),
                    gid: r.chance(1, 3).then(|| r.below(5) as u32),
                    size: r.chance(1, 5).then(|| r.below(100_000)),
                    atime_ns: None,
                    mtime_ns: None,
                };
                Op::SetAttr(i, s)
            }
            75..=86 => Op::Commit,
            87..=90 if cfg.snapshots => Op::Snapshot,
            91..=93 if cfg.snapshots => match pick(r, &snaps) {
                Some(s) => Op::SnapDelete(s),
                None => continue,
            },
            94..=95 if cfg.snapshots => match pick(r, &snaps) {
                Some(s) => Op::Rollback(s),
                None => continue,
            },
            96..=97 if cfg.remounts => Op::Remount,
            _ => continue,
        };
        return op;
    }
}

pub fn short(op: &Op) -> String {
    let mut s = format!("{op:?}");
    if s.len() > 120 {
        s.truncate(120);
        s.push_str("...");
    }
    s
}

pub fn fmt_opts(enc: bool) -> FormatOptions {
    FormatOptions {
        passphrase: enc.then(|| b"correct horse".to_vec()),
        kdf: KdfParams::INSECURE_FAST_FOR_TESTS,
    }
}

pub fn mount_opts(enc: bool) -> MountOptions {
    MountOptions {
        passphrase: enc.then(|| b"correct horse".to_vec()),
        cache_nodes: 512,
        dirty_limit_bytes: 256 * 1024,
        ..Default::default()
    }
}

/// Runs operations against a real filesystem and the model in lockstep.
pub struct Harness {
    pub fs: Option<Filesystem<CrashDevice>>,
    pub model: Model,
    /// Model state at each durable commit generation.
    pub committed: BTreeMap<u64, Model>,
    pub enc: bool,
    pub seed: u64,
    pub mounts: u64,
    pub ops_done: u64,
}

impl Harness {
    pub fn new(blocks: u64, enc: bool, seed: u64) -> Result<Harness> {
        let dev = CrashDevice::new(blocks, seed ^ 0xdead);
        let fs = Filesystem::format(
            dev,
            &fmt_opts(enc),
            &mut Rng::new(seed),
            Box::new(ZeroClock),
        )?;
        let model = Model::new();
        let mut committed = BTreeMap::new();
        committed.insert(fs.generation(), model.clone());
        Ok(Harness {
            fs: Some(fs),
            model,
            committed,
            enc,
            seed,
            mounts: 0,
            ops_done: 0,
        })
    }

    /// Wraps an already-mounted filesystem whose state is `model`.
    pub fn from_mounted(
        fs: Filesystem<CrashDevice>,
        model: Model,
        enc: bool,
        seed: u64,
    ) -> Harness {
        let mut committed = BTreeMap::new();
        committed.insert(fs.generation(), model.clone());
        Harness {
            fs: Some(fs),
            model,
            committed,
            enc,
            seed,
            mounts: 100,
            ops_done: 0,
        }
    }

    pub fn fs(&mut self) -> &mut Filesystem<CrashDevice> {
        self.fs.as_mut().unwrap()
    }

    fn last_committed(&self) -> Model {
        self.committed.values().next_back().unwrap().clone()
    }

    /// Records model states for the generations an op committed. `states`
    /// are the states after each commit the op may perform, in order; the
    /// first ones are optional (skipped when nothing was dirty).
    fn record(&mut self, before: u64, states: &[Model]) {
        let after = self.fs.as_ref().unwrap().generation();
        let n = (after - before) as usize;
        assert!(
            n <= states.len().max(2),
            "op committed {n} times, expected at most {}",
            states.len()
        );
        for (i, g) in (before + 1..=after).enumerate() {
            let idx = states.len() as isize - n as isize + i as isize;
            let st = if idx < 0 {
                states[0].clone()
            } else {
                states[idx as usize].clone()
            };
            self.committed.insert(g, st);
        }
    }

    /// Applies `op`. Panics on any mismatch between the filesystem and the model.
    /// Returns Err only for I/O errors (power cut or injected faults).
    pub fn apply(&mut self, op: &Op) -> Result<()> {
        self.ops_done += 1;
        if std::env::var("OFS_DEBUG").is_ok() {
            eprintln!("start op #{} {}", self.ops_done, short(op));
        }
        let before_gen = self.fs().generation();
        let pre = self.model.clone();
        macro_rules! cmp {
            ($real:expr, $model:expr) => {{
                let real = $real;
                if let Err(Error::Io(e)) = &real {
                    return Err(Error::Io(*e));
                }
                let model = &$model;
                let ok = match (&real, model) {
                    (Ok(a), Ok(b)) => a == b,
                    (Err(a), Err(b)) => core::mem::discriminant(a) == core::mem::discriminant(b),
                    _ => false,
                };
                assert!(
                    ok,
                    "seed {:#x} op #{} {:?}: filesystem returned {:?}, model {:?}",
                    self.seed, self.ops_done, op, real, model
                );
            }};
        }
        match op.clone() {
            Op::Create(d, n, p) => {
                let mut m = self.model.live.clone();
                let mr = m.create(d, &n, FileKind::File, p);
                cmp!(self.fs().create(d, &n, p), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Mkdir(d, n, p) => {
                let mut m = self.model.live.clone();
                let mr = m.create(d, &n, FileKind::Dir, p);
                cmp!(self.fs().mkdir(d, &n, p), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Symlink(d, n, t) => {
                let mut m = self.model.live.clone();
                let mr = m.symlink(d, &n, &t);
                cmp!(self.fs().symlink(d, &n, &t), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Link(i, d, n) => {
                let mut m = self.model.live.clone();
                let mr = m.link(i, d, &n);
                cmp!(self.fs().link(i, d, &n), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Unlink(d, n) => {
                let mut m = self.model.live.clone();
                let mr = m.unlink(d, &n);
                cmp!(self.fs().unlink(d, &n), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Rmdir(d, n) => {
                let mut m = self.model.live.clone();
                let mr = m.rmdir(d, &n);
                cmp!(self.fs().rmdir(d, &n), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Rename(a, b, c, d) => {
                let mut m = self.model.live.clone();
                let mr = m.rename(a, &b, c, &d);
                cmp!(self.fs().rename(a, &b, c, &d), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Write(i, off, len, seed) => {
                let data = content(seed, len);
                let mut m = self.model.live.clone();
                let mr = m.write(i, off, &data);
                let fr = self.fs().write(i, off, &data);
                if matches!(fr, Err(Error::NoSpace)) && mr.is_ok() {
                    // The model has no space limit; a full disk is fine.
                } else {
                    cmp!(fr, mr);
                    if mr.is_ok() {
                        self.model.live = m;
                    }
                }
            }
            Op::Truncate(i, s) => {
                let mut m = self.model.live.clone();
                let mr = m.truncate(i, s);
                cmp!(self.fs().truncate(i, s), mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::SetAttr(i, s) => {
                let mut m = self.model.live.clone();
                let mr = m.setattr(i, &s);
                let fr = self.fs().setattr(i, &s).map(|_| ());
                cmp!(fr, mr);
                if mr.is_ok() {
                    self.model.live = m;
                }
            }
            Op::Commit => {
                let r = self.fs().commit();
                cmp!(r, Ok::<(), Error>(()));
                let st = self.model.clone();
                self.record(before_gen, &[st]);
                return Ok(());
            }
            Op::Snapshot => {
                let r = self.fs().snapshot_create(b"snap");
                if let Err(Error::Io(e)) = r {
                    return Err(Error::Io(e));
                }
                if r == Err(Error::NoSpace) {
                    return Ok(());
                }
                let id = r.unwrap();
                assert_eq!(id, self.model.next_snap, "snapshot id");
                let s1 = self.model.clone();
                self.model.snaps.insert(id, self.model.live.clone());
                self.model.next_snap += 1;
                let s2 = self.model.clone();
                self.record(before_gen, &[s1, s2]);
                return Ok(());
            }
            Op::SnapDelete(id) => {
                let r = self.fs().snapshot_delete(id);
                if let Err(Error::Io(e)) = r {
                    return Err(Error::Io(e));
                }
                cmp!(
                    r,
                    if self.model.snaps.contains_key(&id) {
                        Ok(())
                    } else {
                        Err(Error::NotFound)
                    }
                );
                let s1 = self.model.clone();
                self.model.snaps.remove(&id);
                let s2 = self.model.clone();
                self.record(before_gen, &[s1, s2]);
                return Ok(());
            }
            Op::Rollback(id) => {
                let r = self.fs().rollback(id);
                if let Err(Error::Io(e)) = r {
                    return Err(Error::Io(e));
                }
                cmp!(
                    r,
                    if self.model.snaps.contains_key(&id) {
                        Ok(())
                    } else {
                        Err(Error::NotFound)
                    }
                );
                let s1 = self.model.clone();
                self.model.snaps.retain(|&k, _| k <= id);
                let s2 = self.model.clone();
                let snap = self.model.snaps[&id].clone();
                let next = self.model.live.next_ino.max(snap.next_ino);
                self.model.live = snap;
                self.model.live.next_ino = next;
                let s3 = self.model.clone();
                self.record(before_gen, &[s1, s2, s3]);
                return Ok(());
            }
            Op::Remount => {
                let fs = self.fs.take().unwrap();
                let dev = fs.into_device();
                self.mounts += 1;
                let fs = Filesystem::mount(
                    dev,
                    &mount_opts(self.enc),
                    &mut Rng::new(self.seed ^ self.mounts),
                    Box::new(ZeroClock),
                )?;
                if std::env::var("OFS_DEBUG").is_ok() {
                    eprintln!(
                        "remount: fs gen {} model committed up to {:?}",
                        fs.generation(),
                        self.committed.keys().next_back()
                    );
                }
                self.fs = Some(fs);
                self.model = self.last_committed();
                return Ok(());
            }
        }
        if std::env::var("OFS_DEBUG").is_ok() {
            eprintln!(
                "op #{} gen {}->{} {:?}",
                self.ops_done,
                before_gen,
                self.fs.as_ref().unwrap().generation(),
                short(op)
            );
            let m = self.model.clone();
            assert_eq!(
                self.fs.as_ref().unwrap().next_ino_for_tests(),
                m.live.next_ino,
                "next_ino diverged"
            );
            self.verify(&m);
        }
        // A non-snapshot op may commit internally (to recycle quarantined space) before changing anything.
        self.record(before_gen, &[pre.clone(), pre]);
        Ok(())
    }

    /// Full comparison of the mounted filesystem against `m` (live tree and snapshots).
    pub fn verify(&mut self, m: &Model) {
        let seed = self.seed;
        let fs = self.fs.as_mut().unwrap();
        compare_tree(fs, &m.live, seed, "live");
        let ids: Vec<u64> = fs.snapshot_list().unwrap().iter().map(|s| s.id).collect();
        let want: Vec<u64> = m.snaps.keys().copied().collect();
        assert_eq!(ids, want, "seed {seed:#x}: snapshot list");
        for (id, snap) in &m.snaps {
            fs.with_snapshot(*id, |fs| {
                compare_tree(fs, snap, seed, "snapshot");
                Ok(())
            })
            .unwrap();
        }
    }

    pub fn fsck(&mut self) {
        let seed = self.seed;
        let r = fsck::check(self.fs(), true).unwrap();
        assert!(
            r.ok(),
            "seed {seed:#x}: fsck found problems: {:#?}",
            r.errors
        );
    }
}

pub fn compare_tree<D: BlockDevice>(fs: &mut Filesystem<D>, m: &MFs, seed: u64, what: &str) {
    for (&ino, n) in &m.nodes {
        let a = fs
            .getattr(ino)
            .unwrap_or_else(|e| panic!("seed {seed:#x} {what}: getattr({ino}) failed: {e:?}"));
        let got = (a.kind, a.perm, a.uid, a.gid, a.nlink, a.size, a.parent);
        let want = (n.kind, n.perm, n.uid, n.gid, n.nlink, n.size(), n.parent);
        assert_eq!(
            got, want,
            "seed {seed:#x} {what}: attributes of inode {ino}"
        );
        match n.kind {
            FileKind::Dir => {
                let mut got: Vec<(Vec<u8>, u64)> = fs
                    .readdir(ino)
                    .unwrap()
                    .into_iter()
                    .map(|e| (e.name, e.ino))
                    .collect();
                got.sort();
                let want: Vec<(Vec<u8>, u64)> =
                    n.entries.iter().map(|(k, v)| (k.clone(), *v)).collect();
                assert_eq!(got, want, "seed {seed:#x} {what}: entries of dir {ino}");
            }
            FileKind::File => {
                let mut buf = vec![0u8; n.data.len() + 10];
                let r = fs.read(ino, 0, &mut buf).unwrap();
                assert_eq!(
                    r,
                    n.data.len(),
                    "seed {seed:#x} {what}: read length of {ino}"
                );
                if buf[..r] != n.data[..] {
                    let first = buf.iter().zip(&n.data).position(|(a, b)| a != b).unwrap();
                    let last = buf[..r]
                        .iter()
                        .zip(&n.data)
                        .rposition(|(a, b)| a != b)
                        .unwrap();
                    panic!(
                        "seed {seed:#x} {what}: content of inode {ino} differs at bytes {first}..={last} (size {}); got {:?} want {:?}",
                        n.data.len(),
                        &buf[first..(first + 8).min(r)],
                        &n.data[first..(first + 8).min(r)]
                    );
                }
            }
            FileKind::Symlink => {
                assert_eq!(
                    fs.readlink(ino).unwrap(),
                    n.target,
                    "seed {seed:#x} {what}: symlink {ino}"
                );
            }
        }
    }
}
