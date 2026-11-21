//! x86_64 4-level page-table editing.
//!
//! This module only computes entries and walks tables; the kernel provides
//! the memory through [`TableMemory`] (reading and writing entries through
//! its direct map, allocating zeroed frames) and flushes the TLB.
//!
//! Rules enforced here, for every caller, rather than trusted to each caller:
//! - no mapping is ever both writable and executable (W^X),
//! - addresses must be canonical, aligned to the page size, and physical
//!   addresses must fit in 52 bits,
//! - mapping over an existing mapping is refused, never silently replaced,
//! - a failed operation leaves every existing translation unchanged.
//!
//! Proof target 1 in docs/verification.md: the Kani proofs at the bottom
//! check these properties for every address, size and flag combination.

use crate::PAGE_SIZE;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
/// Physical address bits of a table pointer or 4 KiB page entry.
pub const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
/// Largest physical address the architecture allows (52 bits).
pub const MAX_PHYS: u64 = 1 << 52;

/// Supplies and accesses page-table memory. Tables are named by physical
/// address.
pub trait TableMemory {
    fn read(&self, table: u64, index: usize) -> u64;
    fn write(&mut self, table: u64, index: usize, value: u64);
    /// A zero-filled, 4 KiB-aligned frame for a new table, or None if memory
    /// is exhausted.
    fn alloc_table(&mut self) -> Option<u64>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    Size4K,
    Size2M,
    Size1G,
}

