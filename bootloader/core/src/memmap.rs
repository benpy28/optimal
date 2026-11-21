//! Turning the firmware's memory map into the Limine memory map.
//!
//! Firmware maps are hostile input too: entries can be unsorted, overlap, or
//! have odd sizes on real machines. We "paint" every range with a priority so
//! that when two ranges overlap, the most conservative type wins (a byte that
//! any source calls reserved is never reported as usable).

use crate::protocol::*;

pub const PAGE: u64 = 4096;

// EFI memory types (UEFI spec, EFI_MEMORY_TYPE).
pub const EFI_RESERVED: u32 = 0;
pub const EFI_LOADER_CODE: u32 = 1;
pub const EFI_LOADER_DATA: u32 = 2;
pub const EFI_BOOT_SERVICES_CODE: u32 = 3;
pub const EFI_BOOT_SERVICES_DATA: u32 = 4;
pub const EFI_RUNTIME_SERVICES_CODE: u32 = 5;
pub const EFI_RUNTIME_SERVICES_DATA: u32 = 6;
pub const EFI_CONVENTIONAL: u32 = 7;
pub const EFI_UNUSABLE: u32 = 8;
pub const EFI_ACPI_RECLAIM: u32 = 9;
pub const EFI_ACPI_NVS: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EfiRegion {
    pub typ: u32,
    pub start: u64,
    pub pages: u64,
}

/// A range the loader itself knows more about than the firmware: the kernel
/// and modules, the framebuffer, and tables that must stay mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Override {
    pub start: u64,
    pub end: u64,
    pub typ: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MemmapError {
    OutOfSpace,
    FramebufferOverlap,
    BadRange,
}

/// Limine type for an EFI type, per the spec's EFI conversion table.
pub fn efi_to_limine(typ: u32, revision: u64) -> u64 {
    match typ {
        EFI_LOADER_CODE | EFI_LOADER_DATA | EFI_BOOT_SERVICES_CODE | EFI_BOOT_SERVICES_DATA => {
            MEMMAP_BOOTLOADER_RECLAIMABLE
        }
        EFI_RUNTIME_SERVICES_CODE | EFI_RUNTIME_SERVICES_DATA => {
            if revision >= 5 {
                MEMMAP_RESERVED_MAPPED
            } else {
                MEMMAP_RESERVED
            }
        }
        EFI_ACPI_RECLAIM => MEMMAP_ACPI_RECLAIMABLE,
        EFI_ACPI_NVS => MEMMAP_ACPI_NVS,
        EFI_CONVENTIONAL => MEMMAP_USABLE,
        _ => MEMMAP_RESERVED,
    }
}

/// Higher number wins where ranges overlap.
fn priority(typ: u64) -> usize {
    match typ {
        MEMMAP_USABLE => 0,
        MEMMAP_BOOTLOADER_RECLAIMABLE => 1,
        MEMMAP_RESERVED => 2,
        MEMMAP_RESERVED_MAPPED => 3,
        MEMMAP_ACPI_RECLAIMABLE => 4,
        MEMMAP_ACPI_NVS => 5,
        MEMMAP_BAD_MEMORY => 6,
        MEMMAP_FRAMEBUFFER => 7,
        MEMMAP_EXECUTABLE_AND_MODULES => 8,
        _ => 2,
    }
}
const PRIO_TYPES: [u64; 9] = [
    MEMMAP_USABLE,
    MEMMAP_BOOTLOADER_RECLAIMABLE,
    MEMMAP_RESERVED,
    MEMMAP_RESERVED_MAPPED,
    MEMMAP_ACPI_RECLAIMABLE,
    MEMMAP_ACPI_NVS,
    MEMMAP_BAD_MEMORY,
    MEMMAP_FRAMEBUFFER,
    MEMMAP_EXECUTABLE_AND_MODULES,
];

/// A sweep event: at `addr`, a range of priority `prio` starts or ends.
#[derive(Clone, Copy, Debug, Default)]
pub struct Event {
    addr: u64,
    prio: u8,
    start: bool,
}

/// Scratch space needed for `efi_count` firmware entries plus `override_count`.
pub const fn scratch_needed(efi_count: usize, override_count: usize) -> usize {
    2 * (efi_count + override_count)
}

fn page_down(x: u64) -> u64 {
    x & !(PAGE - 1)
}
fn page_up(x: u64) -> u64 {
    x.saturating_add(PAGE - 1) & !(PAGE - 1)
}

