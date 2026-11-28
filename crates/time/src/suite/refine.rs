use proptest::prelude::*;

use super::{config, mul_wide};
use crate::calibrate::{Estimate, MAX_WINDOW, agrees};
use crate::counter::Extend;
use crate::freq::Freq;
use crate::refine::{
    Boot, Outcome, WINDOW_NS, consistent, decide, expected, poll_ns, rescale, unwrap,
};
use crate::{NS_PER_SEC, pm};

fn hz(f: u64) -> Freq {
    Freq::hz(f).unwrap()
}

fn est(freq: Freq, ppb: u64) -> Estimate {
    Estimate {
        freq,
        ppb,
        window: 0,
    }
}

#[test]
fn window_is_ten_seconds() {
    assert_eq!(WINDOW_NS, 10 * NS_PER_SEC);
}

#[test]
fn poll_hand_worked() {
    // A 24-bit PM timer: 2^21 ticks at 315/88 MHz is
    // 2097152 * 88 * 10^9 / (315 * 10^6) = 184549376000 / 315
    // = 585871034.9 ns, rounded down.
    assert_eq!(poll_ns(pm::FREQ, 24), 585_871_034);
    // A 32-bit PM timer: 2^29 ticks, 256 times as long:
    // 536870912 * 88000 / 315 = 149982984939.7 ns.
    assert_eq!(poll_ns(pm::FREQ, 32), 149_982_984_939);
    // A 32-bit HPET at 100 MHz: 2^29 ticks of 10 ns.
    assert_eq!(poll_ns(hz(100_000_000), 32), 5_368_709_120);
    // 4 bits at 1 Hz: 2 ticks, 2 s.
    assert_eq!(poll_ns(hz(1), 4), 2 * NS_PER_SEC);
    // 64 bits never wraps; under 4 bits has no eighth.
    assert_eq!(poll_ns(hz(100_000_000), 64), u64::MAX);
    assert_eq!(poll_ns(hz(1), 3), u64::MAX);
}

#[test]
fn expected_hand_worked() {
    // 10^9 ticks of a 1 GHz TSC is 10^8 ticks of a 100 MHz HPET: from
    // half to twice that.
    assert_eq!(
        expected(1_000_000_000, hz(1_000_000_000), hz(100_000_000)),
        Some((50_000_000, 200_000_000))
    );
    // 7 ticks at 2 Hz is 3.5 ticks at 1 Hz: lo = floor(1.75) = 1,
    // hi = ceil(7) = 7.
    assert_eq!(expected(7, hz(2), hz(1)), Some((1, 7)));
    // The PM timer against a 3 GHz TSC over 1 s: 3579545.45 ticks, so
    // floor(1789772.7) and ceil(7159090.9).
    assert_eq!(
        expected(3_000_000_000, hz(3_000_000_000), pm::FREQ),
        Some((1_789_772, 7_159_091))
    );
    assert_eq!(expected(0, hz(2), hz(1)), Some((0, 0)));
    // Past MAX_WINDOW: refused.
    assert_eq!(expected(MAX_WINDOW + 1, hz(2), hz(1)), None);
    assert!(expected(MAX_WINDOW, hz(2), hz(1)).is_some());
}

