use std::collections::VecDeque;

use proptest::prelude::*;

use super::config;
use crate::freq::Freq;
use crate::hpet::{self, T_64BIT_CAPABLE};
use crate::lapic::ticks_at_least;
use crate::oneshot::*;
use crate::pit;

const WIDE: u64 = T_64BIT_CAPABLE;
const NARROW: u64 = 0;

fn mhz100() -> Freq {
    Freq::hz(100_000_000).unwrap()
}

#[test]
fn hpet_max_ticks_by_width() {
    assert_eq!(hpet_max_ticks(NARROW), 2_147_483_647);
    assert_eq!(hpet_max_ticks(WIDE), 1 << 62);
    // Only Tn_SIZE_CAP counts: every other bit leaves it narrow.
    assert_eq!(hpet_max_ticks(!T_64BIT_CAPABLE), 2_147_483_647);
    assert_eq!(hpet_max_ticks(u64::MAX), 1 << 62);
}

#[test]
fn hpet_ticks_hand_worked() {
    let f = mhz100();
    // 1 ms at 100 MHz exactly: 100,000 ticks, plus one for the phase of
    // the counter read.
    assert_eq!(hpet_ticks(f, 0, 1_000_000, WIDE), Some(100_001));
    // 1 ppm fast: 100,000 * 1.000001 = 100,000.1, up to 100,001, plus one.
    assert_eq!(hpet_ticks(f, 1_000, 1_000_000, WIDE), Some(100_002));
    // 0 ns still waits one tick; 1 ns is 0.1 ticks, up to 1, plus one.
    assert_eq!(hpet_ticks(f, 0, 0, WIDE), Some(1));
    assert_eq!(hpet_ticks(f, 0, 1, WIDE), Some(2));
    // A 32-bit comparator: ceil(ns / 10) + 1 <= 2^31 - 1 holds up to
    // ns = 21,474,836,460 (2,147,483,646 + 1 ticks) and fails at the next
    // ns (ceil(2,147,483,646.1) + 1 = 2^31).
    assert_eq!(
        hpet_ticks(f, 0, 21_474_836_460, NARROW),
        Some(2_147_483_647)
    );
    assert_eq!(hpet_ticks(f, 0, 21_474_836_461, NARROW), None);
    assert_eq!(hpet_longest_ns(f, 0, NARROW), 21_474_836_460);
    // 64-bit: u64::MAX ns is ceil(1.8e18) + 1 ticks, far under 2^62.
    assert_eq!(hpet_longest_ns(f, 0, WIDE), u64::MAX);
    // An ICH HPET (69,841,279 fs, 14.318 MHz) 100 ppm fast, 32-bit: the
    // edge sits where ceil(ns * f * 1.0001) + 1 crosses 2^31 - 1, at
    // (2^31 - 2) / (14,318,180.4 Hz * 1.0001) = 149.968 s.
    let ich = Freq::from_period_fs(69_841_279).unwrap();
    let edge = hpet_longest_ns(ich, 100_000, NARROW);
    assert_eq!(edge / 1_000_000, 149_968, "{edge}");
    assert!(hpet_ticks(ich, 100_000, edge, NARROW).is_some());
    assert!(hpet_ticks(ich, 100_000, edge + 1, NARROW).is_none());
}

#[test]
fn pit_count_hand_worked() {
    let f = pit::FREQ;
    // 1 ms at 13,125,000 / 11 Hz is 1,193.18... periods: 1,194.
    assert_eq!(pit_count(f, 0, 1_000_000), Some(1_194));
    // 0 ns: the smallest mode 0 count, 1.
    assert_eq!(pit_count(f, 0, 0), Some(1));
    assert_eq!(pit_count(f, 0, 1), Some(1));
    // The edge: ns * 13,125,000 / (11 * 10^9) <= 65,536 up to
    // ns = 54,925,409 (65,535.9994 periods) and not at 54,925,410
    // (65,536.0006).
    assert_eq!(pit_count(f, 0, 54_925_409), Some(65_536));
    assert_eq!(pit_count(f, 0, 54_925_410), None);
    assert_eq!(pit_longest_ns(f, 0), 54_925_409);
    assert_eq!(PIT_MAX_COUNT, 65_536);
    // QEMU's integer 1,193,182 Hz: 65,536 / 1,193,182 s = 54,925,401.2 ns.
    let q = Freq::hz(1_193_182).unwrap();
    assert_eq!(pit_longest_ns(q, 0), 54_925_401);
    // 100 ppm fast: 54,925,409.5 / 1.0001 = 54,919,917.5 ns.
    assert_eq!(pit_longest_ns(f, 100_000), 54_919_917);
}

