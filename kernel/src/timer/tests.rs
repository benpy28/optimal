//! Kernel tests for timer events: the boot choice follows the rules, the
//! LAPIC timer's rate agrees with the reference, every way of arming
//! (TSC-deadline, one-shot, one-shot in several pieces, periodic) never
//! fires early, against the clock and against the reference read
//! independently, and cancelling works.

use core::sync::atomic::{AtomicU64, Ordering};

use optimal_apic::lapic as regs;
use optimal_time::calibrate;
use optimal_time::counter::delta;
use optimal_time::lapic as lt;
use optimal_time::select::{self, Clock, Events};

use super::{ARMED, CHUNK_CAP, arm_at, cancel, events_now, fires, force_events, get, set_hook};
use crate::arch::cpu;
use crate::clock;
use crate::ktest;
use crate::time::{self, Counter, pit_start, pit_stop};

/// With interrupts off (no usable local APIC) there are no timer events,
/// and boot said so; every test here then has nothing to check.
fn no_lapic() -> bool {
    if crate::irq::controllers().is_some() {
        return false;
    }
    let t = get().expect("timer::init ran");
    assert_eq!(t.events, None);
    assert_eq!(events_now(), None);
    assert!(t.lapic.is_none());
    crate::println!("ktest: no local APIC in use; no timer events to check");
    true
}

/// Rates measured two ways agree within both bounds plus this, and the
/// reference may disagree with the clock by this much: 20 ppm.
const SLACK_PPB: u64 = 20_000;

ktest!(boot_chose_events_by_the_rules, {
    if no_lapic() {
        return;
    }
    let t = get().expect("timer::init ran");
    let tm = time::get().unwrap();
    assert!(
        t.lapic.is_some(),
        "the LAPIC timer was not measured: {:?}",
        t.warnings
    );
    let want = select::events(
        t.deadline_mode,
        tm.tsc_estimate().is_some() && clock::kind() == Some(Clock::Tsc),
        tm.tsc_features.invariant,
        t.lapic.is_some(),
    );
    assert_eq!(t.events, want);
    assert_eq!(events_now(), want);
    // The LVT is in that mode, unmasked, on vector 0xF0.
    let l = t.lapic_regs.unwrap();
    let lvt = l.read(regs::LVT_TIMER);
    assert_eq!(lvt & 0xFF, 0xF0);
    assert_eq!(lvt & lt::LVT_MASKED, 0);
    let mode = match want.unwrap() {
        Events::TscDeadline => lt::Mode::TscDeadline,
        Events::LapicOneShot => lt::Mode::OneShot,
    };
    assert_eq!(lt::lvt_mode(lvt), Some(mode));
});

ktest!(lapic_rate_agrees_with_the_reference, {
    if no_lapic() {
        return;
    }
    // Measured again for up to 500 ms against the calibration reference
    // (the boot measurement was against the clock), with the timer held
    // so nothing else arms it meanwhile.
    let t = get().unwrap();
    let tm = time::get().unwrap();
    let boot = t.lapic.unwrap();
    let r = tm.reference_counter().unwrap();
    let l = t.lapic_regs.unwrap();
    let pit_b = matches!(r, Counter::Pit).then(pit_start);
    let again = {
        let mut a = ARMED.lock();
        let e = super::calibrate(l, r);
        super::set_mode(l, events_now());
        super::program(&mut a);
        e
    };
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    let again = again.expect("LAPIC timer against the reference");
    // Boot's bound is against the clock; the clock is the TSC (within its
    // own bound of the reference) or the reference itself.
    let clock_ppb = match clock::kind().unwrap() {
        Clock::Tsc | Clock::TscNotInvariant => tm.tsc_estimate().unwrap().ppb,
        _ => 0,
    };
    let allowed = lt::compose_ppb(boot.ppb, clock_ppb).saturating_add(SLACK_PPB);
    let r = calibrate::agrees(again, boot.freq, allowed);
    assert!(
        r.is_ok(),
        "{again:?} against boot's {boot:?}: {r:?} ppb apart"
    );
    crate::println!("ktest: the LAPIC timer runs at {}", time::Mhz(boot.freq));
});