/// Builds the Limine memory map into `out` and returns how many entries it
/// wrote. Overrides are widened to whole pages, because the kernel cannot use
/// part of a page and the framebuffer needs pages of its own.
pub fn build(
    efi: impl Iterator<Item = EfiRegion>,
    overrides: &[Override],
    revision: u64,
    scratch: &mut [Event],
    out: &mut [MemmapEntry],
) -> Result<usize, MemmapError> {
    let mut n = 0usize;
    let mut push = |scratch: &mut [Event], start: u64, end: u64, typ: u64| {
        if end <= start {
            return Ok(());
        }
        if n + 2 > scratch.len() {
            return Err(MemmapError::OutOfSpace);
        }
        let prio = priority(typ) as u8;
        scratch[n] = Event {
            addr: start,
            prio,
            start: true,
        };
        scratch[n + 1] = Event {
            addr: end,
            prio,
            start: false,
        };
        n += 2;
        Ok(())
    };
    for r in efi {
        let Some(len) = r.pages.checked_mul(PAGE) else {
            return Err(MemmapError::BadRange);
        };
        let end = r.start.saturating_add(len);
        push(scratch, r.start, end, efi_to_limine(r.typ, revision))?;
    }
    for o in overrides {
        if o.end < o.start {
            return Err(MemmapError::BadRange);
        }
        push(scratch, page_down(o.start), page_up(o.end), o.typ)?;
    }
    let events = &mut scratch[..n];
    events.sort_unstable_by_key(|e| e.addr);

    let mut active = [0u32; 9];
    let mut count = 0usize;
    let mut i = 0;
    let mut prev_addr = 0u64;
    while i < events.len() {
        let addr = events[i].addr;
        // Emit the segment [prev_addr, addr) with the type active over it.
        if i > 0
            && addr > prev_addr
            && let Some(top) = (0..9).rev().find(|&p| active[p] > 0)
        {
            let typ = PRIO_TYPES[top];
            if active[7] > 0 && (active[4] | active[5] | active[6] | active[3] | active[8]) > 0 {
                return Err(MemmapError::FramebufferOverlap);
            }
            emit(out, &mut count, prev_addr, addr, typ)?;
        }
        while i < events.len() && events[i].addr == addr {
            let e = events[i];
            if e.start {
                active[e.prio as usize] += 1;
            } else {
                active[e.prio as usize] -= 1;
            }
            i += 1;
        }
        prev_addr = addr;
    }
    Ok(count)
}

fn emit(
    out: &mut [MemmapEntry],
    count: &mut usize,
    mut start: u64,
    mut end: u64,
    typ: u64,
) -> Result<(), MemmapError> {
    if typ == MEMMAP_USABLE || typ == MEMMAP_BOOTLOADER_RECLAIMABLE {
        start = page_up(start);
        end = page_down(end);
        if end <= start {
            return Ok(());
        }
    }
    if *count > 0 {
        let last = &mut out[*count - 1];
        if last.typ == typ && last.end() == start {
            last.length += end - start;
            return Ok(());
        }
    }
    if *count == out.len() {
        return Err(MemmapError::OutOfSpace);
    }
    out[*count] = MemmapEntry {
        base: start,
        length: end - start,
        typ,
    };
    *count += 1;
    Ok(())
}

/// Whether a memory map type goes into the higher half direct map.
pub fn in_hhdm(typ: u64, revision: u64) -> bool {
    match typ {
        MEMMAP_USABLE
        | MEMMAP_BOOTLOADER_RECLAIMABLE
        | MEMMAP_EXECUTABLE_AND_MODULES
        | MEMMAP_FRAMEBUFFER => true,
        MEMMAP_RESERVED_MAPPED | MEMMAP_ACPI_RECLAIMABLE | MEMMAP_ACPI_NVS => revision >= 4,
        _ => false,
    }
}

/// A run of physical pages to put in the HHDM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HhdmRange {
    pub start: u64,
    pub end: u64,
    pub write_combining: bool,
}