#[test]
fn pit_arm0_hand_worked() {
    assert_eq!(pit::MODE0_CH0, 0x30);
    assert_eq!(pit::CHANNEL0, 0x40);
    assert_eq!(pit::arm0(1_194), [(0x43, 0x30), (0x40, 0xAA), (0x40, 0x04)]);
    // 65,536 is written as 0, 0; 65,535 as 0xFF, 0xFF; 1 as 1, 0.
    assert_eq!(pit::arm0(65_536), [(0x43, 0x30), (0x40, 0), (0x40, 0)]);
    assert_eq!(
        pit::arm0(65_535),
        [(0x43, 0x30), (0x40, 0xFF), (0x40, 0xFF)]
    );
    assert_eq!(pit::arm0(1), [(0x43, 0x30), (0x40, 1), (0x40, 0)]);
    assert_eq!(pit::arm0(0x1234)[1..], [(0x40, 0x34), (0x40, 0x12)]);
}

#[test]
fn edge_one_shot_hand_worked() {
    let q35 = (0x00FF_0104u64 << 32) | hpet::T_PERIODIC_CAPABLE | T_64BIT_CAPABLE;
    // level_one_shot's 0x2036 (route 16, INT_ENB, TYPE level, caps)
    // without the level bit: 0x2034.
    assert_eq!(
        hpet::edge_one_shot(q35, 16, true),
        Some((0x00FF_0104 << 32) | 0x2034)
    );
    assert_eq!(
        hpet::edge_one_shot(q35, 16, false),
        Some((0x00FF_0104 << 32) | 0x2030)
    );
    // A comparator firmware left level-triggered comes out edge.
    assert_eq!(
        hpet::edge_one_shot(q35 | hpet::T_LEVEL, 2, true),
        Some((0x00FF_0104 << 32) | 0x434)
    );
    assert_eq!(hpet::edge_one_shot(q35, 3, true), None);
    assert_eq!(hpet::edge_one_shot(q35, 32, true), None);
}

/// A comparator whose counter reads come from a list, recording writes
/// and when the interrupt was enabled (after how many writes).
struct Fake {
    reads: VecDeque<u64>,
    writes: Vec<u64>,
    enabled_after: Option<usize>,
}

impl Comparator for Fake {
    fn counter(&mut self) -> u64 {
        self.reads.pop_front().expect("read past the script")
    }
    fn set(&mut self, value: u64) {
        self.writes.push(value);
    }
    fn enable(&mut self) {
        assert!(self.enabled_after.is_none(), "enabled twice");
        self.enabled_after = Some(self.writes.len());
    }
}

fn fake(reads: &[u64]) -> Fake {
    Fake {
        reads: reads.iter().copied().collect(),
        writes: Vec::new(),
        enabled_after: None,
    }
}

#[test]
fn hpet_program_hand_worked() {
    // A counter that does not move: one write, 50 ahead.
    let mut c = fake(&[1000, 1000]);
    assert_eq!(
        hpet_program(&mut c, 50, 10, WIDE, false),
        Ok(Programmed {
            start: 1000,
            value: 1050,
            distance: 50,
            retries: 0
        })
    );
    assert_eq!(c.writes, [1050]);
    // Reaching the value exactly counts as passed (equal is the match).
    let mut c = fake(&[1000, 1050, 1050, 1051]);
    let p = hpet_program(&mut c, 50, 10, WIDE, false).unwrap();
    assert_eq!(c.writes, [1050, 1110]);
    assert_eq!(
        p,
        Programmed {
            start: 1050,
            value: 1110,
            distance: 60,
            retries: 1
        }
    );
    // Passed twice: 50, then 50 + 10, then 60 + 20.
    let mut c = fake(&[1000, 1100, 1100, 1200, 1200, 1201]);
    let p = hpet_program(&mut c, 50, 10, WIDE, false).unwrap();
    assert_eq!(c.writes, [1050, 1160, 1280]);
    assert_eq!((p.value, p.distance, p.retries), (1280, 80, 2));
    // A 32-bit comparator across the wrap: 0xFFFF_FFF0 + 0x20 = 0x10, and
    // a read of 0xFFFF_FFF5 after it is before 0x10 in 32 bits.
    let mut c = fake(&[0xFFFF_FFF0, 0xFFFF_FFF5]);
    let p = hpet_program(&mut c, 0x20, 1, NARROW, false).unwrap();
    assert_eq!((p.value, p.retries), (0x10, 0));
    // But a read of 0x10 or past it is passed.
    let mut c = fake(&[0xFFFF_FFF0, 0x1_0000_0010, 0x1_0000_0010, 0x1_0000_0011]);
    let p = hpet_program(&mut c, 0x20, 1, NARROW, false).unwrap();
    assert_eq!((p.value, p.distance, p.retries), (0x31, 0x21, 1));
    // A step of 0 is taken as 1, so a retry always moves ahead.
    let mut c = fake(&[5, 6, 6, 6]);
    let p = hpet_program(&mut c, 1, 0, WIDE, false).unwrap();
    assert_eq!((p.value, p.distance), (8, 2));
}

