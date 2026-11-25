//! Kernel tests for the clocks on the real (virtual) machine: the HPET is
//! running as optimal_time::hpet::start leaves it, every reference agrees
//! with every other one about the TSC, the PM timer measures at its
//! specified frequency against the HPET, and time measured on the TSC
//! matches time measured on the reference over the same interval.
//!
//! QEMU derives the HPET, the PM timer and the PIT from one clock, so they
//! agree far more closely than real crystals would; the slack below
//! allows for QEMU's own rounding (it runs the PM timer at 3,579,545 Hz,
//! 0.13 ppm slow, and the PIT at 1,193,182 Hz, 0.15 ppm fast) and for the
//! host's clock being steered by NTP between measurements.

use optimal_time::calibrate::{self, Estimate};
use optimal_time::hpet;
use optimal_time::select::{Clock, HpetCheck, Reference};
use optimal_time::{pit, pm};

use super::{Counter, Time, get, measure, pit_read, pit_start, pit_stop};
use crate::arch::cpu;
use crate::ktest;

/// Two measurements of one clock against different references agree
/// within both bounds plus this: 20 ppm.
const SLACK_PPB: u64 = 20_000;
/// Tests measure for up to 500 ms, longer than boot does.
const WINDOW_NS: u64 = 500_000_000;

fn time() -> &'static Time {
    get().expect("time::init ran before the tests")
}

/// |a - b| <= a.ppb + b.ppb + SLACK_PPB, relative to a.
fn agree(a: Estimate, b: Estimate, what: &str) {
    let r = calibrate::agrees(a, b.freq, b.ppb.saturating_add(SLACK_PPB));
    assert!(r.is_ok(), "{what}: {a:?} and {b:?} are {r:?} ppb apart");
}

ktest!(boot_found_a_reference_and_a_clock, {
    let t = time();
    assert!(
        t.reference.is_some(),
        "no reference clock on a QEMU machine"
    );
    assert!(t.clock.is_some());
    // Warnings are checked on the boot log's time: line by the test
    // scripts, which know what each machine should say.
    // The reference is the one the rules pick.
    let want = if t.hpet.is_some() && !matches!(t.hpet_check, HpetCheck::Disagrees(_)) {
        Reference::Hpet
    } else if t.pm.is_some() {
        Reference::PmTimer
    } else {
        Reference::Pit
    };
    assert_eq!(t.reference, Some(want));
    // Both present means they were checked against each other, and agreed.
    if t.hpet.is_some() && t.pm.is_some() {
        assert_eq!(t.hpet_check, HpetCheck::Agrees);
    }
    // An invariant TSC is the clock; otherwise the HPET or the PM timer,
    // and only without either a TSC that is not invariant.
    let clock = t.clock.unwrap();
    let want = if t.tsc_features.invariant {
        Clock::Tsc
    } else if t.hpet.is_some() {
        Clock::Hpet
    } else if t.pm.is_some() {
        Clock::PmTimer
    } else {
        Clock::TscNotInvariant
    };
    assert_eq!(clock, want);
});

ktest!(hpet_runs_with_every_comparator_quiet, {
    let Some(h) = &time().hpet else {
        crate::println!("ktest: no HPET on this machine; nothing to check");
        return;
    };
    let config = h.read(hpet::CONFIG);
    assert_eq!(config & hpet::ENABLE, hpet::ENABLE);
    assert_eq!(config & hpet::LEGACY_ROUTE, 0);
    for n in 0..h.caps.comparators {
        let c = h.read(hpet::timer_config(n));
        assert_eq!(
            c & (hpet::T_INT_ENABLE | hpet::T_FSB_ENABLE),
            0,
            "comparator {n}: {c:#x}"
        );
    }
    // Counting up, and at about its rate. The HPET reads come before and
    // after the TSC reads, so the HPET interval always contains the TSC
    // one: at least 1 ms of TSC is never under 0.5 ms of HPET. A vCPU
    // descheduled between the reads (a loaded host) can only make the HPET
    // interval longer, so the upper check takes the shortest of 20 tries:
    // under 2 ms. Loose on purpose: this checks direction and scale, not
    // accuracy.
    let tsc = boot_tsc(time()).freq;
    let one_ms = tsc.ns_to_ticks_ceil(1_000_000);
    let shortest = (0..20)
        .map(|_| {
            let a = h.counter();
            let t0 = cpu::rdtsc_ordered();
            while cpu::rdtsc_ordered() - t0 < one_ms {}
            let b = h.counter();
            let ticks = optimal_time::counter::delta(a, b, h.caps.counter_bits());
            let ns = h.caps.freq().ticks_to_ns(ticks);
            assert!(ns >= 500_000, "1 ms of TSC was only {ns} ns of HPET");
            ns
        })
        .min()
        .unwrap();
    assert!(
        shortest < 2_000_000,
        "1 ms of TSC was at least {shortest} ns of HPET"
    );
});

