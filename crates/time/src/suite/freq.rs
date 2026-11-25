use proptest::prelude::*;

use super::config;
use crate::freq::{CAL_DEN, Freq, MAX_DEN, MAX_HZ, Scale};
use crate::{pit, pm};

#[test]
fn new_reduces_and_checks_range() {
    // 300/3 = 100/1.
    assert_eq!(Freq::new(300, 3), Freq::hz(100));
    assert_eq!(Freq::new(300, 3).unwrap().num(), 100);
    assert_eq!(Freq::new(300, 3).unwrap().den(), 1);
    assert_eq!(Freq::new(0, 1), None);
    assert_eq!(Freq::new(1, 0), None);
    // Below 1 Hz: 1/2. Exactly 1 Hz is fine.
    assert_eq!(Freq::new(1, 2), None);
    assert!(Freq::new(7, 7).is_some());
    // 2^40 Hz is the most; one 2^32th of a hertz more is too much.
    assert!(Freq::hz(MAX_HZ).is_some());
    assert_eq!(Freq::hz(MAX_HZ + 1), None);
    assert_eq!(Freq::new((MAX_HZ << 20) + 1, 1 << 20), None);
    assert!(Freq::new((MAX_HZ << 20) - 1, 1 << 20).is_some());
    // With the largest denominator, num (a u64) limits it to under 2^32 Hz.
    assert!(Freq::new(u64::MAX, MAX_DEN).is_some());
    // A denominator over 2^32 that does not reduce is refused; one that
    // reduces below it is fine.
    assert_eq!(Freq::new((MAX_DEN + 1) * 3, MAX_DEN + 1), Freq::hz(3));
    assert_eq!(Freq::new((MAX_DEN + 1) * 3 + 1, MAX_DEN + 1), None);
    assert!(Freq::new(MAX_DEN * 3 + 1, MAX_DEN).is_some());
}

#[test]
fn hpet_periods_hand_worked() {
    // QEMU: 10^7 fs is exactly 100 MHz.
    assert_eq!(Freq::from_period_fs(10_000_000), Freq::hz(100_000_000));
    // Intel ICH: 0x0429B17F = 69,841,279 fs is 10^15 / 69,841,279 Hz =
    // 14,318,179.9405... Hz, which has no common factor to reduce.
    let f = Freq::from_period_fs(69_841_279).unwrap();
    assert_eq!((f.num(), f.den()), (1_000_000_000_000_000, 69_841_279));
    assert_eq!(f.hz_rounded(), 14_318_180);
    assert_eq!(f.millihertz(), 14_318_179_941);
    assert_eq!(Freq::from_period_fs(0), None);
}

#[test]
fn colour_burst_clocks_hand_worked() {
    // PM timer: 315/88 MHz = 39,375,000/11 Hz = 3,579,545.4545... Hz.
    assert_eq!((pm::FREQ.num(), pm::FREQ.den()), (39_375_000, 11));
    assert_eq!(pm::FREQ.hz_rounded(), 3_579_545);
    // 3,579,545,454.545... mHz rounds up.
    assert_eq!(pm::FREQ.millihertz(), 3_579_545_455);
    // PIT: 315/264 MHz = 13,125,000/11 Hz = 1,193,181.8181... Hz.
    assert_eq!((pit::FREQ.num(), pit::FREQ.den()), (13_125_000, 11));
    assert_eq!(pit::FREQ.hz_rounded(), 1_193_182);
    assert_eq!(pit::FREQ.millihertz(), 1_193_181_818);
    // The PM timer is exactly three times the PIT.
    assert_eq!(pm::FREQ.num(), 3 * pit::FREQ.num());
}

#[test]
fn rounding_halves_up() {
    // 5/2 Hz = 2.5 Hz rounds to 3; 2.5 mHz-steps: 1/1024 Hz = 0.9765625 mHz -> 1.
    assert_eq!(Freq::new(5, 2).unwrap().hz_rounded(), 3);
    assert_eq!(Freq::new(9, 4).unwrap().hz_rounded(), 2);
    assert_eq!(Freq::new(1025, CAL_DEN).unwrap().millihertz(), 1001);
    // 2049/2048 Hz = 1000.48828125 mHz -> 1000.
    assert_eq!(Freq::new(2049, 2048).unwrap().millihertz(), 1000);
    assert_eq!(Freq::hz(MAX_HZ).unwrap().millihertz(), MAX_HZ * 1000);
}