#[test]
fn hpet_program_enables_after_the_first_write() {
    // Enabled after the first write, before the check, and only once
    // even when it retries.
    let mut c = fake(&[1000, 1000]);
    hpet_program(&mut c, 50, 10, WIDE, true).unwrap();
    assert_eq!(c.enabled_after, Some(1));
    let mut c = fake(&[1000, 1100, 1100, 1200, 1200, 1201]);
    let p = hpet_program(&mut c, 50, 10, WIDE, true).unwrap();
    assert_eq!(c.enabled_after, Some(1));
    assert_eq!(p.retries, 2);
    // Without: never.
    let mut c = fake(&[1000, 1000]);
    hpet_program(&mut c, 50, 10, WIDE, false).unwrap();
    assert_eq!(c.enabled_after, None);
}

#[test]
fn hpet_program_gives_up() {
    // A counter that jumps 2^40 on every read passes every value: 17
    // writes, then Missed with the last one.
    let reads: Vec<u64> = (0..2 * (MAX_RETRIES as u64 + 1)).map(|i| i << 40).collect();
    let mut c = fake(&reads);
    let r = hpet_program(&mut c, 50, 10, WIDE, false);
    assert_eq!(c.writes.len(), MAX_RETRIES as usize + 1);
    assert_eq!(
        r,
        Err(Missed {
            value: *c.writes.last().unwrap()
        })
    );
    assert!(c.reads.is_empty());
    // Distances: 50, 60, 80, ..., 50 + 10 * (2^16 - 1).
    let last = c.writes.last().unwrap() - reads[2 * MAX_RETRIES as usize];
    assert_eq!(last, 50 + 10 * ((1 << MAX_RETRIES) - 1));
}

#[test]
fn hpet_program_never_past_the_width() {
    // Asked for the most a 32-bit comparator allows, with a counter that
    // passes once: the retry cannot go further, and stays at the max.
    let max = hpet_max_ticks(NARROW);
    let mut c = fake(&[0, max, 0, 1]);
    let p = hpet_program(&mut c, max, 1000, NARROW, false).unwrap();
    assert_eq!((p.distance, p.retries), (max, 1));
    // More than the max is cut to it.
    let mut c = fake(&[7, 7]);
    let p = hpet_program(&mut c, u64::MAX, 1, NARROW, false).unwrap();
    assert_eq!((p.value, p.distance), (7 + max, max));
}

fn hpet_freq() -> impl Strategy<Value = Freq> {
    // Periods from 910 fs (1.1 THz) to 10^8 fs (10 MHz), the decoder's
    // range.
    (hpet::MIN_PERIOD_FS..=hpet::MAX_PERIOD_FS)
        .prop_map(|fs| Freq::from_period_fs(u64::from(fs)).unwrap())
}

fn pit_freq() -> impl Strategy<Value = Freq> {
    // Within 1% of nominal, in 1/1024 Hz, as a measured rate would be.
    (1_181_250u64 * 1024..1_205_114u64 * 1024).prop_map(|n| Freq::new(n, 1024).unwrap())
}