ktest!(pm_timer_runs_at_its_specified_rate, {
    let t = time();
    let (Some(h), Some(p)) = (&t.hpet, &t.pm) else {
        crate::println!("ktest: not both an HPET and a PM timer; nothing to check");
        return;
    };
    let e = measure(Counter::Hpet(h), Counter::Pm(p), WINDOW_NS).expect("PM timer against HPET");
    // Within 2 ppm (QEMU's PM timer is 0.13 ppm slow) beyond the bound.
    assert_eq!(calibrate::agrees(e, pm::FREQ, 2_000), Ok(()), "{e:?}");
    // How tight the bound gets depends on how slow reads are (nested VMs
    // make every HPET and port read an exit: 78 ppm over 500 ms on
    // GitHub's runners), so only check it is usable; the agreement above
    // is the correctness check.
    assert!(e.ppb <= super::WARN_PPB, "bound {} ppb", e.ppb);
});

/// The TSC's boot frequency as an estimate (Time::tsc_estimate).
fn boot_tsc(t: &Time) -> Estimate {
    t.tsc_estimate()
        .expect("the TSC's frequency is known on QEMU")
}

ktest!(every_reference_agrees_about_the_tsc, {
    let t = time();
    let boot = boot_tsc(t);
    let mut seen = 0;
    if let Some(h) = &t.hpet {
        let e = measure(Counter::Hpet(h), Counter::Tsc, WINDOW_NS).expect("TSC against HPET");
        agree(e, boot, "HPET against boot");
        seen += 1;
    }
    if let Some(p) = &t.pm {
        let e = measure(Counter::Pm(p), Counter::Tsc, WINDOW_NS).expect("TSC against PM timer");
        agree(e, boot, "PM timer against boot");
        seen += 1;
    }
    // The PIT is on every PC QEMU builds; check it as a reference too.
    let b = pit_start();
    let e = measure(Counter::Pit, Counter::Tsc, WINDOW_NS);
    pit_stop(b);
    let e = e.expect("TSC against PIT");
    agree(e, boot, "PIT against boot");
    // The PIT is the coarsest (838 ns a tick) and the slowest to read;
    // as above, only check the bound is usable.
    assert!(e.ppb <= super::WARN_PPB, "PIT bound {} ppb", e.ppb);
    seen += 1;
    crate::println!("ktest: the TSC agreed against {seen} references");
});

ktest!(pit_channel2_counts_down_from_ffff_and_restores_port_b, {
    // SAFETY: reading port B has no side effects.
    let before = unsafe { crate::arch::port::inb(pit::PORT_B) };
    let b = pit_start();
    // Bits 4 to 7 are live status (bit 4 toggles with memory refresh).
    assert_eq!(b & pit::PORT_B_WRITABLE, before & pit::PORT_B_WRITABLE);
    // SAFETY: as above.
    let during = unsafe { crate::arch::port::inb(pit::PORT_B) };
    assert_eq!(during & pit::GATE2, pit::GATE2);
    assert_eq!(during & pit::SPEAKER, 0);
    let a = pit_read();
    // Just after starting, under 55 ms have passed: the up-count is small
    // but it moves.
    assert!((0..100_000).any(|_| pit_read() != a));
    pit_stop(b);
    // SAFETY: as above.
    let after = unsafe { crate::arch::port::inb(pit::PORT_B) };
    assert_eq!(after & pit::PORT_B_WRITABLE, before & pit::PORT_B_WRITABLE);
});

ktest!(tsc_time_matches_reference_time, {
    // Over about 50 ms, time on the TSC (converted with the measured
    // frequency, through the fast Scale) must fall inside the reference
    // time between the two TSC reads, widened by the TSC's bound plus the
    // slack. The reads are bracketed exactly as in a measurement, so a
    // descheduled vCPU only widens the interval, never breaks the check.
    let t = time();
    let e = boot_tsc(t);
    let r = t.reference_counter().expect("a reference");
    let pit_b = matches!(r, Counter::Pit).then(pit_start);
    let rf = r.freq().unwrap();
    let mut reader = super::Reader::new(r).unwrap();
    let mut tsc = super::Reader::new(Counter::Tsc).unwrap();
    let s0 = super::sample(&mut reader, &mut tsc).unwrap();
    let target = s0.after + rf.ns_to_ticks_ceil(50_000_000);
    while reader.read().unwrap() < target {}
    let s1 = super::sample(&mut reader, &mut tsc).unwrap();
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    // The TSC reads were more than `lo` and less than `hi` reference ticks
    // apart (see optimal_time::calibrate).
    let lo_ns = rf.ticks_to_ns(s1.before - s0.after - 1);
    let hi_ns = rf.ticks_to_ns(s1.after - s0.before + 1) + 1;
    let tsc_ns = e.freq.scale().ticks_to_ns(s1.target - s0.target);
    let tol = |ns: u64| ns / 1_000_000 * ((e.ppb + SLACK_PPB) / 1000 + 1) + 2;
    assert!(
        tsc_ns + tol(lo_ns) >= lo_ns && tsc_ns <= hi_ns + tol(hi_ns),
        "TSC {tsc_ns} ns, reference between {lo_ns} and {hi_ns} ns, bound {} ppb",
        e.ppb
    );
    // And the fast conversion is within 1 ns of the exact one.
    let exact = e.freq.ticks_to_ns(s1.target - s0.target);
    assert!(exact.abs_diff(tsc_ns) <= 1, "{exact} vs {tsc_ns}");
});
