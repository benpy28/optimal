//! A read-only FAT12/16/32 reader for the BIOS stage, written from the
//! Microsoft FAT specification (FAT: General Overview of On-Disk Format,
//! version 1.03) with long names (VFAT LFN entries).
//!
//! Everything on the volume is untrusted: every BPB field is range checked,
//! every cluster number is checked against the cluster count, directory
//! walks stop at the spec's 65 536-entry limit, and a file read never
//! follows more clusters than its size needs, so a looped or damaged chain
//! cannot hang the boot. What a damaged chain returns is caught by the
//! SHA-512 check the loader runs on every file.

use crate::disk::{BlockDevice, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatError {
    ReadFailed,
    /// The boot sector is not a FAT boot sector, or its BPB is inconsistent.
    BadBpb,
    /// A cluster number or chain on the volume is out of range.
    Corrupt,
    NotFound,
    /// A path component other than the last names a file.
    NotADirectory,
    /// The last path component names a directory.
    IsADirectory,
    /// The path is not absolute, has an empty, "." or ".." component, or
    /// a component longer than 255 bytes.
    BadPath,
    TooLarge,
    /// The file's cluster chain ends before its size says it should.
    Truncated,
    /// A directory runs past the 65 536 entries the spec allows.
    DirectoryTooLarge,
}

impl FatError {
    pub fn describe(&self) -> &'static str {
        match self {
            FatError::ReadFailed => "reading the boot volume failed",
            FatError::BadBpb => "the boot volume is not a valid FAT file system",
            FatError::Corrupt => "the boot volume's file system is damaged",
            FatError::NotFound => "file not found",
            FatError::NotADirectory => "a path component is a file, not a directory",
            FatError::IsADirectory => "the path names a directory",
            FatError::BadPath => "bad path",
            FatError::TooLarge => "file is too large",
            FatError::Truncated => "the file is shorter on disk than its size (damaged volume)",
            FatError::DirectoryTooLarge => "a directory is larger than FAT allows (damaged volume)",
        }
    }
}

const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LONG_NAME: u8 = 0x0f;
/// The spec's limit on entries in one directory.
pub const MAX_DIR_ENTRIES: u32 = 65_536;
const MAX_NAME: usize = 255;

/// A file found by `open`: where its data starts and how long it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRef {
    pub first_cluster: u32,
    pub size: u32,
}

pub struct Fat<'a, D: BlockDevice> {
    win: Window<'a, D>,
    pub kind: FatKind,
    cluster_bytes: u32,
    fat_offset: u64,
    root_offset: u64,
    root_entries: u32,
    root_cluster: u32,
    data_offset: u64,
    clusters: u32,
    cache: [u8; 512],
    cache_off: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// The checksum of an 8.3 name that its LFN entries carry.
pub fn short_name_checksum(name: &[u8; 11]) -> u8 {
    name.iter().fold(0u8, |s, &c| {
        ((s & 1) << 7).wrapping_add(s >> 1).wrapping_add(c)
    })
}

enum Dir {
    Root,
    Chain(u32),
}

struct Entry {
    attr: u8,
    cluster: u32,
    size: u32,
}

impl<'a, D: BlockDevice> Fat<'a, D> {
    /// Checks the boot sector and works out the layout.
    pub fn mount(mut win: Window<'a, D>) -> Result<Self, FatError> {
        let mut b = [0u8; 512];
        if !win.read_at(0, &mut b) {
            return Err(FatError::ReadFailed);
        }
        let jump_ok = (b[0] == 0xeb && b[2] == 0x90) || b[0] == 0xe9;
        if !jump_ok || b[510] != 0x55 || b[511] != 0xaa {
            return Err(FatError::BadBpb);
        }
        let bps = u16_at(&b, 11) as u32;
        let spc = b[13] as u32;
        let reserved = u16_at(&b, 14) as u32;
        let nfats = b[16] as u32;
        let root_entries = u16_at(&b, 17) as u32;
        let tot16 = u16_at(&b, 19) as u32;
        let media = b[21];
        let fatsz16 = u16_at(&b, 22) as u32;
        let tot32 = u32_at(&b, 32);
        if !matches!(bps, 512 | 1024 | 2048 | 4096)
            || !spc.is_power_of_two()
            || spc > 128
            || bps * spc > 64 * 1024
            || reserved == 0
            || !(1..=4).contains(&nfats)
            || !(media == 0xf0 || media >= 0xf8)
        {
            return Err(FatError::BadBpb);
        }
        let total = match (tot16, tot32) {
            (0, 0) => return Err(FatError::BadBpb),
            (0, t) => t,
            (t, 0) => t,
            (a, b) if a == b => a,
            _ => return Err(FatError::BadBpb),
        };
        let fatsz = if fatsz16 != 0 {
            fatsz16
        } else {
            u32_at(&b, 36)
        };
        if fatsz == 0 {
            return Err(FatError::BadBpb);
        }
        if (total as u64) * (bps as u64) > win.len() {
            return Err(FatError::BadBpb);
        }
        let root_sectors = (root_entries * 32).div_ceil(bps);
        let meta = reserved as u64 + nfats as u64 * fatsz as u64 + root_sectors as u64;
        let Some(data_sectors) = (total as u64).checked_sub(meta) else {
            return Err(FatError::BadBpb);
        };
        let clusters64 = data_sectors / spc as u64;
        if clusters64 == 0 || clusters64 > 0x0fff_fff5 {
            return Err(FatError::BadBpb);
        }
        let clusters = clusters64 as u32;
        // The spec's one rule for the type: the cluster count.
        let kind = if clusters < 4085 {
            FatKind::Fat12
        } else if clusters < 65525 {
            FatKind::Fat16
        } else {
            FatKind::Fat32
        };
        let mut active_fat = 0u32;
        let mut root_cluster = 0;
        match kind {
            FatKind::Fat32 => {
                if fatsz16 != 0 || root_entries != 0 || tot16 != 0 {
                    return Err(FatError::BadBpb);
                }
                let ext_flags = u16_at(&b, 40);
                if u16_at(&b, 42) != 0 {
                    return Err(FatError::BadBpb); // FAT32 version 0.0 only
                }
                if ext_flags & 0x80 != 0 {
                    active_fat = (ext_flags & 0xf) as u32;
                    if active_fat >= nfats {
                        return Err(FatError::BadBpb);
                    }
                }
                root_cluster = u32_at(&b, 44);
                if root_cluster < 2 || root_cluster - 2 >= clusters {
                    return Err(FatError::BadBpb);
                }
            }
            _ => {
                if fatsz16 == 0 || root_entries == 0 {
                    return Err(FatError::BadBpb);
                }
            }
        }
        // The FAT must hold an entry for every cluster (plus the two reserved).
        let need_bits = (clusters as u64 + 2)
            * match kind {
                FatKind::Fat12 => 12,
                FatKind::Fat16 => 16,
                FatKind::Fat32 => 32,
            };
        if need_bits.div_ceil(8) > fatsz as u64 * bps as u64 {
            return Err(FatError::BadBpb);
        }
        let fat_offset = (reserved as u64 + active_fat as u64 * fatsz as u64) * bps as u64;
        let root_offset = (reserved as u64 + nfats as u64 * fatsz as u64) * bps as u64;
        Ok(Fat {
            win,
            kind,
            cluster_bytes: bps * spc,
            fat_offset,
            root_offset,
            root_entries,
            root_cluster,
            data_offset: meta * bps as u64,
            clusters,
            cache: [0; 512],
            cache_off: u64::MAX,
        })
    }