/// What the hook saw: the clock's time + 1 (0 for not yet), and the
/// reference's raw reading then.
static HIT_NS: AtomicU64 = AtomicU64::new(0);
static HIT_REF: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
/// The reference as a raw reader the hook can use: 0 for none, 1 the
/// HPET, 2 the PM timer, 3 the PIT.
static REF_KIND: AtomicU64 = AtomicU64::new(0);

fn reference() -> Counter<'static> {
    time::get().unwrap().reference_counter().unwrap()
}

fn hook(now: u64) {
    if REF_KIND.load(Ordering::SeqCst) != 0 {
        HIT_REF.store(reference().raw().unwrap_or(0), Ordering::SeqCst);
    }
    HIT_NS.store(now + 1, Ordering::SeqCst);
    HITS.fetch_add(1, Ordering::SeqCst);
}

/// Spin (interrupts on) until the hook has run or about `limit_ns` of
/// TSC have passed. True if it ran.
fn wait_for_hook(limit_ns: u64) -> bool {
    let f = time::get().unwrap().tsc_estimate().unwrap().freq;
    let ticks = f.ns_to_ticks_ceil(limit_ns);
    let t0 = cpu::rdtsc();
    while HIT_NS.load(Ordering::SeqCst) == 0 {
        if cpu::rdtsc().wrapping_sub(t0) > ticks {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Arm for each duration in the current mode and check the hook ran no
/// earlier than asked, on the clock and on the reference, and that the
/// hardware never fired before what it was armed for.
fn never_early(what: &str) {
    let tm = time::get().unwrap();
    let r = reference();
    let rf = r.freq().unwrap();
    let bits = r.bits();
    // How far the reference may run slow against the clock.
    let clock_ppb = match clock::kind().unwrap() {
        Clock::Tsc | Clock::TscNotInvariant => tm.tsc_estimate().unwrap().ppb,
        _ => 0,
    } + SLACK_PPB;
    let pit_b = matches!(r, Counter::Pit).then(pit_start);
    REF_KIND.store(1, Ordering::SeqCst);
    set_hook(Some(hook));
    let (_, early0) = fires();
    let mut worst_late = 0;
    // Up to 20 ms, so a PIT reference (wraps at 55 ms) is read in time.
    for &d in &[
        0u64, 1_000, 10_000, 100_000, 1_000_000, 5_000_000, 20_000_000,
    ] {
        for _ in 0..3 {
            HIT_NS.store(0, Ordering::SeqCst);
            let ref0 = r.raw().unwrap();
            let start = clock::now_ns();
            arm_at(start + d);
            assert!(
                wait_for_hook(5_000_000_000),
                "{what}: a {d} ns timer never fired"
            );
            let at = HIT_NS.load(Ordering::SeqCst) - 1;
            let ref_ns = rf.ticks_to_ns(delta(ref0, HIT_REF.load(Ordering::SeqCst), bits));
            assert!(
                at >= start + d,
                "{what}: {d} ns timer ran at +{} ns on the clock",
                at - start
            );
            // The reference was read before the clock at the start and
            // after it in the hook, so its interval contains the clock's:
            // at least d, less the bound and one reference tick.
            let allowed = d / 1_000_000 * (clock_ppb / 1000 + 1) + rf.ticks_to_ns(1) + 1;
            assert!(
                ref_ns + allowed >= d,
                "{what}: {d} ns timer ran after {ref_ns} ns of the reference"
            );
            worst_late = worst_late.max(at - start - d);
        }
    }
    set_hook(None);
    REF_KIND.store(0, Ordering::SeqCst);
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    let (_, early1) = fires();
    assert_eq!(
        early1, early0,
        "{what}: the hardware fired before it was armed for"
    );
    // Lateness depends on the host (a loaded one deschedules the vCPU),
    // so it is reported, and only bounded loosely here.
    assert!(worst_late < 1_000_000_000, "{what}: {worst_late} ns late");
    crate::println!("ktest: {what}: never early, at worst {worst_late} ns late");
}

ktest!(every_event_mode_never_fires_early, {
    if no_lapic() {
        return;
    }
    let t = get().unwrap();
    let was = t.events.expect("timer events on QEMU");
    let mut modes = 0;
    if t.tsc.is_some() {
        force_events(Events::TscDeadline).unwrap();
        never_early("tsc-deadline");
        modes += 1;
    }
    force_events(Events::LapicOneShot).unwrap();
    never_early("lapic one-shot");
    modes += 1;
    // In several armings: cap each at 3 ms, so 20 ms takes seven.
    CHUNK_CAP.store(3_000_000, Ordering::SeqCst);
    let (f0, _) = fires();
    never_early("lapic one-shot in 3 ms pieces");
    CHUNK_CAP.store(u64::MAX, Ordering::SeqCst);
    let (f1, _) = fires();
    // 21 timers, and the six longer than one piece (5 and 20 ms) need at
    // least two armings each. Not more: a piece that fires late can leave
    // the deadline already passed, and then no further piece is needed.
    assert!(f1 - f0 >= 21 + 6, "only {} interrupts", f1 - f0);
    crate::println!("ktest: {} interrupts for 21 timers in 3 ms pieces", f1 - f0);
    modes += 1;
    force_events(was);
    crate::println!("ktest: {modes} ways of arming checked");
});

ktest!(cancel_and_past_deadlines, {
    if no_lapic() {
        return;
    }
    set_hook(Some(hook));
    HITS.store(0, Ordering::SeqCst);
    HIT_NS.store(0, Ordering::SeqCst);
    // Cancelled before it is due: never runs.
    arm_at(clock::now_ns() + 5_000_000);
    cancel();
    assert!(!wait_for_hook(30_000_000), "a cancelled timer ran");
    // A deadline already past runs at once (well within 100 ms).
    arm_at(0);
    assert!(wait_for_hook(100_000_000), "a past deadline did not run");
    // A new deadline replaces the old one: only the second runs.
    HIT_NS.store(0, Ordering::SeqCst);
    let now = clock::now_ns();
    arm_at(now + 200_000_000);
    arm_at(now + 2_000_000);
    assert!(
        wait_for_hook(100_000_000),
        "the replacing deadline did not run"
    );
    assert!(!wait_for_hook(0) || HITS.load(Ordering::SeqCst) == 2);
    HIT_NS.store(0, Ordering::SeqCst);
    assert!(!wait_for_hook(300_000_000), "the replaced deadline ran too");
    set_hook(None);
    assert_eq!(HITS.load(Ordering::SeqCst), 2);
});

ktest!(periodic_never_runs_fast, {
    if no_lapic() {
        return;
    }
    // 1 ms ticks for about 100 ms of the reference, bracketed: the ticks
    // seen between the two reference reads can be at most one more than
    // whole periods in the longer interval. A loaded host can drop ticks,
    // so only "some" is required on the low side.
    let r = reference();
    let rf = r.freq().unwrap();
    let bits = r.bits();
    let pit_b = matches!(r, Counter::Pit).then(pit_start);
    let period = 1_000_000;
    let arm = super::start_periodic(period).expect("periodic mode");
    let t = get().unwrap();
    let e = t.lapic.unwrap();
    // The shortest the period can be: its guaranteed ticks at the fastest
    // rate the bound allows (it is never under `period` ns; see
    // optimal_time::lapic::arm).
    let n0 = super::periodic_ticks();
    let a = r.raw().unwrap();
    let f = time::get().unwrap().tsc_estimate().unwrap().freq;
    let t0 = cpu::rdtsc();
    let span = if matches!(r, Counter::Pit) {
        40_000_000
    } else {
        100_000_000
    };
    while f.ticks_to_ns(cpu::rdtsc() - t0) < span {
        core::hint::spin_loop();
    }
    let c = r.raw().unwrap();
    let n1 = super::periodic_ticks();
    super::stop_periodic();
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    let ticks = n1 - n0;
    let hi_ns = rf.ticks_to_ns(delta(a, c, bits) + 1) + 1;
    // With the reference within SLACK of the clock the LAPIC was measured
    // against.
    let hi_ns = hi_ns + hi_ns / 1_000_000 * (SLACK_PPB / 1000 + 1);
    assert!(
        ticks <= hi_ns / period + 1,
        "{ticks} ticks of at least {period} ns in at most {hi_ns} ns ({arm:?}, {e:?})"
    );
    assert!(ticks >= 1, "no periodic ticks");
    crate::println!("ktest: {ticks} periodic ticks in about {hi_ns} ns");
});
