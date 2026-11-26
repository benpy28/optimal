use proptest::prelude::*;

use super::{config, mul_wide};
use crate::freq::Freq;
use crate::lapic::*;

const G: u128 = 1_000_000_000;

fn hz(f: u64) -> Freq {
    Freq::hz(f).unwrap()
}

#[test]
fn dcr_matches_the_sdm_table() {
    // SDM Vol. 3A Figure 11-10, bits 3, 1, 0: 000 = 2, 001 = 4, 010 = 8,
    // 011 = 16, 100 = 32, 101 = 64, 110 = 128, 111 = 1.
    let table = [
        0b1011, 0b0000, 0b0001, 0b0010, 0b0011, 0b1000, 0b1001, 0b1010,
    ];
    for (log2, &want) in table.iter().enumerate() {
        assert_eq!(dcr(log2 as u32), want, "divide by {}", 1 << log2);
        assert_eq!(dcr_log2(want), log2 as u32);
        // Bit 2 is reserved and bits 4 up are not part of the divider.
        assert_eq!(dcr_log2(want | 0b100 | 0xFFFF_FFF0), log2 as u32);
    }
}

#[test]
#[should_panic]
fn dcr_past_128_is_a_bug() {
    let _ = dcr(8);
}

#[test]
fn lvt_hand_worked() {
    assert_eq!(lvt(0xF0, Mode::OneShot, false), 0x0_00F0);
    assert_eq!(lvt(0xF0, Mode::OneShot, true), 0x1_00F0);
    assert_eq!(lvt(0xF0, Mode::Periodic, false), 0x2_00F0);
    assert_eq!(lvt(0xF0, Mode::Periodic, true), 0x3_00F0);
    assert_eq!(lvt(0xF0, Mode::TscDeadline, false), 0x4_00F0);
    assert_eq!(lvt(0x31, Mode::TscDeadline, true), 0x5_0031);
    assert_eq!(lvt_mode(0x0_00F0), Some(Mode::OneShot));
    assert_eq!(lvt_mode(0x3_00F0), Some(Mode::Periodic));
    assert_eq!(lvt_mode(0x4_00F0), Some(Mode::TscDeadline));
    // 0b11 is reserved.
    assert_eq!(lvt_mode(0x6_00F0), None);
    // Other bits do not change the mode.
    assert_eq!(lvt_mode(0xFFF9_FFFF), Some(Mode::OneShot));
}

#[test]
fn ticks_at_least_hand_worked() {
    // 1 GHz: one tick a nanosecond.
    assert_eq!(ticks_at_least(hz(1_000_000_000), 0, 1000), 1000);
    assert_eq!(ticks_at_least(hz(1_000_000_000), 0, 0), 0);
    // Within 1 ppm, for a second: 10^9 * 1.000001 = 1,000,001,000.
    assert_eq!(
        ticks_at_least(hz(1_000_000_000), 1000, 1_000_000_000),
        1_000_001_000
    );
    // 315/88 MHz for a second: 3,579,545.45... rounds up to 3,579,546.
    let pm = Freq::new(315_000_000, 88).unwrap();
    assert_eq!(ticks_at_least(pm, 0, 1_000_000_000), 3_579_546);
    // One nanosecond at 3.58 MHz is under a tick: one whole tick.
    assert_eq!(ticks_at_least(pm, 0, 1), 1);
    // 1 ppb on 3 ticks: ceil(3 * 1.000000001) = 4.
    assert_eq!(ticks_at_least(hz(1_000_000_000), 1, 3), 4);
    // A bound past 100% is taken as 100%: twice the ticks.
    assert_eq!(ticks_at_least(hz(1_000_000_000), u64::MAX, 500), 1000);
    assert_eq!(ticks_at_least(hz(1_000_000_000), 1_000_000_000, 500), 1000);
    // The largest inputs do not overflow: 2^40 Hz for u64::MAX ns at
    // 100% is 2 * (2^64 - 1) * 2^40 / 10^9 ticks, rounded up:
    // 40564819207303340845696 (worked with exact integers).
    let f = Freq::hz(1 << 40).unwrap();
    assert_eq!(
        ticks_at_least(f, u64::MAX, u64::MAX),
        40_564_819_207_303_340_845_696
    );
    // A fraction that does not divide: 7/3 Hz for 10^9 ns at 0 ppb is
    // 7/3 ticks, rounded up to 3; at 50% it is 3.5, rounded up to 4.
    let f = Freq::new(7, 3).unwrap();
    assert_eq!(ticks_at_least(f, 0, 1_000_000_000), 3);
    assert_eq!(ticks_at_least(f, 500_000_000, 1_000_000_000), 4);
}

