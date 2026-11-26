use proptest::prelude::*;

use super::{config, mul_wide};
use crate::calibrate::{Estimate, Sample};
use crate::freq::Freq;
use crate::pm;
use crate::refine::{Boot, Outcome};
use crate::watchdog::{
    CHECK_NS, CHECKS, Count, LIMIT_PPB, READS, Read, Unstable, Verdict, Watch, carry, count,
    deviates, tsc_bound_ppb,
};

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
fn constants_are_roadmap_1c() {
    // ROADMAP 1c: 500 ppm over 10 checks, from the minimum of 3
    // bracketed reads; checked every second.
    assert_eq!(LIMIT_PPB, 500_000);
    assert_eq!(CHECKS, 10);
    assert_eq!(READS, 3);
    assert_eq!(CHECK_NS, 1_000_000_000);
}

#[test]
fn count_hand_worked() {
    // 4 bits (wraps at 16), raw difference 3: in 10..=25 only 19 fits.
    assert_eq!(count(3, 4, 10, 25), Count::One(19));
    // In 2..=20 both 3 and 19 fit.
    assert_eq!(count(3, 4, 2, 20), Count::Many);
    // In 4..=18 none does.
    assert_eq!(count(3, 4, 4, 18), Count::None);
    // Edges: 0..=16 holds 0 and 16; 0..=15 only 0; 1..=16 only 16.
    assert_eq!(count(0, 4, 0, 16), Count::Many);
    assert_eq!(count(0, 4, 0, 15), Count::One(0));
    assert_eq!(count(0, 4, 1, 16), Count::One(16));
    // Only the low bits of the raw difference count.
    assert_eq!(count(0x13, 4, 0, 15), Count::One(3));
    // An empty range holds nothing.
    assert_eq!(count(3, 4, 5, 4), Count::None);
    // 64 bits: the raw difference itself, if it is in range.
    assert_eq!(count(1000, 64, 900, 1100), Count::One(1000));
    assert_eq!(count(1000, 64, 0, u64::MAX), Count::One(1000));
    assert_eq!(count(1000, 64, 1001, u64::MAX), Count::None);
    assert_eq!(count(u64::MAX, 64, 0, u64::MAX), Count::One(u64::MAX));
}

#[test]
#[should_panic]
fn count_rejects_zero_bits() {
    count(0, 0, 0, 1);
}

#[test]
#[should_panic]
fn count_rejects_65_bits() {
    count(0, 65, 0, 1);
}

#[test]
fn deviates_hand_worked() {
    let e = hz(1_000_000_000);
    // Exactly 500 ppm fast with a bound of 0: 500 ppm or more, so it
    // fires (F (1 - 0) = 1.0005 GHz >= E (1 + 0.0005)).
    assert_eq!(
        deviates(est(hz(1_000_500_000), 0), e, LIMIT_PPB),
        Some(500_000)
    );
    // One hertz less is not 500 ppm.
    assert_eq!(deviates(est(hz(1_000_499_999), 0), e, LIMIT_PPB), None);
    // Exactly 500 ppm slow.
    assert_eq!(
        deviates(est(hz(999_500_000), 0), e, LIMIT_PPB),
        Some(-500_000)
    );
    assert_eq!(deviates(est(hz(999_500_001), 0), e, LIMIT_PPB), None);
    // 600 ppm fast measured to +/-100 ppm: the slowest it can be is
    // 1.0006 GHz * 0.9999 = 1.00049994 GHz, under 1.0005: not proven.
    assert_eq!(
        deviates(est(hz(1_000_600_000), 100_000), e, LIMIT_PPB),
        None
    );
    // To +/-99.9 ppm: 1.0006 * (1 - 0.0000999) = 1.00050005... >= 1.0005.
    assert_eq!(
        deviates(est(hz(1_000_600_000), 99_900), e, LIMIT_PPB),
        Some(600_000)
    );
    // 600 ppm slow to +/-100 ppm: 0.9994 * 1.0001 = 0.99949994 < 0.9995,
    // proven; to +/-100.1 ppm, 0.9994 * 1.0001001 = 0.99950004: not.
    assert_eq!(
        deviates(est(hz(999_400_000), 100_000), e, LIMIT_PPB),
        Some(-600_000)
    );
    assert_eq!(deviates(est(hz(999_400_000), 100_100), e, LIMIT_PPB), None);
    // A huge bound proves nothing, and nothing is right at the nominal.
    assert_eq!(
        deviates(est(hz(2_000_000_000), u64::MAX), e, LIMIT_PPB),
        None
    );
    assert_eq!(deviates(est(e, 0), e, LIMIT_PPB), None);
    // A limit of 0 fires on any provable difference, but not on none.
    assert_eq!(deviates(est(hz(1_000_000_001), 0), e, 0), Some(1));
    assert_eq!(deviates(est(e, 0), e, 0), Some(0));
    // Fractions: 315/88 MHz against 3,581,335 Hz, 500.0 ppm and a bit
    // fast (3579545.4545 * 1.0005 = 3581335.227: 3,581,336 is past it,
    // 3,581,335 is not.
    // (3581336 * 88 - 315 * 10^6) / (315 * 10^6) = 157568 / 315000000,
    // 500,215.87 ppb, rounded toward zero.
    assert_eq!(
        deviates(est(hz(3_581_336), 0), pm::FREQ, LIMIT_PPB),
        Some(500_215)
    );
    assert_eq!(deviates(est(hz(3_581_335), 0), pm::FREQ, LIMIT_PPB), None);
}