/// The HHDM ranges for a finished memory map, widened to pages and merged.
/// Calls `f` for each, in address order.
pub fn hhdm_ranges(entries: &[MemmapEntry], revision: u64, mut f: impl FnMut(HhdmRange)) {
    let mut cur: Option<HhdmRange> = None;
    for e in entries.iter().filter(|e| in_hhdm(e.typ, revision)) {
        let r = HhdmRange {
            start: page_down(e.base),
            end: page_up(e.end()),
            write_combining: e.typ == MEMMAP_FRAMEBUFFER,
        };
        match cur.as_mut() {
            Some(c) if c.write_combining == r.write_combining && r.start <= c.end => {
                c.end = c.end.max(r.end);
            }
            Some(c) => {
                // A different cache type must never share a page; build()
                // guarantees this, and we cut the earlier range to be sure.
                if r.start < c.end {
                    c.end = r.start;
                }
                if c.end > c.start {
                    f(*c);
                }
                cur = Some(r);
            }
            None => cur = Some(r),
        }
    }
    if let Some(c) = cur
        && c.end > c.start
    {
        f(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn run(efi: &[EfiRegion], ov: &[Override], rev: u64) -> Result<Vec<MemmapEntry>, MemmapError> {
        let mut scratch = vec![Event::default(); scratch_needed(efi.len(), ov.len())];
        let mut out = vec![
            MemmapEntry {
                base: 0,
                length: 0,
                typ: 0
            };
            efi.len() * 3 + ov.len() * 3 + 4
        ];
        let n = build(efi.iter().copied(), ov, rev, &mut scratch, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    fn r(typ: u32, start: u64, pages: u64) -> EfiRegion {
        EfiRegion { typ, start, pages }
    }

    #[test]
    fn simple_map_merges_and_converts() {
        let m = run(
            &[
                r(EFI_CONVENTIONAL, 0, 0x9f),
                r(EFI_RESERVED, 0x9f000, 0x61),
                r(EFI_CONVENTIONAL, 0x100000, 0x100),
                r(EFI_BOOT_SERVICES_DATA, 0x200000, 0x10),
                r(EFI_LOADER_DATA, 0x210000, 0x10),
                r(EFI_ACPI_RECLAIM, 0x220000, 1),
                r(EFI_RUNTIME_SERVICES_DATA, 0x221000, 2),
            ],
            &[],
            5,
        )
        .unwrap();
        let t: Vec<_> = m.iter().map(|e| (e.base, e.length, e.typ)).collect();
        assert_eq!(
            t,
            vec![
                (0, 0x9f000, MEMMAP_USABLE),
                (0x9f000, 0x61000, MEMMAP_RESERVED),
                (0x100000, 0x100000, MEMMAP_USABLE),
                (0x200000, 0x20000, MEMMAP_BOOTLOADER_RECLAIMABLE),
                (0x220000, 0x1000, MEMMAP_ACPI_RECLAIMABLE),
                (0x221000, 0x2000, MEMMAP_RESERVED_MAPPED),
            ]
        );
        // Revision 3 reports runtime services as plain reserved.
        let m3 = run(&[r(EFI_RUNTIME_SERVICES_CODE, 0x1000, 1)], &[], 3).unwrap();
        assert_eq!(m3[0].typ, MEMMAP_RESERVED);
    }

    #[test]
    fn overlaps_pick_the_safest_type() {
        let m = run(
            &[r(EFI_CONVENTIONAL, 0, 0x100), r(EFI_RESERVED, 0x10000, 4)],
            &[],
            3,
        )
        .unwrap();
        let t: Vec<_> = m.iter().map(|e| (e.base, e.end(), e.typ)).collect();
        assert_eq!(
            t,
            vec![
                (0, 0x10000, MEMMAP_USABLE),
                (0x10000, 0x14000, MEMMAP_RESERVED),
                (0x14000, 0x100000, MEMMAP_USABLE),
            ]
        );
    }

    #[test]
    fn overrides_carve_out_pages() {
        let m = run(
            &[
                r(EFI_LOADER_DATA, 0x100000, 0x100),
                r(EFI_CONVENTIONAL, 0x200000, 0x100),
            ],
            &[
                Override {
                    start: 0x110000,
                    end: 0x120000,
                    typ: MEMMAP_EXECUTABLE_AND_MODULES,
                },
                // An ACPI table living in conventional memory, unaligned.
                Override {
                    start: 0x250010,
                    end: 0x250100,
                    typ: MEMMAP_RESERVED_MAPPED,
                },
            ],
            4,
        )
        .unwrap();
        let t: Vec<_> = m.iter().map(|e| (e.base, e.end(), e.typ)).collect();
        assert_eq!(
            t,
            vec![
                (0x100000, 0x110000, MEMMAP_BOOTLOADER_RECLAIMABLE),
                (0x110000, 0x120000, MEMMAP_EXECUTABLE_AND_MODULES),
                (0x120000, 0x200000, MEMMAP_BOOTLOADER_RECLAIMABLE),
                (0x200000, 0x250000, MEMMAP_USABLE),
                (0x250000, 0x251000, MEMMAP_RESERVED_MAPPED),
                (0x251000, 0x300000, MEMMAP_USABLE),
            ]
        );
    }

    #[test]
    fn framebuffer_may_not_share_a_page_with_acpi() {
        let e = run(
            &[r(EFI_ACPI_NVS, 0x8000_0000, 1)],
            &[Override {
                start: 0x8000_0800,
                end: 0x8100_0000,
                typ: MEMMAP_FRAMEBUFFER,
            }],
            3,
        );
        assert_eq!(e.unwrap_err(), MemmapError::FramebufferOverlap);
        // Usable memory sharing a framebuffer page is trimmed instead.
        let m = run(
            &[r(EFI_CONVENTIONAL, 0x8000_0000, 4)],
            &[Override {
                start: 0x8000_1800,
                end: 0x8000_2000,
                typ: MEMMAP_FRAMEBUFFER,
            }],
            3,
        )
        .unwrap();
        let t: Vec<_> = m.iter().map(|e| (e.base, e.end(), e.typ)).collect();
        assert_eq!(
            t,
            vec![
                (0x8000_0000, 0x8000_1000, MEMMAP_USABLE),
                (0x8000_1000, 0x8000_2000, MEMMAP_FRAMEBUFFER),
                (0x8000_2000, 0x8000_4000, MEMMAP_USABLE),
            ]
        );
    }

    #[test]
    fn hhdm_follows_revision_rules() {
        let entries = [
            MemmapEntry {
                base: 0,
                length: 0x1000,
                typ: MEMMAP_USABLE,
            },
            MemmapEntry {
                base: 0x1000,
                length: 0x800,
                typ: MEMMAP_ACPI_RECLAIMABLE,
            },
            MemmapEntry {
                base: 0x2000,
                length: 0x1000,
                typ: MEMMAP_BOOTLOADER_RECLAIMABLE,
            },
            MemmapEntry {
                base: 0x3000,
                length: 0x1000,
                typ: MEMMAP_RESERVED,
            },
            MemmapEntry {
                base: 0x4000,
                length: 0x1000,
                typ: MEMMAP_FRAMEBUFFER,
            },
            MemmapEntry {
                base: 0x5000,
                length: 0x1000,
                typ: MEMMAP_USABLE,
            },
        ];
        let mut v3 = Vec::new();
        hhdm_ranges(&entries, 3, |r| {
            v3.push((r.start, r.end, r.write_combining))
        });
        assert_eq!(
            v3,
            vec![
                (0, 0x1000, false),
                (0x2000, 0x3000, false),
                (0x4000, 0x5000, true),
                (0x5000, 0x6000, false)
            ]
        );
        let mut v4 = Vec::new();
        hhdm_ranges(&entries, 4, |r| {
            v4.push((r.start, r.end, r.write_combining))
        });
        assert_eq!(
            v4,
            vec![
                (0, 0x3000, false),
                (0x4000, 0x5000, true),
                (0x5000, 0x6000, false)
            ]
        );
    }

    /// Randomised maps: whatever the firmware says, the result must be sorted,
    /// usable/reclaimable must be page aligned and never overlap anything,
    /// and no byte any source called non-usable may come out usable.
    #[test]
    fn random_maps_keep_invariants() {
        let mut rng = crate::testrng::Rng::new(42);
        let types = [
            0u32,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            7,
            7,
            8,
            9,
            10,
            11,
            14,
            0x8000_0001,
        ];
        for round in 0..3000 {
            let n = 1 + rng.below(40) as usize;
            let mut efi = Vec::new();
            for _ in 0..n {
                efi.push(r(
                    types[rng.below(types.len() as u64) as usize],
                    rng.below(256) * PAGE,
                    1 + rng.below(32),
                ));
            }
            let mut ov = Vec::new();
            for _ in 0..rng.below(4) {
                let s = rng.below(256 * PAGE);
                ov.push(Override {
                    start: s,
                    end: s + 1 + rng.below(8 * PAGE),
                    typ: [MEMMAP_EXECUTABLE_AND_MODULES, MEMMAP_RESERVED_MAPPED]
                        [rng.below(2) as usize],
                });
            }
            let rev = 3 + round % 4;
            let m = run(&efi, &ov, rev).unwrap();
            for w in m.windows(2) {
                assert!(
                    w[0].end() <= w[1].base,
                    "sorted and disjoint: {:?}",
                    m.len()
                );
            }
            for e in &m {
                assert!(e.length > 0);
                if e.typ == MEMMAP_USABLE || e.typ == MEMMAP_BOOTLOADER_RECLAIMABLE {
                    assert_eq!(e.base % PAGE, 0);
                    assert_eq!(e.length % PAGE, 0);
                }
            }
            // Every page must have the highest-priority type of its sources.
            for page in 0..300u64 {
                let a = page * PAGE;
                let mut want: Option<u64> = None;
                let mut consider = |t: u64| {
                    if want.is_none_or(|w| priority(t) > priority(w)) {
                        want = Some(t);
                    }
                };
                for e in &efi {
                    if a >= e.start && a < e.start + e.pages * PAGE {
                        consider(efi_to_limine(e.typ, rev));
                    }
                }
                for o in &ov {
                    if a >= page_down(o.start) && a < page_up(o.end) {
                        consider(o.typ);
                    }
                }
                let got = m.iter().find(|e| a >= e.base && a < e.end()).map(|e| e.typ);
                assert_eq!(got, want, "page {page:#x} round {round}");
            }
        }
    }
}
