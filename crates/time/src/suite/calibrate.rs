use proptest::prelude::*;

use super::{config, mul_wide};
use crate::calibrate::{
    Estimate, MAX_TOLERANCE_PPB, MAX_WINDOW, Refused, Sample, agrees, ceil_ppb, estimate, tightest,
};
use crate::freq::{CAL_DEN, Freq};
use crate::{pit, pm};

fn s(before: u64, target: u64, after: u64) -> Sample {
    Sample {
        before,
        target,
        after,
    }
}

fn hz(f: u64) -> Freq {
    Freq::hz(f).unwrap()
}

#[test]
fn hand_worked_3_ghz_against_100_mhz() {
    // The target is exactly 30 ticks per reference tick, read at reference
    // times 1001 and 1,001,001 with a reference read 1 tick either side.
    // dt = 30,030,030 - 30,030 = 30,000,000; sum = 2,002,002 - 2,002 =
    // 2,000,000, so F = 2 * 3e7 * 1e8 / 2e6 = 3 GHz.
    // min = 1,001,000 - 1,002 - 1 = 999,997 and max = 1,001,002 - 1,000 + 1
    // = 1,000,003, so hi / F - 1 = (3e7 + 1) / 29,999,910 - 1 =
    // 91 / 29,999,910 = 3033.34 ppb and 1 - lo / F = 91 / 30,000,090 =
    // 3033.32 ppb; both round up to 3034.
    let e = estimate(
        hz(100_000_000),
        s(1000, 30_030, 1002),
        s(1_001_000, 30_030_030, 1_001_002),
    )
    .unwrap();
    assert_eq!(
        e,
        Estimate {
            freq: hz(3_000_000_000),
            ppb: 3034,
            window: 1_000_000
        }
    );
}

#[test]
fn hand_worked_rounding_to_1024ths() {
    // Against the PM timer (39,375,000/11 Hz), a target that moved 7 ticks
    // in 11 reference ticks read exactly (before = after): sum = 22, so
    // F = 2 * 7 * (39,375,000/11) / 22 Hz, and in 1024ths of a hertz
    // N = round(2 * 7 * 39,375,000 * 1024 / (22 * 11)) =
    // round(564,480,000,000 / 242) = round(2,332,561,983.47).
    let e = estimate(pm::FREQ, s(100, 0, 100), s(111, 7, 111)).unwrap();
    assert_eq!(e.freq, Freq::new(2_332_561_983, CAL_DEN).unwrap());
    // min = 111 - 100 - 1 = 10, max = 12; hi = 8/10 * ref and lo = 6/12 *
    // ref, so the high side is the wider: (hi - F) / F =
    // (8 * 39,375,000 * 1024 - N * 10 * 11) / (10 * 11 * N) =
    // 65,978,181,870 / 256,581,818,130, about 25.7%.
    let n: u128 = 2_332_561_983;
    let up = 8 * 39_375_000 * 1024 - n * 110;
    let want = (up * 1_000_000_000).div_ceil(110 * n) as u64;
    assert_eq!(e.ppb, want);
    assert_eq!(want, 257_142_858);
}