    pub fn cluster_bytes(&self) -> u32 {
        self.cluster_bytes
    }

    fn fat_byte(&mut self, off: u64) -> Result<u8, FatError> {
        let base = off & !511;
        if self.cache_off != base {
            if !self.win.read_at(self.fat_offset + base, &mut self.cache) {
                self.cache_off = u64::MAX;
                return Err(FatError::ReadFailed);
            }
            self.cache_off = base;
        }
        Ok(self.cache[(off - base) as usize])
    }

    fn valid(&self, c: u32) -> bool {
        c >= 2 && c - 2 < self.clusters
    }

    /// The cluster after `c`: Some(next), or None at the end of the chain.
    fn next(&mut self, c: u32) -> Result<Option<u32>, FatError> {
        if !self.valid(c) {
            return Err(FatError::Corrupt);
        }
        let (v, end, bad) = match self.kind {
            FatKind::Fat12 => {
                let off = c as u64 + c as u64 / 2;
                let w = self.fat_byte(off)? as u32 | (self.fat_byte(off + 1)? as u32) << 8;
                let v = if c & 1 != 0 { w >> 4 } else { w & 0xfff };
                (v, 0xff8, 0xff7)
            }
            FatKind::Fat16 => {
                let off = 2 * c as u64;
                let v = self.fat_byte(off)? as u32 | (self.fat_byte(off + 1)? as u32) << 8;
                (v, 0xfff8, 0xfff7)
            }
            FatKind::Fat32 => {
                let off = 4 * c as u64;
                let mut v = 0u32;
                for i in 0..4 {
                    v |= (self.fat_byte(off + i)? as u32) << (8 * i);
                }
                (v & 0x0fff_ffff, 0x0fff_fff8, 0x0fff_fff7)
            }
        };
        if v >= end {
            return Ok(None);
        }
        if v == bad || !self.valid(v) {
            return Err(FatError::Corrupt);
        }
        Ok(Some(v))
    }

    fn cluster_offset(&self, c: u32) -> u64 {
        self.data_offset + (c - 2) as u64 * self.cluster_bytes as u64
    }

    /// Calls `f` with each 32-byte entry of a directory, in order, until it
    /// returns true or the directory ends (an entry starting with 0).
    fn walk(&mut self, dir: Dir, mut f: impl FnMut(&[u8; 32]) -> bool) -> Result<bool, FatError> {
        let mut buf = [0u8; 512];
        let mut seen = 0u32;
        // `cap` entries at most: the fixed root's size (then the directory
        // simply ends), or the spec's limit for a chain (then it is damaged).
        let mut chunk = |this: &mut Self,
                         off: u64,
                         seen: &mut u32,
                         cap: u32|
         -> Result<Option<bool>, FatError> {
            if !this.win.read_at(off, &mut buf) {
                return Err(FatError::ReadFailed);
            }
            for e in buf.as_chunks::<32>().0 {
                if *seen >= cap {
                    if cap == MAX_DIR_ENTRIES {
                        return Err(FatError::DirectoryTooLarge);
                    }
                    return Ok(Some(false));
                }
                *seen += 1;
                if e[0] == 0 {
                    return Ok(Some(false));
                }
                if f(e) {
                    return Ok(Some(true));
                }
            }
            Ok(None)
        };
        match dir {
            Dir::Root if self.kind != FatKind::Fat32 => {
                let bytes = self.root_entries as u64 * 32;
                let cap = self.root_entries.min(MAX_DIR_ENTRIES - 1);
                let mut off = 0;
                while off < bytes {
                    if let Some(r) = chunk(self, self.root_offset + off, &mut seen, cap)? {
                        return Ok(r);
                    }
                    off += 512;
                }
                Ok(false)
            }
            d => {
                let mut c = match d {
                    Dir::Chain(c) => c,
                    Dir::Root => self.root_cluster,
                };
                loop {
                    if !self.valid(c) {
                        return Err(FatError::Corrupt);
                    }
                    let base = self.cluster_offset(c);
                    let mut off = 0;
                    while off < self.cluster_bytes as u64 {
                        if let Some(r) = chunk(self, base + off, &mut seen, MAX_DIR_ENTRIES)? {
                            return Ok(r);
                        }
                        off += 512;
                    }
                    match self.next(c)? {
                        Some(n) => c = n,
                        None => return Ok(false),
                    }
                }
            }
        }
    }