#[test]
fn tsc_bound_hand_worked() {
    let m = est(hz(2_000_000_000), 800);
    let b = est(hz(2_000_001_000), 9_000);
    // Took the measurement: its bound.
    assert_eq!(tsc_bound_ppb(Boot::Measured(b), m, Outcome::Refined), 800);
    assert_eq!(
        tsc_bound_ppb(Boot::Measured(b), m, Outcome::Changed(5)),
        800
    );
    let c = Boot::Claimed {
        freq: hz(2_000_000_000),
        slack_ppb: 200_000,
    };
    assert_eq!(tsc_bound_ppb(c, m, Outcome::ClaimWrong(-300_000)), 800);
    // Kept the boot measurement: its bound.
    assert_eq!(
        tsc_bound_ppb(Boot::Measured(b), m, Outcome::KeptBoot),
        9_000
    );
    // Kept a claim 1,000 ppb away: q = 1,800, plus
    // ceil(2 * 1800 * 1000 / 10^9) = ceil(0.0036) = 1.
    assert_eq!(tsc_bound_ppb(c, m, Outcome::Confirmed(1_000)), 1_801);
    assert_eq!(tsc_bound_ppb(c, m, Outcome::Confirmed(-1_000)), 1_801);
    // 200 ppm away, bound 800 ppb: q = 200,800, plus
    // ceil(2 * 200800 * 200000 / 10^9) = ceil(80.32) = 81.
    assert_eq!(tsc_bound_ppb(c, m, Outcome::Confirmed(200_000)), 200_881);
    // Exactly on it: no rounding to add.
    assert_eq!(tsc_bound_ppb(c, m, Outcome::Confirmed(0)), 800);
}

#[test]
fn carry_hand_worked() {
    let r = Estimate {
        freq: hz(1_000_000_000),
        ppb: 50_000,
        window: 77,
    };
    // 50,000 + 800 + ceil(50000 * 800 / 10^9) = 50,800 + ceil(0.04).
    assert_eq!(carry(r, 800), Estimate { ppb: 50_801, ..r });
    // Nothing to add.
    assert_eq!(carry(r, 0), r);
    assert_eq!(carry(Estimate { ppb: 0, ..r }, 800).ppb, 800);
    // 10% and 10%: 0.2 + 0.01.
    assert_eq!(
        carry(
            Estimate {
                ppb: 100_000_000,
                ..r
            },
            100_000_000
        )
        .ppb,
        210_000_000
    );
    assert_eq!(
        carry(Estimate { ppb: u64::MAX, ..r }, u64::MAX).ppb,
        u64::MAX
    );
}

/// A machine with a clock and a reference, both driven from the true
/// time in nanoseconds. The clock's rate can change (in ppb from its
/// nominal frequency) and it can be stepped; the reference is exact.
#[derive(Clone, Debug)]
struct Sim {
    /// True time, ns.
    t: u128,
    /// Clock ticks times 10^18.
    clock_fx: u128,
    clock: u64,
    /// Clock error now, ppb.
    err: i64,
    rn: u128,
    rd: u128,
    bits: u32,
    /// Every clock value read, with the true time it was read at.
    log: Vec<(u64, u128)>,
}

impl Sim {
    fn new(clock_hz: u64, reference: Freq, bits: u32) -> Sim {
        Sim {
            t: 1_000_000_000,
            clock_fx: 0,
            clock: clock_hz,
            err: 0,
            rn: u128::from(reference.num()),
            rd: u128::from(reference.den()),
            bits,
            log: Vec::new(),
        }
    }