#[test]
fn hand_worked_low_side_wider() {
    // Usually the high side is the wider one (min < max). Rounding F to
    // 1/1024 Hz can make the low side wider for a slow target: 5 ticks in
    // 1438 exact ticks of a 1 kHz reference. sum = 2876, so N =
    // round(2 * 5 * 1000 * 1024 / 2876) = round(3560.50...) = 3561, and
    // F = 3561/1024 Hz = 3.4775... Hz, above the midpoint 3.4770... Hz.
    // min = 1437, max = 1439:
    // (hi - F) / F = (6 * 1000 * 1024 - 3561 * 1437) / (1437 * 3561)
    //              = 1,026,843 / 5,117,157 = 0.20066669...
    // (F - lo) / F = (3561 * 1439 - 4 * 1000 * 1024) / (1439 * 3561)
    //              = 1,028,279 / 5,124,279 = 0.20066803..., the wider:
    //              200,668,036 ppb rounded up.
    let e = estimate(hz(1000), s(0, 0, 0), s(1438, 5, 1438)).unwrap();
    assert_eq!(e.freq, Freq::new(3561, CAL_DEN).unwrap());
    assert_eq!(e.ppb, 200_668_036);
    assert_eq!(e.window, 1438);
    // The same with a reference whose frequency is a fraction (the PM
    // timer, 39,375,000/11 Hz): 4 ticks in 2,000,000 exact ticks. N =
    // round(2 * 4 * 39,375,000 * 1024 / (4,000,000 * 11)) =
    // round(7330.909...) = 7331; min = 1,999,999 and max = 2,000,001;
    // (F - lo) / F = (7331 * 2,000,001 * 11 - 3 * 39,375,000 * 1024)
    //   / (2,000,001 * 11 * 7331) = 0.2500096757..., which is wider than
    // (hi - F) / F = 0.2499851...
    let e = estimate(pm::FREQ, s(0, 0, 0), s(2_000_000, 4, 2_000_000)).unwrap();
    assert_eq!(e.freq, Freq::new(7331, CAL_DEN).unwrap());
    assert_eq!(e.ppb, 250_009_676);
}

#[test]
fn refusals() {
    let r = hz(100_000_000);
    let ok_start = s(10, 100, 12);
    // A reference that went backwards inside a sample, or between them.
    assert_eq!(
        estimate(r, s(12, 100, 10), s(100, 200, 101)),
        Err(Refused::ReferenceBackwards)
    );
    assert_eq!(
        estimate(r, ok_start, s(101, 200, 100)),
        Err(Refused::ReferenceBackwards)
    );
    assert_eq!(
        estimate(r, ok_start, s(11, 200, 13)),
        Err(Refused::ReferenceBackwards)
    );
    // end.before == start.after is allowed by order but leaves no window.
    assert_eq!(
        estimate(r, ok_start, s(12, 200, 14)),
        Err(Refused::WindowTooShort)
    );
    assert_eq!(
        estimate(r, ok_start, s(13, 200, 14)),
        Err(Refused::WindowTooShort)
    );
    assert!(estimate(r, ok_start, s(14, 200, 14)).is_ok());
    // The target.
    assert_eq!(
        estimate(r, ok_start, s(100, 99, 101)),
        Err(Refused::TargetBackwards)
    );
    assert_eq!(
        estimate(r, ok_start, s(100, 100, 101)),
        Err(Refused::TargetStopped)
    );
    assert_eq!(
        estimate(r, ok_start, s(100, 101, 101)),
        Err(Refused::TargetStopped)
    );
    assert!(estimate(r, ok_start, s(100, 102, 101)).is_ok());
    // Windows: exactly MAX_WINDOW is fine on either counter.
    assert!(estimate(r, s(0, 0, 0), s(MAX_WINDOW, MAX_WINDOW, MAX_WINDOW)).is_ok());
    assert_eq!(
        estimate(r, s(0, 0, 0), s(MAX_WINDOW, MAX_WINDOW + 1, MAX_WINDOW)),
        Err(Refused::WindowTooLong)
    );
    assert_eq!(
        estimate(r, s(0, 0, 0), s(MAX_WINDOW, 5, MAX_WINDOW + 1)),
        Err(Refused::WindowTooLong)
    );
    // A result over 2^40 Hz: 2^40 target ticks in 2 reference ticks of
    // 1 MHz. And one under 1 Hz: 2 ticks in 2^40 ticks of the PIT.
    assert_eq!(
        estimate(hz(1_000_000), s(0, 0, 0), s(2, 1 << 40, 2)),
        Err(Refused::OutOfRange)
    );
    assert_eq!(
        estimate(pit::FREQ, s(0, 0, 0), s(1 << 40, 2, 1 << 40)),
        Err(Refused::OutOfRange)
    );
}