#[test]
fn arm_hand_worked() {
    let g = hz(1_000_000_000);
    // 0 ns still waits a whole divided tick: count 2, never 0 or 1.
    assert_eq!(
        arm(g, 0, 0),
        Some(Arm {
            divide_log2: 0,
            count: 2
        })
    );
    assert_eq!(
        arm(g, 0, 1),
        Some(Arm {
            divide_log2: 0,
            count: 2
        })
    );
    // 1 ms at 1 GHz: 10^6 ticks plus one for the divider's phase.
    assert_eq!(
        arm(g, 0, 1_000_000),
        Some(Arm {
            divide_log2: 0,
            count: 1_000_001
        })
    );
    // 10 s at 1 GHz is 10^10 ticks: /1 and /2 do not fit in 32 bits
    // (5 * 10^9 + 1 > 2^32 - 1), /4 does: 2.5 * 10^9 + 1.
    let a = arm(g, 0, 10_000_000_000).unwrap();
    assert_eq!(
        a,
        Arm {
            divide_log2: 2,
            count: 2_500_000_001
        }
    );
    assert_eq!(a.dcr(), 0b0001);
    assert_eq!(arm(g, 0, 1).unwrap().dcr(), 0b1011);
    assert_eq!(a.min_ticks(), 10_000_000_000);
    // The edge: 128 * (2^32 - 2) ticks is the most one arming covers.
    let edge = 128 * (u64::from(u32::MAX) - 1);
    assert_eq!(
        arm(g, 0, edge),
        Some(Arm {
            divide_log2: 7,
            count: u32::MAX
        })
    );
    assert_eq!(arm(g, 0, edge + 1), None);
    assert_eq!(longest_ns(g, 0), edge);
    // With a 100% bound it is half as long.
    assert_eq!(longest_ns(g, 1_000_000_000), edge / 2);
    // A slow timer covers every u64 of nanoseconds: 1 Hz for 2^64 ns is
    // 1.8 * 10^10 ticks, /128 is 1.4 * 10^8.
    assert_eq!(longest_ns(hz(1), 0), u64::MAX);
    assert!(arm(hz(1), 0, u64::MAX).is_some());
}

#[test]
fn tsc_deadline_hand_worked() {
    let f = hz(2_000_000_000);
    assert_eq!(tsc_deadline(100, f, 0, 1000), 2100);
    // 1 ppm on 2000 ticks is 0.002 of a tick: rounds up to one more.
    assert_eq!(tsc_deadline(100, f, 1000, 1000), 2101);
    assert_eq!(tsc_deadline(u64::MAX - 5, f, 0, 1000), u64::MAX);
    assert_eq!(tsc_deadline(5, f, 0, u64::MAX), u64::MAX);
    // 0 would disarm the timer.
    assert_eq!(tsc_deadline(0, f, 0, 0), 1);
    assert_eq!(tsc_deadline(7, f, 0, 0), 7);
}

#[test]
fn elapsed_hand_worked() {
    assert_eq!(elapsed(100, 40), Some(60));
    assert_eq!(elapsed(100, 100), Some(0));
    assert_eq!(elapsed(100, 1), Some(99));
    // Stopped, or a reading the arming cannot have given.
    assert_eq!(elapsed(100, 0), None);
    assert_eq!(elapsed(100, 101), None);
    assert_eq!(elapsed(u32::MAX, u32::MAX), Some(0));
    assert_eq!(elapsed(u32::MAX, 1), Some(u64::from(u32::MAX) - 1));
}

#[test]
fn compose_hand_worked() {
    assert_eq!(compose_ppb(0, 0), 0);
    assert_eq!(compose_ppb(5, 0), 5);
    assert_eq!(compose_ppb(0, 5), 5);
    // 1 ppm and 1 ppm: 2000 + ceil(10^6 / 10^9) = 2001.
    assert_eq!(compose_ppb(1000, 1000), 2001);
    // 100% and 100%: (1 + 1)(1 + 1) - 1 = 3.
    assert_eq!(compose_ppb(1_000_000_000, 1_000_000_000), 3_000_000_000);
    assert_eq!(compose_ppb(u64::MAX, 1), u64::MAX);
    assert_eq!(compose_ppb(u64::MAX, u64::MAX), u64::MAX);
}