#[test]
fn unwrap_hand_worked() {
    // 4 bits (wraps at 16), raw difference 3.
    // In 10..=25 only 19 fits (3 is too small, 35 too big).
    assert_eq!(unwrap(3, 4, 10, 25), Some(19));
    // In 2..=20 both 3 and 19 fit: unknown.
    assert_eq!(unwrap(3, 4, 2, 20), None);
    // In 4..=18 none fits.
    assert_eq!(unwrap(3, 4, 4, 18), None);
    // The edges: 0 and 16 both fit 0..=16, only 0 fits 0..=15, only 16
    // fits 1..=16.
    assert_eq!(unwrap(0, 4, 0, 16), None);
    assert_eq!(unwrap(0, 4, 0, 15), Some(0));
    assert_eq!(unwrap(0, 4, 1, 16), Some(16));
    // The raw difference is taken modulo 2^bits: 19 is 3.
    assert_eq!(unwrap(19, 4, 10, 25), Some(19));
    // A range of one count: 19 fits 19..=19, 18 does not.
    assert_eq!(unwrap(3, 4, 19, 19), Some(19));
    assert_eq!(unwrap(3, 4, 18, 18), None);
    // Several wraps: 3 + 5 * 16 = 83 is the one in 80..=90.
    assert_eq!(unwrap(3, 4, 80, 90), Some(83));
    // 64 bits: the difference itself, if it is in range.
    assert_eq!(unwrap(5, 64, 0, 10), Some(5));
    assert_eq!(unwrap(5, 64, 6, 10), None);
    assert_eq!(unwrap(u64::MAX, 64, 0, u64::MAX), Some(u64::MAX));
    // An empty range.
    assert_eq!(unwrap(5, 8, 10, 9), None);
    // A 24-bit PM timer (wraps at 16777216) read an eighth of a wrap
    // apart, with the TSC saying half to twice that: one answer.
    let raw = 2_097_152;
    assert_eq!(unwrap(raw, 24, 1_048_576, 4_194_304), Some(2_097_152));
    // The same raw difference with the vCPU stopped for about 12 s in
    // between: the TSC says 21480000 to 85920000 ticks, and 2097152 plus
    // 2, 3 or 4 wraps (35651584, 52428800, 69206016) all fit, so it
    // refuses.
    assert_eq!(unwrap(raw, 24, 21_480_000, 85_920_000), None);
}

#[test]
#[should_panic]
fn unwrap_rejects_zero_bits() {
    let _ = unwrap(0, 0, 0, 1);
}

#[test]
#[should_panic]
fn unwrap_rejects_65_bits() {
    let _ = unwrap(0, 65, 0, 1);
}

#[test]
fn extend_advance_hand_worked() {
    let mut e = Extend::new(4, 14);
    // 14 to 3 is 5 ticks (one wrap).
    assert_eq!(e.advance(3, 5), Some(5));
    assert_eq!(e.last(), 3);
    // 3 to 3 is 0, 16 or 32 ticks: told 16.
    assert_eq!(e.advance(3, 16), Some(21));
    // 3 to 4 is 1 modulo 16, not 2: refused, nothing changes.
    assert_eq!(e.advance(4, 2), None);
    assert_eq!((e.total(), e.last()), (21, 3));
    // Raw readings are masked to the counter's width: 0x13 is 3.
    assert_eq!(e.advance(0x13, 32), Some(53));
    // update carries on from there.
    assert_eq!(e.update(5), 55);
    // Overflow is refused.
    let mut e = Extend::new(64, 0);
    assert_eq!(e.advance(u64::MAX, u64::MAX), Some(u64::MAX));
    assert_eq!(e.advance(0, 1), None);
    assert_eq!(e.total(), u64::MAX);
}

#[test]
fn consistent_hand_worked() {
    // 1000 Hz +/-1000 ppm is [999, 1001].
    let a = est(hz(1000), 1_000_000);
    // 1003 Hz +/-1000 ppm is [1001.997, 1004.003]: apart.
    assert_eq!(consistent(a, est(hz(1003), 1_000_000)), Err(3_000_000));
    // 1002 Hz +/-1000 ppm starts at 1000.998: they overlap.
    assert_eq!(consistent(a, est(hz(1002), 1_000_000)), Ok(()));
    // 1001.5 Hz +/-500 ppm starts at 1000.99925: they overlap.
    let b = est(Freq::new(2003, 2).unwrap(), 500_000);
    assert_eq!(consistent(a, b), Ok(()));
    assert_eq!(consistent(b, a), Ok(()));
    // Touching counts: 1001 Hz exactly.
    assert_eq!(consistent(a, est(hz(1001), 0)), Ok(()));
    // 1/1024 Hz past it does not: (1001 + 1/1024 - 1000) / 1000 =
    // 1000976.5625 ppb, rounded toward zero.
    let c = est(Freq::new(1001 * 1024 + 1, 1024).unwrap(), 0);
    assert_eq!(consistent(a, c), Err(1_000_976));
    // From c's side the offset is in ppb of c, negative.
    assert!(matches!(consistent(c, a), Err(x) if x < 0));
    // A bound over 100% reaches 0 Hz: anything below agrees.
    assert_eq!(
        consistent(est(hz(1000), 2_000_000_000), est(hz(1), 0)),
        Ok(())
    );
}

