//! Turning the firmware's list of usable memory into allocator zones.
//!
//! Each zone is one [`Buddy`](crate::frames::Buddy) allocator over a
//! contiguous run of frame numbers. A zone's per-frame metadata array covers
//! its holes too, so a big hole would waste metadata: a hole of more than
//! [`SPLIT_GAP`] starts a new zone instead. Zones start on a 1 GiB boundary
//! (the largest buddy block) and never cross an 8 TiB boundary, which keeps
//! every zone under the allocator's 2^32-frame limit.
//!
//! The metadata itself is carved out of usable memory, preferably inside the
//! zone it describes (so on NUMA machines it is local), otherwise from
//! wherever there is room. Carving always takes the lowest free part of a
//! usable range, so the memory left for the allocators is simply each range
//! minus a prefix ([`free_ranges`]).
//!
//! Nothing here trusts the map: ranges must be sorted and non-overlapping or
//! planning is refused, and every range is trimmed to whole pages, to at
//! least `min_addr`, and to below the 52-bit physical address limit.

use crate::PAGE_SIZE;
use crate::frames::{FrameMeta, MAX_ORDER};

/// A gap between usable ranges larger than this starts a new zone. A gap
/// this size costs 3 MiB of metadata if it stays inside one zone.
pub const SPLIT_GAP: u64 = 1 << 30;
/// Zones never cross a multiple of this address (8 TiB = 2^31 frames).
pub const ZONE_WINDOW: u64 = 1 << 43;
/// Zone bases are multiples of the largest buddy block (1 GiB).
pub const ZONE_ALIGN: u64 = PAGE_SIZE << MAX_ORDER;
/// Metadata bytes per frame.
pub const META_BYTES: u64 = core::mem::size_of::<FrameMeta>() as u64;
/// Physical addresses at or above this don't exist on x86_64.
const PHYS_LIMIT: u64 = 1 << 52;

/// Half-open byte range `[start, end)` of physical memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub const EMPTY: Range = Range { start: 0, end: 0 };

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    fn contains(&self, addr: u64) -> bool {
        self.start <= addr && addr < self.end
    }
}

/// One planned zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    /// First frame number the zone's allocator covers (a multiple of 2^18).
    pub base_frame: u64,
    /// Number of frames covered, holes included.
    pub frames: u64,
    /// Where its metadata array lives: `frames * META_BYTES` bytes, rounded
    /// up to whole pages, inside usable memory.
    pub meta: Range,
}

impl Zone {
    pub const EMPTY: Zone = Zone {
        base_frame: 0,
        frames: 0,
        meta: Range::EMPTY,
    };

    /// Byte range of physical addresses this zone covers.
    pub fn span(&self) -> Range {
        Range {
            start: self.base_frame * PAGE_SIZE,
            end: (self.base_frame + self.frames) * PAGE_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The map isn't sorted, or two ranges overlap.
    Unsorted,
    /// More zones are needed than the output has room for.
    TooManyZones,
    /// No usable range has room for a zone's metadata.
    NoRoomForMetadata,
}

/// The usable pieces of the map, in order: each range trimmed to whole pages
/// in `[min_addr, 2^52)` and split at every [`ZONE_WINDOW`] boundary. Empty
/// pieces are skipped. The map must already have been checked as sorted.
fn pieces(usable: &[Range], min_addr: u64) -> impl Iterator<Item = Range> + '_ {
    usable.iter().flat_map(move |r| {
        let start = r
            .start
            .max(min_addr)
            .min(PHYS_LIMIT)
            .next_multiple_of(PAGE_SIZE);
        let end = r.end.min(PHYS_LIMIT) & !(PAGE_SIZE - 1);
        let mut at = start;
        core::iter::from_fn(move || {
            if at >= end {
                return None;
            }
            let cut = ((at / ZONE_WINDOW) + 1) * ZONE_WINDOW;
            let piece = Range {
                start: at,
                end: end.min(cut),
            };
            at = piece.end;
            Some(piece)
        })
    })
}

/// Pieces of both lists, in address order.
fn merged<'a>(a: &'a [Range], b: &'a [Range], min_addr: u64) -> impl Iterator<Item = Range> + 'a {
    let mut a = pieces(a, min_addr).peekable();
    let mut b = pieces(b, min_addr).peekable();
    core::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if y.start < x.start => b.next(),
        (Some(_), _) => a.next(),
        (None, _) => b.next(),
    })
}