proptest! {
    #![proptest_config(config(2_000))]

    #[test]
    fn hpet_ticks_is_ticks_at_least_plus_one(
        f in hpet_freq(),
        ppb in 0u64..2_000_000,
        ns in any::<u64>(),
        wide in any::<bool>(),
    ) {
        let cfg = if wide { WIDE } else { NARROW };
        let want = ticks_at_least(f, ppb, ns) + 1;
        match hpet_ticks(f, ppb, ns, cfg) {
            Some(t) => {
                prop_assert_eq!(u128::from(t), want);
                prop_assert!(t <= hpet_max_ticks(cfg));
            }
            None => prop_assert!(want > u128::from(hpet_max_ticks(cfg))),
        }
    }

    #[test]
    fn hpet_longest_is_the_edge(
        f in hpet_freq(),
        ppb in 0u64..2_000_000,
        wide in any::<bool>(),
    ) {
        let cfg = if wide { WIDE } else { NARROW };
        let l = hpet_longest_ns(f, ppb, cfg);
        prop_assert!(hpet_ticks(f, ppb, l, cfg).is_some());
        if l < u64::MAX {
            prop_assert!(hpet_ticks(f, ppb, l + 1, cfg).is_none());
        }
        // A 32-bit comparator's longest arming reaches its width to within
        // one nanosecond's ticks: ticks(l + 1) <= ticks(l) + ticks(1)
        // (ceilings add at most that), and ticks(l + 1) + 1 is past it.
        if !wide {
            prop_assert!(
                ticks_at_least(f, ppb, l) + ticks_at_least(f, ppb, 1)
                    >= u128::from(hpet_max_ticks(cfg))
            );
        }
    }

    #[test]
    fn pit_count_is_ticks_at_least(
        f in pit_freq(),
        ppb in 0u64..2_000_000,
        ns in 0u64..200_000_000,
    ) {
        let want = ticks_at_least(f, ppb, ns).max(1);
        match pit_count(f, ppb, ns) {
            Some(n) => {
                prop_assert_eq!(u128::from(n), want);
                prop_assert!((1..=PIT_MAX_COUNT).contains(&n));
            }
            None => prop_assert!(want > u128::from(PIT_MAX_COUNT)),
        }
    }

    #[test]
    fn pit_longest_is_the_edge(f in pit_freq(), ppb in 0u64..2_000_000) {
        let l = pit_longest_ns(f, ppb);
        prop_assert_eq!(pit_count(f, ppb, l), Some(PIT_MAX_COUNT));
        prop_assert!(pit_count(f, ppb, l + 1).is_none());
        // About 54.9 ms, less the bound: within 1% plus the bound of it.
        prop_assert!(l > 52_000_000 && l < 56_000_000, "{}", l);
    }

    #[test]
    fn arm0_round_trips(count in 1u32..=PIT_MAX_COUNT) {
        let w = pit::arm0(count);
        prop_assert_eq!(w[0], (pit::COMMAND, pit::MODE0_CH0));
        prop_assert_eq!(w[1].0, pit::CHANNEL0);
        prop_assert_eq!(w[2].0, pit::CHANNEL0);
        let back = u32::from(u16::from_le_bytes([w[1].1, w[2].1]));
        // The 8254 reads a count of 0 as 65,536.
        prop_assert_eq!(if back == 0 { PIT_MAX_COUNT } else { back }, count);
    }

    /// Against a counter that moves up to `jump` ticks between any two
    /// reads: the comparator ends at least `ticks` past the read before
    /// its write, never past the width, and ahead of the read after it.
    /// It always succeeds when the doublings can outgrow the jumps.
    #[test]
    fn hpet_program_against_a_moving_counter(
        start in any::<u64>(),
        steps in proptest::collection::vec(0u64..1_000_000, 40),
        ticks in 1u64..2_000_000,
        step in 1u64..1_000,
        wide in any::<bool>(),
        enable in any::<bool>(),
    ) {
        let cfg = if wide { WIDE } else { NARROW };
        let mut reads = Vec::new();
        let mut now = start;
        for s in &steps {
            now = now.wrapping_add(*s);
            reads.push(now);
        }
        let mut c = fake(&reads);
        let r = hpet_program(&mut c, ticks, step, cfg, enable);
        let used = reads.len() - c.reads.len();
        prop_assert_eq!(c.enabled_after, enable.then_some(1));
        prop_assert_eq!(used, 2 * c.writes.len());
        match r {
            Ok(p) => {
                let before = reads[used - 2];
                let after = reads[used - 1];
                prop_assert_eq!(p.start, before);
                prop_assert_eq!(p.value, hpet::deadline(before, p.distance, cfg));
                prop_assert_eq!(*c.writes.last().unwrap(), p.value);
                prop_assert!(p.distance >= ticks);
                prop_assert!(p.distance <= hpet_max_ticks(cfg));
                prop_assert!(!hpet::passed(after, p.value, cfg));
                prop_assert_eq!(p.retries as usize, c.writes.len() - 1);
                // Each earlier write was passed by the read after it.
                for k in 0..c.writes.len() - 1 {
                    prop_assert!(hpet::passed(reads[2 * k + 1], c.writes[k], cfg));
                }
            }
            Err(_) => {
                prop_assert_eq!(c.writes.len(), MAX_RETRIES as usize + 1);
            }
        }
        // Every jump is under 10^6. After 16 retries a step of 16 or
        // more has added at least 16 * (2^16 - 1) = 1,048,560 ticks, more
        // than any jump, so it never gives up.
        if step >= 16 {
            prop_assert!(r.is_ok());
        }
    }
}
