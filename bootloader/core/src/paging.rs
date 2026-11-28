//! x86-64 page table construction (4-level and 5-level).
//!
//! The builder only needs a source of zeroed 4 KiB frames and a way to reach
//! a frame's contents; in the loader that is identity-mapped firmware memory,
//! in tests it is a simulated physical memory.

pub const PAGE: u64 = 4096;
pub const SIZE_2M: u64 = 2 << 20;
pub const SIZE_1G: u64 = 1 << 30;

pub const P: u64 = 1 << 0;
pub const RW: u64 = 1 << 1;
pub const US: u64 = 1 << 2;
pub const PWT: u64 = 1 << 3;
pub const PCD: u64 = 1 << 4;
pub const PS: u64 = 1 << 7;
pub const PAT_4K: u64 = 1 << 7;
pub const PAT_LARGE: u64 = 1 << 12;
pub const NX: u64 = 1 << 63;
pub const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

/// The PAT layout the Limine protocol promises:
/// PAT0 WB, PAT1 WT, PAT2 UC-, PAT3 UC, PAT4 WP, PAT5 WC, PAT6 UC-, PAT7 UC.
pub const PAT_VALUE: u64 = 0x0007_0105_0007_0406;

pub trait FrameSource {
    /// A new zeroed 4 KiB frame, or None if the arena is exhausted.
    fn alloc(&mut self) -> Option<u64>;
    /// The 512 entries of the table at physical address `phys`.
    fn table(&mut self, phys: u64) -> &mut [u64; 512];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub write: bool,
    pub exec: bool,
    pub write_combining: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    OutOfFrames,
    AlreadyMapped(u64),
    Unaligned,
    NonCanonical,
}

pub struct Mapper {
    pub root: u64,
    pub levels: u32,
    /// Whether the CPU supports the NX bit (EFER.NXE can be set).
    pub nx: bool,
    /// Whether the CPU supports 1 GiB pages.
    pub huge_1g: bool,
}