fn le(a: (u128, u128), b: (u128, u128)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

/// x = ns * f * (1 + ppb / 10^9), in ticks, as the exact fraction
/// num / den: num = ns * fnum * (10^9 + ppb), den = 10^9 * fden * 10^9.
/// Is `t` >= x?
fn at_least(t: u128, f: Freq, ppb: u64, ns: u64) -> bool {
    let ppb = u128::from(ppb.min(1_000_000_000));
    let num = mul_wide(u128::from(ns) * u128::from(f.num()), G + ppb);
    let den = G * u128::from(f.den()) * G;
    le(num, mul_wide(t, den))
}

fn timer_freq() -> impl Strategy<Value = Freq> {
    // Any LAPIC bus or crystal clock, and any TSC: 1 MHz to 10 GHz, with
    // denominators like a calibration's.
    (
        1_000_000u64..10_000_000_000,
        prop_oneof![Just(1u64), Just(1024u64), 1u64..=1 << 32],
    )
        .prop_filter_map("in range", |(hz, den)| {
            Freq::new(hz.saturating_mul(den), den)
        })
}

proptest! {
    #![proptest_config(config(5000))]

    #[test]
    fn dcr_round_trips(log2 in 0u32..=7, junk: u32) {
        let v = dcr(log2);
        prop_assert_eq!(v & !0b1011, 0);
        prop_assert_eq!(dcr_log2(v), log2);
        prop_assert_eq!(dcr_log2((v & 0b1011) | (junk & !0b1011)), log2);
    }

    #[test]
    fn lvt_round_trips(vector: u8, mode in prop_oneof![
        Just(Mode::OneShot), Just(Mode::Periodic), Just(Mode::TscDeadline)
    ], masked: bool) {
        let v = lvt(vector, mode, masked);
        prop_assert_eq!(v as u8, vector);
        prop_assert_eq!(lvt_mode(v), Some(mode));
        prop_assert_eq!(v & LVT_MASKED != 0, masked);
        // Nothing outside the vector, mask and mode bits.
        prop_assert_eq!(v & !(0xFF | LVT_MASKED | 0b11 << 17), 0);
    }

    #[test]
    fn ticks_at_least_is_the_exact_ceiling(
        f in timer_freq(), ppb in prop_oneof![Just(0u64), 0u64..100_000_000, any::<u64>()], ns: u64,
    ) {
        let t = ticks_at_least(f, ppb, ns);
        prop_assert!(at_least(t, f, ppb, ns), "{t} is under the exact value");
        // And it is the ceiling: one fewer is under x.
        if t >= 1 {
            prop_assert!(!at_least(t - 1, f, ppb, ns), "{t} is past the ceiling");
        }
    }

    #[test]
    fn arm_never_fires_early_and_uses_the_finest_divider(
        f in timer_freq(), ppb in 0u64..100_000_000, ns in prop_oneof![0u64..10_000_000_000, any::<u64>()],
    ) {
        let t = ticks_at_least(f, ppb, ns);
        match arm(f, ppb, ns) {
            Some(a) => {
                prop_assert!(a.count >= 2);
                prop_assert!(a.divide_log2 <= MAX_DIVIDE_LOG2);
                // The guaranteed undivided ticks cover the exact need at
                // the fastest the clock can be.
                prop_assert!(at_least(u128::from(a.min_ticks()), f, ppb, ns));
                prop_assert!(u128::from(a.min_ticks()) >= t);
                // A finer divider would not fit.
                if a.divide_log2 > 0 {
                    let finer = t.div_ceil(1 << (a.divide_log2 - 1)) + 1;
                    prop_assert!(finer > u128::from(u32::MAX));
                }
                // And it is no longer than it needs to be: one divided
                // tick of slack, plus one for the phase.
                prop_assert!(u128::from(a.count) <= t.div_ceil(1 << a.divide_log2) + 1 || a.count == 2);
            }
            None => prop_assert!(t.div_ceil(128) + 1 > u128::from(u32::MAX)),
        }
    }

    #[test]
    fn longest_ns_is_the_edge(f in timer_freq(), ppb in 0u64..1_000_000_000) {
        let l = longest_ns(f, ppb);
        prop_assert!(arm(f, ppb, l).is_some());
        if l < u64::MAX {
            prop_assert!(arm(f, ppb, l + 1).is_none());
        }
        // At least a second for any clock up to 10 GHz.
        prop_assert!(l >= 1_000_000_000);
    }

    #[test]
    fn tsc_deadline_never_fires_early(
        now: u64, f in timer_freq(), ppb in 0u64..100_000_000, ns in 0u64..1 << 50,
    ) {
        let d = tsc_deadline(now, f, ppb, ns);
        prop_assert!(d >= 1 && d >= now);
        if d < u64::MAX {
            prop_assert!(at_least(u128::from(d - now), f, ppb, ns));
        }
    }

    #[test]
    fn elapsed_counts_up(initial: u32, current: u32) {
        match elapsed(initial, current) {
            Some(e) => {
                prop_assert!(current != 0 && current <= initial);
                prop_assert_eq!(e + u64::from(current), u64::from(initial));
            }
            None => prop_assert!(current == 0 || current > initial),
        }
    }

    #[test]
    fn compose_is_the_product_bound(a in 0u64..1 << 40, b in 0u64..1 << 40) {
        let c = compose_ppb(a, b);
        prop_assert_eq!(c, compose_ppb(b, a));
        // c >= a + b + ab / 10^9 exactly, and is the ceiling of it.
        let exact_num = (u128::from(a) + u128::from(b)) * G + u128::from(a) * u128::from(b);
        prop_assert!(u128::from(c) * G >= exact_num);
        prop_assert!((u128::from(c) - 1) * G < exact_num || c == 0);
    }
}