#[test]
fn conversions_hand_worked() {
    let f = Freq::hz(100_000_000).unwrap();
    assert_eq!(f.ticks_to_ns(1), 10);
    assert_eq!(f.ticks_to_ns(u64::MAX), u64::MAX);
    // 1 ns and 10 ns take one 10 ns tick; 11 ns take two. 0 takes none.
    assert_eq!(f.ns_to_ticks_ceil(0), 0);
    assert_eq!(f.ns_to_ticks_ceil(1), 1);
    assert_eq!(f.ns_to_ticks_ceil(10), 1);
    assert_eq!(f.ns_to_ticks_ceil(11), 2);
    // PM timer: 39,375,000 ticks are exactly 11 s; one tick is
    // 279.365... ns.
    assert_eq!(pm::FREQ.ticks_to_ns(39_375_000), 11_000_000_000);
    assert_eq!(pm::FREQ.ticks_to_ns(1), 279);
    assert_eq!(pm::FREQ.ns_to_ticks_ceil(11_000_000_000), 39_375_000);
    assert_eq!(pm::FREQ.ns_to_ticks_ceil(280), 2);
    assert_eq!(pm::FREQ.ns_to_ticks_ceil(279), 1);
    // A 1 Hz clock: u64::MAX ns is 18,446,744,073.7 seconds, so
    // 18,446,744,074 ticks rounded up.
    let one = Freq::hz(1).unwrap();
    assert_eq!(one.ns_to_ticks_ceil(u64::MAX), 18_446_744_074);
    // The other way saturates.
    assert_eq!(one.ticks_to_ns(18_446_744_074), u64::MAX);
    assert_eq!(one.ticks_to_ns(18_446_744_073), 18_446_744_073_000_000_000);
    // The fastest clock: one tick of 2^40 Hz is under 1 ns, and
    // u64::MAX ns is more ticks than fit.
    let max = Freq::hz(MAX_HZ).unwrap();
    assert_eq!(max.ticks_to_ns(1), 0);
    assert_eq!(max.ns_to_ticks_ceil(u64::MAX), u64::MAX);
}

#[test]
fn ppb_hand_worked() {
    let a = Freq::hz(100_000_000).unwrap();
    let b = Freq::hz(100_000_100).unwrap();
    // 100 Hz faster on 100 MHz is +1 ppm = +1000 ppb.
    assert_eq!(a.ppb_from(b), 1000);
    // From b, a is 100/100,000,100 * 10^9 = 999.999 ppb slower, toward 0.
    assert_eq!(b.ppb_from(a), -999);
    assert_eq!(a.ppb_from(a), 0);
    // The extremes clamp.
    let lo = Freq::hz(1).unwrap();
    let hi = Freq::hz(MAX_HZ).unwrap();
    assert_eq!(lo.ppb_from(hi), i64::MAX);
    // 1 Hz against 2^40 Hz is -(1 - 2^-40) * 10^9, toward 0: -999,999,999.
    assert_eq!(hi.ppb_from(lo), -999_999_999);
}

#[test]
fn scale_hand_worked() {
    // 100 MHz: exactly 10 ns a tick. 10 * 2^60 < 2^64 <= 10 * 2^61.
    let s = Freq::hz(100_000_000).unwrap().scale();
    assert_eq!(
        s,
        Scale {
            mult: 10 << 60,
            shift: 60
        }
    );
    assert_eq!(s.ticks_to_ns(123), 1230);
    // 1 Hz: 10^9 * 2^34 < 2^64 <= 10^9 * 2^35.
    let s = Freq::hz(1).unwrap().scale();
    assert_eq!(
        s,
        Scale {
            mult: 1_000_000_000 << 34,
            shift: 34
        }
    );
    // PM timer: 11 * 10^9 / 39,375,000 * 2^55 = 10,065,187,738,631,203,759.2...
    let s = pm::FREQ.scale();
    assert_eq!(
        s,
        Scale {
            mult: 10_065_187_738_631_203_759,
            shift: 55
        }
    );
    assert_eq!(s.ticks_to_ns(39_375_000), 11_000_000_000);
    assert_eq!(s.ticks_to_ns(u64::MAX), u64::MAX);
}