#[test]
fn tightest_hand_worked() {
    assert_eq!(tightest(&[]), None);
    assert_eq!(tightest(&[s(5, 0, 4)]), None);
    let a = s(0, 1, 5);
    let b = s(10, 11, 12);
    let c = s(20, 21, 22);
    assert_eq!(tightest(&[a, b, c]), Some(b));
    assert_eq!(tightest(&[a, s(9, 0, 1), c]), Some(c));
    assert_eq!(tightest(&[s(0, 0, 0), b]), Some(s(0, 0, 0)));
}

#[test]
fn ceil_ppb_hand_worked() {
    assert_eq!(ceil_ppb(0, 7), 0);
    assert_eq!(ceil_ppb(1, 1_000_000_000), 1);
    assert_eq!(ceil_ppb(1, 1_000_000_001), 1);
    assert_eq!(ceil_ppb(1, 3), 333_333_334);
    assert_eq!(ceil_ppb(5, 0), u64::MAX);
    assert_eq!(ceil_ppb(u128::MAX, 1), u64::MAX);
    // Big operands: 2^100 / 2^99 = 2 exactly, still.
    assert_eq!(ceil_ppb(1 << 100, 1 << 99), 2_000_000_000);
    // Halving rounds x up: (2^97 + 1) / 2^97 is just over 1, so 10^9 + 1.
    assert_eq!(ceil_ppb((1 << 97) + 1, 1 << 97), 1_000_000_001);
    assert_eq!(ceil_ppb(1 << 97, 1 << 97), 1_000_000_000);
    // y halves to 0 while x is huge.
    assert_eq!(ceil_ppb(u128::MAX, 1 << 20), u64::MAX);
}

#[test]
fn agrees_hand_worked() {
    let e = |f: u64, ppb| Estimate {
        freq: hz(f),
        ppb,
        window: 1,
    };
    // 1,000,001 Hz measured against 1 MHz is 1/1,000,001 * 10^9 = 999.999
    // ppb of the measurement.
    assert_eq!(agrees(e(1_000_001, 999), hz(1_000_000), 0), Err(999));
    assert_eq!(agrees(e(1_000_001, 1000), hz(1_000_000), 0), Ok(()));
    assert_eq!(agrees(e(1_000_001, 500), hz(1_000_000), 500), Ok(()));
    assert_eq!(agrees(e(999_999, 999), hz(1_000_000), 0), Err(-1000));
    assert_eq!(agrees(e(999_999, 1001), hz(1_000_000), 0), Ok(()));
    // 1/999,999 * 10^9 = 1000.001 ppb: 1000 is not enough.
    assert_eq!(agrees(e(999_999, 1000), hz(1_000_000), 0), Err(-1000));
    // Tolerance is capped at 2^31 ppb.
    assert_eq!(
        agrees(e(1_000_000, u64::MAX), hz(4_000_000), u64::MAX),
        Err(-3_000_000_000)
    );
    assert_eq!(
        agrees(e(1_000_000, MAX_TOLERANCE_PPB), hz(3_000_000), 0),
        Ok(())
    );
}

/// The model: two counters at exact frequencies, read at exact times (in
/// femtoseconds). A counter at f Hz started at time `phase` reads
/// floor((t + phase) * f / 10^15) at time t.
fn read(f: Freq, phase: u64, t: u64) -> u64 {
    let x = (t as u128 + phase as u128) * f.num() as u128;
    let d = f.den() as u128 * 1_000_000_000_000_000;
    // (t + phase) < 2^63 and num < 2^64: x < 2^127.
    (x / d) as u64
}

fn reference_freq() -> impl Strategy<Value = Freq> {
    prop_oneof![
        Just(pm::FREQ),
        Just(pit::FREQ),
        Just(Freq::from_period_fs(10_000_000).unwrap()),
        Just(Freq::from_period_fs(69_841_279).unwrap()),
        (1_000_000u64..=1_000_000_000).prop_map(|f| Freq::hz(f).unwrap()),
    ]
}

fn target_freq() -> impl Strategy<Value = Freq> {
    prop_oneof![
        (1_000_000u64..=10_000_000_000).prop_map(|f| Freq::hz(f).unwrap()),
        (1_000_000u64..=10_000_000_000, 1u64..=1 << 20)
            .prop_filter_map("in range", |(f, d)| Freq::new(f * d + d / 3, d)),
    ]
}