    /// Looks `name` up in one directory, by long name or 8.3 alias,
    /// ignoring ASCII case.
    fn lookup(&mut self, dir: Dir, name: &[u8]) -> Result<Option<Entry>, FatError> {
        let kind = self.kind;
        let mut lfn = [0u16; 20 * 13];
        let mut lfn_next = 0u8; // sequence number expected next; 0 = none in progress
        let mut lfn_sum = 0u8;
        let mut lfn_ok = false;
        let mut found = None;
        self.walk(dir, |e| {
            if e[0] == 0xe5 {
                lfn_next = 0;
                lfn_ok = false;
                return false;
            }
            if e[11] & 0x3f == ATTR_LONG_NAME {
                let ord = e[0];
                let seq = ord & 0x1f;
                if ord & 0x40 != 0 {
                    if seq == 0 || seq > 20 {
                        lfn_next = 0;
                        lfn_ok = false;
                        return false;
                    }
                    lfn = [0xffff; 260];
                    lfn_sum = e[13];
                    lfn_next = seq;
                } else if lfn_next == 0 || seq != lfn_next || e[13] != lfn_sum {
                    lfn_next = 0;
                    lfn_ok = false;
                    return false;
                }
                let at = (seq as usize - 1) * 13;
                for (k, o) in (1..11)
                    .step_by(2)
                    .chain((14..26).step_by(2))
                    .chain((28..32).step_by(2))
                    .enumerate()
                {
                    lfn[at + k] = u16_at(e, o);
                }
                lfn_next = seq - 1;
                lfn_ok = lfn_next == 0;
                if lfn_ok {
                    lfn_next = 0;
                }
                return false;
            }
            let long = lfn_ok;
            lfn_ok = false;
            lfn_next = 0;
            if e[11] & ATTR_VOLUME_ID != 0 {
                return false;
            }
            let short: &[u8; 11] = e[0..11].try_into().unwrap();
            let mut matched = false;
            if long && short_name_checksum(short) == lfn_sum {
                let len = lfn.iter().position(|&c| c == 0).unwrap_or(lfn.len());
                let l = &lfn[..len];
                matched = l.len() == name.len()
                    && l.iter()
                        .zip(name)
                        .all(|(&c, &n)| c < 0x80 && (c as u8).eq_ignore_ascii_case(&n));
            }
            if !matched {
                matched = short_matches(short, name);
            }
            if matched {
                let hi = if kind == FatKind::Fat32 {
                    (u16_at(e, 20) as u32) << 16
                } else {
                    0
                };
                found = Some(Entry {
                    attr: e[11],
                    cluster: hi | u16_at(e, 26) as u32,
                    size: u32_at(e, 28),
                });
                return true;
            }
            false
        })?;
        Ok(found)
    }

    /// Finds the file at an absolute path such as `/boot/optimal-boot.conf`.
    pub fn open(&mut self, path: &str) -> Result<FileRef, FatError> {
        let Some(rest) = path.strip_prefix('/') else {
            return Err(FatError::BadPath);
        };
        let parts = rest.split('/');
        let count = rest.split('/').count();
        let mut dir = Dir::Root;
        for (i, part) in parts.enumerate() {
            if part.is_empty() || part == "." || part == ".." || part.len() > MAX_NAME {
                return Err(FatError::BadPath);
            }
            let Some(e) = self.lookup(dir, part.as_bytes())? else {
                return Err(FatError::NotFound);
            };
            let last = i + 1 == count;
            if !last {
                if e.attr & ATTR_DIRECTORY == 0 {
                    return Err(FatError::NotADirectory);
                }
                if !self.valid(e.cluster) {
                    return Err(FatError::Corrupt);
                }
                dir = Dir::Chain(e.cluster);
            } else {
                if e.attr & ATTR_DIRECTORY != 0 {
                    return Err(FatError::IsADirectory);
                }
                if e.size > 0 && !self.valid(e.cluster) {
                    return Err(FatError::Corrupt);
                }
                return Ok(FileRef {
                    first_cluster: e.cluster,
                    size: e.size,
                });
            }
        }
        Err(FatError::BadPath)
    }

    /// Reads the whole file into `buf`, which must be exactly `f.size`
    /// bytes. Runs of consecutive clusters are read in one request.
    pub fn read(&mut self, f: &FileRef, buf: &mut [u8]) -> Result<(), FatError> {
        if buf.len() != f.size as usize {
            return Err(FatError::TooLarge);
        }
        if f.size == 0 {
            return Ok(());
        }
        let cb = self.cluster_bytes as u64;
        let need = (f.size as u64).div_ceil(cb);
        if need > self.clusters as u64 {
            return Err(FatError::Corrupt);
        }
        let mut c = f.first_cluster;
        let mut done = 0u64; // clusters read
        while done < need {
            // Extend the run while the chain stays contiguous.
            let start = c;
            let mut run = 1u64;
            let mut next = None;
            while done + run < need {
                match self.next(c)? {
                    Some(n) if n == c + 1 => {
                        c = n;
                        run += 1;
                    }
                    Some(n) => {
                        next = Some(n);
                        break;
                    }
                    None => return Err(FatError::Truncated),
                }
            }
            let from = (done * cb) as usize;
            let to = ((done + run) * cb).min(f.size as u64) as usize;
            if !self.valid(start) {
                return Err(FatError::Corrupt);
            }
            if !self
                .win
                .read_at(self.cluster_offset(start), &mut buf[from..to])
            {
                return Err(FatError::ReadFailed);
            }
            done += run;
            if done < need {
                c = match next {
                    Some(n) => n,
                    None => return Err(FatError::Truncated),
                };
            }
        }
        Ok(())
    }
}

/// Whether an 8.3 directory name matches `name` (ASCII case ignored).
fn short_matches(short: &[u8; 11], name: &[u8]) -> bool {
    let mut base = [0u8; 8];
    base.copy_from_slice(&short[0..8]);
    if base[0] == 0x05 {
        base[0] = 0xe5;
    }
    let blen = base.iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
    let ext = &short[8..11];
    let elen = ext.iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
    let mut full = [0u8; 12];
    full[..blen].copy_from_slice(&base[..blen]);
    let mut len = blen;
    if elen > 0 {
        full[len] = b'.';
        full[len + 1..len + 1 + elen].copy_from_slice(&ext[..elen]);
        len += 1 + elen;
    }
    len > 0 && full[..len].eq_ignore_ascii_case(name)
}

