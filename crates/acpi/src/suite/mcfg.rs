use proptest::prelude::*;

use crate::Error;
use crate::build_table;
use crate::mcfg::{BUS_BYTES, Ecam, Mcfg};
use crate::reach::Span;

fn entry(base: u64, segment: u16, start: u8, end: u8) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[..8].copy_from_slice(&base.to_le_bytes());
    e[8..10].copy_from_slice(&segment.to_le_bytes());
    e[10] = start;
    e[11] = end;
    e
}

fn mcfg(entries: &[[u8; 16]]) -> Vec<u8> {
    let mut body = vec![0u8; 8]; // reserved
    for e in entries {
        body.extend_from_slice(e);
    }
    build_table(b"MCFG", 1, &body)
}

#[test]
fn qemu_q35_mcfg_hand_worked() {
    // QEMU q35 under OVMF: one entry, ECAM at 0xe000_0000 for segment 0,
    // buses 0 to 255. Table: 36 header + 8 reserved + 16 = 60 bytes.
    let t = mcfg(&[entry(0xe000_0000, 0, 0, 255)]);
    assert_eq!(t.len(), 60);
    let m = Mcfg::parse(&t).unwrap();
    let e = Ecam {
        base: 0xe000_0000,
        segment: 0,
        start_bus: 0,
        end_bus: 255,
    };
    assert_eq!(
        m,
        Mcfg {
            checksum_ok: true,
            entries: vec![e],
            bad: vec![],
            partial_entry: false,
        }
    );
    // 256 buses x 1 MiB = 256 MiB: 0xe000_0000 + 0x1000_0000 = 0xf000_0000.
    let s = Span {
        start: 0xe000_0000,
        end: 0xf000_0000,
    };
    assert_eq!(e.span(), Some(s));
    assert_eq!(m.refused(), vec![s]);
}

#[test]
fn bus_range_hand_worked() {
    // Buses 0x10..=0x1f of a base at 0x8000_0000: bus 0x10 starts
    // 0x10 MiB in (0x8100_0000), and bus 0x1f's MiB ends at 0x20 MiB in
    // (0x8200_0000).
    let m = Mcfg::parse(&mcfg(&[entry(0x8000_0000, 1, 0x10, 0x1f)])).unwrap();
    assert_eq!(
        m.refused(),
        vec![Span {
            start: 0x8100_0000,
            end: 0x8200_0000
        }]
    );
    // One bus: exactly 1 MiB.
    let one = Mcfg::parse(&mcfg(&[entry(0x8000_0000, 0, 7, 7)])).unwrap();
    assert_eq!(
        one.refused(),
        vec![Span {
            start: 0x8070_0000,
            end: 0x8080_0000
        }]
    );
}

#[test]
fn bad_entries_refuse_all_256_buses_of_their_base() {
    let all = |base: u64| Span {
        start: base,
        end: base + 256 * BUS_BYTES,
    };
    // Reversed bus range.
    let m = Mcfg::parse(&mcfg(&[entry(0x8000_0000, 0, 9, 8)])).unwrap();
    assert!(m.entries.is_empty());
    assert_eq!(m.bad.len(), 1);
    assert_eq!(m.refused(), vec![all(0x8000_0000)]);
    // A base off 1 MiB alignment (by one page).
    let m = Mcfg::parse(&mcfg(&[entry(0x8000_1000, 0, 0, 0)])).unwrap();
    assert_eq!((m.entries.len(), m.refused()), (0, vec![all(0x8000_1000)]));
    // Aligned exactly at 1 MiB is fine.
    let m = Mcfg::parse(&mcfg(&[entry(0x10_0000, 0, 0, 0)])).unwrap();
    assert_eq!(m.entries.len(), 1);
    // A second entry overlapping the first in the same segment: the
    // second is bad. Buses 0..=15 then 15..=20 share bus 15.
    let m = Mcfg::parse(&mcfg(&[
        entry(0x8000_0000, 0, 0, 15),
        entry(0x9000_0000, 0, 15, 20),
    ]))
    .unwrap();
    assert_eq!(m.entries.len(), 1);
    assert_eq!(m.bad[0].base, 0x9000_0000);
    assert!(m.refused().contains(&all(0x9000_0000)));
    // Touching but not overlapping (15 then 16), and the same buses in
    // another segment: all usable.
    let m = Mcfg::parse(&mcfg(&[
        entry(0x8000_0000, 0, 0, 15),
        entry(0x9000_0000, 0, 16, 20),
        entry(0xa000_0000, 1, 0, 15),
    ]))
    .unwrap();
    assert_eq!((m.entries.len(), m.bad.len()), (3, 0));
    // Running past the top of the address space: the end of bus 255 of a
    // base 255 MiB below the top is 1 MiB past it. Bad, and the refusal
    // is cut at the top.
    let top = u64::MAX - 255 * BUS_BYTES + 1;
    let top = top & !(BUS_BYTES - 1);
    let m = Mcfg::parse(&mcfg(&[entry(top, 0, 0, 255)])).unwrap();
    assert_eq!(m.bad.len(), 1);
    assert_eq!(
        m.refused(),
        vec![Span {
            start: top,
            end: u64::MAX
        }]
    );
    // A zero base: bad, and refuses nothing.
    let m = Mcfg::parse(&mcfg(&[entry(0, 0, 0, 255)])).unwrap();
    assert_eq!((m.entries.len(), m.bad.len()), (0, 1));
    assert!(m.refused().is_empty());
}