/// When walking two sorted lists in start order: does the next of `x`
/// (from the first list) come before the next of `y`? Ties go to `x`; a
/// tie between non-empty ranges is an overlap, refused whichever is first.
fn first_list_next(x: Option<&&Range>, y: Option<&&Range>) -> bool {
    match (x, y) {
        (Some(x), Some(y)) => x.start <= y.start,
        (x, _) => x.is_some(),
    }
}

/// No two non-empty ranges overlap, within either list or across them,
/// and each list is in address order. Checked on the ranges as given, not
/// trimmed to pages, which would hide an overlap of under a page. Walking
/// both lists in start order, any range that starts before the end of the
/// one before it is an overlap or out of order.
fn check_sorted(usable: &[Range], later: &[Range]) -> Result<(), PlanError> {
    let mut a = usable.iter().filter(|r| !r.is_empty()).peekable();
    let mut b = later.iter().filter(|r| !r.is_empty()).peekable();
    let mut prev_end = 0;
    loop {
        let next = if first_list_next(a.peek(), b.peek()) {
            a.next()
        } else {
            b.next()
        };
        let Some(r) = next else { return Ok(()) };
        if r.start < prev_end {
            return Err(PlanError::Unsorted);
        }
        prev_end = r.end;
    }
}

/// Bytes of `piece` already carved for metadata (always a prefix).
fn carved(piece: Range, zones: &[Zone]) -> u64 {
    zones
        .iter()
        .filter(|z| piece.contains(z.meta.start))
        .map(|z| z.meta.len())
        .sum()
}

/// Plan zones for `usable` memory, ignoring everything below `min_addr`.
/// Writes them to `out` and returns how many there are. On error nothing
/// useful is in `out`.
///
/// `usable` is memory free now: metadata is only ever carved from it.
/// `later` is memory that becomes free later (the bootloader's, once the
/// kernel no longer needs it): zones cover it too, so it can be handed to
/// the allocators then ([`later_ranges`]), but nothing is placed in it.
/// Each list must be sorted, and no two ranges (in either list) may overlap.
pub fn plan(
    usable: &[Range],
    later: &[Range],
    min_addr: u64,
    out: &mut [Zone],
) -> Result<usize, PlanError> {
    check_sorted(usable, later)?;
    // Group pieces into zones.
    let mut n = 0;
    let mut last_end = 0;
    for p in merged(usable, later, min_addr) {
        let same_zone = n > 0
            && p.start - last_end <= SPLIT_GAP
            && p.start / ZONE_WINDOW == (last_end - 1) / ZONE_WINDOW;
        if !same_zone {
            if n == out.len() {
                return Err(PlanError::TooManyZones);
            }
            out[n] = Zone {
                base_frame: (p.start & !(ZONE_ALIGN - 1)) / PAGE_SIZE,
                ..Zone::EMPTY
            };
            n += 1;
        }
        let z = &mut out[n - 1];
        z.frames = p.end / PAGE_SIZE - z.base_frame;
        last_end = p.end;
    }
    // Carve each zone's metadata: from the piece inside the zone with the
    // most room left, else from the piece anywhere with the most room left.
    for i in 0..n {
        let need = (out[i].frames * META_BYTES).next_multiple_of(PAGE_SIZE);
        let span = out[i].span();
        let room = |p: Range| p.len() - carved(p, &out[..i]);
        let best = |local: bool| {
            pieces(usable, min_addr)
                .filter(|p| !local || span.contains(p.start))
                .filter(|p| room(*p) >= need)
                .max_by_key(|p| (room(*p), core::cmp::Reverse(p.start)))
        };
        let p = best(true)
            .or_else(|| best(false))
            .ok_or(PlanError::NoRoomForMetadata)?;
        let start = p.start + carved(p, &out[..i]);
        out[i].meta = Range {
            start,
            end: start + need,
        };
    }
    Ok(n)
}