#[cfg(test)]
pub mod build {
    //! A reference FAT writer for the tests, laid out by hand from the
    //! spec: boot sector and BPB, FATs, the fixed root (FAT12/16), and
    //! directories with LFN entries. Writes into a sparse disk.
    extern crate std;
    use super::*;
    use crate::disk::sim::Sparse;
    use std::string::String;
    use std::vec;
    use std::vec::Vec;

    pub struct Opts {
        pub kind: FatKind,
        pub bps: u32,
        pub spc: u32,
        /// Allocate clusters out of order, so chains are fragmented.
        pub fragment: bool,
        /// Use every `stride`-th cluster (1 = all), so chains have gaps of
        /// exactly `stride - 1` clusters.
        pub stride: usize,
        /// Number of FATs (the second is a mirror).
        pub nfats: u32,
    }

    pub struct Layout {
        pub total: u32,
        pub fatsz: u32,
        pub reserved: u32,
        pub root_entries: u32,
        pub clusters: u32,
    }

    pub fn layout(o: &Opts) -> Layout {
        // Cluster counts well inside each type's range.
        let clusters: u32 = match o.kind {
            FatKind::Fat12 => 3000,
            FatKind::Fat16 => 40_000,
            FatKind::Fat32 => 70_000,
        };
        let bits = match o.kind {
            FatKind::Fat12 => 12,
            FatKind::Fat16 => 16,
            FatKind::Fat32 => 32,
        };
        let fatsz = ((clusters as u64 + 2) * bits)
            .div_ceil(8)
            .div_ceil(o.bps as u64) as u32;
        let reserved = if o.kind == FatKind::Fat32 { 32 } else { 1 };
        let root_entries: u32 = if o.kind == FatKind::Fat32 { 0 } else { 512 };
        let root_sectors = (root_entries * 32).div_ceil(o.bps);
        let total = reserved + o.nfats * fatsz + root_sectors + clusters * o.spc;
        Layout {
            total,
            fatsz,
            reserved,
            root_entries,
            clusters,
        }
    }

