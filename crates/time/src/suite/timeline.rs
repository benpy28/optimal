use proptest::prelude::*;

use super::config;
use crate::freq::Freq;
use crate::timeline::Timeline;

fn hz(f: u64) -> Freq {
    Freq::hz(f).unwrap()
}

#[test]
fn hand_worked() {
    // 1 GHz from the reading 1000: one nanosecond a tick after it.
    let t = Timeline::new(hz(1_000_000_000), 1000);
    assert_eq!(t.ns(1000), 0);
    assert_eq!(t.ns(2000), 1000);
    // Before the base: the base.
    assert_eq!(t.ns(500), 0);
    assert_eq!(t.ns(0), 0);
    // 3 GHz: 3 * 10^9 ticks is a second.
    let t = Timeline::new(hz(3_000_000_000), 0);
    assert_eq!(t.ns(3_000_000_000), 1_000_000_000);
    assert_eq!(t.ns(2), 0);
    assert_eq!(t.ns(3), 1);
    // Saturates rather than wrapping: 1 Hz for u64::MAX ticks.
    let t = Timeline::new(hz(1), 0);
    assert_eq!(t.ns(u64::MAX), u64::MAX);
}

#[test]
fn rebase_hand_worked() {
    // 1 GHz from 0, then 2 GHz from the reading 1000 (1000 ns).
    let a = Timeline::new(hz(1_000_000_000), 0);
    let b = a.rebase(1000, hz(2_000_000_000));
    assert_eq!(b.base_ticks, 1000);
    assert_eq!(b.base_ns, 1000);
    assert_eq!(b.ns(1000), 1000);
    assert_eq!(b.ns(3000), 2000);
    // A reading before the new base gives the new base's time.
    assert_eq!(b.ns(500), 1000);
    // Rebasing at a reading before the base keeps the base: it never
    // moves the clock back.
    let c = b.rebase(10, hz(1_000_000_000));
    assert_eq!(c.base_ticks, 1000);
    assert_eq!(c.base_ns, 1000);
    assert_eq!(c.ns(1500), 1500);
    // From a non-zero base_ns, the offset carries.
    let d = Timeline {
        base_ticks: 50,
        base_ns: 7,
        scale: hz(1_000_000_000).scale(),
    };
    assert_eq!(d.ns(60), 17);
    assert_eq!(d.rebase(60, hz(500_000_000)).ns(70), 37);
}

fn freq() -> impl Strategy<Value = Freq> {
    (
        1_000u64..10_000_000_000,
        prop_oneof![Just(1u64), Just(1024u64), 1u64..=1 << 32],
    )
        .prop_filter_map("in range", |(hz, den)| {
            Freq::new(hz.saturating_mul(den), den)
        })
}

proptest! {
    #![proptest_config(config(5000))]

    #[test]
    fn matches_exact_conversion(f in freq(), start: u64, d: u64) {
        // Within one nanosecond of the exact 128-bit conversion, for any
        // span, as Scale promises.
        let t = Timeline::new(f, start);
        let r = start.saturating_add(d);
        let exact = f.ticks_to_ns(r - start);
        let got = t.ns(r);
        if exact < u64::MAX - 3 {
            prop_assert!(got.abs_diff(exact) <= 1, "{got} vs {exact}");
        }
    }

    #[test]
    fn monotonic(f in freq(), start: u64, mut rs in proptest::collection::vec(any::<u64>(), 2..32)) {
        let t = Timeline::new(f, start);
        rs.sort_unstable();
        for w in rs.windows(2) {
            prop_assert!(t.ns(w[0]) <= t.ns(w[1]));
        }
    }

    #[test]
    fn rebases_are_continuous_and_monotonic(
        f0 in freq(),
        start in 0u64..1 << 62,
        steps in proptest::collection::vec((0u64..1 << 40, freq()), 1..16),
        probes in proptest::collection::vec(0u64..1 << 40, 1..16),
    ) {
        // A clock read at increasing readings, rebased at some of them
        // to new frequencies, never goes backwards, and each rebase gives
        // exactly the old value at its reading.
        let mut t = Timeline::new(f0, start);
        let mut at = start;
        let mut last = 0;
        for ((gap, f), probe) in steps.into_iter().zip(probes.into_iter().cycle()) {
            at += gap;
            let before = t.ns(at);
            prop_assert!(before >= last);
            t = t.rebase(at, f);
            prop_assert_eq!(t.ns(at), before);
            prop_assert_eq!(t.base_ns, before);
            let later = t.ns(at + probe);
            prop_assert!(later >= before);
            last = before;
        }
    }
}