/// A random valid frequency: from the clocks the kernel meets, or any.
fn any_freq() -> impl Strategy<Value = Freq> {
    prop_oneof![
        Just(pm::FREQ),
        Just(pit::FREQ),
        (910u64..=100_000_000).prop_map(|fs| Freq::from_period_fs(fs).unwrap()),
        (1_000_000u64..10_000_000_000 * CAL_DEN).prop_map(|n| Freq::new(n, CAL_DEN).unwrap()),
        (1u64..=MAX_DEN, any::<u64>()).prop_filter_map("in range", |(d, n)| Freq::new(n, d)),
    ]
}

/// Exact floor(t * 10^9 * den / num), unsaturated (it fits in a u128).
fn exact_ns(f: Freq, t: u64) -> u128 {
    t as u128 * 1_000_000_000 * f.den() as u128 / f.num() as u128
}

proptest! {
    #![proptest_config(config(2000))]

    #[test]
    fn new_is_lowest_terms(n in 1u64..1 << 40, d in 1u64..1 << 20, k in 1u64..1 << 20) {
        let a = Freq::new(n, d);
        prop_assert_eq!(a, Freq::new(n * k, d * k));
        if let Some(f) = a {
            prop_assert_eq!(crate::freq::Freq::new(f.num(), f.den()), Some(f));
            let mut g = (f.num(), f.den());
            while g.1 != 0 { g = (g.1, g.0 % g.1); }
            prop_assert_eq!(g.0, 1);
        }
    }

    #[test]
    fn exact_conversions_are_exact(f in any_freq(), t: u64, ns: u64) {
        let e = exact_ns(f, t);
        prop_assert_eq!(f.ticks_to_ns(t) as u128, e.min(u64::MAX as u128));
        // ns_to_ticks_ceil: the fewest ticks that take at least ns.
        let k = f.ns_to_ticks_ceil(ns) as u128;
        let need = ns as u128 * f.num() as u128;
        let per = 1_000_000_000u128 * f.den() as u128;
        if k < u64::MAX as u128 {
            prop_assert!(k * per >= need);
            prop_assert!(k == 0 || (k - 1) * per < need);
        } else {
            prop_assert!((u64::MAX as u128 - 1) * per < need);
        }
    }

    #[test]
    fn scale_is_within_one_ns(f in any_freq(), t: u64) {
        let s = f.scale();
        prop_assert!(s.mult >= (1 << 63) - 1, "mult {} shift {}", s.mult, s.shift);
        let e = exact_ns(f, t);
        let got = s.ticks_to_ns(t) as u128;
        if e < u64::MAX as u128 - 3 {
            prop_assert!(got.abs_diff(e) <= 1, "{} vs exact {}", got, e);
        } else {
            prop_assert!(got >= u64::MAX as u128 - 5);
        }
    }

    #[test]
    fn scale_is_monotonic(f in any_freq(), a: u64, b: u64) {
        let s = f.scale();
        let (lo, hi) = (a.min(b), a.max(b));
        prop_assert!(s.ticks_to_ns(lo) <= s.ticks_to_ns(hi));
        prop_assert!(s.ticks_to_ns(lo) <= s.ticks_to_ns(lo.saturating_add(1)));
    }

    #[test]
    fn ppb_sign_and_size(a in any_freq(), b in any_freq()) {
        let p = a.ppb_from(b);
        // Sign: positive exactly when b is faster.
        let x = b.num() as u128 * a.den() as u128;
        let y = a.num() as u128 * b.den() as u128;
        prop_assert_eq!(p > 0, x > y && (x - y) * 1_000_000_000 >= y);
        prop_assert_eq!(p == 0, x.abs_diff(y) * 1_000_000_000 < y);
        // |p| is the truncation of |x - y| / y * 10^9 (when not clamped).
        if p != i64::MAX && p != i64::MIN {
            let m = p.unsigned_abs() as u128;
            let d = x.abs_diff(y) * 1_000_000_000;
            prop_assert!(m * y <= d && d < (m + 1) * y);
        }
    }
}