#[test]
fn decide_hand_worked() {
    let ghz = hz(1_000_000_000);
    // Measured at boot: 1 GHz +/-50 ppm.
    let boot = Boot::Measured(est(ghz, 50_000));
    // 10 s later: 1.00001 GHz (+10 ppm) +/-0.5 ppm: agrees, tighter.
    let r = est(hz(1_000_010_000), 500);
    assert_eq!(decide(boot, r), Outcome::Refined);
    assert!(decide(boot, r).adopts());
    // Agrees but no tighter: keep the boot value.
    assert_eq!(decide(boot, est(ghz, 50_000)), Outcome::KeptBoot);
    assert_eq!(decide(boot, est(ghz, 60_000)), Outcome::KeptBoot);
    assert!(!Outcome::KeptBoot.adopts());
    // 1.0001 GHz +/-0.5 ppm is 100 ppm off: past both bounds (50.5 ppm).
    assert_eq!(
        decide(boot, est(hz(1_000_100_000), 500)),
        Outcome::Changed(100_000)
    );
    assert!(Outcome::Changed(1).adopts());

    // A CPUID claim of 2 GHz trusted to 200 ppm.
    let claim = Boot::Claimed {
        freq: hz(2_000_000_000),
        slack_ppb: 200_000,
    };
    // Measured 2.0001 GHz +/-1 ppm: 50 ppm off, inside 201 ppm.
    // (2.0001 - 2) / 2.0001 = 49.9975 ppm, rounded toward zero.
    assert_eq!(
        decide(claim, est(hz(2_000_100_000), 1_000)),
        Outcome::Confirmed(49_997)
    );
    assert!(!Outcome::Confirmed(0).adopts());
    // Measured 1.9 GHz: the claim is 5.26% fast. (1.9 - 2) / 1.9 =
    // -52631578.9 ppb.
    assert_eq!(
        decide(claim, est(hz(1_900_000_000), 1_000)),
        Outcome::ClaimWrong(-52_631_578)
    );
    assert!(Outcome::ClaimWrong(1).adopts());
    // Just inside and just outside the claim's slack plus the bound:
    // 2 GHz * (1 + 201 ppm) = 2.000402 GHz is 200.96 ppm of itself.
    assert!(matches!(
        decide(claim, est(hz(2_000_402_000), 1_000)),
        Outcome::Confirmed(_)
    ));
    assert!(matches!(
        decide(claim, est(hz(2_000_403_000), 1_000)),
        Outcome::ClaimWrong(_)
    ));
}

#[test]
fn rescale_hand_worked() {
    // 1 GHz measured against a clock taken as 2 GHz, now 2.0002 GHz: the
    // clock runs 100 ppm faster than thought, so 1.0001 GHz, exactly.
    let e = Estimate {
        freq: hz(1_000_000_000),
        ppb: 100,
        window: 77,
    };
    let r = rescale(e, hz(2_000_000_000), hz(2_000_200_000)).unwrap();
    assert_eq!(r.freq, hz(1_000_100_000));
    assert_eq!((r.ppb, r.window), (100, 77));
    // 3 Hz * 5 / 7 = 2.142857 Hz = 2194.29 / 1024, rounded to 2194/1024;
    // that rounding is up to 1/2048 Hz, 1/(2 * 2194) = 227894.3 ppb,
    // rounded up.
    let r = rescale(est(hz(3), 10), hz(7), hz(5)).unwrap();
    assert_eq!(r.freq, Freq::new(2194, 1024).unwrap());
    assert_eq!(r.ppb, 10 + 227_895);
    // No change at all.
    let r = rescale(e, hz(7), hz(7)).unwrap();
    assert_eq!(r, e);
    // Out of range: 2^40 Hz doubled.
    assert_eq!(rescale(est(hz(1 << 40), 0), hz(1), hz(2)), None);
    // Down to under 1 Hz.
    assert_eq!(rescale(est(hz(1), 0), hz(2), hz(1)), None);
}