    fn wait(&mut self, ns: u64) {
        let ns = u128::from(ns);
        let rate = (1_000_000_000i128 + i128::from(self.err)) as u128;
        self.clock_fx += ns * u128::from(self.clock) * rate;
        self.t += ns;
    }

    fn reference(&self) -> u64 {
        let ticks = self.t * self.rn / (self.rd * 1_000_000_000);
        (ticks as u64) & crate::counter::mask(self.bits)
    }

    fn read_clock(&mut self) -> u64 {
        let c = (self.clock_fx / 1_000_000_000_000_000_000) as u64;
        self.log.push((c, self.t));
        c
    }

    /// One bracketed read, `gaps` ns between each of its four reads.
    fn read(&mut self, gaps: [u64; 4]) -> Read {
        let before = self.reference();
        self.wait(gaps[0]);
        let clock = self.read_clock();
        self.wait(gaps[1]);
        let after = self.reference();
        self.wait(gaps[2]);
        let stamp = self.read_clock();
        self.wait(gaps[3]);
        Read {
            before,
            clock,
            after,
            stamp,
        }
    }

    /// A check's reads: the lead, 50 ns, then the bracketed reads.
    fn reads(&mut self, gaps: &[[u64; 4]; READS]) -> (u64, [Read; READS]) {
        let lead = self.read_clock();
        self.wait(50);
        (
            lead,
            [self.read(gaps[0]), self.read(gaps[1]), self.read(gaps[2])],
        )
    }

    fn start(&mut self, slack: u64) -> Watch {
        let pre = self.read_clock();
        self.wait(50);
        let raw = self.reference();
        self.wait(50);
        let stamp = self.read_clock();
        let f = Freq::new(
            u64::try_from(self.rn).unwrap(),
            u64::try_from(self.rd).unwrap(),
        )
        .unwrap();
        Watch::new(hz(self.clock), f, self.bits, slack, pre, raw, stamp).unwrap()
    }

    /// When the clock read `c`.
    fn when(&self, c: u64) -> u128 {
        self.log.iter().find(|(v, _)| *v == c).unwrap().1
    }

    /// Did the clock provably run 500 ppm or more from its nominal rate
    /// between its reads `a` and `b` (true ticks, true time)? Exact, with
    /// the clock's tick quantisation allowed for: the true ticks between
    /// them are in (b - a - 1, b - a + 1).
    fn really_off(&self, a: u64, b: u64) -> bool {
        let dt = self.when(b) - self.when(a);
        let ticks = u128::from(b - a);
        let g = 1_000_000_000u128;
        // ticks per ns against clock / 10^9, scaled by 10^9 * dt.
        let nominal = dt * u128::from(self.clock);
        let fast = (ticks + 1) * g * g >= nominal * (g + u128::from(LIMIT_PPB));
        let slow = ticks.saturating_sub(1) * g * g <= nominal * (g - u128::from(LIMIT_PPB));
        fast || slow
    }
}

/// Close-together reads: 100 ns apart.
const TIGHT: [[u64; 4]; READS] = [[100; 4]; READS];

fn run(sim: &mut Sim, w: &mut Watch, gap: u64) -> Verdict {
    sim.wait(gap);
    let (lead, r) = sim.reads(&TIGHT);
    w.check(lead, &r)
}

