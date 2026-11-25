use proptest::prelude::*;

use crate::reach::{PAGE, Piece, Reach, Readable, Span};

fn s(start: u64, end: u64) -> Span {
    Span { start, end }
}

fn p(phys: u64, len: u64, reach: Reach) -> Piece {
    Piece { phys, len, reach }
}

/// QEMU q35 with SeaBIOS and 512 MiB, as Limine reports it: the tables sit
/// in reserved memory just under the top of RAM, the RSDP in the BIOS area.
fn seabios() -> Readable {
    let usable = [s(0x1000, 0x9_f000), s(0x10_0000, 0x1ffd_f000)];
    let reserved = [
        s(0x9_fc00, 0xa_0000),
        s(0xf_0000, 0x10_0000),
        s(0x1ffd_f000, 0x2000_0000),
    ];
    let bios_area = [s(0xe_0000, 0x10_0000)];
    let allowed: Vec<Span> = reserved.iter().chain(&bios_area).copied().collect();
    // Only RAM is in the direct map, so here that's just the usable memory.
    Readable::new(&allowed, &usable, &usable)
}

#[test]
fn seabios_hand_worked() {
    let r = seabios();
    // RSDP at 0xf5270, 36 bytes, in one BIOS page: one window piece.
    assert_eq!(
        r.plan(0xf_5270, 36),
        Some(vec![p(0xf_5270, 36, Reach::Window)])
    );
    // A 0x1800-byte DSDT at 0x1ffe_0800 runs from page 0x1ffe0 into page
    // 0x1ffe1: two window pieces split at the page boundary 0x1ffe_1000
    // (0x800 bytes, then 0x1000 bytes).
    assert_eq!(
        r.plan(0x1ffe_0800, 0x1800),
        Some(vec![
            p(0x1ffe_0800, 0x800, Reach::Window),
            p(0x1ffe_1000, 0x1000, Reach::Window)
        ])
    );
    // Usable RAM is refused, so is a read that starts in reserved memory and
    // runs into usable RAM, and one that runs past the top of RAM.
    assert_eq!(r.plan(0x10_0000, 4), None);
    assert_eq!(r.plan(0x1ffd_effc, 8), None);
    assert_eq!(r.plan(0x1fff_fffc, 8), None);
    assert!(r.allows(0x1fff_fffc, 4));
    // The hole at 0xa0000 (VGA memory) is not allowed.
    assert!(!r.allows(0xa_0000, 1));
    // The last byte of the EBDA is fine, one more is not.
    assert!(r.allows(0x9_fc00, 0x400));
    assert!(!r.allows(0x9_fc00, 0x401));
    // Empty reads are always fine, even nowhere; wrapping reads never are.
    assert_eq!(r.plan(0x10_0000, 0), Some(vec![]));
    assert_eq!(r.plan(u64::MAX, 2), None);
    assert!(!r.allows(1, u64::MAX));
}

/// QEMU q35 with OVMF: tables in ACPI reclaimable RAM (direct-mapped) and
/// the FACS in ACPI NVS.
fn ovmf() -> Readable {
    let usable = [s(0, 0xa_0000), s(0x10_0000, 0x7f00_0000)];
    let acpi_reclaim = s(0x7fb7_e000, 0x7fb8_2000);
    let acpi_nvs = s(0x7fb8_2000, 0x7fbf_e000);
    let reserved = s(0x7fbf_e000, 0x7fc0_0000);
    let direct: Vec<Span> = usable
        .iter()
        .copied()
        .chain([acpi_reclaim, acpi_nvs])
        .collect();
    Readable::new(&[acpi_reclaim, acpi_nvs, reserved], &usable, &direct)
}

#[test]
fn ovmf_hand_worked() {
    let r = ovmf();
    // A table wholly in reclaimable RAM: one direct-map piece, even across
    // pages.
    assert_eq!(
        r.plan(0x7fb7_ef00, 0x2000),
        Some(vec![p(0x7fb7_ef00, 0x2000, Reach::DirectMap)])
    );
    // From reclaimable into NVS: both direct-mapped, still one piece.
    assert_eq!(
        r.plan(0x7fb8_1ff0, 0x20),
        Some(vec![p(0x7fb8_1ff0, 0x20, Reach::DirectMap)])
    );
    // From NVS into reserved: a direct-map piece up to 0x7fbfe000, then a
    // window piece per page.
    assert_eq!(
        r.plan(0x7fbf_dff0, 0x1020),
        Some(vec![
            p(0x7fbf_dff0, 0x10, Reach::DirectMap),
            p(0x7fbf_e000, 0x1000, Reach::Window),
            p(0x7fbf_f000, 0x10, Reach::Window),
        ])
    );
    // Free RAM below the tables is refused; low memory too.
    assert_eq!(r.plan(0x7eff_fff0, 4), None);
    assert_eq!(r.plan(0x1000, 36), None);
}

#[test]
fn forbidden_wins_over_allowed() {
    // A reserved entry that a usable entry overlaps (contradictory map):
    // only the part outside the usable entry is readable.
    let r = Readable::new(&[s(0x1000, 0x5000)], &[s(0x2000, 0x3000)], &[]);
    assert!(r.allows(0x1000, 0x1000));
    assert!(!r.allows(0x1fff, 2));
    assert!(!r.allows(0x2000, 1));
    assert!(r.allows(0x3000, 0x2000));
    assert!(!r.allows(0x1000, 0x4000));
}