fn index(virt: u64, level: u32) -> usize {
    ((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize
}

pub fn canonical(virt: u64, levels: u32) -> bool {
    let bits = 12 + 9 * levels; // 48 or 57
    let top = (virt as i64) >> (bits - 1);
    top == 0 || top == -1
}

impl Mapper {
    pub fn new(
        src: &mut impl FrameSource,
        levels: u32,
        nx: bool,
        huge_1g: bool,
    ) -> Result<Self, MapError> {
        assert!(levels == 4 || levels == 5);
        let root = src.alloc().ok_or(MapError::OutOfFrames)?;
        Ok(Mapper {
            root,
            levels,
            nx,
            huge_1g,
        })
    }

    fn leaf_bits(&self, flags: Flags, large: bool) -> u64 {
        let mut e = P;
        if flags.write {
            e |= RW;
        }
        if !flags.exec && self.nx {
            e |= NX;
        }
        if flags.write_combining {
            // PAT index 5 = PAT:1 PCD:0 PWT:1.
            e |= PWT | if large { PAT_LARGE } else { PAT_4K };
        }
        e
    }

    /// Maps `[virt, virt + len)` to `[phys, phys + len)`, using 2 MiB and
    /// 1 GiB pages wherever both addresses are aligned and the whole large
    /// page is inside the range. Refuses to replace an existing mapping.
    pub fn map(
        &mut self,
        src: &mut impl FrameSource,
        mut virt: u64,
        mut phys: u64,
        len: u64,
        flags: Flags,
    ) -> Result<(), MapError> {
        if !virt.is_multiple_of(PAGE) || !phys.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) {
            return Err(MapError::Unaligned);
        }
        let end = virt.checked_add(len);
        if len > 0 {
            let last = virt.wrapping_add(len - 1);
            if !canonical(virt, self.levels) || !canonical(last, self.levels) {
                return Err(MapError::NonCanonical);
            }
            if end.is_none() && last != u64::MAX {
                return Err(MapError::NonCanonical);
            }
        }
        let mut left = len;
        while left > 0 {
            let size = if self.huge_1g
                && virt.is_multiple_of(SIZE_1G)
                && phys.is_multiple_of(SIZE_1G)
                && left >= SIZE_1G
            {
                SIZE_1G
            } else if virt.is_multiple_of(SIZE_2M)
                && phys.is_multiple_of(SIZE_2M)
                && left >= SIZE_2M
            {
                SIZE_2M
            } else {
                PAGE
            };
            self.map_one(src, virt, phys, size, flags)?;
            virt = virt.wrapping_add(size);
            phys += size;
            left -= size;
        }
        Ok(())
    }

    fn map_one(
        &mut self,
        src: &mut impl FrameSource,
        virt: u64,
        phys: u64,
        size: u64,
        flags: Flags,
    ) -> Result<(), MapError> {
        let leaf_level = match size {
            SIZE_1G => 3,
            SIZE_2M => 2,
            _ => 1,
        };
        let mut table = self.root;
        let mut level = self.levels;
        while level > leaf_level {
            let i = index(virt, level);
            let e = src.table(table)[i];
            if e & P != 0 {
                if e & PS != 0 && level <= 3 {
                    return Err(MapError::AlreadyMapped(virt));
                }
                table = e & ADDR_MASK;
            } else {
                let new = src.alloc().ok_or(MapError::OutOfFrames)?;
                src.table(table)[i] = new | P | RW;
                table = new;
            }
            level -= 1;
        }
        let i = index(virt, leaf_level);
        let slot = &mut src.table(table)[i];
        if *slot & P != 0 {
            return Err(MapError::AlreadyMapped(virt));
        }
        let large = leaf_level > 1;
        *slot = phys | self.leaf_bits(flags, large) | if large { PS } else { 0 };
        Ok(())
    }
}

/// What the handoff maps, for `table_frames_bound`.
#[derive(Clone, Copy, Debug)]
pub struct TableBudget {
    pub levels: u32,
    pub huge_1g: bool,
    /// Most HHDM ranges there can be (memmap::hhdm_ranges output; at most
    /// the number of memory map entries).
    pub hhdm_ranges: u64,
    /// The HHDM covers physical [0, phys_end) at a 1 GiB-aligned offset.
    pub phys_end: u64,
    /// The kernel: one virtually contiguous span mapped with 4 KiB pages.
    pub kernel_span: u64,
    /// Other single 4 KiB pages mapped anywhere (trampoline, handoff alias).
    pub single_pages: u64,
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

/// An upper bound on the frames `Mapper` allocates for a handoff's
/// mappings, counted level by level. A table at level L (PT = 1, PD = 2,
/// PDPT = 3, PML4 under 5-level = 4) covers one aligned slot of 2 MiB,
/// 1 GiB, 512 GiB or 256 TiB, so the tables of a level are at most the
/// slots the mappings touch at that level, summed over the mappings
/// (sharing between mappings only lowers the real count):
///
/// - HHDM, PT: a PT is needed only in a 2 MiB slot holding a 4 KiB page.
///   The HHDM offset is 1 GiB aligned, so virtual and physical addresses
///   agree mod 2 MiB, and 4 KiB pages occur only in the first and last
///   2 MiB slot of each range: at most 2 per range, and never more than
///   the window's 2 MiB slots.
/// - HHDM, PD: with 1 GiB pages, likewise at most 2 per range (the partial
///   1 GiB slots); without them, every 1 GiB slot of the window. The window
///   starts 1 GiB aligned, so that is ceil(phys_end / 1 GiB).
/// - HHDM, PDPT and PML4: the window starts 1 GiB aligned, not 512 GiB
///   aligned, so it touches ceil(phys_end / unit) + 1 slots.
/// - Kernel: a contiguous span at any page offset touches at most
///   ceil(span / unit) + 1 slots at every level.
/// - Each single page: one table at each level below the root.
/// - The root: 1.
///
/// Checked against the real Mapper by `fuzz_table_frames_bound_holds`.
pub fn table_frames_bound(b: &TableBudget) -> u64 {
    let pe = b.phys_end.max(PAGE);
    let two = 2 * b.hhdm_ranges;
    let gib = ceil_div(pe, SIZE_1G);
    let mut n = 1; // root
    n += two.min(ceil_div(pe, SIZE_2M));
    n += if b.huge_1g { two.min(gib) } else { gib };
    n += ceil_div(pe, 512 * SIZE_1G) + 1;
    if b.levels == 5 {
        n += ceil_div(pe, 1 << 48) + 1;
    }
    let ks = b.kernel_span.max(PAGE);
    n += ceil_div(ks, SIZE_2M) + 1 + ceil_div(ks, SIZE_1G) + 1 + ceil_div(ks, 512 * SIZE_1G) + 1;
    if b.levels == 5 {
        n += ceil_div(ks, 1 << 48) + 1;
    }
    n + b.single_pages * (b.levels as u64 - 1)
}

/// A resolved mapping, for checks and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    pub phys: u64,
    pub page_size: u64,
    pub write: bool,
    pub exec: bool,
    pub user: bool,
    /// PAT index (0..7) selected by the PAT/PCD/PWT bits.
    pub pat_index: u8,
}

/// Walks the tables like the CPU does. `nx` says whether NX bits are honoured.
pub fn translate(
    src: &mut impl FrameSource,
    root: u64,
    levels: u32,
    nx: bool,
    virt: u64,
) -> Option<Translation> {
    let mut table = root;
    let mut write = true;
    let mut exec = true;
    let mut user = true;
    let mut level = levels;
    loop {
        let e = src.table(table)[index(virt, level)];
        if e & P == 0 {
            return None;
        }
        write &= e & RW != 0;
        user &= e & US != 0;
        if nx && e & NX != 0 {
            exec = false;
        }
        let large = level <= 3 && level > 1 && e & PS != 0;
        if level == 1 || large {
            let size = match level {
                3 => SIZE_1G,
                2 => SIZE_2M,
                _ => PAGE,
            };
            let pat_bit = if large {
                e & PAT_LARGE != 0
            } else {
                e & PAT_4K != 0
            };
            let base = if large {
                e & ADDR_MASK & !(size - 1)
            } else {
                e & ADDR_MASK
            };
            let pat_index =
                (pat_bit as u8) << 2 | ((e & PCD != 0) as u8) << 1 | (e & PWT != 0) as u8;
            return Some(Translation {
                phys: base + (virt & (size - 1)),
                page_size: size,
                write,
                exec,
                user,
                pat_index,
            });
        }
        table = e & ADDR_MASK;
        level -= 1;
    }
}

/// Index of `virt` in the top-level table.
pub fn top_index(virt: u64, levels: u32) -> usize {
    index(virt, levels)
}

#[cfg(test)]
pub mod sim {
    //! Simulated physical memory for page table tests.
    extern crate std;
    use super::FrameSource;
    use std::boxed::Box;
    use std::collections::BTreeMap;

    pub struct SimMemory {
        pub frames: BTreeMap<u64, Box<[u64; 512]>>,
        pub next: u64,
        pub limit: usize,
    }

    impl SimMemory {
        pub fn new(limit: usize) -> Self {
            SimMemory {
                frames: BTreeMap::new(),
                next: 0x10_0000,
                limit,
            }
        }
    }

    impl FrameSource for SimMemory {
        fn alloc(&mut self) -> Option<u64> {
            if self.frames.len() >= self.limit {
                return None;
            }
            let p = self.next;
            self.next += 0x1000;
            self.frames.insert(p, Box::new([0; 512]));
            Some(p)
        }
        fn table(&mut self, phys: u64) -> &mut [u64; 512] {
            self.frames.get_mut(&phys).expect("table frame exists")
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::sim::SimMemory;
    use super::*;

    const RX: Flags = Flags {
        write: false,
        exec: true,
        write_combining: false,
    };
    const RWNX: Flags = Flags {
        write: true,
        exec: false,
        write_combining: false,
    };
    const WC: Flags = Flags {
        write: true,
        exec: true,
        write_combining: true,
    };

    #[test]
    fn maps_with_large_pages_and_flags() {
        let mut mem = SimMemory::new(100);
        let mut m = Mapper::new(&mut mem, 4, true, true).unwrap();
        let hhdm = 0xffff_8000_0000_0000u64;
        m.map(&mut mem, hhdm, 0, 3 * SIZE_1G + SIZE_2M + PAGE, RWNX)
            .unwrap();
        let t = translate(&mut mem, m.root, 4, true, hhdm + 5).unwrap();
        assert_eq!(
            (t.phys, t.page_size, t.write, t.exec, t.user),
            (5, SIZE_1G, true, false, false)
        );
        let t = translate(&mut mem, m.root, 4, true, hhdm + 3 * SIZE_1G + 77).unwrap();
        assert_eq!(t.page_size, SIZE_2M);
        let t = translate(&mut mem, m.root, 4, true, hhdm + 3 * SIZE_1G + SIZE_2M).unwrap();
        assert_eq!(t.page_size, PAGE);
        assert!(
            translate(
                &mut mem,
                m.root,
                4,
                true,
                hhdm + 3 * SIZE_1G + SIZE_2M + PAGE
            )
            .is_none()
        );
        // Kernel text, read + execute.
        m.map(&mut mem, 0xffff_ffff_8000_0000, 0x20_0000, PAGE, RX)
            .unwrap();
        let t = translate(&mut mem, m.root, 4, true, 0xffff_ffff_8000_0123).unwrap();
        assert_eq!((t.phys, t.write, t.exec), (0x20_0123, false, true));
        // Remapping is refused.
        assert_eq!(
            m.map(&mut mem, 0xffff_ffff_8000_0000, 0, PAGE, RX),
            Err(MapError::AlreadyMapped(0xffff_ffff_8000_0000))
        );
    }

    #[test]
    fn write_combining_uses_pat5() {
        let mut mem = SimMemory::new(100);
        let mut m = Mapper::new(&mut mem, 4, true, false).unwrap();
        m.map(
            &mut mem,
            0xffff_8000_c000_0000,
            0xc000_0000,
            SIZE_2M + PAGE,
            WC,
        )
        .unwrap();
        let big = translate(&mut mem, m.root, 4, true, 0xffff_8000_c000_0000).unwrap();
        assert_eq!((big.page_size, big.pat_index), (SIZE_2M, 5));
        let small = translate(&mut mem, m.root, 4, true, 0xffff_8000_c020_0000).unwrap();
        assert_eq!((small.page_size, small.pat_index), (PAGE, 5));
        // PAT5 in our PAT value is write-combining (type 1).
        assert_eq!((PAT_VALUE >> 40) & 0xff, 1);
        assert_eq!(PAT_VALUE & 0xff, 6);
    }

    #[test]
    fn five_level_and_no_nx() {
        let mut mem = SimMemory::new(100);
        let mut m = Mapper::new(&mut mem, 5, false, false).unwrap();
        let v = 0xff00_0000_0000_0000u64;
        m.map(&mut mem, v, 0x1000, PAGE, RWNX).unwrap();
        let t = translate(&mut mem, m.root, 5, false, v).unwrap();
        assert_eq!(t.phys, 0x1000);
        assert!(t.exec, "without NX support everything is executable");
        assert_eq!(top_index(v, 5), 256);
        assert_eq!(
            m.map(&mut mem, 0x0100_0000_0000_0000, 0, PAGE, RX),
            Err(MapError::NonCanonical)
        );
        let mut m4 = Mapper::new(&mut mem, 4, true, true).unwrap();
        assert_eq!(
            m4.map(&mut mem, v, 0, PAGE, RX),
            Err(MapError::NonCanonical)
        );
    }

    #[test]
    fn running_out_of_frames_is_an_error() {
        let mut mem = SimMemory::new(3);
        let mut m = Mapper::new(&mut mem, 4, true, true).unwrap();
        assert_eq!(
            m.map(&mut mem, 0xffff_8000_0000_0000, 0, PAGE, RX),
            Err(MapError::OutOfFrames)
        );
    }

    #[test]
    fn random_ranges_translate_exactly() {
        let mut rng = crate::testrng::Rng::new(3);
        for _ in 0..200 {
            let mut mem = SimMemory::new(10_000);
            let mut m = Mapper::new(&mut mem, 4, true, rng.below(2) == 0).unwrap();
            let base = 0xffff_8000_0000_0000u64;
            let mut placed: std::vec::Vec<(u64, u64)> = std::vec::Vec::new();
            let mut cursor = 0u64;
            for _ in 0..8 {
                cursor += rng.below(1 << 10) * PAGE;
                let len = (1 + rng.below(1 << 10)) * PAGE;
                m.map(&mut mem, base + cursor, cursor, len, RWNX).unwrap();
                placed.push((cursor, len));
                cursor += len;
            }
            for _ in 0..500 {
                let a = rng.below(cursor + (1 << 20));
                let inside = placed.iter().any(|&(s, l)| a >= s && a < s + l);
                let t = translate(&mut mem, m.root, 4, true, base + a);
                assert_eq!(t.is_some(), inside);
                if let Some(t) = t {
                    assert_eq!(t.phys, a);
                }
            }
        }
    }

    /// Hand-worked: 4-level, 1 GiB pages, one HHDM range [0, 160 MiB + 4 KiB)
    /// with phys_end 4 GiB, a 6 MiB kernel, 3 single pages.
    /// HHDM: PT min(2, 2048) = 2, PD min(2, 4) = 2, PDPT 1 + 1 = 2.
    /// Kernel: PT 3 + 1 = 4, PD 1 + 1 = 2, PDPT 1 + 1 = 2.
    /// Singles 3 * 3 = 9. Root 1. Total 2 + 2 + 2 + 4 + 2 + 2 + 9 + 1 = 24.
    #[test]
    fn table_frames_bound_by_hand() {
        let b = TableBudget {
            levels: 4,
            huge_1g: true,
            hhdm_ranges: 1,
            phys_end: 4 * SIZE_1G,
            kernel_span: 6 << 20,
            single_pages: 3,
        };
        assert_eq!(table_frames_bound(&b), 24);
        // Without 1 GiB pages the PD term is every GiB of the window: 4.
        assert_eq!(
            table_frames_bound(&TableBudget {
                huge_1g: false,
                ..b
            }),
            26
        );
        // 5-level adds a PML4 term for each: 2 + 2, and a level per single page.
        assert_eq!(
            table_frames_bound(&TableBudget { levels: 5, ..b }),
            24 + 4 + 3
        );
    }

    /// Seeded fuzzing of the bound: random handoff-shaped mappings (sorted
    /// disjoint HHDM ranges, often single pages spaced to defeat sharing,
    /// at a random 1 GiB-aligned offset; a kernel span at a random page in
    /// the top 2 GiB; single pages below 4 GiB) mapped by the real Mapper
    /// never use more frames than `table_frames_bound`.
    #[test]
    fn fuzz_table_frames_bound_holds() {
        crate::fuzzkit::run("pt-bound", 8, |rng| {
            let levels = if rng.below(4) == 0 { 5 } else { 4 };
            let huge = rng.below(2) == 0;
            let phys_end = SIZE_1G * (1 + rng.below(64)) - PAGE * rng.below(4);
            let mut ranges = std::vec::Vec::new();
            let mut at = 0u64;
            let want = 1 + rng.below(48);
            while (ranges.len() as u64) < want {
                let gap = match rng.below(3) {
                    0 => PAGE * rng.below(4),
                    1 => SIZE_2M * rng.below(3) + PAGE * (1 + rng.below(511)),
                    _ => SIZE_1G * rng.below(2) + PAGE * rng.below(1 << 18),
                };
                let len = match rng.below(3) {
                    0 => PAGE,
                    1 => PAGE * (1 + rng.below(1024)),
                    _ => PAGE * (1 + rng.below(1 << 16)),
                };
                let start = at + gap + if ranges.is_empty() { 0 } else { PAGE };
                if start + len > phys_end {
                    break;
                }
                ranges.push((start, start + len));
                at = start + len;
            }
            let hh = crate::kaslr::higher_half_start(levels);
            let hhdm = hh + SIZE_1G * rng.below(1 << 14);
            let mut mem = SimMemory::new(usize::MAX);
            let mut m = Mapper::new(&mut mem, levels, true, huge).unwrap();
            for &(s, e) in &ranges {
                m.map(&mut mem, hhdm + s, s, e - s, RWNX).unwrap();
            }
            let span = PAGE * (1 + rng.below(16 << 8));
            let kvirt = 0u64.wrapping_sub(2 * SIZE_1G) + PAGE * rng.below((SIZE_1G - span) / PAGE);
            m.map(&mut mem, kvirt, 0x100_0000, span, RX).unwrap();
            let singles = rng.below(4);
            for i in 0..singles {
                let v = PAGE * (1 + i * (1 + rng.below(1 << 18)) * 4);
                let _ = m.map(&mut mem, v, v, PAGE, RX);
            }
            let used = mem.frames.len() as u64;
            let bound = table_frames_bound(&TableBudget {
                levels,
                huge_1g: huge,
                hhdm_ranges: ranges.len() as u64,
                phys_end,
                kernel_span: span,
                single_pages: singles,
            });
            assert!(
                used <= bound,
                "used {used} > bound {bound}, {} ranges",
                ranges.len()
            );
        });
    }
}