    struct Writer<'a> {
        d: &'a mut Sparse,
        base: u64,
        o: &'a Opts,
        l: Layout,
        order: Vec<u32>,
        next_free: usize,
        shorts: u32,
    }

    impl Writer<'_> {
        fn fat_set(&mut self, c: u32, v: u32) {
            for f in 0..self.o.nfats {
                let fat = self.base
                    + (self.l.reserved as u64 + f as u64 * self.l.fatsz as u64) * self.o.bps as u64;
                match self.o.kind {
                    FatKind::Fat12 => {
                        let off = fat + c as u64 + c as u64 / 2;
                        let mut w = self.d.read_bytes(off, 2);
                        let cur = u16::from_le_bytes([w[0], w[1]]);
                        let new = if c & 1 != 0 {
                            (cur & 0x000f) | ((v as u16) << 4)
                        } else {
                            (cur & 0xf000) | (v as u16 & 0xfff)
                        };
                        w.copy_from_slice(&new.to_le_bytes());
                        self.d.write(off, &w);
                    }
                    FatKind::Fat16 => self.d.write(fat + 2 * c as u64, &(v as u16).to_le_bytes()),
                    FatKind::Fat32 => self.d.write(fat + 4 * c as u64, &v.to_le_bytes()),
                }
            }
        }

        fn eoc(&self) -> u32 {
            match self.o.kind {
                FatKind::Fat12 => 0xfff,
                FatKind::Fat16 => 0xffff,
                FatKind::Fat32 => 0x0fff_ffff,
            }
        }

        fn cluster_off(&self, c: u32) -> u64 {
            let root_sectors = (self.l.root_entries * 32).div_ceil(self.o.bps);
            let meta = self.l.reserved + self.o.nfats * self.l.fatsz + root_sectors;
            self.base + (meta as u64 + (c - 2) as u64 * self.o.spc as u64) * self.o.bps as u64
        }

        /// Allocates and writes a chain holding `data` (at least one cluster).
        fn chain(&mut self, data: &[u8]) -> u32 {
            let cb = (self.o.bps * self.o.spc) as usize;
            let n = data.len().div_ceil(cb).max(1);
            let cs: Vec<u32> = (0..n)
                .map(|_| {
                    let c = self.order[self.next_free];
                    self.next_free += 1;
                    c
                })
                .collect();
            for (i, &c) in cs.iter().enumerate() {
                let chunk = &data[(i * cb).min(data.len())..((i + 1) * cb).min(data.len())];
                let off = self.cluster_off(c);
                self.d.write(off, chunk);
                let next = if i + 1 < n { cs[i + 1] } else { self.eoc() };
                self.fat_set(c, next);
            }
            cs[0]
        }

        fn short_name(&mut self, name: &str) -> ([u8; 11], bool) {
            let up = name.to_ascii_uppercase();
            let (b, e) = match up.rfind('.') {
                Some(p) if p > 0 => (&up[..p], &up[p + 1..]),
                _ => (&up[..], ""),
            };
            let plain = |s: &str| s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
            let mut s = [b' '; 11];
            if b.len() <= 8 && e.len() <= 3 && plain(b) && plain(e) && up == name {
                s[..b.len()].copy_from_slice(b.as_bytes());
                s[8..8 + e.len()].copy_from_slice(e.as_bytes());
                return (s, false);
            }
            self.shorts += 1;
            let tag = std::format!("~{}", self.shorts);
            let stem: Vec<u8> = b
                .bytes()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8 - tag.len())
                .collect();
            s[..stem.len()].copy_from_slice(&stem);
            s[stem.len()..stem.len() + tag.len()].copy_from_slice(tag.as_bytes());
            let ext: Vec<u8> = e
                .bytes()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(3)
                .collect();
            s[8..8 + ext.len()].copy_from_slice(&ext);
            (s, true)
        }
    }

    pub fn entries_for(
        name: &str,
        short: [u8; 11],
        long: bool,
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Vec<[u8; 32]> {
        let mut out = Vec::new();
        if long {
            let u: Vec<u16> = name.encode_utf16().collect();
            let n = u.len().div_ceil(13);
            let sum = short_name_checksum(&short);
            for seq in (1..=n).rev() {
                let mut e = [0u8; 32];
                e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
                e[11] = ATTR_LONG_NAME;
                e[13] = sum;
                let offs: Vec<usize> = (1..11)
                    .step_by(2)
                    .chain((14..26).step_by(2))
                    .chain((28..32).step_by(2))
                    .collect();
                for (k, &o) in offs.iter().enumerate() {
                    let i = (seq - 1) * 13 + k;
                    let c: u16 = match i.cmp(&u.len()) {
                        core::cmp::Ordering::Less => u[i],
                        core::cmp::Ordering::Equal => 0,
                        core::cmp::Ordering::Greater => 0xffff,
                    };
                    e[o..o + 2].copy_from_slice(&c.to_le_bytes());
                }
                out.push(e);
            }
        }
        let mut e = [0u8; 32];
        e[0..11].copy_from_slice(&short);
        e[11] = attr;
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        out.push(e);
        out
    }

    pub enum Node {
        File(String, Vec<u8>),
        Dir(String, Vec<Node>),
    }

    fn write_dir(w: &mut Writer, nodes: &[Node], root: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if root {
            let mut label = [0u8; 32];
            label[0..11].copy_from_slice(b"OPTIMAL    ");
            label[11] = ATTR_VOLUME_ID;
            bytes.extend_from_slice(&label);
            // A deleted entry and a stray LFN run, which must be skipped.
            let mut del = entries_for("gone.txt", *b"GONE    TXT", false, 0x20, 0, 0)[0];
            del[0] = 0xe5;
            bytes.extend_from_slice(&del);
        }
        for n in nodes {
            match n {
                Node::File(name, data) => {
                    let (s, long) = w.short_name(name);
                    let c = if data.is_empty() { 0 } else { w.chain(data) };
                    for e in entries_for(name, s, long, 0x20, c, data.len() as u32) {
                        bytes.extend_from_slice(&e);
                    }
                }
                Node::Dir(name, kids) => {
                    let (s, long) = w.short_name(name);
                    let content = write_dir(w, kids, false);
                    let c = w.chain(&content);
                    for e in entries_for(name, s, long, ATTR_DIRECTORY, c, 0) {
                        bytes.extend_from_slice(&e);
                    }
                }
            }
        }
        bytes
    }

    /// Writes a FAT volume at byte `base` of `d` and returns its size in sectors.
    pub fn mkfat(d: &mut Sparse, base: u64, o: &Opts, tree: &[Node]) -> u64 {
        let l = layout(o);
        let mut bs = vec![0u8; 512];
        bs[0] = 0xeb;
        bs[1] = 0x58;
        bs[2] = 0x90;
        bs[3..11].copy_from_slice(b"OPTTEST ");
        bs[11..13].copy_from_slice(&(o.bps as u16).to_le_bytes());
        bs[13] = o.spc as u8;
        bs[14..16].copy_from_slice(&(l.reserved as u16).to_le_bytes());
        bs[16] = o.nfats as u8;
        bs[17..19].copy_from_slice(&(l.root_entries as u16).to_le_bytes());
        bs[21] = 0xf8;
        if o.kind == FatKind::Fat32 {
            bs[32..36].copy_from_slice(&l.total.to_le_bytes());
            bs[36..40].copy_from_slice(&l.fatsz.to_le_bytes());
            bs[44..48].copy_from_slice(&2u32.to_le_bytes());
        } else {
            if l.total < 0x10000 {
                bs[19..21].copy_from_slice(&(l.total as u16).to_le_bytes());
            } else {
                bs[32..36].copy_from_slice(&l.total.to_le_bytes());
            }
            bs[22..24].copy_from_slice(&(l.fatsz as u16).to_le_bytes());
        }
        bs[510] = 0x55;
        bs[511] = 0xaa;
        d.write(base, &bs);
        let mut order: Vec<u32> = (2..l.clusters + 2).collect();
        if o.kind == FatKind::Fat32 {
            order.retain(|&c| c != 2); // the root takes cluster 2
        }
        order = order.into_iter().step_by(o.stride.max(1)).collect();
        if o.fragment {
            // Interleave from both ends: every chain jumps around.
            let mut f = Vec::new();
            let (mut a, mut b) = (0usize, order.len() - 1);
            while a <= b {
                f.push(order[b]);
                if a != b {
                    f.push(order[a]);
                }
                a += 1;
                if b == 0 {
                    break;
                }
                b -= 1;
            }
            order = f;
        }
        let mut w = Writer {
            d,
            base,
            o,
            l: layout(o),
            order,
            next_free: 0,
            shorts: 0,
        };
        let media = 0xf8u32;
        let (r0, r1) = match o.kind {
            FatKind::Fat12 => (0xf00 | media, 0xfff),
            FatKind::Fat16 => (0xff00 | media, 0xffff),
            FatKind::Fat32 => (0x0fff_ff00 | media, 0x0fff_ffff),
        };
        w.fat_set(0, r0);
        w.fat_set(1, r1);
        let root = write_dir(&mut w, tree, true);
        if o.kind == FatKind::Fat32 {
            let cb = (o.bps * o.spc) as usize;
            assert!(root.len() <= cb, "test root must fit one cluster");
            let off = w.cluster_off(2);
            w.d.write(off, &root);
            let e = w.eoc();
            w.fat_set(2, e);
        } else {
            let root_off =
                base + (w.l.reserved as u64 + o.nfats as u64 * w.l.fatsz as u64) * o.bps as u64;
            assert!(root.len() <= w.l.root_entries as usize * 32);
            w.d.write(root_off, &root);
        }
        l.total as u64 * o.bps as u64 / w.d.ss as u64
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;
    use crate::disk::sim::Sparse;
    use crate::testrng::Rng;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn data(len: usize, seed: u64) -> Vec<u8> {
        let mut r = Rng::new(seed);
        (0..len).map(|_| r.next() as u8).collect()
    }

    fn tree() -> Vec<Node> {
        tree_with(300_000)
    }

    fn tree_with(kernel: usize) -> Vec<Node> {
        vec![
            Node::File("README.TXT".into(), b"hello".to_vec()),
            Node::Dir(
                "boot".into(),
                vec![
                    Node::File("optimal-boot.conf".into(), data(3000, 1)),
                    Node::File("optimal-boot.conf.sig".into(), data(104, 2)),
                    Node::File("optimal-kernel".into(), data(kernel, 3)),
                    Node::File("empty".into(), Vec::new()),
                    Node::File(
                        "a-name-that-needs-three-lfn-entries-to-hold.bin".into(),
                        data(700, 4),
                    ),
                    Node::Dir("deeper".into(), vec![Node::File("x".into(), data(1, 5))]),
                ],
            ),
        ]
    }

    const KINDS: [FatKind; 3] = [FatKind::Fat12, FatKind::Fat16, FatKind::Fat32];

    fn volume(kind: FatKind, bps: u32, ss: usize, fragment: bool) -> (Sparse, u64) {
        volume_of(kind, bps, ss, fragment, &tree())
    }

    fn volume_of(kind: FatKind, bps: u32, ss: usize, fragment: bool, t: &[Node]) -> (Sparse, u64) {
        let o = Opts {
            kind,
            bps,
            spc: if kind == FatKind::Fat12 { 4 } else { 1 },
            fragment,
            stride: 1,
            nfats: 2,
        };
        let total_bytes = layout(&o).total as u64 * bps as u64;
        let mut d = Sparse::new(ss, total_bytes / ss as u64 + 16);
        let n = mkfat(&mut d, 0, &o, t);
        (d, n)
    }

    fn read(fat: &mut Fat<Sparse>, path: &str) -> Result<Vec<u8>, FatError> {
        let f = fat.open(path)?;
        let mut buf = vec![0u8; f.size as usize];
        fat.read(&f, &mut buf)?;
        Ok(buf)
    }

    #[test]
    fn reads_every_file_on_every_kind() {
        for kind in KINDS {
            for (bps, ss) in [(512, 512), (4096, 4096), (4096, 512)] {
                for fragment in [false, true] {
                    let (mut d, n) = volume(kind, bps, ss, fragment);
                    let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
                    assert_eq!(fat.kind, kind);
                    let ctx = std::format!("{kind:?} bps {bps} ss {ss} frag {fragment}");
                    assert_eq!(read(&mut fat, "/README.TXT").unwrap(), b"hello", "{ctx}");
                    assert_eq!(
                        read(&mut fat, "/boot/optimal-boot.conf").unwrap(),
                        data(3000, 1),
                        "{ctx}"
                    );
                    assert_eq!(
                        read(&mut fat, "/boot/optimal-boot.conf.sig").unwrap(),
                        data(104, 2)
                    );
                    assert_eq!(
                        read(&mut fat, "/boot/optimal-kernel").unwrap(),
                        data(300_000, 3),
                        "{ctx}"
                    );
                    assert_eq!(read(&mut fat, "/boot/empty").unwrap(), b"");
                    assert_eq!(
                        read(
                            &mut fat,
                            "/boot/a-name-that-needs-three-lfn-entries-to-hold.bin"
                        )
                        .unwrap(),
                        data(700, 4)
                    );
                    assert_eq!(read(&mut fat, "/boot/deeper/x").unwrap(), data(1, 5));
                }
            }
        }
    }

    #[test]
    fn chains_with_one_cluster_gaps() {
        // Clusters 3, 5, 7, ...: no two are contiguous, and each gap is
        // exactly one cluster (a reader that merged c and c + 2 would read
        // the cluster in between).
        for kind in KINDS {
            let o = Opts {
                kind,
                bps: 512,
                spc: 1,
                fragment: false,
                stride: 2,
                nfats: 2,
            };
            let total = layout(&o).total as u64;
            let mut d = Sparse::new(512, total + 16);
            let n = mkfat(&mut d, 0, &o, &tree_with(20_000));
            let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            assert_eq!(
                read(&mut fat, "/boot/optimal-kernel").unwrap(),
                data(20_000, 3),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn chain_into_a_cluster_past_the_end_is_corrupt() {
        // A two-cluster file in the volume's last cluster, whose FAT entry
        // points one past it: contiguous, but not a cluster that exists.
        for kind in KINDS {
            let (mut d, n) = volume(kind, 512, 512, false);
            let fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            let (last, fat_off, cb) = (fat.clusters + 1, fat.fat_offset, fat.cluster_bytes);
            let _ = fat;
            let at = find_entry(&d, n, b"README  TXT");
            d.write(at + 20, &((last >> 16) as u16).to_le_bytes());
            d.write(at + 26, &(last as u16).to_le_bytes());
            d.write(at + 28, &(2 * cb).to_le_bytes());
            let next = last + 1;
            match kind {
                FatKind::Fat12 => {
                    let off = fat_off + last as u64 + last as u64 / 2;
                    let mut w = d.read_bytes(off, 2);
                    let cur = u16::from_le_bytes([w[0], w[1]]);
                    let v = if last & 1 != 0 {
                        (cur & 0xf) | (next as u16) << 4
                    } else {
                        (cur & 0xf000) | next as u16
                    };
                    w.copy_from_slice(&v.to_le_bytes());
                    d.write(off, &w);
                }
                FatKind::Fat16 => d.write(fat_off + 2 * last as u64, &(next as u16).to_le_bytes()),
                FatKind::Fat32 => d.write(fat_off + 4 * last as u64, &next.to_le_bytes()),
            }
            let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            assert_eq!(
                read(&mut fat, "/README.TXT"),
                Err(FatError::Corrupt),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn names_match_without_case_and_by_alias() {
        let (mut d, n) = volume(FatKind::Fat16, 512, 512, false);
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        assert!(read(&mut fat, "/BOOT/Optimal-Boot.CONF").is_ok());
        assert!(read(&mut fat, "/readme.txt").is_ok());
        // The 8.3 alias the writer gave the config ("OPTIMA~2.CON").
        assert_eq!(read(&mut fat, "/boot/optima~2.con").unwrap(), data(3000, 1));
    }

    #[test]
    fn contiguous_files_are_read_in_one_request() {
        let (mut d, n) = volume(FatKind::Fat32, 512, 512, false);
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        let f = fat.open("/boot/optimal-kernel").unwrap();
        let mut buf = vec![0u8; f.size as usize];
        let _ = fat;
        d.reads = 0;
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        fat.read(&f, &mut buf).unwrap();
        let _ = fat;
        // 586 data sectors, plus the boot sector and the FAT sectors walked.
        let data_sectors = 300_000u64.div_ceil(512);
        assert!(d.reads <= data_sectors + 1 + 5, "{} reads", d.reads);
    }

    #[test]
    fn path_refusals() {
        let (mut d, n) = volume(FatKind::Fat12, 512, 512, false);
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        for (p, e) in [
            ("boot/x", FatError::BadPath),
            ("/boot//optimal-kernel", FatError::BadPath),
            ("/boot/../README.TXT", FatError::BadPath),
            ("/./README.TXT", FatError::BadPath),
            ("/", FatError::BadPath),
            ("/boot/", FatError::BadPath),
            ("/nope", FatError::NotFound),
            ("/README.TXT/x", FatError::NotADirectory),
            ("/boot", FatError::IsADirectory),
            ("/boot/deeper", FatError::IsADirectory),
            ("/gone.txt", FatError::NotFound), // deleted entry
            ("/OPTIMAL", FatError::NotFound),  // the volume label is not a file
        ] {
            assert_eq!(read(&mut fat, p), Err(e), "{p}");
        }
        let long = std::format!("/{}", "a".repeat(256));
        assert_eq!(fat.open(&long), Err(FatError::BadPath));
    }

    /// Finds the directory entry for `name` (short form) in the raw volume.
    fn find_entry(d: &Sparse, n: u64, short: &[u8; 11]) -> u64 {
        let total = n * d.ss as u64;
        let mut off = 0;
        while off < total {
            if d.data.contains_key(&(off / d.ss as u64)) && &d.read_bytes(off, 11)[..] == short {
                return off;
            }
            off += 32;
        }
        panic!("entry not found");
    }

    #[test]
    fn damaged_chains_are_refused_not_followed() {
        for kind in KINDS {
            let (d0, n) = volume(kind, 512, 512, false);
            let fresh = || Sparse {
                ss: d0.ss,
                total: d0.total,
                data: d0.data.clone(),
                reads: 0,
                bad: Vec::new(),
            };
            let at = find_entry(&d0, n, b"README  TXT");
            // File size larger than its one-cluster chain: truncated.
            let mut d = fresh();
            d.write(at + 28, &100_000u32.to_le_bytes());
            let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            assert_eq!(
                read(&mut fat, "/README.TXT"),
                Err(FatError::Truncated),
                "{kind:?}"
            );
            // First cluster past the end of the volume.
            let mut d = fresh();
            d.write(at + 26, &0xfff0u16.to_le_bytes());
            if kind == FatKind::Fat32 {
                d.write(at + 20, &0x0fffu16.to_le_bytes());
            }
            let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            assert_eq!(
                read(&mut fat, "/README.TXT"),
                Err(FatError::Corrupt),
                "{kind:?}"
            );
            // First cluster 1 (reserved) on a non-empty file.
            let mut d = fresh();
            d.write(at + 20, &0u16.to_le_bytes());
            d.write(at + 26, &1u16.to_le_bytes());
            let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
            assert_eq!(
                read(&mut fat, "/README.TXT"),
                Err(FatError::Corrupt),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn looped_chain_terminates() {
        // The kernel's chain points back at its own first cluster: the read
        // still stops after exactly size/cluster clusters (and the loader's
        // hash check rejects what came back).
        let (mut d, n) = volume(FatKind::Fat16, 512, 512, false);
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        let f = fat.open("/boot/optimal-kernel").unwrap();
        let _ = fat;
        let l = layout(&Opts {
            kind: FatKind::Fat16,
            bps: 512,
            spc: 1,
            fragment: false,
            stride: 1,
            nfats: 2,
        });
        let fat_off = l.reserved as u64 * 512;
        d.write(
            fat_off + 2 * (f.first_cluster as u64 + 1),
            &(f.first_cluster as u16).to_le_bytes(),
        );
        d.reads = 0;
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        let mut buf = vec![0u8; f.size as usize];
        assert!(fat.read(&f, &mut buf).is_ok());
        let _ = fat;
        assert!(d.reads < 2000, "{}", d.reads);
        assert_ne!(buf, data(300_000, 3));
    }

    #[test]
    fn broken_lfn_falls_back_to_the_alias() {
        let (mut d, n) = volume(FatKind::Fat16, 512, 512, false);
        // Corrupt the checksum byte of the config's LFN entries' short entry
        // by renaming its alias: the long name no longer belongs to it.
        let at = find_entry(&d, n, b"OPTIMA~2CON");
        d.write(at, b"OPTIMA~9");
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        assert_eq!(
            read(&mut fat, "/boot/optimal-boot.conf"),
            Err(FatError::NotFound)
        );
        assert!(read(&mut fat, "/boot/optima~9.con").is_ok());
        // The other long names are unaffected.
        assert!(read(&mut fat, "/boot/optimal-kernel").is_ok());
    }

    #[test]
    fn bpb_refusals() {
        let (d0, n) = volume(FatKind::Fat32, 512, 512, false);
        let boot = d0.read_bytes(0, 512);
        let try_bpb = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut d = Sparse {
                ss: 512,
                total: d0.total,
                data: d0.data.clone(),
                reads: 0,
                bad: Vec::new(),
            };
            let mut b = boot.clone();
            f(&mut b);
            d.write(0, &b);
            Fat::mount(Window::new(&mut d, 0, n).unwrap()).err()
        };
        assert_eq!(try_bpb(&|_| {}), None);
        let bad = Some(FatError::BadBpb);
        assert_eq!(try_bpb(&|b| b[510] = 0), bad); // no signature
        assert_eq!(try_bpb(&|b| b[0] = 0), bad); // no jump
        assert_eq!(
            try_bpb(&|b| b[11..13].copy_from_slice(&500u16.to_le_bytes())),
            bad
        );
        assert_eq!(try_bpb(&|b| b[13] = 3), bad); // not a power of two
        assert_eq!(try_bpb(&|b| b[14] = 0), bad); // no reserved sectors
        assert_eq!(try_bpb(&|b| b[16] = 0), bad); // no FATs
        assert_eq!(try_bpb(&|b| b[21] = 0x12), bad); // media byte
        assert_eq!(
            try_bpb(&|b| b[32..36].copy_from_slice(&0u32.to_le_bytes())),
            bad
        ); // no size
        assert_eq!(
            try_bpb(&|b| b[32..36].copy_from_slice(&(n as u32 + 1).to_le_bytes())),
            bad
        ); // past the partition
        assert_eq!(
            try_bpb(&|b| b[36..40].copy_from_slice(&1u32.to_le_bytes())),
            bad
        ); // FAT too small
        assert_eq!(
            try_bpb(&|b| b[44..48].copy_from_slice(&1u32.to_le_bytes())),
            bad
        ); // root cluster 1
        assert_eq!(try_bpb(&|b| b[42] = 1), bad); // FAT32 version
        assert_eq!(try_bpb(&|b| b[17] = 1), bad); // FAT32 with a fixed root
        assert_eq!(
            try_bpb(&|b| {
                b[40] = 0x83;
            }),
            bad
        ); // active FAT 3 of 2
        // Active FAT 1 (mirroring off) is valid: the second FAT is a copy.
        assert_eq!(
            try_bpb(&|b| {
                b[40] = 0x81;
            }),
            None
        );
    }

    #[test]
    fn huge_directory_stops_at_the_spec_limit() {
        // A FAT32 directory chain that loops forever must stop at 65 536 entries.
        let (mut d, n) = volume(FatKind::Fat32, 512, 512, false);
        let l = layout(&Opts {
            kind: FatKind::Fat32,
            bps: 512,
            spc: 1,
            fragment: false,
            stride: 1,
            nfats: 2,
        });
        // Root (cluster 2) points to itself; its entries never end.
        let mut root = d.read_bytes((l.reserved as u64 + 2 * l.fatsz as u64) * 512, 512);
        for e in root.as_chunks_mut::<32>().0 {
            if e[0] == 0 {
                e[0] = b'Z';
                e[11] = 0x20;
            }
        }
        d.write((l.reserved as u64 + 2 * l.fatsz as u64) * 512, &root);
        for f in 0..2u64 {
            d.write(
                (l.reserved as u64 + f * l.fatsz as u64) * 512 + 8,
                &2u32.to_le_bytes(),
            );
        }
        let mut fat = Fat::mount(Window::new(&mut d, 0, n).unwrap()).unwrap();
        assert_eq!(fat.open("/nothing"), Err(FatError::DirectoryTooLarge));
    }

    #[test]
    fn checksum_of_a_known_name() {
        // Worked by hand: fold "README  TXT" with rotate-right-and-add.
        let mut s = 0u8;
        for c in *b"README  TXT" {
            s = s.rotate_right(1).wrapping_add(c);
        }
        assert_eq!(short_name_checksum(b"README  TXT"), s);
        assert_eq!(s, 0x73); // computed independently (Python, same fold)
        assert_eq!(short_name_checksum(&[0; 11]), 0);
    }

    static OPENED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    #[test]
    fn fuzz_fat() {
        // Mutated metadata (boot sector, FAT, directories): never a panic,
        // never an unbounded walk, and a read result always has the
        // recorded size.
        let bases: Vec<(Sparse, u64)> = (0..6)
            .map(|i| volume_of(KINDS[i / 2], 512, 512, i % 2 == 1, &tree_with(20_000)))
            .collect();
        crate::fuzzkit::run("fat", 0xfa7, |rng| {
            let (base, n) = &bases[rng.below(6) as usize];
            let n = *n;
            let mut d = Sparse {
                ss: 512,
                total: base.total,
                data: base.data.clone(),
                reads: 0,
                bad: Vec::new(),
            };
            let meta: Vec<u64> = d
                .data
                .keys()
                .copied()
                .filter(|&k| k < 1000 || rng.below(8) == 0)
                .collect();
            for _ in 0..1 + rng.below(3) {
                let lba = meta[rng.below(meta.len() as u64) as usize];
                let mut b = d.read_bytes(lba * 512, 512);
                crate::fuzzkit::mutate(rng, &mut b);
                b.resize(512, 0);
                d.write(lba * 512, &b);
            }
            d.reads = 0;
            let Ok(mut fat) = Fat::mount(Window::new(&mut d, 0, n).unwrap()) else {
                return;
            };
            for p in [
                "/boot/optimal-kernel",
                "/boot/optimal-boot.conf",
                "/README.TXT",
                "/boot/deeper/x",
            ] {
                if let Ok(f) = fat.open(p) {
                    OPENED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    if f.size > 1 << 20 {
                        continue;
                    }
                    let mut buf = vec![0u8; f.size as usize];
                    let _ = fat.read(&f, &mut buf);
                }
            }
            let _ = fat;
            // Each of 4 paths: at most 2 directories of 65 536 entries
            // (4096 sectors each) plus a 1 MiB file and its FAT sectors.
            assert!(
                d.reads < 4 * (2 * 4096 + 2 * 2048 + 64),
                "{} reads",
                d.reads
            );
        });
        // Most mutations must leave something to walk, or this fuzzes nothing.
        let opened = OPENED.load(core::sync::atomic::Ordering::Relaxed);
        let cases = crate::fuzzkit::cases();
        assert!(
            cases < 100 || opened >= cases,
            "only {opened} opens in {cases} cases"
        );
    }
}