/// Call `f` for every range of usable memory left for the allocators once
/// the zones' metadata is carved out: each usable piece minus its carved
/// prefix. Every range lies inside exactly one zone.
pub fn free_ranges(usable: &[Range], min_addr: u64, zones: &[Zone], mut f: impl FnMut(Range)) {
    for p in pieces(usable, min_addr) {
        let r = Range {
            start: p.start + carved(p, zones),
            end: p.end,
        };
        if !r.is_empty() {
            f(r);
        }
    }
}

/// Call `f` for every range of `later` memory (as passed to [`plan`]), trimmed
/// the same way. Every range lies inside exactly one zone.
pub fn later_ranges(later: &[Range], min_addr: u64, mut f: impl FnMut(Range)) {
    pieces(later, min_addr).for_each(&mut f);
}

/// Index of the zone covering physical address `addr`.
pub fn zone_of(zones: &[Zone], addr: u64) -> Option<usize> {
    zones.iter().position(|z| z.span().contains(addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    fn r(start: u64, end: u64) -> Range {
        Range { start, end }
    }

    fn plan_vec(usable: &[Range], min: u64) -> Result<Vec<Zone>, PlanError> {
        let mut out = [Zone::EMPTY; 64];
        plan(usable, &[], min, &mut out).map(|n| out[..n].to_vec())
    }

    fn free_vec(usable: &[Range], min: u64, zones: &[Zone]) -> Vec<Range> {
        let mut v = Vec::new();
        free_ranges(usable, min, zones, |r| v.push(r));
        v
    }

    #[test]
    fn metadata_is_twelve_bytes_per_frame() {
        assert_eq!(META_BYTES, 12);
    }

    #[test]
    fn qemu_512m_map() {
        // QEMU q35 with 512 MiB under SeaBIOS, as Limine reports it (usable
        // entries only): 0x1000..0x9f000 and 0x100000..0x1fee0000 (the last
        // 0x120000 bytes hold ACPI tables and the like).
        //
        // Worked by hand, with min_addr = 1 MiB:
        //   the low range is all below 1 MiB, so it is dropped;
        //   one zone, base frame 0 (1 GiB aligned down), frames up to
        //   0x1fee0000 / 4096 = 0x1fee0 = 130784;
        //   metadata 130784 * 12 = 1569408 bytes = 383.16 pages -> 384 pages
        //   = 0x180000 bytes, at the start of the only range: 0x100000..0x280000.
        let map = [r(0x1000, 0x9f000), r(0x100000, 0x1fee0000)];
        let z = plan_vec(&map, MIB).unwrap();
        assert_eq!(
            z,
            [Zone {
                base_frame: 0,
                frames: 0x1fee0,
                meta: r(0x100000, 0x280000)
            }]
        );
        assert_eq!(free_vec(&map, MIB, &z), [r(0x280000, 0x1fee0000)]);
        // With min_addr = 0 the low range joins (from 0x1000) and the
        // metadata goes in the bigger range.
        let z = plan_vec(&map, 0).unwrap();
        assert_eq!(z[0].meta, r(0x100000, 0x280000));
        assert_eq!(
            free_vec(&map, 0, &z),
            [r(0x1000, 0x9f000), r(0x280000, 0x1fee0000)]
        );
    }

    #[test]
    fn a_big_hole_starts_a_new_zone() {
        // 2 GiB below the PCI hole, then 2 GiB from 4 GiB: the 2 GiB gap is
        // more than SPLIT_GAP, so two zones, each with its own metadata.
        //   zone 0: frames 0..0x80000, meta 0x80000 * 12 = 0x600000 bytes
        //   zone 1: base 4 GiB = frame 0x100000, frames 0x80000, same size,
        //           carved at its own start, 4 GiB.
        let map = [r(MIB, 2 * GIB), r(4 * GIB, 6 * GIB)];
        let z = plan_vec(&map, MIB).unwrap();
        assert_eq!(
            z,
            [
                Zone {
                    base_frame: 0,
                    frames: 0x80000,
                    meta: r(MIB, MIB + 0x600000)
                },
                Zone {
                    base_frame: 0x100000,
                    frames: 0x80000,
                    meta: r(4 * GIB, 4 * GIB + 0x600000)
                },
            ]
        );
        // A gap of exactly SPLIT_GAP stays in one zone.
        let map = [r(MIB, GIB), r(2 * GIB, 3 * GIB)];
        let z = plan_vec(&map, MIB).unwrap();
        assert_eq!(z.len(), 1);
        assert_eq!(z[0].frames, 3 * GIB / 4096);
        // One byte more and it splits.
        let map = [r(MIB, GIB - 4096), r(2 * GIB, 3 * GIB)];
        assert_eq!(plan_vec(&map, MIB).unwrap().len(), 2);
    }

    #[test]
    fn zones_never_cross_an_8_tib_boundary() {
        // One range from 8 TiB - 1 GiB to 8 TiB + 1 GiB becomes two zones
        // meeting at 8 TiB.
        let t8 = ZONE_WINDOW;
        let map = [r(t8 - GIB, t8 + GIB)];
        let z = plan_vec(&map, 0).unwrap();
        assert_eq!(z.len(), 2);
        assert_eq!(z[0].span(), r(t8 - GIB, t8));
        assert_eq!(z[1].span(), r(t8, t8 + GIB));
        // Each zone's metadata is in its own half.
        assert!(z[0].span().contains(z[0].meta.start));
        assert!(z[1].span().contains(z[1].meta.start));
        // A full 8 TiB window is exactly 2^31 frames, the most a zone holds.
        let map = [r(0, t8 * 2)];
        let z = plan_vec(&map, 0).unwrap();
        assert_eq!(z.len(), 2);
        assert_eq!(z[0].frames, 1 << 31);
        assert_eq!(z[1].frames, 1 << 31);
    }

    #[test]
    fn metadata_goes_elsewhere_when_the_zone_has_no_room() {
        // Zone 1 is two single pages, at 4 GiB and at 5 GiB - 4 KiB (less
        // than SPLIT_GAP apart, so one zone). It spans 0x40000 frames, so
        // its metadata is 0x40000 * 12 = 3 MiB, which doesn't fit in two
        // pages: it goes in zone 0's big range, right after zone 0's own
        // metadata (zone 0 spans 1 GiB = 0x40000 frames, also 3 MiB).
        let map = [
            r(MIB, GIB),
            r(4 * GIB, 4 * GIB + 4096),
            r(5 * GIB - 4096, 5 * GIB),
        ];
        let z = plan_vec(&map, MIB).unwrap();
        assert_eq!(z.len(), 2);
        let z0_meta = (GIB / 4096 * 12).next_multiple_of(4096);
        assert_eq!(z[0].meta, r(MIB, MIB + z0_meta));
        assert_eq!(z[1].frames, 0x40000);
        assert_eq!(z[1].meta, r(MIB + z0_meta, MIB + z0_meta + 0x40000 * 12));
        assert_eq!(
            free_vec(&map, MIB, &z),
            [
                r(MIB + z0_meta + 0x40000 * 12, GIB),
                r(4 * GIB, 4 * GIB + 4096),
                r(5 * GIB - 4096, 5 * GIB)
            ]
        );
    }

    #[test]
    fn room_left_counts_what_earlier_zones_carved() {
        // Zone 0 is two pieces: A = 1..5 MiB (4 MiB) and B = the last
        // 3.5 MiB below 1 GiB. It spans 0x40000 frames, so its metadata is
        // 3 MiB, carved from A (the most room): A has 1 MiB left. Zone 1 is
        // two single pages at 4 GiB and 5 GiB - 4 KiB, also 0x40000 frames
        // and 3 MiB of metadata, with no room of its own. Of the pieces
        // elsewhere, A has 1 MiB left (too little) and B 3.5 MiB, so it
        // goes at the start of B. Counting A's room as its length plus what
        // was carved (7 MiB) would put it past A's end.
        let b = GIB - 7 * MIB / 2;
        let map = [
            r(MIB, 5 * MIB),
            r(b, GIB),
            r(4 * GIB, 4 * GIB + 4096),
            r(5 * GIB - 4096, 5 * GIB),
        ];
        let z = plan_vec(&map, MIB).unwrap();
        assert_eq!(z.len(), 2);
        assert_eq!(z[0].meta, r(MIB, 4 * MIB));
        assert_eq!(z[1].meta, r(b, b + 3 * MIB));
        assert_eq!(
            free_vec(&map, MIB, &z),
            [
                r(4 * MIB, 5 * MIB),
                r(b + 3 * MIB, GIB),
                r(4 * GIB, 4 * GIB + 4096),
                r(5 * GIB - 4096, 5 * GIB)
            ]
        );
    }

    #[test]
    fn later_memory_is_covered_but_never_used_for_metadata() {
        // Usable 1..64 MiB now; bootloader memory 64..128 MiB and 4 GiB +
        // 64 MiB later. Worked by hand:
        //   zone 0 spans 0..128 MiB (32768 frames): metadata 32768 * 12 =
        //   384 KiB (0x60000), carved at 1 MiB from the usable range;
        //   zone 1 spans 4 GiB..4 GiB + 64 MiB (16384 frames, 192 KiB =
        //   0x30000) but holds only later memory, so its metadata is carved
        //   from the usable range too, right after zone 0's.
        let usable = [r(MIB, 64 * MIB)];
        let later = [r(64 * MIB, 128 * MIB), r(4 * GIB, 4 * GIB + 64 * MIB)];
        let mut out = [Zone::EMPTY; 8];
        let n = plan(&usable, &later, MIB, &mut out).unwrap();
        let z = &out[..n];
        assert_eq!(
            z,
            [
                Zone {
                    base_frame: 0,
                    frames: 32768,
                    meta: r(MIB, MIB + 0x60000)
                },
                Zone {
                    base_frame: 4 * GIB / 4096,
                    frames: 16384,
                    meta: r(MIB + 0x60000, MIB + 0x90000)
                },
            ]
        );
        assert_eq!(free_vec(&usable, MIB, z), [r(MIB + 0x90000, 64 * MIB)]);
        let mut l = Vec::new();
        later_ranges(&later, MIB, |x| l.push(x));
        assert_eq!(l, later);
        // Later memory overlapping usable memory is refused.
        assert_eq!(
            plan(&usable, &[r(32 * MIB, 65 * MIB)], MIB, &mut out),
            Err(PlanError::Unsorted)
        );
        // Only later memory, nothing to carve from: refused.
        assert_eq!(
            plan(&[], &later, MIB, &mut out),
            Err(PlanError::NoRoomForMetadata)
        );
    }

    #[test]
    fn refusals() {
        // Unsorted, overlapping, and no room at all.
        assert_eq!(
            plan_vec(&[r(2 * MIB, 3 * MIB), r(MIB, 2 * MIB)], 0),
            Err(PlanError::Unsorted)
        );
        assert_eq!(
            plan_vec(&[r(MIB, 3 * MIB), r(2 * MIB, 4 * MIB)], 0),
            Err(PlanError::Unsorted)
        );
        // Touching ranges are fine.
        assert!(plan_vec(&[r(MIB, 2 * MIB), r(2 * MIB, 4 * MIB)], 0).is_ok());
        // Out of order or overlapping by less than a page: trimming to whole
        // pages would hide it, but the map is still refused, in either list.
        let sub_page = [r(MIB, 4 * MIB), r(2 * MIB + 0x100, 2 * MIB + 0x200)];
        assert_eq!(plan_vec(&sub_page, 0), Err(PlanError::Unsorted));
        let mut out = [Zone::EMPTY; 4];
        assert_eq!(
            plan(&[r(8 * MIB, 16 * MIB)], &sub_page, 0, &mut out),
            Err(PlanError::Unsorted)
        );
        // And across the two lists: a usable range ending one byte into a
        // later range (found by CI's random maps). Trimmed to pages they
        // only touch, but the map itself overlaps.
        assert_eq!(
            plan(
                &[r(MIB + 0x123, 2 * MIB + 1)],
                &[r(2 * MIB, 2 * MIB + 0x1000)],
                0,
                &mut out
            ),
            Err(PlanError::Unsorted)
        );
        assert_eq!(
            plan(
                &[r(2 * MIB + 0xFFF, 4 * MIB)],
                &[r(MIB, 2 * MIB + 0x1000)],
                0,
                &mut out
            ),
            Err(PlanError::Unsorted)
        );
        // Empty entries (firmware does list them) are skipped, wherever
        // they sit; so is an inverted one.
        assert_eq!(
            plan_vec(
                &[r(MIB, 4 * MIB), r(2 * MIB, 2 * MIB), r(4 * MIB, 8 * MIB)],
                0
            ),
            plan_vec(&[r(MIB, 4 * MIB), r(4 * MIB, 8 * MIB)], 0)
        );
        assert!(
            plan_vec(
                &[r(MIB, 4 * MIB), r(3 * MIB, 2 * MIB), r(4 * MIB, 8 * MIB)],
                0
            )
            .is_ok()
        );
        assert!(r(5, 5).is_empty() && r(6, 5).is_empty() && !r(5, 6).is_empty());
        // One page of memory spread so that 1 GiB of span needs 3 MiB of
        // metadata but only a few pages exist.
        assert_eq!(
            plan_vec(&[r(0, 4096), r(GIB - 4096, GIB)], 0),
            Err(PlanError::NoRoomForMetadata)
        );
        // More zones than room in the output.
        let map: Vec<Range> = (0..5)
            .map(|i| r(i * 4 * GIB, i * 4 * GIB + 64 * MIB))
            .collect();
        let mut out = [Zone::EMPTY; 4];
        assert_eq!(plan(&map, &[], 0, &mut out), Err(PlanError::TooManyZones));
        let mut out = [Zone::EMPTY; 5];
        assert_eq!(plan(&map, &[], 0, &mut out), Ok(5));
    }

    #[test]
    fn ranges_are_trimmed_to_pages_and_limits() {
        // Unaligned ends shrink inwards; a range inside one page vanishes;
        // memory at or above 2^52 is ignored; empty ranges are skipped.
        let map = [
            r(0x100, 0x200),
            r(0x2001, 0x9fff),
            r(0x10000, 0x10000),
            r(MIB + 1, 64 * MIB - 1),
            r(PHYS_LIMIT - GIB, PHYS_LIMIT + GIB),
        ];
        let z = plan_vec(&map, 0).unwrap();
        let mut all = free_vec(&map, 0, &z);
        for zz in &z {
            all.push(zz.meta);
        }
        all.sort_by_key(|r| r.start);
        // Everything handed out is page aligned and below 2^52.
        for x in &all {
            assert_eq!(x.start % 4096, 0);
            assert_eq!(x.end % 4096, 0);
            assert!(x.end <= PHYS_LIMIT);
        }
        assert_eq!(all.first().unwrap().start, 0x3000);
        assert_eq!(all.last().unwrap().end, PHYS_LIMIT);
        assert_eq!(zone_of(&z, 0x3000), Some(0));
        assert_eq!(zone_of(&z, PHYS_LIMIT - 4096), Some(z.len() - 1));
        assert_eq!(zone_of(&z, PHYS_LIMIT), None);
    }

    /// Merge a list of ranges into sorted, disjoint, non-touching intervals.
    fn normalise(mut v: Vec<Range>) -> Vec<Range> {
        v.retain(|r| !r.is_empty());
        v.sort_by_key(|r| r.start);
        let mut out: Vec<Range> = Vec::new();
        for r in v {
            match out.last_mut() {
                Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
                _ => out.push(r),
            }
        }
        out
    }

    /// Reference model of `plan` and `free_ranges`, written plainly with
    /// vectors: the spec in the module comment, step by step.
    /// `map` holds every range in address order, each tagged free now or
    /// later.
    fn model(map: &[(Range, bool)], min: u64) -> Result<(Vec<Zone>, Vec<Range>), PlanError> {
        let live: Vec<&(Range, bool)> = map.iter().filter(|x| x.0.start < x.0.end).collect();
        if live.windows(2).any(|w| w[1].0.start < w[0].0.end) {
            return Err(PlanError::Unsorted);
        }
        let mut pieces = Vec::new();
        let mut now = Vec::new();
        for &&(x, is_now) in &live {
            let mut s = x.start.max(min).min(PHYS_LIMIT).div_ceil(4096) * 4096;
            let e = x.end.min(PHYS_LIMIT) / 4096 * 4096;
            while s < e {
                let w = s / ZONE_WINDOW;
                let pe = e.min((w + 1) * ZONE_WINDOW);
                pieces.push(r(s, pe));
                now.push(is_now);
                s = pe;
            }
        }
        let mut groups: Vec<Vec<Range>> = Vec::new();
        for p in &pieces {
            let join = groups.last().is_some_and(|g| {
                let last = g.last().unwrap();
                p.start - last.end <= SPLIT_GAP
                    && p.start / ZONE_WINDOW == (last.end - 1) / ZONE_WINDOW
            });
            if join {
                groups.last_mut().unwrap().push(*p)
            } else {
                groups.push(vec![*p])
            }
        }
        if groups.len() > 64 {
            return Err(PlanError::TooManyZones);
        }
        let mut used = vec![0u64; pieces.len()];
        let mut zones = Vec::new();
        for g in &groups {
            let base = g[0].start / ZONE_ALIGN * ZONE_ALIGN;
            let frames = (g.last().unwrap().end - base) / 4096;
            let need = (frames * 12).div_ceil(4096) * 4096;
            let span = r(base, base + frames * 4096);
            let pick = |local: bool| {
                let mut best: Option<usize> = None;
                for (i, p) in pieces.iter().enumerate() {
                    let room = p.len() - used[i];
                    if !now[i]
                        || (local && !(span.start <= p.start && p.start < span.end))
                        || room < need
                    {
                        continue;
                    }
                    if best.is_none_or(|b| room > pieces[b].len() - used[b]) {
                        best = Some(i);
                    }
                }
                best
            };
            let i = pick(true)
                .or_else(|| pick(false))
                .ok_or(PlanError::NoRoomForMetadata)?;
            let start = pieces[i].start + used[i];
            used[i] += need;
            zones.push(Zone {
                base_frame: base / 4096,
                frames,
                meta: r(start, start + need),
            });
        }
        let free = pieces
            .iter()
            .zip(&used)
            .zip(&now)
            .filter(|(_, n)| **n)
            .map(|((p, u), _)| r(p.start + u, p.end))
            .filter(|x| !x.is_empty())
            .collect();
        Ok((zones, free))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::test_cases(3000)))]
        /// Random memory maps (sorted gaps and lengths at page and odd byte
        /// granularity, up to tens of TiB): checked against the rules every
        /// plan must satisfy.
        #[test]
        fn random_maps_plan_correctly(
            parts in prop::collection::vec(
                (0u64..(4u64 << 40), 1u64..(1u64 << 36), any::<bool>(), prop::bool::weighted(0.75)),
                1..24,
            ),
            min in prop_oneof![Just(0u64), Just(MIB), 0u64..(16 * MIB)],
        ) {
            let mut tagged = Vec::new();
            let mut at = 0u64;
            for (gap, len, ragged, now) in parts {
                // Mostly small gaps, sometimes huge ones.
                let gap = if gap % 3 == 0 { gap } else { gap % (2 * GIB) };
                let len = if len % 2 == 0 { len % (64 * MIB) + 1 } else { len };
                let (s, e) = (at + gap, at + gap + len);
                let (s, e) = if ragged { (s, e) } else { (s & !4095, e & !4095) };
                tagged.push((r(s, e), now));
                at = e;
            }
            let map: Vec<Range> = tagged.iter().filter(|t| t.1).map(|t| t.0).collect();
            let later: Vec<Range> = tagged.iter().filter(|t| !t.1).map(|t| t.0).collect();
            let mut out = [Zone::EMPTY; 64];
            let got = plan(&map, &later, min, &mut out).map(|n| out[..n].to_vec());
            let (want, want_free) = match model(&tagged, min) {
                Ok((z, f)) => (Ok(z), f),
                Err(e) => (Err(e), Vec::new()),
            };
            prop_assert_eq!(&got, &want);
            let Ok(zones) = got else { return Ok(()) };
            let zones = &zones[..];
            prop_assert_eq!(free_vec(&map, min, zones), want_free);
            // Model of usable memory: trimmed pages in [min, 2^52).
            let usable = normalise(map.iter().map(|x| {
                let s = x.start.max(min).next_multiple_of(4096);
                let e = x.end.min(PHYS_LIMIT) & !4095;
                r(s, e.max(s))
            }).collect());
            let free = free_vec(&map, min, zones);
            // Zones: aligned, in order, disjoint, inside one 8 TiB window,
            // under the frame limit.
            for (i, z) in zones.iter().enumerate() {
                prop_assert_eq!(z.base_frame % (1 << MAX_ORDER), 0);
                prop_assert!(z.frames > 0 && z.frames <= 1 << 31);
                let s = z.span();
                prop_assert_eq!(s.start / ZONE_WINDOW, (s.end - 1) / ZONE_WINDOW);
                if i > 0 { prop_assert!(zones[i - 1].span().end <= s.start); }
                // Metadata: big enough, page aligned, inside usable memory.
                prop_assert!(z.meta.len() >= z.frames * META_BYTES);
                prop_assert!(z.meta.len() < z.frames * META_BYTES + 4096);
                let _ = i;
                prop_assert_eq!(z.meta.start % 4096, 0);
                prop_assert!(usable.iter().any(|u| u.start <= z.meta.start && z.meta.end <= u.end));
            }
            // Metadata ranges and free ranges together are exactly the
            // usable memory, with no overlaps.
            let mut parts: Vec<Range> = free.clone();
            parts.extend(zones.iter().map(|z| z.meta));
            parts.sort_by_key(|x| x.start);
            for w in parts.windows(2) { prop_assert!(w[0].end <= w[1].start, "overlap {:?}", w); }
            prop_assert_eq!(normalise(parts), usable.clone());
            // Later memory: trimmed the same way, each range inside one
            // zone, never overlapping metadata or free memory.
            let mut later_got = Vec::new();
            later_ranges(&later, min, |x| later_got.push(x));
            let later_model = normalise(later.iter().map(|x| {
                let s = x.start.max(min).next_multiple_of(4096);
                let e = x.end.min(PHYS_LIMIT) & !4095;
                r(s, e.max(s))
            }).collect());
            prop_assert_eq!(normalise(later_got.clone()), later_model.clone());
            for l in &later_got {
                let i = zone_of(zones, l.start);
                prop_assert!(i.is_some(), "later range {:?} outside every zone", l);
                prop_assert!(l.end <= zones[i.unwrap()].span().end);
                for z in zones {
                    prop_assert!(l.end <= z.meta.start || z.meta.end <= l.start);
                }
            }
            // Every free range lies inside exactly one zone.
            for f in &free {
                let i = zone_of(zones, f.start);
                prop_assert!(i.is_some());
                prop_assert!(f.end <= zones[i.unwrap()].span().end);
            }
            // Memory (now or later) more than SPLIT_GAP apart is in
            // different zones.
            let mut all = usable.clone();
            all.extend(later_model.iter().copied());
            let all = normalise(all);
            for w in all.windows(2) {
                if w[1].start - w[0].end > SPLIT_GAP {
                    prop_assert_ne!(zone_of(zones, w[0].start), zone_of(zones, w[1].start));
                }
            }
        }
    }
}