#[test]
fn half_page_and_rounding() {
    // Allowed bytes 0x1800..0x2000 share page 0x1000 with direct-mapped RAM
    // 0x1000..0x1800. The direct map covers the whole page (rounded out), so
    // the read goes through it, and no second mapping with another cache
    // type is made.
    let r = Readable::new(
        &[s(0x1800, 0x2000)],
        &[s(0x1000, 0x1800)],
        &[s(0x1000, 0x1800)],
    );
    assert_eq!(
        r.plan(0x1800, 0x800),
        Some(vec![p(0x1800, 0x800, Reach::DirectMap)])
    );
    assert_eq!(r.plan(0x17ff, 2), None);
}

#[test]
fn top_of_address_space() {
    // Allowed up to u64::MAX (exclusive), direct span ending in the last
    // page: rounding saturates instead of wrapping.
    let top = u64::MAX - PAGE + 1;
    let r = Readable::new(&[s(top - PAGE, u64::MAX)], &[], &[s(top + 5, top + 6)]);
    assert_eq!(
        r.plan(top - 4, 8),
        Some(vec![
            p(top - 4, 4, Reach::Window),
            p(top, 4, Reach::DirectMap)
        ])
    );
    assert_eq!(
        r.plan(u64::MAX - 1, 1),
        Some(vec![p(u64::MAX - 1, 1, Reach::DirectMap)])
    );
    assert_eq!(r.plan(u64::MAX - 1, 2), None);
}

#[test]
fn nothing_allowed() {
    let r = Readable::new(&[], &[], &[s(0, 1 << 30)]);
    assert_eq!(r.plan(0x1000, 1), None);
    assert_eq!(r.plan(0x1000, 0), Some(vec![]));
}

/// Model over a small address space of `PAGES` pages: a byte-level bitmap of
/// what may be read and a page-level bitmap of the direct map.
const PAGES: u64 = 8;
const SPACE: u64 = PAGES * PAGE;

fn span_strategy() -> impl Strategy<Value = Span> {
    // Bias the ends towards page boundaries and their neighbours, where the
    // bugs live.
    let addr = prop_oneof![
        0..=SPACE,
        (0..=PAGES, -2i64..=2).prop_map(|(pg, d)| (pg * PAGE).saturating_add_signed(d).min(SPACE)),
    ];
    (addr.clone(), addr).prop_map(|(a, b)| s(a, b))
}

proptest! {
    #![proptest_config(super::config(5000))]

    #[test]
    fn plan_matches_byte_model(
        allowed in proptest::collection::vec(span_strategy(), 0..6),
        forbidden in proptest::collection::vec(span_strategy(), 0..4),
        direct in proptest::collection::vec(span_strategy(), 0..4),
        phys in 0..SPACE + 8,
        len in prop_oneof![0..64u64, 0..2 * PAGE + 8],
    ) {
        let r = Readable::new(&allowed, &forbidden, &direct);
        let byte_ok = |b: u64| {
            allowed.iter().any(|a| a.start <= b && b < a.end)
                && !forbidden.iter().any(|f| f.start <= b && b < f.end)
        };
        let page_direct = |b: u64| {
            let pg = b / PAGE;
            direct.iter().any(|d| d.start < d.end && d.start / PAGE <= pg && pg <= (d.end - 1) / PAGE)
        };
        let expect_ok = (phys..phys + len).all(byte_ok);
        prop_assert_eq!(r.allows(phys, len), expect_ok);
        let plan = r.plan(phys, len);
        prop_assert_eq!(plan.is_some(), expect_ok);
        if let Some(pieces) = plan {
            // Pieces are non-empty, in order, back to back, and cover the
            // read exactly.
            let mut at = phys;
            for (i, pc) in pieces.iter().enumerate() {
                prop_assert_eq!(pc.phys, at);
                prop_assert!(pc.len > 0);
                at += pc.len;
                // Every byte's reach is its page's.
                for b in pc.phys..pc.phys + pc.len {
                    let want = if page_direct(b) { Reach::DirectMap } else { Reach::Window };
                    prop_assert_eq!(pc.reach, want, "byte {:#x}", b);
                }
                if pc.reach == Reach::Window {
                    prop_assert_eq!(pc.phys / PAGE, (pc.phys + pc.len - 1) / PAGE);
                }
                // No two direct-map pieces in a row: they'd have been one.
                if i > 0 && pc.reach == Reach::DirectMap {
                    prop_assert_eq!(pieces[i - 1].reach, Reach::Window);
                }
            }
            prop_assert_eq!(at, phys + len);
        }
    }

    /// Far from zero, the same rules hold: shifting everything up by a big
    /// page-aligned offset shifts the plan and changes nothing else.
    #[test]
    fn plan_is_translation_invariant(
        allowed in proptest::collection::vec(span_strategy(), 0..6),
        forbidden in proptest::collection::vec(span_strategy(), 0..4),
        direct in proptest::collection::vec(span_strategy(), 0..4),
        phys in 0..SPACE,
        len in 0..2 * PAGE,
        shift_pages in 1u64..(1 << 40),
    ) {
        let k = shift_pages * PAGE;
        let up = |v: &[Span]| v.iter().map(|x| s(x.start + k, x.end + k)).collect::<Vec<_>>();
        let a = Readable::new(&allowed, &forbidden, &direct).plan(phys, len);
        let b = Readable::new(&up(&allowed), &up(&forbidden), &up(&direct)).plan(phys + k, len);
        let a = a.map(|v| v.into_iter().map(|x| Piece { phys: x.phys + k, ..x }).collect::<Vec<_>>());
        prop_assert_eq!(a, b);
    }
}
