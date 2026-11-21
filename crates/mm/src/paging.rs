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

pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NO_EXECUTE: u64 = 1 << 63;
/// PAT index bit 2 in a 4 KiB page entry (the same bit is HUGE higher up).
pub const PAT_4K: u64 = 1 << 7;
/// PAT index bit 2 in a 2 MiB or 1 GiB page entry.
pub const PAT_HUGE: u64 = 1 << 12;
/// The PAT MSR (0x277) value these entries assume, the same layout Limine
/// sets up: PA0 WB, PA1 WT, PA2 UC-, PA3 UC, PA4 WP, PA5 WC, PA6 UC-, PA7 UC.
/// An entry picks PA(PAT*4 + PCD*2 + PWT): write-back is index 0, uncached
/// index 3 (PCD|PWT), write-combining index 5 (PAT|PWT).
pub const PAT_VALUE: u64 = 0x0007_0105_0007_0406;
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
    /// Framebuffers: writes are buffered and combined, reads are uncached.
    WriteCombining,
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

    /// Entry bits for a leaf at `level` (1 = 4 KiB). Without `nx` the CPU
    /// has no no-execute bit (it would be a reserved-bit fault), so it is
    /// left out.
    fn bits(self, level: usize, nx: bool) -> u64 {
        let mut b = PRESENT;
        if self.writable {
            b |= WRITABLE;
        }
        if !self.executable && nx {
            b |= NO_EXECUTE;
        }
        if self.user {
            b |= USER;
        }
        if self.global {
            b |= GLOBAL;
        }
        let pat = if level == 1 { PAT_4K } else { PAT_HUGE };
        b |= match self.cache {
            Cache::WriteBack => 0,
            Cache::Uncached => CACHE_DISABLE | WRITE_THROUGH,
            Cache::WriteCombining => pat | WRITE_THROUGH,
        };
        b
    }

    /// Flags of a leaf entry at `level`. Without `nx` every page is
    /// executable, and is reported so. Cache types this module never
    /// writes (WT, UC-, WP) read back as Uncached, the safe assumption.
    fn from_bits(e: u64, level: usize, nx: bool) -> Flags {
        let pat = if level == 1 { PAT_4K } else { PAT_HUGE };
        let index = (e & pat != 0) as u8 * 4
            + (e & CACHE_DISABLE != 0) as u8 * 2
            + (e & WRITE_THROUGH != 0) as u8;
        Flags {
            writable: e & WRITABLE != 0,
            executable: !nx || e & NO_EXECUTE == 0,
            user: e & USER != 0,
            global: e & GLOBAL != 0,
            cache: match index {
                0 => Cache::WriteBack,
                5 => Cache::WriteCombining,
                _ => Cache::Uncached,
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
    /// Whether the CPU has the no-execute bit (EFER.NXE on). Without it
    /// non-executable mappings can't be expressed, and none are written.
    pub nx: bool,
}

impl AddressSpace {
    /// A new, empty address space on a CPU with the no-execute bit.
    pub fn new<M: TableMemory>(mem: &mut M) -> Option<AddressSpace> {
        mem.alloc_table()
            .map(|root| AddressSpace { root, nx: true })
    }

    /// The same, for a CPU without the no-execute bit.
    pub fn new_without_nx<M: TableMemory>(mem: &mut M) -> Option<AddressSpace> {
        mem.alloc_table()
            .map(|root| AddressSpace { root, nx: false })
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
        let mut e = phys | flags.bits(leaf, self.nx);
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
            flags: Flags::from_bits(e, level, self.nx),
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
        let mut new = (e & leaf_addr_mask(level)) | flags.bits(level, self.nx);
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
            nx: bool,
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
                            flags: Flags::from_bits(e, level, nx),
                        },
                    );
                } else {
                    walk(mem, e & ADDR_MASK, level - 1, virt, nx, f);
                }
            }
        }
        walk(mem, self.root, 4, 0, self.nx, &mut f);
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
            // 1024 pages expected; far more means the split is broken (and
            // stops a broken split from eating all memory).
            assert!(v.len() < 4096, "far too many pages");
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

    /// Map one page into empty tables and return every raw entry on the
    /// walk, root first. Tables are allocated in order: root 0x1000, then
    /// 0x2000, 0x3000, 0x4000.
    fn raw_walk(virt: u64, phys: u64, size: PageSize, flags: Flags) -> Vec<u64> {
        let mut m = Mem::new(8);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(&mut m, virt, phys, size, flags).unwrap();
        let mut out = Vec::new();
        let mut table = a.root;
        for level in (size.level()..=4).rev() {
            let e = m.read(table, index(virt, level));
            out.push(e);
            table = e & ADDR_MASK;
        }
        out
    }

    #[test]
    fn exact_entry_bits() {
        // Worked by hand from the Intel SDM vol. 3 table 4-19/4-20 layouts:
        // P=bit0 W=bit1 U=bit2 PWT=bit3 PCD=bit4 PS=bit7 G=bit8 XD=bit63.
        // Kernel intermediate entries: next table | P | W = 0x..003.
        const NX: u64 = 1 << 63;
        let kernel_path = [0x2003, 0x3003, 0x4003];
        let cases = [
            (Flags::READ, NX | 0x5001),
            (Flags::READ_WRITE, NX | 0x5003),
            (Flags::READ_EXECUTE, 0x5001),
            (
                Flags {
                    global: true,
                    ..Flags::READ
                },
                NX | 0x5101,
            ),
            (
                Flags {
                    cache: Cache::Uncached,
                    ..Flags::READ_WRITE
                },
                NX | 0x501b,
            ),
            (
                Flags {
                    global: true,
                    ..Flags::READ_EXECUTE
                },
                0x5101,
            ),
        ];
        for (flags, leaf) in cases {
            let mut want = kernel_path.to_vec();
            want.push(leaf);
            assert_eq!(
                raw_walk(K, 0x5000, PageSize::Size4K, flags),
                want,
                "{flags:?}"
            );
        }
        // User page at 0x1000: every level carries U (bit 2) so the leaf
        // decides; the leaf is P | W | U | XD.
        let user_rw = Flags {
            user: true,
            ..Flags::READ_WRITE
        };
        assert_eq!(
            raw_walk(0x1000, 0x5000, PageSize::Size4K, user_rw),
            [0x2007, 0x3007, 0x4007, NX | 0x5007]
        );
        // 2 MiB page: leaf in the PD with PS (bit 7): 0x40_0000 | 0x80 | P.
        assert_eq!(
            raw_walk(K, 0x40_0000, PageSize::Size2M, Flags::READ_EXECUTE),
            [0x2003, 0x3003, 0x40_0081]
        );
        // 1 GiB page: leaf in the PDPT: P | W | PS | G | XD.
        let g_rw = Flags {
            global: true,
            ..Flags::READ_WRITE
        };
        assert_eq!(
            raw_walk(K, 0x4000_0000, PageSize::Size1G, g_rw),
            [0x2003, NX | 0x4000_0183]
        );
    }

    #[test]
    fn write_combining_uses_pat_index_5_at_every_level() {
        // PA5 = PAT bit + PWT. In a 4 KiB entry the PAT bit is bit 7; in a
        // 2 MiB or 1 GiB entry bit 7 is PS (HUGE) and PAT moves to bit 12.
        const NX: u64 = 1 << 63;
        let wc = Flags {
            cache: Cache::WriteCombining,
            ..Flags::READ_WRITE
        };
        // 4 KiB: 0x5000 | PAT(0x80) | PWT(0x8) | W | P.
        assert_eq!(raw_walk(K, 0x5000, PageSize::Size4K, wc)[3], NX | 0x508b);
        // 2 MiB: 0x40_0000 | PAT(0x1000) | PS(0x80) | PWT | W | P.
        assert_eq!(
            raw_walk(K, 0x40_0000, PageSize::Size2M, wc)[2],
            NX | 0x40_108b
        );
        // 1 GiB, same layout as 2 MiB.
        assert_eq!(
            raw_walk(K, 0x4000_0000, PageSize::Size1G, wc)[1],
            NX | 0x4000_108b
        );
        // Each reads back as write-combining, at the right size and frame.
        let mut m = Mem::new(16);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(&mut m, K, 0x5000, PageSize::Size4K, wc).unwrap();
        a.map(&mut m, K + (2 << 20), 0x40_0000, PageSize::Size2M, wc)
            .unwrap();
        a.map(&mut m, K + (1 << 30), 0x4000_0000, PageSize::Size1G, wc)
            .unwrap();
        for (v, p, s) in [
            (K, 0x5000, PageSize::Size4K),
            (K + (2 << 20), 0x40_0000, PageSize::Size2M),
            (K + (1 << 30), 0x4000_0000, PageSize::Size1G),
        ] {
            let t = a.translate(&m, v).unwrap();
            assert_eq!((t.phys, t.size, t.flags), (p, s, wc));
        }
        // An uncached huge page is index 3 (PCD | PWT), not mistaken for WC.
        let uc = Flags {
            cache: Cache::Uncached,
            ..Flags::READ
        };
        assert_eq!(
            raw_walk(K, 0x40_0000, PageSize::Size2M, uc)[2],
            NX | 0x40_0099
        );
        // Cache types never written read back as Uncached: WT (PWT only,
        // index 1) in a 4 KiB entry.
        assert_eq!(Flags::from_bits(0x5009, 1, true).cache, Cache::Uncached);
        assert_eq!(Flags::from_bits(0x5001, 1, true).cache, Cache::WriteBack);
        // PAT bit alone (index 4, WP) at 2 MiB.
        assert_eq!(Flags::from_bits(0x40_1081, 2, true).cache, Cache::Uncached);
    }

    #[test]
    fn without_nx_no_execute_bit_is_ever_written() {
        let mut m = Mem::new(16);
        let a = AddressSpace::new_without_nx(&mut m).unwrap();
        assert!(!a.nx);
        a.map(&mut m, K, 0x5000, PageSize::Size4K, Flags::READ_WRITE)
            .unwrap();
        a.map(
            &mut m,
            K + (2 << 20),
            0x40_0000,
            PageSize::Size2M,
            Flags::READ,
        )
        .unwrap();
        // Leaves: plain P|W and P|PS, no bit 63.
        assert_eq!(m.read(0x4000, 0), 0x5003);
        assert_eq!(m.read(0x3000, 1), 0x40_0081);
        // The hardware will execute them, and translate says so.
        assert!(a.translate(&m, K).unwrap().flags.executable);
        // W^X is still refused as a request.
        let wx = Flags {
            executable: true,
            ..Flags::READ_WRITE
        };
        assert_eq!(
            a.map(&mut m, K + 0x1000, 0, PageSize::Size4K, wx),
            Err(MapError::WriteAndExecute)
        );
        a.protect(&mut m, K, PageSize::Size4K, Flags::READ).unwrap();
        assert_eq!(m.read(0x4000, 0), 0x5001);
        let mut leaves = 0;
        a.for_each_leaf(&m, |_, t| {
            assert!(t.flags.executable);
            leaves += 1;
        });
        assert_eq!(leaves, 2);
    }

    #[test]
    fn flag_constants_are_what_they_say() {
        let plain = |writable, executable| Flags {
            writable,
            executable,
            user: false,
            global: false,
            cache: Cache::WriteBack,
        };
        assert_eq!(Flags::READ, plain(false, false));
        assert_eq!(Flags::READ_WRITE, plain(true, false));
        assert_eq!(Flags::READ_EXECUTE, plain(false, true));
    }

    #[test]
    fn canonical_addresses() {
        for v in [
            0,
            0x1000,
            0x0000_7fff_ffff_ffff,
            0xffff_8000_0000_0000,
            0xffff_ffff_ffff_f000,
            u64::MAX,
        ] {
            assert!(is_canonical(v), "{v:#x}");
        }
        for v in [
            0x0000_8000_0000_0000,
            0xffff_0000_0000_0000,
            0xffff_7fff_ffff_ffff,
            0x8000_0000_0000_0000,
            0x7fff_ffff_ffff_ffff,
            0x0001_0000_0000_0000,
        ] {
            assert!(!is_canonical(v), "{v:#x}");
        }
        let mut m = Mem::new(16);
        let a = AddressSpace::new(&mut m).unwrap();
        // Upper half by bit 63 but not canonical: only the canonical check
        // stops it.
        assert_eq!(
            a.map(
                &mut m,
                0xffff_0000_0000_0000,
                0,
                PageSize::Size4K,
                Flags::READ
            ),
            Err(MapError::BadVirtual)
        );
        // 0x0000_8000_0000_0000 has the same PML4 index (256) as K, so
        // without the canonical check translate would find K's page.
        a.map(&mut m, K, 0x7000, PageSize::Size4K, Flags::READ)
            .unwrap();
        assert_eq!(a.translate(&m, 0x0000_8000_0000_0000), None);
        assert_eq!(a.translate(&m, 0x7fff_8000_0000_0000), None);
        assert_eq!(a.translate(&m, K).unwrap().phys, 0x7000);
        assert_eq!(
            a.unmap(&mut m, 0x0000_8000_0000_0000, PageSize::Size4K),
            Err(MapError::NotMapped)
        );
        assert_eq!(
            a.protect(
                &mut m,
                0xffff_0000_0000_0000,
                PageSize::Size4K,
                Flags::READ_WRITE
            ),
            Err(MapError::NotMapped)
        );
        assert_eq!(a.translate(&m, K).unwrap().flags, Flags::READ);
    }

    #[test]
    fn leaves_are_reported_with_the_right_half() {
        let mut m = Mem::new(32);
        let a = AddressSpace::new(&mut m).unwrap();
        let user = Flags {
            user: true,
            ..Flags::READ
        };
        // PT index 256 in the user half and in the kernel half.
        a.map(&mut m, 0x10_0000, 0x1000, PageSize::Size4K, user)
            .unwrap();
        a.map(&mut m, 0x1000, 0x2000, PageSize::Size4K, user)
            .unwrap();
        a.map(&mut m, K + 0x10_0000, 0x3000, PageSize::Size4K, Flags::READ)
            .unwrap();
        a.map(
            &mut m,
            K + (1 << 30),
            0x20_0000,
            PageSize::Size2M,
            Flags::READ,
        )
        .unwrap();
        a.map(
            &mut m,
            0xffff_ffff_c000_0000,
            0x4000_0000,
            PageSize::Size1G,
            Flags::READ,
        )
        .unwrap();
        let mut got = Vec::new();
        a.for_each_leaf(&m, |v, t| got.push((v, t.phys, t.size, t.flags.user)));
        assert_eq!(
            got,
            [
                (0x1000, 0x2000, PageSize::Size4K, true),
                (0x10_0000, 0x1000, PageSize::Size4K, true),
                (K + 0x10_0000, 0x3000, PageSize::Size4K, false),
                (K + (1 << 30), 0x20_0000, PageSize::Size2M, false),
                (0xffff_ffff_c000_0000, 0x4000_0000, PageSize::Size1G, false),
            ]
        );
        // Each one translates back the same way.
        for (v, p, s, _) in got {
            let t = a.translate(&m, v).unwrap();
            assert_eq!((t.phys, t.size), (p, s));
        }
    }

    #[test]
    fn protect_keeps_page_size_and_frame() {
        let mut m = Mem::new(16);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(&mut m, K, 0x60_0000, PageSize::Size2M, Flags::READ)
            .unwrap();
        a.protect(&mut m, K, PageSize::Size2M, Flags::READ_WRITE)
            .unwrap();
        // P | W | PS | XD, same frame.
        assert_eq!(m.read(0x3000, 0), (1 << 63) | 0x60_0083);
        let t = a.translate(&m, K + 0x1234).unwrap();
        assert_eq!(
            (t.phys, t.size, t.flags),
            (0x60_1234, PageSize::Size2M, Flags::READ_WRITE)
        );
        a.map(&mut m, K + (1 << 30), 0x8000, PageSize::Size4K, Flags::READ)
            .unwrap();
        a.protect(&mut m, K + (1 << 30), PageSize::Size4K, Flags::READ_EXECUTE)
            .unwrap();
        let t = a.translate(&m, K + (1 << 30)).unwrap();
        assert_eq!(
            (t.phys, t.size, t.flags),
            (0x8000, PageSize::Size4K, Flags::READ_EXECUTE)
        );
        // Refusals: wrong half, wrong size, misaligned, not mapped.
        let user = Flags {
            user: true,
            ..Flags::READ
        };
        assert_eq!(
            a.protect(&mut m, K, PageSize::Size2M, user),
            Err(MapError::BadVirtual)
        );
        assert_eq!(
            a.protect(&mut m, 0x1000, PageSize::Size4K, Flags::READ),
            Err(MapError::BadVirtual)
        );
        assert_eq!(
            a.protect(&mut m, K, PageSize::Size4K, Flags::READ),
            Err(MapError::NotMapped)
        );
        assert_eq!(
            a.protect(&mut m, K + 0x1000, PageSize::Size2M, Flags::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            a.protect(&mut m, K + (2 << 30), PageSize::Size4K, Flags::READ),
            Err(MapError::NotMapped)
        );
        assert_eq!(
            a.unmap(&mut m, K + 0x1000, PageSize::Size2M),
            Err(MapError::Misaligned)
        );
        // Nothing above changed the two mappings.
        assert_eq!(a.translate(&m, K).unwrap().flags, Flags::READ_WRITE);
    }

    #[test]
    fn neighbouring_pages_share_tables() {
        // Two 4K pages in the same page table: the second map must reuse the
        // tables and succeed, using no new memory.
        let mut m = Mem::new(4);
        let a = AddressSpace::new(&mut m).unwrap();
        a.map(&mut m, K, 0x1000, PageSize::Size4K, Flags::READ)
            .unwrap();
        a.map(&mut m, K + 0x1000, 0x2000, PageSize::Size4K, Flags::READ)
            .unwrap();
        assert_eq!(m.tables.len(), 4);
        assert_eq!(a.translate(&m, K + 0x1000).unwrap().phys, 0x2000);
    }

    fn size_of(i: u8) -> PageSize {
        [PageSize::Size4K, PageSize::Size2M, PageSize::Size1G][i as usize % 3]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::test_cases(3000)))]
        /// Random map/unmap/protect sequences in both halves against an exact
        /// reference model: every call's result is predicted, not just
        /// allowed, and every probe translates exactly as the model says.
        #[test]
        fn random_ops_match_model(
            ops in prop::collection::vec(
                (0u8..3, 0u8..3, 0u64..64, 0u8..3, 0u64..1024, 0u8..6),
                1..160,
            ),
            probes in prop::collection::vec((any::<bool>(), 0u64..(64u64 << 30)), 80),
            nx in prop::bool::weighted(0.8),
        ) {
            let mut m = Mem::new(1 << 13);
            let a = if nx { AddressSpace::new(&mut m) } else { AddressSpace::new_without_nx(&mut m) }.unwrap();
            // What the hardware will do: without NX everything executes.
            let seen = |f: Flags| Flags { executable: f.executable || !nx, ..f };
            // Model: virt page start -> (phys, size, flags).
            let mut model: BTreeMap<u64, (u64, PageSize, Flags)> = BTreeMap::new();
            // Regions that have a page table (2 MiB) or page directory (1 GiB)
            // under them. Tables are never freed, so a huge page can't go there.
            let mut pt_regions = std::collections::BTreeSet::new();
            let mut pd_regions = std::collections::BTreeSet::new();
            let wx = Flags { writable: true, executable: true, ..Flags::READ };
            let flag_set = [Flags::READ, Flags::READ_WRITE, Flags::READ_EXECUTE,
                Flags { cache: Cache::Uncached, global: true, ..Flags::READ_WRITE }, wx,
                Flags { cache: Cache::WriteCombining, ..Flags::READ_WRITE }];
            for (kind, half, slot, sz, phys_slot, fl) in ops {
                let size = size_of(sz);
                // half 0: user page, user flags; 1: kernel page, kernel flags;
                // 2: kernel page with user flags (must be refused).
                let virt = if half == 0 { 0 } else { K } + slot * size.bytes();
                let phys = phys_slot * size.bytes() % MAX_PHYS;
                let flags = Flags { user: half != 1, ..flag_set[fl as usize] };
                let overlaps = model.iter().any(|(&v, &(_, s, _))| v < virt + size.bytes() && virt < v + s.bytes());
                let r2m = virt & !((2 << 20) - 1);
                let r1g = virt & !((1 << 30) - 1);
                let rights = if flags.writable && flags.executable {
                    Err(MapError::WriteAndExecute)
                } else if half == 2 {
                    Err(MapError::BadVirtual)
                } else {
                    Ok(())
                };
                match kind {
                    0 => {
                        let expect = rights.and_then(|()| {
                            let blocked = overlaps
                                || (size == PageSize::Size2M && pt_regions.contains(&r2m))
                                || (size == PageSize::Size1G && pd_regions.contains(&r1g));
                            if blocked { Err(MapError::AlreadyMapped) } else { Ok(()) }
                        });
                        prop_assert_eq!(a.map(&mut m, virt, phys, size, flags), expect);
                        if expect.is_ok() {
                            model.insert(virt, (phys, size, flags));
                            if size == PageSize::Size4K { pt_regions.insert(r2m); }
                            if size != PageSize::Size1G { pd_regions.insert(r1g); }
                        }
                    }
                    1 => {
                        let r = a.unmap(&mut m, virt, size);
                        match model.get(&virt) {
                            Some(&(p, s, _)) if s == size => {
                                prop_assert_eq!(r, Ok(p));
                                model.remove(&virt);
                            }
                            _ => prop_assert_eq!(r, Err(MapError::NotMapped)),
                        }
                    }
                    _ => {
                        let r = a.protect(&mut m, virt, size, flags);
                        match (rights, model.get_mut(&virt)) {
                            (Err(e), _) => prop_assert_eq!(r, Err(e)),
                            (Ok(()), Some(e)) if e.1 == size => { prop_assert_eq!(r, Ok(())); e.2 = flags; }
                            _ => prop_assert_eq!(r, Err(MapError::NotMapped)),
                        }
                    }
                }
            }
            for (user, p) in probes {
                let virt = if user { p } else { K + p };
                let expect = model.range(..=virt).next_back()
                    .filter(|(v, (_, s, _))| virt < **v + s.bytes())
                    .map(|(v, (ph, s, f))| Translation { phys: ph + (virt - v), size: *s, flags: seen(*f) });
                prop_assert_eq!(a.translate(&m, virt), expect);
            }
            // Every leaf in the tables is in the model with the same frame,
            // size and flags, and nothing is W+X.
            let mut leaves = 0;
            a.for_each_leaf(&m, |v, t| {
                leaves += 1;
                assert!(!nx || !(t.flags.writable && t.flags.executable));
                assert_eq!(model.get(&v).map(|e| (e.0, e.1, seen(e.2))), Some((t.phys, t.size, t.flags)));
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
    /// Most entry writes one proof makes: a map writes at most 4 (three new
    /// table links and the leaf), so two maps and an unmap or protect write
    /// at most 9. A proof that wrote more would fail loudly, never silently.
    const WRITES: usize = 12;

    /// Table memory as a log of entry writes over all-zero tables, which is
    /// exactly what fresh tables are. Reading scans the log, newest first.
    /// This models the same memory as `[[u64; 512]; T]` but keeps the
    /// solver's problem small: a dense array of 8 x 512 symbolic words
    /// exhausts the solver's memory before any proof completes.
    struct Mem {
        log: [(u64, usize, u64); WRITES],
        writes: usize,
        used: usize,
        limit: usize,
    }

    impl Mem {
        fn check_table(&self, t: u64, i: usize) {
            let n = (t / 4096) as usize;
            // Touching a table that was never allocated is a bug.
            assert!(n != 0 && n <= self.used && t % 4096 == 0, "not a table");
            assert!(i < 512, "index out of range");
        }
    }

    impl TableMemory for Mem {
        fn read(&self, t: u64, i: usize) -> u64 {
            self.check_table(t, i);
            let mut k = self.writes;
            while k > 0 {
                k -= 1;
                let (wt, wi, v) = self.log[k];
                if wt == t && wi == i {
                    return v;
                }
            }
            0
        }
        fn write(&mut self, t: u64, i: usize, v: u64) {
            self.check_table(t, i);
            assert!(self.writes < WRITES, "write log full: raise WRITES");
            self.log[self.writes] = (t, i, v);
            self.writes += 1;
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
            log: [(0, 0, 0); WRITES],
            writes: 0,
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
            cache: match kani::any::<u8>() % 3 {
                0 => Cache::WriteBack,
                1 => Cache::Uncached,
                _ => Cache::WriteCombining,
            },
        }
    }

    /// A successful map translates exactly as asked, at every offset in the
    /// page, and never produces a writable+executable mapping.
    #[kani::proof]
    #[kani::unwind(14)]
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
    /// it, whether the map succeeds or fails (including running out of memory),
    /// and never replaces a translation inside it.
    #[kani::proof]
    #[kani::unwind(14)]
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
        // And a map never replaces an existing translation: over an address
        // that was already mapped it is refused.
        if inside && before.is_some() {
            assert!(r.is_err());
        }
    }

    /// Unmap removes exactly the page asked for and nothing else.
    #[kani::proof]
    #[kani::unwind(14)]
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
    #[kani::unwind(14)]
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
