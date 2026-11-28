//! The BIOS memory map (INT 15h, EAX=E820h, ACPI 6.5 chapter 15) as regions
//! the shared memory-map builder understands.
//!
//! E820 entries arrive unsorted, overlapping, unaligned and sometimes
//! nonsensical. Each entry becomes one EFI-typed region: usable RAM is
//! rounded inwards to whole pages, everything else outwards, so a partial
//! page is never reported usable. `memmap::build` then resolves overlaps
//! with the most conservative type winning, exactly as for UEFI maps.

use crate::memmap::{
    EFI_ACPI_NVS, EFI_ACPI_RECLAIM, EFI_CONVENTIONAL, EFI_RESERVED, EfiRegion, PAGE,
};

pub const E820_USABLE: u32 = 1;
pub const E820_RESERVED: u32 = 2;
pub const E820_ACPI: u32 = 3;
pub const E820_NVS: u32 = 4;
pub const E820_BAD: u32 = 5;

/// Extended attribute bit 0: clear means "ignore this entry" (ACPI 3.0+).
pub const ATTR_ENABLED: u32 = 1;
/// Extended attribute bit 1: non-volatile (persistent) memory, never RAM.
pub const ATTR_NONVOLATILE: u32 = 2;

/// Highest physical address we believe (52-bit physical addressing).
pub const PHYS_LIMIT: u64 = 1 << 52;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct E820Entry {
    pub base: u64,
    pub len: u64,
    pub typ: u32,
    /// Extended attributes; firmware that returns 20-byte entries has none,
    /// which the caller passes as ATTR_ENABLED.
    pub attr: u32,
}

/// What one entry becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Converted {
    /// A region for `memmap::build`.
    Region(EfiRegion),
    /// Memory the firmware says is broken: `[start, end)`, page aligned,
    /// for a bad-memory override (it outranks every firmware type).
    Bad { start: u64, end: u64 },
}

fn page_down(x: u64) -> u64 {
    x & !(PAGE - 1)
}
fn page_up(x: u64) -> u64 {
    x.saturating_add(PAGE - 1) & !(PAGE - 1)
}

pub fn convert(e: &E820Entry) -> Option<Converted> {
    if e.attr & ATTR_ENABLED == 0 || e.len == 0 || e.base >= PHYS_LIMIT {
        return None;
    }
    // An entry running past the address space is cut at the limit, not
    // wrapped round to low memory.
    let end = e.base.saturating_add(e.len).min(PHYS_LIMIT);
    let typ = if e.attr & ATTR_NONVOLATILE != 0 {
        EFI_RESERVED
    } else {
        match e.typ {
            E820_USABLE => EFI_CONVENTIONAL,
            E820_ACPI => EFI_ACPI_RECLAIM,
            E820_NVS => EFI_ACPI_NVS,
            E820_BAD => {
                return Some(Converted::Bad {
                    start: page_down(e.base),
                    end: page_up(end),
                });
            }
            // Reserved, persistent memory (7), and every type we do not know.
            _ => EFI_RESERVED,
        }
    };
    let (start, stop) = if typ == EFI_CONVENTIONAL {
        (page_up(e.base), page_down(end))
    } else {
        (page_down(e.base), page_up(end))
    };
    if stop <= start {
        return None;
    }
    Some(Converted::Region(EfiRegion {
        typ,
        start,
        pages: (stop - start) / PAGE,
    }))
}

