//! Kernel tests for the clock: it never goes backwards, it agrees with the
//! calibration reference over the same (bracketed) interval, and a narrow
//! counter stays extended across several wraps with nothing but the
//! timer's housekeeping reading it.

use optimal_time::select::Clock;

use super::{housekeeping_ns, kind, now_ns};
use crate::arch::cpu;
use crate::ktest;
use crate::time::{self, Counter, pit_start, pit_stop};

/// The clock and the reference agree over the same interval within the
/// clock's own bound plus this: 20 ppm (see crate::time::tests).
const SLACK_PPB: u64 = 20_000;

ktest!(clock_runs_and_never_goes_backwards, {
    let k = kind().expect("QEMU machines always have a clock");
    crate::println!(
        "ktest: clock {k:?}, housekeeping {:?} ns",
        housekeeping_ns()
    );
    let first = now_ns();
    let mut last = first;
    for _ in 0..200_000 {
        let n = now_ns();
        assert!(n >= last, "the clock went back from {last} to {n}");
        last = n;
    }
    assert!(last > first, "the clock did not move in 200,000 reads");
    // Narrow exactly when the counter is under 64 bits.
    let narrow = match k {
        Clock::PmTimer => true,
        Clock::Hpet => {
            time::get()
                .unwrap()
                .hpet
                .as_ref()
                .unwrap()
                .caps
                .counter_bits()
                < 64
        }
        Clock::Tsc | Clock::TscNotInvariant => false,
    };
    assert_eq!(housekeeping_ns().is_some(), narrow);
});

ktest!(clock_matches_the_reference, {
    // Over about 100 ms: clock time between two clock reads must fall
    // inside the reference time between the reference reads around them,
    // widened by the clock's bound against the reference plus the slack.
    // Bracketed, so a descheduled vCPU only widens the interval.
    let t = time::get().unwrap();
    let r = t.reference_counter().expect("a reference");
    let rf = r.freq().unwrap();
    let pit_b = matches!(r, Counter::Pit).then(pit_start);
    // The clock is the TSC (measured against the reference, or claimed),
    // or it is the reference itself, which agrees exactly.
    let ppb = match kind().unwrap() {
        Clock::Tsc | Clock::TscNotInvariant => t.tsc_estimate().unwrap().ppb,
        Clock::Hpet | Clock::PmTimer => 0,
    } + SLACK_PPB;
    let bits = r.bits();
    let read = || r.raw().expect("a reference reading");
    let shortest = |f: &dyn Fn() -> (u64, u64, u64)| {
        (0..8)
            .map(|_| f())
            .min_by_key(|&(a, _, c)| optimal_time::counter::delta(a, c, bits))
            .unwrap()
    };
    let sample = || {
        let were = cpu::interrupts_disable();
        let s = (read(), now_ns(), read());
        if were {
            cpu::interrupts_enable();
        }
        s
    };
    let (a0, n0, c0) = shortest(&sample);
    // Wait about 100 ms on the clock (for the PIT, 40 ms: it wraps at 55).
    let wait = if matches!(r, Counter::Pit) {
        40_000_000
    } else {
        100_000_000
    };
    while now_ns() - n0 < wait {}
    let (a1, n1, c1) = shortest(&sample);
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    let d = |x, y| optimal_time::counter::delta(x, y, bits);
    let lo = rf.ticks_to_ns(d(c0, a1).saturating_sub(1));
    let hi = rf.ticks_to_ns(d(a0, c1) + 1) + 1;
    let clock = n1 - n0;
    let tol = |ns: u64| ns / 1_000_000 * (ppb / 1000 + 1) + 2;
    assert!(
        clock + tol(lo) >= lo && clock <= hi + tol(hi),
        "clock {clock} ns, reference between {lo} and {hi} ns, {ppb} ppb allowed"
    );
});

ktest!(a_narrow_clock_stays_extended_across_wraps, {
    // Only the timer's housekeeping reads the clock while this waits two
    // and a half wraps on the TSC, so if it missed a wrap the clock would
    // come up short by a whole wrap (4.7 s for a 24-bit PM timer).
    let Some(hk) = housekeeping_ns() else {
        crate::println!("ktest: the clock is 64-bit; nothing to check");
        return;
    };
    let t = time::get().unwrap();
    let tsc = t.tsc_estimate().expect("a TSC frequency on QEMU");
    let wrap = hk * 4;
    let span = wrap * 5 / 2;
    let ticks = tsc.freq.ns_to_ticks_ceil(span);
    let n0 = now_ns();
    let t0 = cpu::rdtsc_ordered();
    while cpu::rdtsc_ordered() - t0 < ticks {
        core::hint::spin_loop();
    }
    let t1 = cpu::rdtsc_ordered();
    let n1 = now_ns();
    let tsc_ns = tsc.freq.ticks_to_ns(t1 - t0);
    let clock = n1 - n0;
    // Within 1% plus a millisecond: a missed wrap is 40% of this span.
    let diff = clock.abs_diff(tsc_ns);
    assert!(
        diff <= tsc_ns / 100 + 1_000_000,
        "over {tsc_ns} ns of TSC the clock moved {clock} ns (wrap {wrap} ns)"
    );
    crate::println!(
        "ktest: {} wraps of the clock's counter, {diff} ns apart",
        span / wrap
    );
});
