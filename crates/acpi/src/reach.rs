//! Which physical memory the kernel may read ACPI tables from, and how to
//! reach each byte of it.
//!
//! Firmware hands the kernel physical addresses (the RSDP, the root table,
//! every table it lists). A corrupt or hostile address must never make the
//! kernel read its own free RAM, read a device register, or fault. So every
//! read goes through [`Readable::plan`], which allows a range only if every
//! byte of it is in memory the kernel was told holds firmware data, and then
//! says how to reach each part:
//!
//! - [`Reach::DirectMap`]: the page is already in the kernel's direct map
//!   (it is RAM: ACPI reclaimable or NVS memory, as UEFI firmware uses), so
//!   it is read there, with the cache type the direct map already uses.
//! - [`Reach::Window`]: the page is not in the direct map (reserved memory,
//!   as SeaBIOS and many BIOSes use, or the legacy BIOS area), so the kernel
//!   maps that one page on its own, read-only and uncached, for the read.
//!   Uncached is right for both RAM and anything else, and since no other
//!   mapping of the page exists, two cache types can never alias.
//!
//! The decision is made per 4 KiB page for the reach and per byte for the
//! permission, so a table that straddles two kinds of memory still reads
//! correctly, and a page that is half firmware data and half free RAM can
//! only be read on its firmware half.

use alloc::vec::Vec;

/// x86 page size; the direct map and the read window work in whole pages.
pub const PAGE: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Read through the kernel's direct map.
    DirectMap,
    /// Map this one page on its own (read-only, uncached) and read it.
    Window,
}

/// One part of a planned read. A `Window` piece never crosses a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub phys: u64,
    pub len: u64,
    pub reach: Reach,
}

/// A half-open physical range `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

/// Sorted, merged, non-empty spans from any list.
fn normalise(spans: &[Span]) -> Vec<Span> {
    let mut v: Vec<Span> = spans.iter().copied().filter(|s| s.start < s.end).collect();
    v.sort_unstable_by_key(|s| s.start);
    let mut out: Vec<Span> = Vec::with_capacity(v.len());
    for s in v {
        match out.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

/// `a` minus `b`; both normalised, and so is the result.
fn subtract(a: &[Span], b: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    for s in a {
        let mut start = s.start;
        for f in b {
            if f.end <= start || f.start >= s.end {
                continue;
            }
            if f.start > start {
                out.push(Span {
                    start,
                    end: f.start,
                });
            }
            start = start.max(f.end);
        }
        if start < s.end {
            out.push(Span { start, end: s.end });
        }
    }
    out
}

/// The span in sorted, disjoint `spans` that contains `addr`, if any.
fn containing(spans: &[Span], addr: u64) -> Option<Span> {
    // Index of the first span starting after addr; the one before it is
    // the only candidate.
    let i = spans.partition_point(|s| s.start <= addr);
    let s = *spans.get(i.checked_sub(1)?)?;
    (addr < s.end).then_some(s)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readable {
    /// Bytes that may be read: allowed minus forbidden.
    allowed: Vec<Span>,
    /// Pages in the direct map, rounded out to whole pages.
    direct: Vec<Span>,
}

impl Readable {
    /// - `allowed`: memory that holds firmware data (the memory map's ACPI
    ///   reclaimable, ACPI NVS and reserved entries, and the legacy BIOS
    ///   area where the map has nothing).
    /// - `forbidden`: memory that must never be read even if an allowed
    ///   entry overlaps it (usable RAM, the kernel, device windows). It wins
    ///   over `allowed`, so overlapping or contradictory map entries can
    ///   only shrink what is readable.
    /// - `direct`: what the kernel's direct map covers. Each span is rounded
    ///   out to whole pages, exactly as the direct map is built.
    ///
    /// Lists may be unsorted and may overlap; empty spans are ignored.
    pub fn new(allowed: &[Span], forbidden: &[Span], direct: &[Span]) -> Readable {
        let rounded: Vec<Span> = direct
            .iter()
            .filter(|s| s.start < s.end)
            .map(|s| Span {
                start: s.start & !(PAGE - 1),
                // A span ending in the last page of the address space ends
                // at u64::MAX: the final byte can never be part of a read
                // anyway (reads end at most at u64::MAX, exclusive).
                end: s.end.checked_next_multiple_of(PAGE).unwrap_or(u64::MAX),
            })
            .collect();
        Readable {
            allowed: subtract(&normalise(allowed), &normalise(forbidden)),
            direct: normalise(&rounded),
        }
    }

    /// Is every byte of `[phys, phys + len)` readable?
    pub fn allows(&self, phys: u64, len: u64) -> bool {
        let Some(end) = phys.checked_add(len) else {
            return false;
        };
        len == 0 || containing(&self.allowed, phys).is_some_and(|s| end <= s.end)
    }

    /// How to read `[phys, phys + len)`: its pieces, in order, covering it
    /// exactly. `None` if any byte of it may not be read, so a refused
    /// read reads nothing at all. An empty read is an empty plan.
    pub fn plan(&self, phys: u64, len: u64) -> Option<Vec<Piece>> {
        if !self.allows(phys, len) {
            return None;
        }
        let end = phys + len;
        let mut out: Vec<Piece> = Vec::new();
        let mut at = phys;
        while at < end {
            let page_end = (at & !(PAGE - 1)).saturating_add(PAGE).min(end);
            // A run of direct-mapped pages is one piece (the direct spans
            // are merged, so the next byte after one is never direct-mapped);
            // anything else is one window piece per page.
            let (reach, piece_end) = match containing(&self.direct, at) {
                Some(d) => (Reach::DirectMap, d.end.min(end)),
                None => (Reach::Window, page_end),
            };
            out.push(Piece {
                phys: at,
                len: piece_end - at,
                reach,
            });
            at = piece_end;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(start: u64, end: u64) -> Span {
        Span { start, end }
    }

    #[test]
    fn normalise_sorts_merges_and_drops_empty() {
        assert_eq!(
            normalise(&[s(10, 20), s(0, 5), s(5, 7), s(15, 30), s(40, 40), s(9, 3)]),
            [s(0, 7), s(10, 30)]
        );
    }

    #[test]
    fn subtract_hand_worked() {
        // [0,100) minus [10,20) and [90,200) leaves [0,10) and [20,90).
        assert_eq!(
            subtract(&[s(0, 100)], &[s(10, 20), s(90, 200)]),
            [s(0, 10), s(20, 90)]
        );
        // A hole covering everything leaves nothing; touching ends remove
        // nothing.
        assert_eq!(subtract(&[s(5, 10)], &[s(0, 20)]), []);
        assert_eq!(subtract(&[s(5, 10)], &[s(0, 5), s(10, 20)]), [s(5, 10)]);
        // A hole at either end leaves no empty span behind.
        assert_eq!(subtract(&[s(0, 10)], &[s(0, 5)]), [s(5, 10)]);
        assert_eq!(subtract(&[s(0, 10)], &[s(5, 10)]), [s(0, 5)]);
    }

    #[test]
    fn containing_hand_worked() {
        let v = [s(10, 20), s(30, 40)];
        assert_eq!(containing(&v, 9), None);
        assert_eq!(containing(&v, 10), Some(s(10, 20)));
        assert_eq!(containing(&v, 19), Some(s(10, 20)));
        assert_eq!(containing(&v, 20), None);
        assert_eq!(containing(&v, 39), Some(s(30, 40)));
        assert_eq!(containing(&v, 40), None);
        assert_eq!(containing(&[], 0), None);
    }
}