/// Parses one entry as INT 15h E820 returned it (20 or 24 bytes).
pub fn parse(raw: &[u8]) -> Option<E820Entry> {
    if raw.len() < 20 {
        return None;
    }
    let u64_at = |o: usize| u64::from_le_bytes(raw[o..o + 8].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    Some(E820Entry {
        base: u64_at(0),
        len: u64_at(8),
        typ: u32_at(16),
        attr: if raw.len() >= 24 {
            u32_at(20)
        } else {
            ATTR_ENABLED
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memmap::{self, Event, Override};
    use crate::protocol::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn e(base: u64, len: u64, typ: u32) -> E820Entry {
        E820Entry {
            base,
            len,
            typ,
            attr: ATTR_ENABLED,
        }
    }

    /// The Limine map these entries produce (revision 3).
    fn limine(entries: &[E820Entry]) -> Vec<(u64, u64, u64)> {
        let mut regions = Vec::new();
        let mut bad = Vec::new();
        for x in entries {
            match convert(x) {
                Some(Converted::Region(r)) => regions.push(r),
                Some(Converted::Bad { start, end }) => bad.push(Override {
                    start,
                    end,
                    typ: MEMMAP_BAD_MEMORY,
                }),
                None => {}
            }
        }
        let mut scratch = vec![Event::default(); memmap::scratch_needed(regions.len(), bad.len())];
        let mut out = vec![
            MemmapEntry {
                base: 0,
                length: 0,
                typ: 0
            };
            2 * (regions.len() + bad.len()) + 1
        ];
        let n = memmap::build(regions.into_iter(), &bad, 3, &mut scratch, &mut out).unwrap();
        out[..n].iter().map(|m| (m.base, m.length, m.typ)).collect()
    }

    #[test]
    fn typical_qemu_map() {
        // SeaBIOS on QEMU with 512 MiB, as it reports it.
        let m = limine(&[
            e(0, 0x9fc00, 1),
            e(0x9fc00, 0x400, 2),
            e(0xf0000, 0x10000, 2),
            e(0x100000, 0x1fee0000, 1),
            e(0x1ffe0000, 0x20000, 2),
            e(0xfffc0000, 0x40000, 2),
        ]);
        assert_eq!(
            m,
            vec![
                // 0x9fc00 rounds down to 0x9f000: the partial page is not usable.
                (0, 0x9f000, MEMMAP_USABLE),
                (0x9f000, 0x1000, MEMMAP_RESERVED),
                (0xf0000, 0x10000, MEMMAP_RESERVED),
                (0x100000, 0x1fee0000, MEMMAP_USABLE),
                (0x1ffe0000, 0x20000, MEMMAP_RESERVED),
                (0xfffc0000, 0x40000, MEMMAP_RESERVED),
            ]
        );
    }

    #[test]
    fn overlaps_go_to_the_conservative_type() {
        // Usable RAM with a reserved hole punched in the middle by a
        // second, overlapping entry; an ACPI table inside usable RAM.
        let m = limine(&[
            e(0x100000, 0x1000000, 1),
            e(0x200800, 0x100, 2),
            e(0x300000, 0x3000, 3),
            e(0x400000, 0x1000, 4),
        ]);
        assert_eq!(
            m,
            vec![
                (0x100000, 0x100000, MEMMAP_USABLE),
                (0x200000, 0x1000, MEMMAP_RESERVED),
                (0x201000, 0xff000, MEMMAP_USABLE),
                (0x300000, 0x3000, MEMMAP_ACPI_RECLAIMABLE),
                (0x303000, 0xfd000, MEMMAP_USABLE),
                (0x400000, 0x1000, MEMMAP_ACPI_NVS),
                (0x401000, 0xcff000, MEMMAP_USABLE),
            ]
        );
    }

    #[test]
    fn bad_memory_outranks_usable() {
        let m = limine(&[e(0x100000, 0x100000, 1), e(0x150000, 0x10, 5)]);
        assert_eq!(
            m,
            vec![
                (0x100000, 0x50000, MEMMAP_USABLE),
                (0x150000, 0x1000, MEMMAP_BAD_MEMORY),
                (0x151000, 0xaf000, MEMMAP_USABLE),
            ]
        );
    }

    #[test]
    fn odd_entries() {
        let disabled = E820Entry {
            attr: 0,
            ..e(0x100000, 0x100000, 1)
        };
        assert_eq!(convert(&disabled), None);
        let pmem_attr = E820Entry {
            attr: ATTR_ENABLED | ATTR_NONVOLATILE,
            ..e(0x100000, 0x100000, 1)
        };
        assert!(matches!(convert(&pmem_attr), Some(Converted::Region(r)) if r.typ == EFI_RESERVED));
        assert_eq!(convert(&e(0x1000, 0, 1)), None);
        // Usable smaller than a page, unaligned: nothing usable at all.
        assert_eq!(convert(&e(0x1001, 0xfff, 1)), None);
        // Reserved smaller than a page: one whole page.
        assert_eq!(
            convert(&e(0x1001, 1, 2)),
            Some(Converted::Region(EfiRegion {
                typ: EFI_RESERVED,
                start: 0x1000,
                pages: 1
            }))
        );
        // Unknown types (persistent memory 7, 12, 0xffff) are reserved.
        for t in [0, 7, 12, 0xffff_ffff] {
            assert!(
                matches!(convert(&e(0x100000, 0x1000, t)), Some(Converted::Region(r)) if r.typ == EFI_RESERVED)
            );
        }
        // A length that wraps the address space is cut at the 52-bit limit.
        match convert(&e(0xffff_f000_0000, u64::MAX, 1)) {
            Some(Converted::Region(r)) => assert_eq!(r.start + r.pages * PAGE, PHYS_LIMIT),
            other => panic!("{other:?}"),
        }
        assert_eq!(convert(&e(PHYS_LIMIT, 0x1000, 1)), None);
    }

    #[test]
    fn parses_both_entry_sizes() {
        let mut raw = [0u8; 24];
        raw[0..8].copy_from_slice(&0x100000u64.to_le_bytes());
        raw[8..16].copy_from_slice(&0x2000u64.to_le_bytes());
        raw[16..20].copy_from_slice(&1u32.to_le_bytes());
        raw[20..24].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse(&raw[..20]).unwrap().attr, ATTR_ENABLED);
        assert_eq!(parse(&raw).unwrap().attr, 0);
        assert_eq!(parse(&raw[..19]), None);
    }

    #[test]
    fn fuzz_e820_never_reports_reserved_bytes_usable() {
        // Random entry sets against a byte-exact reference: a page is
        // usable only if some entry calls all of it usable and no entry
        // calls any byte of it anything else.
        crate::fuzzkit::run("e820", 0xe82, |rng| {
            let n = 1 + rng.below(12) as usize;
            let span = 64 * PAGE;
            let es: Vec<E820Entry> = (0..n)
                .map(|_| E820Entry {
                    base: rng.below(span),
                    len: rng.below(span / 2),
                    typ: [1, 1, 1, 2, 3, 4, 5, 7][rng.below(8) as usize],
                    attr: if rng.below(10) == 0 {
                        0
                    } else {
                        ATTR_ENABLED | ((rng.below(10) == 0) as u32 * 2)
                    },
                })
                .collect();
            let m = limine(&es);
            for page in 0..(span * 2) / PAGE {
                let (a, b) = (page * PAGE, page * PAGE + PAGE);
                let live = |x: &&E820Entry| x.attr & ATTR_ENABLED != 0 && x.len > 0;
                let touches = |x: &E820Entry| x.base < b && x.base + x.len > a;
                let covers = |x: &E820Entry| x.base <= a && x.base + x.len >= b;
                let usable_ok = es
                    .iter()
                    .filter(live)
                    .any(|x| x.typ == 1 && x.attr & 2 == 0 && covers(x))
                    && !es
                        .iter()
                        .filter(live)
                        .any(|x| (x.typ != 1 || x.attr & 2 != 0) && touches(x));
                let reported = m
                    .iter()
                    .any(|&(base, len, t)| t == MEMMAP_USABLE && base <= a && base + len >= b);
                assert_eq!(
                    reported, usable_ok,
                    "page {page:#x} entries {es:?} map {m:?}"
                );
            }
        });
    }
}