/// |T - F| * 10^9 <= ppb * F, exactly (256-bit products).
fn within(t: Freq, e: Estimate) -> bool {
    let (tn, td) = (t.num() as u128, t.den() as u128);
    let (fn_, fd) = (e.freq.num() as u128, e.freq.den() as u128);
    // |tn fd - fn td| * 10^9 <= ppb * fn td
    let diff = (tn * fd).abs_diff(fn_ * td);
    let lhs = mul_wide(diff, 1_000_000_000);
    let rhs = mul_wide(e.ppb as u128, fn_ * td);
    lhs <= rhs
}

proptest! {
    #![proptest_config(config(3000))]

    /// The true frequency is always within the bound, whatever the jitter
    /// between reads, the phases of the two counters and the frequencies.
    #[test]
    fn the_bound_holds(
        r in reference_freq(),
        t in target_freq(),
        r_phase in 0u64..1 << 62,
        t_phase in 0u64..1 << 62,
        start in 0u64..1 << 50,
        // 1 ms to 1 s.
        window in 1_000_000_000_000u64..1_000_000_000_000_000,
        // Up to 20 us between each read and the next.
        j in proptest::collection::vec(0u64..20_000_000_000, 4),
    ) {
        let t0 = start + j[0];
        let t1 = t0 + window;
        let s0 = Sample {
            before: read(r, r_phase, start),
            target: read(t, t_phase, t0),
            after: read(r, r_phase, t0 + j[1]),
        };
        let s1 = Sample {
            before: read(r, r_phase, t1 - j[2]),
            target: read(t, t_phase, t1),
            after: read(r, r_phase, t1 + j[3]),
        };
        let e = estimate(r, s0, s1).unwrap();
        prop_assert!(within(t, e), "true {:?} estimate {:?}", t, e);
        // And agrees() says so too, with no slack.
        prop_assert_eq!(agrees(e, t, 0), Ok(()));
    }

    /// With exact reads (no jitter) and a long window, the bound is about
    /// one tick of either counter over the window.
    #[test]
    fn exact_reads_give_a_tight_bound(
        r in reference_freq(),
        t in target_freq(),
        r_phase in 0u64..1 << 62,
        t_phase in 0u64..1 << 62,
    ) {
        let window = 100_000_000_000_000u64; // 100 ms
        let s0 = Sample { before: read(r, r_phase, 0), target: read(t, t_phase, 0), after: read(r, r_phase, 0) };
        let s1 = Sample { before: read(r, r_phase, window), target: read(t, t_phase, window), after: read(r, r_phase, window) };
        let e = estimate(r, s0, s1).unwrap();
        // One reference tick in 100 ms is 10^9 / (0.1 * r) ppb; at most
        // 2 ticks of each counter (and 1 for rounding) come into it.
        let ticks_r = r.hz_rounded() / 10;
        let ticks_t = t.hz_rounded() / 10;
        let limit = 3 * (1_000_000_000 / ticks_r + 1_000_000_000 / ticks_t) + 1;
        prop_assert!(e.ppb <= limit, "{} > {}", e.ppb, limit);
    }

    #[test]
    fn ceil_ppb_is_an_upper_bound(x: u128, y in 1u128..) {
        let got = ceil_ppb(x, y);
        // got * y >= x * 10^9, exactly.
        prop_assert!(mul_wide(got as u128, y) >= mul_wide(x, 1_000_000_000));
        // And exact when nothing had to be halved.
        if x < 1 << 96 && y < 1 << 96 {
            let want = (x * 1_000_000_000).div_ceil(y);
            prop_assert_eq!(got as u128, want.min(u64::MAX as u128));
        }
    }

    /// estimate never panics, whatever it is given.
    #[test]
    fn estimate_never_panics(
        r in reference_freq(),
        a: (u64, u64, u64),
        b: (u64, u64, u64),
    ) {
        let _ = estimate(r, s(a.0, a.1, a.2), s(b.0, b.1, b.2));
    }
}