impl PageSize {
    pub const fn bytes(self) -> u64 {
        match self {
            PageSize::Size4K => 4096,
            PageSize::Size2M => 2 << 20,
            PageSize::Size1G => 1 << 30,
        }
    }
    /// Table level the leaf entry lives at (1 = page table, 4 = PML4).
    const fn level(self) -> usize {
        match self {
            PageSize::Size4K => 1,
            PageSize::Size2M => 2,
            PageSize::Size1G => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// Normal memory.
    WriteBack,
    /// Device registers (MMIO): no caching at all.
    Uncached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub global: bool,
    pub cache: Cache,
}

impl Flags {
    /// Read-only, no-execute, kernel-only, cached.
    pub const READ: Flags = Flags {
        writable: false,
        executable: false,
        user: false,
        global: false,
        cache: Cache::WriteBack,
    };
    pub const READ_WRITE: Flags = Flags {
        writable: true,
        ..Flags::READ
    };
    pub const READ_EXECUTE: Flags = Flags {
        executable: true,
        ..Flags::READ
    };

    fn bits(self) -> u64 {
        let mut b = PRESENT;
        if self.writable {
            b |= WRITABLE;
        }
        if !self.executable {
            b |= NO_EXECUTE;
        }
        if self.user {
            b |= USER;
        }
        if self.global {
            b |= GLOBAL;
        }
        if self.cache == Cache::Uncached {
            b |= CACHE_DISABLE | WRITE_THROUGH;
        }
        b
    }

    fn from_bits(e: u64) -> Flags {
        Flags {
            writable: e & WRITABLE != 0,
            executable: e & NO_EXECUTE == 0,
            user: e & USER != 0,
            global: e & GLOBAL != 0,
            cache: if e & CACHE_DISABLE != 0 {
                Cache::Uncached
            } else {
                Cache::WriteBack
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Writable and executable at once: never allowed.
    WriteAndExecute,
    /// Virtual address not canonical, or outside the half the flags allow.
    BadVirtual,
    /// Address not aligned to the page size, or physical address too big.
    Misaligned,
    /// Something is already mapped there.
    AlreadyMapped,
    /// No memory for a new page table.
    OutOfMemory,
    /// Nothing mapped there, or mapped with a different page size.
    NotMapped,
}

/// Result of looking up one virtual address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Physical address the given virtual address maps to (offset included).
    pub phys: u64,
    pub size: PageSize,
    pub flags: Flags,
}

pub fn is_canonical(virt: u64) -> bool {
    let top = virt >> 47;
    top == 0 || top == 0x1_ffff
}

fn index(virt: u64, level: usize) -> usize {
    ((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize
}

fn leaf_addr_mask(level: usize) -> u64 {
    // For 2 MiB and 1 GiB pages bit 12 is the PAT bit, not address.
    ADDR_MASK & !((1u64 << (12 + 9 * (level - 1))) - 1)
}

/// One address space, named by its top-level table (the value for CR3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressSpace {
    pub root: u64,
}

impl AddressSpace {
    pub fn new<M: TableMemory>(mem: &mut M) -> Option<AddressSpace> {
        mem.alloc_table().map(|root| AddressSpace { root })
    }

    /// Map `[virt, virt + size)` to `[phys, phys + size)`.
    pub fn map<M: TableMemory>(
        &self,
        mem: &mut M,
        virt: u64,
        phys: u64,
        size: PageSize,
        flags: Flags,
    ) -> Result<(), MapError> {
        if flags.writable && flags.executable {
            return Err(MapError::WriteAndExecute);
        }
        let user_half = virt >> 63 == 0;
        if !is_canonical(virt) || flags.user != user_half {
            return Err(MapError::BadVirtual);
        }
        if !virt.is_multiple_of(size.bytes())
            || !phys.is_multiple_of(size.bytes())
            || phys >= MAX_PHYS
        {
            return Err(MapError::Misaligned);
        }
        // Walk down, checking first, so a refusal changes nothing.
        let leaf = size.level();
        let mut table = self.root;
        let mut level = 4;
        while level > leaf {
            let e = mem.read(table, index(virt, level));
            if e & PRESENT == 0 {
                break;
            }
            if e & HUGE != 0 {
                return Err(MapError::AlreadyMapped);
            }
            table = e & ADDR_MASK;
            level -= 1;
        }
        if level == leaf && mem.read(table, index(virt, level)) & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        // Allocate the missing tables. If memory runs out part way, the tables
        // already linked in are empty, so no translation has changed.
        while level > leaf {
            let new = mem.alloc_table().ok_or(MapError::OutOfMemory)?;
            // Intermediate entries grant everything; the leaf restricts.
            let mut e = (new & ADDR_MASK) | PRESENT | WRITABLE;
            if flags.user {
                e |= USER;
            }
            mem.write(table, index(virt, level), e);
            table = new;
            level -= 1;
        }
        let mut e = phys | flags.bits();
        if size != PageSize::Size4K {
            e |= HUGE;
        }
        mem.write(table, index(virt, leaf), e);
        Ok(())
    }

    /// Find the leaf entry for `virt`: (table, index, level).
    fn leaf<M: TableMemory>(&self, mem: &M, virt: u64) -> Option<(u64, usize, usize)> {
        if !is_canonical(virt) {
            return None;
        }
        let mut table = self.root;
        let mut level = 4;
        loop {
            let i = index(virt, level);
            let e = mem.read(table, i);
            if e & PRESENT == 0 {
                return None;
            }
            if level == 1 || (level <= 3 && e & HUGE != 0) {
                return Some((table, i, level));
            }
            table = e & ADDR_MASK;
            level -= 1;
        }
    }

    pub fn translate<M: TableMemory>(&self, mem: &M, virt: u64) -> Option<Translation> {
        let (table, i, level) = self.leaf(mem, virt)?;
        let e = mem.read(table, i);
        let size = match level {
            1 => PageSize::Size4K,
            2 => PageSize::Size2M,
            _ => PageSize::Size1G,
        };
        Some(Translation {
            phys: (e & leaf_addr_mask(level)) + (virt & (size.bytes() - 1)),
            size,
            flags: Flags::from_bits(e),
        })
    }

    /// Remove the mapping of the page at `virt` (which must be the start of a
    /// page of `size`). Returns the physical address it mapped. The caller
    /// must flush the TLB for that page.
    pub fn unmap<M: TableMemory>(
        &self,
        mem: &mut M,
        virt: u64,
        size: PageSize,
    ) -> Result<u64, MapError> {
        if !virt.is_multiple_of(size.bytes()) {
            return Err(MapError::Misaligned);
        }
        let (table, i, level) = self.leaf(mem, virt).ok_or(MapError::NotMapped)?;
        if level != size.level() {
            return Err(MapError::NotMapped);
        }
        let e = mem.read(table, i);
        mem.write(table, i, 0);
        Ok(e & leaf_addr_mask(level))
    }

    /// Change the permissions of the page at `virt`. Same W^X and user/kernel
    /// rules as `map`. The caller must flush the TLB for that page.
    pub fn protect<M: TableMemory>(
        &self,
        mem: &mut M,
        virt: u64,
        size: PageSize,
        flags: Flags,
    ) -> Result<(), MapError> {
        if flags.writable && flags.executable {
            return Err(MapError::WriteAndExecute);
        }
        if flags.user != (virt >> 63 == 0) {
            return Err(MapError::BadVirtual);
        }
        if !virt.is_multiple_of(size.bytes()) {
            return Err(MapError::Misaligned);
        }
        let (table, i, level) = self.leaf(mem, virt).ok_or(MapError::NotMapped)?;
        if level != size.level() {
            return Err(MapError::NotMapped);
        }
        let e = mem.read(table, i);
        let mut new = (e & leaf_addr_mask(level)) | flags.bits();
        if level != 1 {
            new |= HUGE;
        }
        mem.write(table, i, new);
        Ok(())
    }

    /// Visit every present leaf: (virt, Translation). For checks and tests.
    pub fn for_each_leaf<M: TableMemory>(&self, mem: &M, mut f: impl FnMut(u64, Translation)) {
        fn walk<M: TableMemory>(
            mem: &M,
            table: u64,
            level: usize,
            base: u64,
            f: &mut impl FnMut(u64, Translation),
        ) {
            for i in 0..512 {
                let e = mem.read(table, i);
                if e & PRESENT == 0 {
                    continue;
                }
                let mut virt = base | ((i as u64) << (12 + 9 * (level - 1)));
                if level == 4 && i >= 256 {
                    virt |= 0xffff_0000_0000_0000;
                }
                if level == 1 || (level <= 3 && e & HUGE != 0) {
                    let size = match level {
                        1 => PageSize::Size4K,
                        2 => PageSize::Size2M,
                        _ => PageSize::Size1G,
                    };
                    f(
                        virt,
                        Translation {
                            phys: e & leaf_addr_mask(level),
                            size,
                            flags: Flags::from_bits(e),
                        },
                    );
                } else {
                    walk(mem, e & ADDR_MASK, level - 1, virt, f);
                }
            }
        }
        walk(mem, self.root, 4, 0, &mut f);
    }
}

/// Split `[virt, virt + len)` mapped to `[phys, ...)` into the largest pages
/// possible (1 GiB where both addresses allow, then 2 MiB, then 4 KiB), and
/// call `f` for each. `virt`, `phys` and `len` must be 4 KiB aligned.
pub fn for_each_page(
    virt: u64,
    phys: u64,
    len: u64,
    allow_1g: bool,
    mut f: impl FnMut(u64, u64, PageSize),
) {
    debug_assert!(virt.is_multiple_of(PAGE_SIZE) && phys.is_multiple_of(PAGE_SIZE));
    let mut off = 0;
    while off < len {
        let (v, p, left) = (virt + off, phys + off, len - off);
        let size = [PageSize::Size1G, PageSize::Size2M, PageSize::Size4K]
            .into_iter()
            .find(|s| {
                (allow_1g || *s != PageSize::Size1G)
                    && v.is_multiple_of(s.bytes())
                    && p.is_multiple_of(s.bytes())
                    && left >= s.bytes()
            })
            .unwrap_or(PageSize::Size4K);
        f(v, p, size);
        off += size.bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    /// Tables in a Vec; table N lives at physical address (N + 1) * 4096.
    struct Mem {
        tables: Vec<[u64; 512]>,
        limit: usize,
    }
    impl Mem {
        fn new(limit: usize) -> Mem {
            Mem {
                tables: Vec::new(),
                limit,
            }
        }
    }
    impl TableMemory for Mem {
        fn read(&self, t: u64, i: usize) -> u64 {
            self.tables[(t / 4096 - 1) as usize][i]
        }
        fn write(&mut self, t: u64, i: usize, v: u64) {
            self.tables[(t / 4096 - 1) as usize][i] = v;
        }
        fn alloc_table(&mut self) -> Option<u64> {
            if self.tables.len() >= self.limit {
                return None;
            }
            self.tables.push([0; 512]);
            Some(self.tables.len() as u64 * 4096)
        }
    }

    const K: u64 = 0xffff_8000_0000_0000;

    #[test]
    fn map_translate_unmap() {
        let mut m = Mem::new(64);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(
            &mut m,
            K + 0x5000,
            0x1234_5000,
            PageSize::Size4K,
            Flags::READ_WRITE,
        )
        .unwrap();
        let t = a.translate(&m, K + 0x5abc).unwrap();
        assert_eq!(t.phys, 0x1234_5abc);
        assert_eq!(t.flags, Flags::READ_WRITE);
        assert_eq!(a.translate(&m, K + 0x6000), None);
        assert_eq!(
            a.unmap(&mut m, K + 0x5000, PageSize::Size4K),
            Ok(0x1234_5000)
        );
        assert_eq!(a.translate(&m, K + 0x5000), None);
    }

    #[test]
    fn huge_pages() {
        let mut m = Mem::new(64);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(&mut m, K, 1 << 30, PageSize::Size1G, Flags::READ)
            .unwrap();
        a.map(
            &mut m,
            K + (1 << 30),
            2 << 20,
            PageSize::Size2M,
            Flags::READ_EXECUTE,
        )
        .unwrap();
        assert_eq!(
            a.translate(&m, K + 0x12345).unwrap().phys,
            (1 << 30) + 0x12345
        );
        let t = a.translate(&m, K + (1 << 30) + 0x1000).unwrap();
        assert_eq!((t.phys, t.size), ((2 << 20) + 0x1000, PageSize::Size2M));
        // Can't map a 4K page inside a huge page.
        assert_eq!(
            a.map(&mut m, K + 0x1000, 0, PageSize::Size4K, Flags::READ),
            Err(MapError::AlreadyMapped)
        );
        // Wrong size on unmap is refused.
        assert_eq!(
            a.unmap(&mut m, K, PageSize::Size2M),
            Err(MapError::NotMapped)
        );
    }

    #[test]
    fn rules_enforced() {
        let mut m = Mem::new(64);
        let a = AddressSpace::new(&mut m).unwrap();
        let wx = Flags {
            writable: true,
            executable: true,
            ..Flags::READ
        };
        assert_eq!(
            a.map(&mut m, K, 0, PageSize::Size4K, wx),
            Err(MapError::WriteAndExecute)
        );
        assert_eq!(
            a.map(
                &mut m,
                0x0000_8000_0000_0000,
                0,
                PageSize::Size4K,
                Flags::READ
            ),
            Err(MapError::BadVirtual)
        );
        // Kernel flags in the user half and the other way round.
        assert_eq!(
            a.map(&mut m, 0x1000, 0, PageSize::Size4K, Flags::READ),
            Err(MapError::BadVirtual)
        );
        let user = Flags {
            user: true,
            ..Flags::READ
        };
        assert_eq!(
            a.map(&mut m, K, 0, PageSize::Size4K, user),
            Err(MapError::BadVirtual)
        );
        assert_eq!(
            a.map(&mut m, K + 0x1000, 0, PageSize::Size2M, Flags::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            a.map(&mut m, K, 0x1001, PageSize::Size4K, Flags::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            a.map(&mut m, K, MAX_PHYS, PageSize::Size4K, Flags::READ),
            Err(MapError::Misaligned)
        );
        a.map(&mut m, K, 0, PageSize::Size4K, Flags::READ).unwrap();
        assert_eq!(
            a.map(&mut m, K, 0x5000, PageSize::Size4K, Flags::READ),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(a.translate(&m, K).unwrap().phys, 0);
        assert_eq!(
            a.protect(&mut m, K, PageSize::Size4K, wx),
            Err(MapError::WriteAndExecute)
        );
        a.protect(&mut m, K, PageSize::Size4K, Flags::READ_WRITE)
            .unwrap();
        assert_eq!(a.translate(&m, K).unwrap().flags, Flags::READ_WRITE);
    }

    #[test]
    fn out_of_memory_changes_nothing() {
        let mut m = Mem::new(3); // root + 2 tables: not enough for a 4K leaf
        let a = AddressSpace::new(&mut m).unwrap();
        assert_eq!(
            a.map(&mut m, K, 0, PageSize::Size4K, Flags::READ),
            Err(MapError::OutOfMemory)
        );
        let mut n = 0;
        a.for_each_leaf(&m, |_, _| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn page_splitting_uses_biggest_pages() {
        // Worked by hand: [0x3ff000, 0x3ff000 + 2 GiB) = [0x3ff000, 0x803ff000).
        //   0x3ff000             one 4K page (not 2M aligned)
        //   0x400000..0x40000000 (0x40000000 - 0x400000) / 2M = 510 2M pages
        //   0x40000000           one 1G page (to 0x80000000)
        //   0x80000000           one 2M page (0x3ff000 left, fits one 2M)
        //   0x80200000..         0x1ff000 / 4K = 511 4K pages
        let mut v = Vec::new();
        for_each_page(K + 0x3ff000, 0x3ff000, 2 << 30, true, |a, b, s| {
            assert_eq!(a - K, b);
            v.push((b, s))
        });
        let count = |s| v.iter().filter(|x| x.1 == s).count();
        assert_eq!(count(PageSize::Size4K), 1 + 511);
        assert_eq!(count(PageSize::Size2M), 510 + 1);
        assert_eq!(count(PageSize::Size1G), 1);
        assert_eq!(v[0], (0x3ff000, PageSize::Size4K));
        assert_eq!(v[1], (0x400000, PageSize::Size2M));
        assert_eq!(v[511], (0x40000000, PageSize::Size1G));
        assert_eq!(v[512], (0x80000000, PageSize::Size2M));
        // Without 1G pages the middle becomes 512 more 2M pages.
        let mut n2 = 0;
        for_each_page(K + 0x3ff000, 0x3ff000, 2 << 30, false, |_, _, s| {
            assert_ne!(s, PageSize::Size1G);
            n2 += (s == PageSize::Size2M) as usize;
        });
        assert_eq!(n2, 511 + 512);
        // Virtual and physical misaligned to each other: 4K pages only.
        let mut big = 0;
        for_each_page(K + 0x200000, 0x201000, 8 << 20, true, |_, _, s| {
            big += (s != PageSize::Size4K) as usize;
        });
        assert_eq!(big, 0);
    }

    fn size_of(i: u8) -> PageSize {
        [PageSize::Size4K, PageSize::Size2M, PageSize::Size1G][i as usize % 3]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::test_cases(3000)))]
        /// Random map/unmap/protect sequences against a reference model of
        /// what each page should translate to.
        #[test]
        fn random_ops_match_model(
            ops in prop::collection::vec((0u8..3, 0u64..64, 0u8..3, 0u64..1024, 0u8..4), 1..120),
            probes in prop::collection::vec(0u64..(64u64 << 30), 50),
        ) {
            let mut m = Mem::new(1 << 12);
            let a = AddressSpace::new(&mut m).unwrap();
            // Model: virt page start -> (phys, size, flags)
            let mut model: BTreeMap<u64, (u64, PageSize, Flags)> = BTreeMap::new();
            let flag_set = [Flags::READ, Flags::READ_WRITE, Flags::READ_EXECUTE,
                Flags { cache: Cache::Uncached, ..Flags::READ_WRITE }];
            for (kind, slot, sz, phys_slot, fl) in ops {
                let size = size_of(sz);
                let virt = K + slot * size.bytes();
                let phys = phys_slot * size.bytes() % MAX_PHYS;
                let flags = flag_set[fl as usize];
                let overlaps = model.iter().any(|(&v, &(_, s, _))| v < virt + size.bytes() && virt < v + s.bytes());
                match kind {
                    0 => {
                        let r = a.map(&mut m, virt, phys, size, flags);
                        if overlaps {
                            // Overlap with an existing mapping must be refused
                            // unless it lives in a separate lower-level table.
                            if r.is_ok() {
                                // Only possible if the existing mapping is smaller
                                // and in a different leaf table: then it's an
                                // overlap bug.
                                prop_assert!(false, "mapped over an existing mapping");
                            }
                        } else {
                            // May still fail if a smaller mapping's table sits
                            // where this huge page would go; that's allowed.
                            if r.is_ok() { model.insert(virt, (phys, size, flags)); }
                            else { prop_assert_eq!(r, Err(MapError::AlreadyMapped)); }
                        }
                    }
                    1 => {
                        let r = a.unmap(&mut m, virt, size);
                        match model.get(&virt) {
                            Some(&(p, s, _)) if s == size => {
                                prop_assert_eq!(r, Ok(p));
                                model.remove(&virt);
                            }
                            _ => prop_assert!(r.is_err()),
                        }
                    }
                    _ => {
                        let r = a.protect(&mut m, virt, size, flags);
                        match model.get_mut(&virt) {
                            Some(e) if e.1 == size => { prop_assert_eq!(r, Ok(())); e.2 = flags; }
                            _ => prop_assert!(r.is_err()),
                        }
                    }
                }
            }
            for p in probes {
                let virt = K + p;
                let expect = model.range(..=virt).next_back()
                    .filter(|(v, (_, s, _))| virt < **v + s.bytes())
                    .map(|(v, (ph, s, f))| Translation { phys: ph + (virt - v), size: *s, flags: *f });
                prop_assert_eq!(a.translate(&m, virt), expect);
            }
            // Every leaf in the tables is in the model, and nothing is W+X.
            let mut leaves = 0;
            a.for_each_leaf(&m, |v, t| {
                leaves += 1;
                assert!(!(t.flags.writable && t.flags.executable));
                assert_eq!(model.get(&v).map(|e| (e.0, e.1)), Some((t.phys, t.size)));
            });
            prop_assert_eq!(leaves, model.len());
        }
    }
}

/// Kani proofs (proof target 1 in docs/verification.md). Every virtual
/// address, physical address, page size and flag combination is covered, on
/// a table memory of up to 8 tables.
#[cfg(kani)]
mod proofs {
    use super::*;

    const T: usize = 8;

    struct Mem {
        tables: [[u64; 512]; T],
        used: usize,
        limit: usize,
    }

    impl TableMemory for Mem {
        fn read(&self, t: u64, i: usize) -> u64 {
            let n = (t / 4096) as usize;
            if n == 0 || n > self.used || t % 4096 != 0 {
                // Reading a table that was never allocated is a bug.
                panic!("read from a non-table");
            }
            self.tables[n - 1][i]
        }
        fn write(&mut self, t: u64, i: usize, v: u64) {
            let n = (t / 4096) as usize;
            if n == 0 || n > self.used || t % 4096 != 0 {
                panic!("write to a non-table");
            }
            self.tables[n - 1][i] = v;
        }
        fn alloc_table(&mut self) -> Option<u64> {
            if self.used >= self.limit {
                return None;
            }
            self.used += 1;
            Some(self.used as u64 * 4096)
        }
    }

    fn mem() -> Mem {
        let limit: usize = kani::any();
        kani::assume(limit >= 1 && limit <= T);
        Mem {
            tables: [[0; 512]; T],
            used: 0,
            limit,
        }
    }

    fn any_size() -> PageSize {
        match kani::any::<u8>() % 3 {
            0 => PageSize::Size4K,
            1 => PageSize::Size2M,
            _ => PageSize::Size1G,
        }
    }

    fn any_flags() -> Flags {
        Flags {
            writable: kani::any(),
            executable: kani::any(),
            user: kani::any(),
            global: kani::any(),
            cache: if kani::any() {
                Cache::WriteBack
            } else {
                Cache::Uncached
            },
        }
    }

    /// A successful map translates exactly as asked, at every offset in the
    /// page, and never produces a writable+executable mapping.
    #[kani::proof]
    #[kani::unwind(6)]
    fn map_then_translate() {
        let mut m = mem();
        let a = AddressSpace::new(&mut m).unwrap();
        let (virt, phys, size, flags): (u64, u64, PageSize, Flags) =
            (kani::any(), kani::any(), any_size(), any_flags());
        if a.map(&mut m, virt, phys, size, flags).is_ok() {
            assert!(!(flags.writable && flags.executable));
            let off: u64 = kani::any();
            kani::assume(off < size.bytes());
            let t = a.translate(&m, virt + off).unwrap();
            assert!(t.phys == phys + off && t.size == size && t.flags == flags);
        }
    }

    /// Mapping one page never changes the translation of any address outside
    /// it, whether the map succeeds or fails (including running out of memory).
    #[kani::proof]
    #[kani::unwind(6)]
    fn map_changes_nothing_else() {
        let mut m = mem();
        let a = AddressSpace::new(&mut m).unwrap();
        // An existing mapping first.
        let (v1, p1, s1, f1) = (kani::any(), kani::any(), any_size(), any_flags());
        let _ = a.map(&mut m, v1, p1, s1, f1);
        let probe: u64 = kani::any();
        let before = a.translate(&m, probe);
        let (v2, p2, s2, f2): (u64, u64, PageSize, Flags) =
            (kani::any(), kani::any(), any_size(), any_flags());
        let r = a.map(&mut m, v2, p2, s2, f2);
        let inside = probe >= v2 && probe - v2 < s2.bytes();
        if r.is_err() || !inside {
            assert!(a.translate(&m, probe) == before);
        }
    }

    /// Unmap removes exactly the page asked for and nothing else.
    #[kani::proof]
    #[kani::unwind(6)]
    fn unmap_removes_only_that_page() {
        let mut m = mem();
        let a = AddressSpace::new(&mut m).unwrap();
        let (v1, p1, s1, f1): (u64, u64, PageSize, Flags) =
            (kani::any(), kani::any(), any_size(), any_flags());
        let (v2, p2, s2, f2) = (kani::any(), kani::any(), any_size(), any_flags());
        if a.map(&mut m, v1, p1, s1, f1).is_ok() {
            let _ = a.map(&mut m, v2, p2, s2, f2);
            let probe: u64 = kani::any();
            let before = a.translate(&m, probe);
            assert!(a.unmap(&mut m, v1, s1) == Ok(p1));
            let inside = probe >= v1 && probe - v1 < s1.bytes();
            if inside {
                assert!(a.translate(&m, probe).is_none());
            } else {
                assert!(a.translate(&m, probe) == before);
            }
        }
    }

    /// protect never creates a writable+executable page and keeps the frame.
    #[kani::proof]
    #[kani::unwind(6)]
    fn protect_keeps_wx_and_frame() {
        let mut m = mem();
        let a = AddressSpace::new(&mut m).unwrap();
        let (v, p, s, f): (u64, u64, PageSize, Flags) =
            (kani::any(), kani::any(), any_size(), any_flags());
        if a.map(&mut m, v, p, s, f).is_ok() {
            let nf = any_flags();
            let r = a.protect(&mut m, v, s, nf);
            let t = a.translate(&m, v).unwrap();
            assert!(t.phys == p);
            assert!(!(t.flags.writable && t.flags.executable));
            if r.is_ok() {
                assert!(t.flags == nf);
            } else {
                assert!(t.flags == f);
            }
        }
    }
}