#[test]
fn lengths() {
    // No entries at all is a valid (empty) MCFG: 44 bytes.
    let t = mcfg(&[]);
    assert_eq!(t.len(), 44);
    let m = Mcfg::parse(&t).unwrap();
    assert!(m.entries.is_empty() && m.bad.is_empty() && !m.partial_entry);
    // 43 bytes is too short to be an MCFG.
    let mut short = t[..43].to_vec();
    short[4..8].copy_from_slice(&43u32.to_le_bytes());
    assert_eq!(Mcfg::parse(&short), Err(Error::BadLength));
    // A partial entry (15 bytes) is flagged and ignored.
    let mut body = vec![0u8; 8];
    body.extend_from_slice(&entry(0x8000_0000, 0, 0, 0));
    body.extend_from_slice(&entry(0x9000_0000, 0, 0, 0)[..15]);
    let m = Mcfg::parse(&build_table(b"MCFG", 1, &body)).unwrap();
    assert!(m.partial_entry);
    assert_eq!((m.entries.len(), m.bad.len()), (1, 0));
    // A wrong checksum is reported, not refused.
    let mut bad = mcfg(&[entry(0x8000_0000, 0, 0, 0)]);
    bad[50] ^= 1;
    assert!(!Mcfg::parse(&bad).unwrap().checksum_ok);
    assert_eq!(
        Mcfg::parse(&build_table(b"MCFX", 1, &[0; 8])),
        Err(Error::BadSignature)
    );
}

/// Reference model: is `addr` refused, worked out entry by entry from the
/// rules in the module documentation, without spans.
fn model_refuses(entries: &[(u64, u16, u8, u8)], addr: u64) -> bool {
    let mut usable: Vec<(u16, u8, u8)> = Vec::new();
    for &(base, seg, s, e) in entries {
        let end = base as u128 + (e as u128 + 1) * BUS_BYTES as u128;
        let ok = base != 0
            && base % BUS_BYTES == 0
            && s <= e
            && end <= u64::MAX as u128
            && !usable
                .iter()
                .any(|&(os, a, b)| os == seg && a <= e && s <= b);
        let hit = if ok {
            usable.push((seg, s, e));
            let lo = base as u128 + s as u128 * BUS_BYTES as u128;
            (lo..end).contains(&(addr as u128))
        } else {
            base != 0
                && addr >= base
                && (addr as u128) < (base as u128 + 256 * BUS_BYTES as u128).min(u64::MAX as u128)
        };
        if hit {
            return true;
        }
    }
    false
}

fn arb_entry() -> impl Strategy<Value = (u64, u16, u8, u8)> {
    (
        prop_oneof![
            Just(0u64),
            (0u64..1 << 16).prop_map(|m| m << 20),
            any::<u64>(),
            (0u64..256).prop_map(|k| u64::MAX - (k << 20)),
            (0u64..256).prop_map(|k| (u64::MAX - (k << 20)) & !(BUS_BYTES - 1)),
        ],
        0u16..3,
        any::<u8>(),
        any::<u8>(),
    )
}

proptest! {
    #![proptest_config(super::config(3000))]

    #[test]
    fn random_mcfg_never_panics(body in proptest::collection::vec(any::<u8>(), 0..120)) {
        if let Ok(m) = Mcfg::parse(&build_table(b"MCFG", 1, &body)) {
            let _ = m.refused();
        }
    }

    /// Every entry lands in exactly one of `entries` and `bad`, with its
    /// fields as written.
    #[test]
    fn entries_round_trip(es in proptest::collection::vec(arb_entry(), 0..6)) {
        let raw: Vec<[u8; 16]> = es.iter().map(|&(b, g, s, e)| entry(b, g, s, e)).collect();
        let m = Mcfg::parse(&mcfg(&raw)).unwrap();
        prop_assert_eq!(m.entries.len() + m.bad.len(), es.len());
        // Every entry comes back exactly once, as usable or bad: compare as
        // multisets, since the list may hold duplicates (a seed once gave
        // [A, (0, 0, 0, 0), A] with A = (0, 2, 63, 118)).
        let mut all: Vec<(u64, u16, u8, u8)> = m.entries.iter().chain(m.bad.iter()).map(|e| (e.base, e.segment, e.start_bus, e.end_bus)).collect();
        let mut want = es.clone();
        all.sort_unstable();
        want.sort_unstable();
        prop_assert_eq!(all, want);
        // Usable entries never overlap within a segment.
        for (i, a) in m.entries.iter().enumerate() {
            for b in &m.entries[..i] {
                prop_assert!(a.segment != b.segment || a.end_bus < b.start_bus || b.end_bus < a.start_bus);
            }
        }
    }

    /// The refused spans agree with the model at the edges of every entry
    /// and at random addresses.
    #[test]
    fn refused_matches_model(es in proptest::collection::vec(arb_entry(), 0..5), probes in proptest::collection::vec(any::<u64>(), 8)) {
        let raw: Vec<[u8; 16]> = es.iter().map(|&(b, g, s, e)| entry(b, g, s, e)).collect();
        let spans = Mcfg::parse(&mcfg(&raw)).unwrap().refused();
        let mut at: Vec<u64> = probes;
        for &(b, _, s, e) in &es {
            for p in [
                b,
                b.wrapping_sub(1),
                b.wrapping_add(u64::from(s) << 20),
                b.wrapping_add(u64::from(s) << 20).wrapping_sub(1),
                b.wrapping_add((u64::from(e) + 1) << 20),
                b.wrapping_add((u64::from(e) + 1) << 20).wrapping_sub(1),
                b.wrapping_add(256 << 20),
                b.wrapping_add(256 << 20).wrapping_sub(1),
            ] {
                at.push(p);
            }
        }
        for a in at {
            let got = spans.iter().any(|s| s.start <= a && a < s.end);
            prop_assert_eq!(got, model_refuses(&es, a), "address {:#x}", a);
        }
    }
}