#[test]
fn steady_clock_hand_worked() {
    // A 2 GHz clock against a 64-bit 100 MHz HPET, exactly in step: no
    // check ever finds anything, and the window fills after 11 checks.
    let mut sim = Sim::new(2_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(100_000);
    assert_eq!(w.last(), None);
    for i in 0..30 {
        assert_eq!(
            run(&mut sim, &mut w, CHECK_NS),
            Verdict::Steady,
            "check {i}"
        );
    }
    assert_eq!(w.checks(), 30);
    assert_eq!(w.restarts(), 0);
    // The newest sample is the last check's tightest read: all are
    // equally wide (100 ns, 10 ticks), so its first.
    let s = w.last().unwrap();
    assert_eq!(s.after - s.before, 20);
}

#[test]
fn rate_rule_hand_worked() {
    // 600 ppm fast from check 5 on: the window over 10 checks reaches
    // 500 ppm once at least 500/600 of it is fast, that is when 9 of its
    // 10 intervals are (540 ppm; 8 of 10 give 480). The reads are 100 ns
    // apart, so each bound is about (2 * 30 ns of reference) / 10 s,
    // 6 ppb: nothing in the way.
    let mut sim = Sim::new(2_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(100_000);
    for i in 0..5 {
        assert_eq!(
            run(&mut sim, &mut w, CHECK_NS),
            Verdict::Steady,
            "check {i}"
        );
    }
    sim.err = 600_000;
    // Checks 5 to 12 (the 6th to 13th): the window holds 1 to 8 fast
    // intervals, or is not full yet.
    for i in 5..13 {
        assert_eq!(
            run(&mut sim, &mut w, CHECK_NS),
            Verdict::Steady,
            "check {i}"
        );
    }
    // Check 13: 9 fast intervals of 10: 540 ppm.
    match run(&mut sim, &mut w, CHECK_NS) {
        Verdict::Unstable(Unstable::Rate(off)) => {
            assert!((539_000..=541_000).contains(&off), "{off}")
        }
        v => panic!("{v:?}"),
    }
}

#[test]
fn slow_clock_fires_too() {
    let mut sim = Sim::new(3_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(100_000);
    sim.err = -800_000;
    // The first full window (11 samples, 10 intervals) is all slow.
    for i in 0..10 {
        assert_eq!(
            run(&mut sim, &mut w, CHECK_NS),
            Verdict::Steady,
            "check {i}"
        );
    }
    match run(&mut sim, &mut w, CHECK_NS) {
        Verdict::Unstable(Unstable::Rate(off)) => {
            assert!((-801_000..=-799_000).contains(&off), "{off}")
        }
        v => panic!("{v:?}"),
    }
}

#[test]
fn under_the_limit_never_fires() {
    // 499 ppm fast, steadily, for a minute: only instability counts.
    let mut sim = Sim::new(2_000_000_000, hz(100_000_000), 64);
    sim.err = 499_000;
    let mut w = sim.start(100_000);
    for i in 0..60 {
        assert_eq!(
            run(&mut sim, &mut w, CHECK_NS),
            Verdict::Steady,
            "check {i}"
        );
    }
}

#[test]
fn backwards_hand_worked() {
    let mut sim = Sim::new(2_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(100_000);
    for _ in 0..3 {
        assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    }
    // Step the clock back by 3 s (6 * 10^9 ticks) between checks. The
    // next check's lead is read 100 ns (after the last stamp) plus 1 s
    // later, so it is 3 s - 1 s - 100 ns back from the last stamp:
    // 4 * 10^9 - 200 ticks.
    sim.clock_fx -= 6_000_000_000u128 * 1_000_000_000_000_000_000;
    sim.wait(CHECK_NS);
    let (lead, r) = sim.reads(&TIGHT);
    assert_eq!(
        w.check(lead, &r),
        Verdict::Unstable(Unstable::Backwards(4_000_000_000 - 200))
    );
}

#[test]
fn backwards_inside_a_read() {
    // The stamp below the clock read just before it.
    let mut sim = Sim::new(1_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(1000);
    let (lead, mut r) = sim.reads(&TIGHT);
    r[1].stamp = r[1].clock - 5;
    assert_eq!(w.check(lead, &r), Verdict::Unstable(Unstable::Backwards(5)));
    // And the clock read below the stamp before it, and the lead below
    // the last check's final stamp.
    let mut w = sim.start(1000);
    sim.wait(1000);
    let (lead, mut r) = sim.reads(&TIGHT);
    r[2].clock = r[1].stamp - 1;
    assert_eq!(w.check(lead, &r), Verdict::Unstable(Unstable::Backwards(1)));
    let mut w = sim.start(1000);
    sim.wait(1000);
    let (lead, r) = sim.reads(&TIGHT);
    assert_eq!(w.check(lead, &r), Verdict::Steady);
    sim.wait(1000);
    let (_, r2) = sim.reads(&TIGHT);
    assert_eq!(
        w.check(r[2].stamp - 1, &r2),
        Verdict::Unstable(Unstable::Backwards(1))
    );
    // Equal readings are not backwards.
    let mut w = sim.start(1000);
    sim.wait(1000);
    let (lead, mut r) = sim.reads(&TIGHT);
    r[0].stamp = r[0].clock;
    r[1].clock = r[0].stamp;
    assert_eq!(w.check(lead, &r), Verdict::Steady);
}

#[test]
fn a_64_bit_reference_going_backwards_stops_the_watchdog() {
    let mut sim = Sim::new(1_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(1000);
    sim.wait(CHECK_NS);
    let (lead, mut r) = sim.reads(&TIGHT);
    r[1].after = r[1].before - 1;
    assert_eq!(w.check(lead, &r), Verdict::ReferenceBackwards);
    // Equal is fine.
    let mut w = sim.start(1000);
    sim.wait(CHECK_NS);
    let (lead, mut r) = sim.reads(&TIGHT);
    r[1].after = r[1].before;
    assert_eq!(w.check(lead, &r), Verdict::Steady);
}

#[test]
fn jumped_hand_worked() {
    // A 24-bit PM timer: a clock that says 1 s passed while the PM timer
    // says 0.4 s cannot be counted: no wrap count is within half to twice
    // the clock's 1 s (1.79 to 7.16 million ticks) and congruent to 0.4 s
    // (1,431,818 ticks) modulo 2^24 (16,777,216).
    let mut sim = Sim::new(1_000_000_000, pm::FREQ, 24);
    let mut w = sim.start(1000);
    sim.wait(400_000_000);
    sim.clock_fx += 600_000_000u128 * 1_000_000_000_000_000_000;
    let (lead, r) = sim.reads(&TIGHT);
    assert_eq!(w.check(lead, &r), Verdict::Unstable(Unstable::Jumped));
}

#[test]
fn a_long_gap_restarts_the_window() {
    // A 24-bit PM timer wraps every 4.69 s; a 5 s gap gives a range of
    // 2.5 to 10 s, which holds 5 s and 9.69 s: the window starts again.
    let mut sim = Sim::new(1_000_000_000, pm::FREQ, 24);
    let mut w = sim.start(1000);
    for _ in 0..5 {
        assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    }
    assert!(w.last().is_some());
    assert_eq!(run(&mut sim, &mut w, 5_000_000_000), Verdict::Restarted);
    assert_eq!(w.restarts(), 1);
    assert_eq!(w.last(), None);
    // From there it measures again: a fast clock is caught once a full
    // window has been taken after the restart.
    sim.err = 1_000_000;
    for i in 0..10 {
        assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady, "{i}");
    }
    assert!(matches!(
        run(&mut sim, &mut w, CHECK_NS),
        Verdict::Unstable(Unstable::Rate(_))
    ));
    // 4.5 s is still counted: 2.25 to 9 s holds 4.5 s, and 9.19 s is
    // past it.
    let mut w = sim.start(1000);
    sim.err = 0;
    assert_eq!(run(&mut sim, &mut w, 4_500_000_000), Verdict::Steady);
    assert_eq!(w.restarts(), 0);
}

#[test]
fn a_gap_past_max_window_restarts() {
    // 2^40 clock ticks at 1 GHz is 18 minutes.
    let mut sim = Sim::new(1_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(1000);
    assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    assert_eq!(run(&mut sim, &mut w, 1_100_000_000_000), Verdict::Restarted);
    assert_eq!(w.last(), None);
    assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    assert!(w.last().is_some());
}

#[test]
fn restart_from_the_middle_of_a_check_keeps_counting() {
    // The long gap is inside the check, between its first and second
    // bracketed reads: the window starts again from the check's last
    // read, and goes on counting from there.
    let mut sim = Sim::new(1_000_000_000, pm::FREQ, 24);
    let mut w = sim.start(1000);
    assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    sim.wait(CHECK_NS);
    let gaps = [[100, 100, 100, 5_000_000_000], [100; 4], [100; 4]];
    let (lead, r) = sim.reads(&gaps);
    assert_eq!(w.check(lead, &r), Verdict::Restarted);
    for _ in 0..20 {
        assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
    }
}

#[test]
fn the_tightest_read_is_used() {
    let mut sim = Sim::new(1_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(1000);
    sim.wait(CHECK_NS);
    // Widths of 3000, 500 and 2000 ns of reference: the second.
    let gaps = [
        [1500, 1500, 100, 100],
        [250, 250, 100, 100],
        [1000, 1000, 100, 100],
    ];
    let (lead, r) = sim.reads(&gaps);
    assert_eq!(w.check(lead, &r), Verdict::Steady);
    let s = w.last().unwrap();
    assert_eq!(s.target, r[1].clock);
    assert_eq!(s.after - s.before, 50);
}

#[test]
fn floor_hand_worked() {
    // 100 MHz reference, 64 bits. With no sample: the time given.
    let mut sim = Sim::new(1_000_000_000, hz(100_000_000), 64);
    let mut w = sim.start(1000);
    assert_eq!(w.floor_ns(12_345, 0), 12_345);
    sim.wait(CHECK_NS);
    let (lead, r) = sim.reads(&TIGHT);
    w.check(lead, &r);
    let s = w.last().unwrap();
    let last_raw = r[2].after;
    // From the sample's first reference read to the last read is
    // (last_raw - start ticks) - s.before... in all, ticks since s.before
    // plus 1,000 more to `raw`: (total - before + 1000) * 10 ns, times
    // 1.0005, rounded up.
    let total = s.after + (last_raw - r[0].after);
    let ticks = total - s.before + 1000;
    let ns = ticks * 10;
    let want = 7 + (ns * 10_005).div_ceil(10_000);
    assert_eq!(w.floor_ns(7, last_raw + 1000), want);
    // A reference read below the last one adds nothing.
    let base = 7 + ((total - s.before) * 10 * 10_005).div_ceil(10_000);
    assert_eq!(w.floor_ns(7, last_raw - 1), base);
    // Saturates.
    assert_eq!(w.floor_ns(u64::MAX - 1, last_raw), u64::MAX);
}

#[test]
fn floor_counts_one_wrap_of_a_narrow_reference() {
    // 4 bits at 1 Hz, read every 2 s by a 1 Hz clock... use the PM
    // timer: the delta since the last read is taken modulo 2^24.
    let mut sim = Sim::new(1_000_000_000, pm::FREQ, 24);
    let mut w = sim.start(1000);
    sim.wait(CHECK_NS);
    let (lead, r) = sim.reads(&TIGHT);
    w.check(lead, &r);
    let s = w.last().unwrap();
    let last = r[2].after;
    // A raw reading 5 ticks past the last one, across the wrap.
    let raw = (last + 5) & 0xff_ffff;
    let total = s.after + crate::counter::delta(r[0].after, last, 24);
    let ticks = u128::from(total - s.before + 5);
    // ticks * 88000 / 315 ns, up, then * 1.0005 up.
    let ns = (ticks * 88_000).div_ceil(315);
    let want = 100 + u64::try_from((ns * 10_005).div_ceil(10_000)).unwrap();
    assert_eq!(w.floor_ns(100, raw), want);
}

#[test]
fn new_refuses_bad_widths() {
    assert!(Watch::new(hz(1), hz(1), 0, 0, 0, 0, 0).is_none());
    assert!(Watch::new(hz(1), hz(1), 65, 0, 0, 0, 0).is_none());
    assert!(Watch::new(hz(1), hz(1), 1, 0, 0, 0, 0).is_some());
    assert!(Watch::new(hz(1), hz(1), 64, 0, 0, 0, 0).is_some());
    // The stamp before the read before it.
    assert!(Watch::new(hz(1), hz(1), 64, 0, 5, 0, 4).is_none());
    assert!(Watch::new(hz(1), hz(1), 64, 0, 5, 0, 5).is_some());
}

/// One check of a random run: how long since the last, the gaps inside
/// each bracketed read, and the clock's error from then on.
#[derive(Clone, Debug)]
struct Step {
    gap: u64,
    reads: [[u64; 4]; READS],
    err: i64,
}

fn reads(max: u64) -> impl Strategy<Value = [[u64; 4]; READS]> {
    proptest::array::uniform3(proptest::array::uniform4(
        prop_oneof![8 => 20u64..2_000, 1 => 0u64..=max],
    ))
}

fn steps(
    gaps: impl Strategy<Value = u64>,
    errs: std::ops::RangeInclusive<i64>,
    stall: u64,
) -> impl Strategy<Value = Vec<Step>> {
    proptest::collection::vec(
        (gaps, reads(stall), errs).prop_map(|(gap, reads, err)| Step { gap, reads, err }),
        1..40,
    )
}

fn reference() -> impl Strategy<Value = (Freq, u32)> {
    prop_oneof![
        Just((hz(100_000_000), 64)),
        Just((Freq::from_period_fs(69_841_279).unwrap(), 32)),
        Just((pm::FREQ, 24)),
        Just((pm::FREQ, 32)),
    ]
}

proptest! {
    #![proptest_config(config(1000))]

    /// count gives the one fit when there is exactly one, says None when
    /// there is none and Many when there are more, by trying every
    /// candidate.
    #[test]
    fn count_matches_brute_force(
        bits in 1u32..=8,
        raw in any::<u64>(),
        lo in 0u64..2000,
        len in 0u64..1000,
    ) {
        let hi = lo + len;
        let m = 1u64 << bits;
        let fits: Vec<u64> = (lo..=hi).filter(|t| t % m == raw % m).collect();
        let want = match fits.len() {
            0 => Count::None,
            1 => Count::One(fits[0]),
            _ => Count::Many,
        };
        prop_assert_eq!(count(raw, bits, lo, hi), want);
    }

    /// deviates is exactly "every frequency within the bound is at least
    /// the limit away", checked with 256-bit products.
    #[test]
    fn deviates_is_exact(
        fnum in 1_000_000u64..10_000_000_000,
        fden in prop_oneof![Just(1u64), Just(1024u64), 1u64..100_000],
        nnum in 1_000_000u64..10_000_000_000,
        nden in prop_oneof![Just(1u64), Just(1024u64), 1u64..100_000],
        p in prop_oneof![0u64..2_000_000, Just(u64::MAX)],
        l in prop_oneof![Just(LIMIT_PPB), 0u64..3_000_000],
    ) {
        let (Some(f), Some(n)) = (Freq::new(fnum, fden), Freq::new(nnum, nden)) else {
            return Ok(());
        };
        let g = 1_000_000_000u128;
        let pc = u128::from(p.min(1 << 31));
        let lc = u128::from(l.min(1 << 31));
        let a = u128::from(f.num()) * u128::from(n.den());
        let b = u128::from(n.num()) * u128::from(f.den());
        let fast = mul_wide(a, g.saturating_sub(pc)) >= mul_wide(b, g + lc);
        let slow = mul_wide(a, g + pc) <= mul_wide(b, g.saturating_sub(lc));
        let want = (fast || slow).then(|| n.ppb_from(f));
        prop_assert_eq!(deviates(est(f, p), n, l), want);
    }

    /// The carried bound is never below the exact product bound.
    #[test]
    fn carry_covers_the_product(a in 0u64..1_000_000_000, b in 0u64..1_000_000_000) {
        let r = carry(est(hz(1_000_000), a), b);
        // (1 + a)(1 + b) - 1 = a + b + ab, in units of 10^-9 (times 10^9).
        let exact = u128::from(a) * 1_000_000_000 + u128::from(b) * 1_000_000_000
            + u128::from(a) * u128::from(b);
        prop_assert!(u128::from(r.ppb) * 1_000_000_000 >= exact);
        prop_assert!(u128::from(r.ppb) * 1_000_000_000 < exact + 1_000_000_000);
    }

    /// Soundness: on any clock whose rate wanders by up to 2000 ppm,
    /// read with random delays and stalls, every Rate verdict is true
    /// (the clock really ran 500 ppm or more off between the window's two
    /// samples), nothing ever says Backwards, Jumped or ReferenceBackwards,
    /// and the tightest sample is one of the check's reads.
    #[test]
    fn every_verdict_is_true(
        (rf, bits) in reference(),
        clock in prop_oneof![Just(1_000_000_000u64), 100_000_000u64..5_000_000_000],
        // Now and then a gap long enough to restart a narrow reference.
        plan in steps(
            prop_oneof![10 => 100_000_000u64..2_000_000_000, 1 => 4_000_000_000u64..12_000_000_000],
            -2_000_000..=2_000_000,
            3_000_000,
        ),
    ) {
        let mut sim = Sim::new(clock, rf, bits);
        let mut w = sim.start(rf.ns_to_ticks_ceil(1_000_000));
        let mut window: Vec<Sample> = Vec::new();
        for (i, s) in plan.iter().enumerate() {
            sim.err = s.err;
            sim.wait(s.gap);
            let (lead, r) = sim.reads(&s.reads);
            let v = w.check(lead, &r);
            match v {
                Verdict::Steady => {}
                Verdict::Restarted => {
                    prop_assert!(bits < 64, "check {i}: a 64-bit reference never restarts");
                    window.clear();
                    continue;
                }
                Verdict::Unstable(Unstable::Rate(off)) => {
                    prop_assert!(window.len() >= CHECKS, "check {i}: {off} before a full window");
                    let old = window[window.len() - CHECKS];
                    let new = w.last().unwrap();
                    prop_assert!(
                        sim.really_off(old.target, new.target),
                        "check {i}: {off} ppb, but the clock was not 500 ppm off"
                    );
                    return Ok(());
                }
                v => prop_assert!(false, "check {}: {:?}", i, v),
            }
            let last = w.last().unwrap();
            prop_assert!(r.iter().any(|x| x.clock == last.target));
            window.push(last);
        }
    }

    /// Completeness: once the clock's rate steps to 700 ppm or more off
    /// and stays there, with reads at most 20 us apart and checks 0.5 to
    /// 1.5 s apart, the watchdog fires within a full window of the step,
    /// with the right sign; a clock within 300 ppm never fires.
    #[test]
    fn instability_is_caught(
        (rf, bits) in reference(),
        clock in 100_000_000u64..5_000_000_000,
        before in 0usize..15,
        off in prop_oneof![700_000i64..5_000_000, -900_000i64..=-700_000],
        calm in -300_000i64..=300_000,
        plan in steps(500_000_000u64..1_500_000_000, 0..=0, 20_000),
    ) {
        let mut sim = Sim::new(clock, rf, bits);
        let mut w = sim.start(rf.ns_to_ticks_ceil(1_000_000));
        sim.err = calm;
        for i in 0..before {
            sim.wait(CHECK_NS);
            let (lead, r) = sim.reads(&TIGHT);
            prop_assert_eq!(w.check(lead, &r), Verdict::Steady, "calm check {}", i);
        }
        sim.err = off;
        let mut fired = None;
        for (i, s) in plan.iter().cycle().take(CHECKS + 1).enumerate() {
            sim.wait(s.gap);
            let (lead, r) = sim.reads(&s.reads);
            match w.check(lead, &r) {
                Verdict::Steady => {}
                Verdict::Unstable(Unstable::Rate(o)) => {
                    fired = Some(o);
                    break;
                }
                v => prop_assert!(false, "check {}: {:?}", i, v),
            }
        }
        prop_assert!(fired.is_some(), "not caught within a full window");
        prop_assert_eq!(fired.unwrap().signum(), off.signum());
    }

    /// A clock stepped back by more than the time since the last check
    /// is caught at the very next check, by how much it went back.
    #[test]
    fn backwards_is_caught_at_once(
        (rf, bits) in reference(),
        clock in 100_000_000u64..5_000_000_000,
        calm in 0usize..15,
        // At least 1,000 ticks: more than the 100 ns (at most 500 ticks)
        // from the last stamp to the end of the check.
        extra in 1_000u64..10_000_000_000,
    ) {
        let mut sim = Sim::new(clock, rf, bits);
        // The clock starts at 200 s, so it can go back far enough.
        sim.clock_fx = 200 * u128::from(clock) * 1_000_000_000_000_000_000;
        let mut w = sim.start(rf.ns_to_ticks_ceil(1_000_000));
        for _ in 0..calm {
            prop_assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
        }
        let last = sim.log.last().unwrap().0;
        // Back by 1 s of the clock plus `extra` ticks, then 1 s on.
        let back = u128::from(clock) + u128::from(extra);
        sim.clock_fx -= back * 1_000_000_000_000_000_000;
        sim.wait(CHECK_NS);
        let (lead, r) = sim.reads(&TIGHT);
        let want = last - lead;
        prop_assert_eq!(w.check(lead, &r), Verdict::Unstable(Unstable::Backwards(want)));
    }

    /// The floor after going backwards is at least where the old timeline
    /// could have reached by the time the check began: the clock's time at
    /// the newest sample plus the true time since, at up to 500 ppm fast.
    #[test]
    fn the_floor_is_never_below_the_old_clock(
        (rf, bits) in reference(),
        clock in 100_000_000u64..5_000_000_000,
        err in -499_000i64..=499_000,
        calm in 1usize..12,
        gap in 100_000_000u64..2_000_000_000,
    ) {
        let mut sim = Sim::new(clock, rf, bits);
        sim.err = err;
        let mut w = sim.start(rf.ns_to_ticks_ceil(1_000_000));
        for _ in 0..calm {
            prop_assert_eq!(run(&mut sim, &mut w, CHECK_NS), Verdict::Steady);
        }
        let s = w.last().unwrap();
        // The clock in ns at the sample, on a timeline at the nominal rate.
        let ns_at = |c: u64| u128::from(c) * 1_000_000_000 / u128::from(clock);
        let last_ns = u64::try_from(ns_at(s.target)).unwrap();
        // The last reading made before the next check starts.
        sim.wait(gap);
        let c = sim.read_clock();
        let raw = sim.reference();
        let floor = w.floor_ns(last_ns, raw);
        prop_assert!(u128::from(floor) >= ns_at(c), "{} < {}", floor, ns_at(c));
    }
}