fn freq() -> impl Strategy<Value = Freq> {
    (
        1_000_000u64..10_000_000_000,
        prop_oneof![Just(1u64), Just(1024u64), 1u64..100_000],
    )
        .prop_filter_map("valid", |(n, d)| Freq::new(n, d))
}

/// x * y <= z * w, exactly.
fn le_wide(x: u128, y: u128, z: u128, w: u128) -> bool {
    mul_wide(x, y) <= mul_wide(z, w)
}

proptest! {
    #![proptest_config(config(2000))]

    /// unwrap gives the one fit when there is exactly one, and None
    /// otherwise, checked by trying every candidate.
    #[test]
    fn unwrap_matches_brute_force(
        bits in 1u32..=8,
        raw in any::<u64>(),
        lo in 0u64..2000,
        len in 0u64..1000,
    ) {
        let hi = lo + len;
        let m = 1u64 << bits;
        let fits: Vec<u64> = (lo..=hi).filter(|t| t % m == raw % m).collect();
        let want = if fits.len() == 1 { Some(fits[0]) } else { None };
        prop_assert_eq!(unwrap(raw, bits, lo, hi), want);
    }

    /// For wide counters: the true count is found whenever the range
    /// holds it and is narrower than a wrap.
    #[test]
    fn unwrap_finds_the_truth(
        bits in 1u32..=64,
        truth in any::<u64>(),
        below in any::<u64>(),
        above in any::<u64>(),
    ) {
        let m = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        // A range around the truth, narrower than 2^bits.
        let below = below % (m / 2 + 1);
        let above = above % (m / 2 + 1);
        let lo = truth.saturating_sub(below);
        let hi = truth.saturating_add(above);
        prop_assume!(u128::from(hi - lo) < 1u128 << bits);
        let raw = truth & m;
        prop_assert_eq!(unwrap(raw, bits, lo, hi), Some(truth));
    }

    /// expected's range holds the exact reference count whenever the TSC
    /// is within a factor of 2 of what it is taken to be, and is exactly
    /// half to twice the nominal count.
    #[test]
    fn expected_holds_the_truth(
        ticks in 0u64..=MAX_WINDOW,
        tsc in freq(),
        reference in freq(),
    ) {
        // Nominal count N = ticks rn td / (rd tn). lo = floor(N / 2):
        // 2 lo rd tn <= ticks rn td < 2 (lo + 1) rd tn.
        let n = u128::from(ticks) * u128::from(reference.num()) * u128::from(tsc.den());
        let d = u128::from(reference.den()) * u128::from(tsc.num());
        let Some((lo, hi)) = expected(ticks, tsc, reference) else {
            // Only when even half the count is past u64::MAX.
            prop_assert!(n / (2 * d) > u128::from(u64::MAX));
            return Ok(());
        };
        prop_assert!(2 * u128::from(lo) * d <= n);
        prop_assert!(n < 2 * (u128::from(lo) + 1) * d);
        // hi = ceil(2 N): (hi - 1) d < 2 n <= hi d.
        // hi saturates at u64::MAX, which no count can pass.
        if hi < u64::MAX {
            prop_assert!(2 * n <= u128::from(hi) * d);
        }
        prop_assert!(u128::from(hi) == 0 || (u128::from(hi) - 1) * d < 2 * n);
    }

    /// Soundness: two estimates that both hold one true frequency are
    /// always consistent, whatever order they come in.
    #[test]
    fn consistent_when_both_hold_the_truth(
        truth in 1_000_000u64..10_000_000_000,
        pa in 0u64..200_000_000,
        pb in 0u64..200_000_000,
        sa in -700_000i64..=700_000,
        sb in -700_000i64..=700_000,
    ) {
        // Each measurement up to 70% of its own bound away from the
        // truth (sa / 10^6 of it), in 1/1024 Hz: with bounds up to 20%,
        // that keeps the truth inside the bound measured from the
        // measurement, |F - T| <= 0.7 p T <= p F, whichever side F is on.
        let off = |p: u64, s: i64| {
            let shift = i128::from(truth) * 1024 * i128::from(p) * i128::from(s)
                / 1_000_000_000_000_000;
            Freq::new(u64::try_from(i128::from(truth) * 1024 + shift).unwrap(), 1024).unwrap()
        };
        let truth_f = hz(truth);
        let a = est(off(pa, sa), pa);
        let b = est(off(pb, sb), pb);
        prop_assert!(agrees(a, truth_f, 0).is_ok() && agrees(b, truth_f, 0).is_ok());
        prop_assert_eq!(consistent(a, b), Ok(()));
        prop_assert_eq!(consistent(b, a), Ok(()));
    }

    /// Symmetric, widening a bound never breaks agreement, and every
    /// estimate agrees with itself.
    #[test]
    fn consistent_is_symmetric_and_monotone(
        fa in freq(),
        fb in freq(),
        pa in 0u64..3_000_000_000,
        pb in 0u64..3_000_000_000,
        more in 0u64..1_000_000,
    ) {
        let a = est(fa, pa);
        let b = est(fb, pb);
        prop_assert_eq!(consistent(a, b).is_ok(), consistent(b, a).is_ok());
        if consistent(a, b).is_ok() {
            prop_assert!(consistent(est(fa, pa + more), b).is_ok());
        }
        prop_assert_eq!(consistent(a, a), Ok(()));
        // Far apart is never consistent: the intervals [A(1-a), A(1+a)]
        // and [B(1-b), B(1+b)] with B > A(1+a)/(1-b) cannot meet. Checked
        // exactly: B (10^9 - b) > A (10^9 + a).
        let g = 1_000_000_000u128;
        let (pa, pb) = (u128::from(pa.min(1 << 31)), u128::from(pb.min(1 << 31)));
        let x = u128::from(fa.num()) * u128::from(fb.den());
        let y = u128::from(fb.num()) * u128::from(fa.den());
        let apart = !le_wide(y, g.saturating_sub(pb), x, g + pa)
            || !le_wide(x, g.saturating_sub(pa), y, g + pb);
        prop_assert_eq!(consistent(a, b).is_err(), apart);
    }

    /// rescale is within its own rounding of the exact value, and
    /// rescaling back and forth comes back within two roundings.
    #[test]
    fn rescale_is_exact_to_its_bound(
        e in freq(),
        p in 0u64..1_000_000,
        from in freq(),
        // `to` within 1% of `from`, as a refinement is.
        num in 990_000u64..1_010_000,
    ) {
        let to = match Freq::new(from.num().saturating_mul(num) / 1_000_000, from.den()) {
            Some(t) => t,
            None => return Ok(()),
        };
        let Some(r) = rescale(est(e, p), from, to) else { return Ok(()) };
        // Exact value X = en tn fd / (ed td fn); result R = rn / rd.
        // |R - X| <= (r.ppb - p) / 10^9 * R, so
        // |rn ed td fn - en tn fd rd| * 10^9 <= (r.ppb - p) rn ed td fn.
        let lhs_a = u128::from(r.freq.num()) * u128::from(e.den()) * u128::from(to.den());
        let lhs = mul_wide(lhs_a, u128::from(from.num()));
        let rhs_a = u128::from(e.num()) * u128::from(to.num()) * u128::from(from.den());
        let rhs = mul_wide(rhs_a, u128::from(r.freq.den()));
        let rounding = u128::from(r.ppb - p);
        // Compare |lhs - rhs| * 10^9 with rounding * lhs, via
        // lhs * (10^9 - rounding) <= rhs * 10^9 <= lhs * (10^9 + rounding)
        // in 256 bits: both sides fit since lhs < 2^200.
        let wide = |(h, l): (u128, u128), k: u128| -> (u128, u128) {
            let (h1, l1) = mul_wide(l, k);
            (h * k + h1, l1)
        };
        let g = 1_000_000_000u128;
        prop_assert!(wide(lhs, g.saturating_sub(rounding)) <= wide(rhs, g));
        prop_assert!(wide(rhs, g) <= wide(lhs, g + rounding));
        if let Some(back) = rescale(r, to, from) {
            prop_assert!(consistent(est(e, back.ppb - p), est(back.freq, 0)).is_ok());
        }
    }
}
