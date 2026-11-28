//! Kernel tests for timer events: the boot choice follows the rules, the
//! LAPIC timer's rate agrees with the reference, every way of arming
//! (TSC-deadline, LAPIC one-shot, HPET one-shot, PIT one-shot, each
//! one-shot also in several pieces, and periodic) never fires early,
//! against the clock and against the reference read independently, and
//! cancelling works, for every source.

use core::sync::atomic::{AtomicU64, Ordering};

use optimal_apic::lapic as regs;
use optimal_time::calibrate;
use optimal_time::counter::delta;
use optimal_time::lapic as lt;
use optimal_time::select::{self, Clock, Events, Sources};

use super::{
    ARMED, CHUNK_CAP, arm_at, cancel, events_now, fires, force_events, get, release_unused,
    set_hook,
};
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
        t.lapic.is_some() || t.skip.lapic,
        "the LAPIC timer was not measured: {:?}",
        t.warnings
    );
    assert!(t.lapic.is_none() || !t.skip.lapic);
    let want = select::events(Sources {
        deadline_mode: t.deadline_mode && !t.skip.lapic,
        tsc_calibrated: tm.tsc_estimate().is_some() && tm.clock == Some(Clock::Tsc),
        tsc_invariant: tm.tsc_features.invariant,
        lapic_calibrated: t.lapic.is_some(),
        hpet: t.hpet.is_some(),
        pit: t.pit.is_some(),
    });
    assert_eq!(t.events, want);
    // The fallbacks are set up only when they are needed, and then only
    // the first that works: the HPET when QEMU's q35 has one.
    let lapic_events = matches!(want, Some(Events::TscDeadline | Events::LapicOneShot));
    if lapic_events {
        assert!(t.hpet.is_none() && t.pit.is_none());
    }
    if want == Some(Events::PitOneShot) {
        assert!(t.hpet.is_none());
    }
    // Not the HPET, though it was not ruled out: boot said why.
    if !lapic_events && !t.skip.hpet && tm.hpet.is_some() && t.hpet.is_none() {
        assert!(
            t.warnings
                .iter()
                .any(|w| w.starts_with("no HPET timer events: ")),
            "{:?}",
            t.warnings
        );
    }
    // Now: the boot choice, unless the watchdog fell back from the TSC,
    // which moves TSC-deadline events to one-shot.
    let now = if crate::time::watchdog::tests::fell_back().is_some()
        && want == Some(Events::TscDeadline)
        && t.lapic.is_some()
    {
        Some(Events::LapicOneShot)
    } else {
        want
    };
    assert_eq!(events_now(), now);
    // The LVT is in that mode, unmasked, on vector 0xF0.
    let l = t.lapic_regs.unwrap();
    let lvt = l.read(regs::LVT_TIMER);
    assert_eq!(lvt & 0xFF, 0xF0);
    if matches!(now, Some(Events::TscDeadline | Events::LapicOneShot)) {
        assert_eq!(lvt & lt::LVT_MASKED, 0);
    }
    let mode = match now.unwrap() {
        Events::TscDeadline => lt::Mode::TscDeadline,
        Events::LapicOneShot => lt::Mode::OneShot,
        // The LAPIC timer is not used: masked.
        Events::HpetOneShot | Events::PitOneShot => {
            assert_ne!(lvt & lt::LVT_MASKED, 0);
            lt::Mode::OneShot
        }
    };
    assert_eq!(lt::lvt_mode(lvt), Some(mode));
});

ktest!(lapic_rate_agrees_with_the_reference, {
    if no_lapic() {
        return;
    }
    if get().unwrap().skip.lapic {
        crate::println!("ktest: the LAPIC timer is skipped (notimer=lapic)");
        return;
    }
    // Measured again for up to 500 ms against the calibration reference
    // (the boot measurement was against the clock), with the timer held
    // so nothing else arms it meanwhile.
    let t = get().unwrap();
    let tm = time::get().unwrap();
    // The rate in use: boot's, carried across any refinement.
    let boot = super::rates().lapic.unwrap();
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
        Clock::Tsc | Clock::TscNotInvariant => time::refine::tsc_bound_ppb(),
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
    // Under QEMU's multi-threaded TCG an HPET one-shot can go unmatched
    // while the vCPU only spins; reading the HPET's counter as it waits
    // makes QEMU bring the HPET up to date (the level-line tests saw the
    // same, crate::irq::level's tests, Dev::fire_until). Real HPETs and
    // KVM's need nothing; the read only costs time here.
    let hpet = (events_now() == Some(Events::HpetOneShot))
        .then(|| time::get().unwrap().hpet.as_ref())
        .flatten();
    let t0 = cpu::rdtsc();
    while HIT_NS.load(Ordering::SeqCst) == 0 {
        if cpu::rdtsc().wrapping_sub(t0) > ticks {
            return false;
        }
        if let Some(h) = hpet {
            h.counter();
        }
        core::hint::spin_loop();
    }
    true
}

/// Arm for each duration in the current mode and check the hook ran no
/// earlier than asked, on the clock and on the reference, and that the
/// hardware never fired before what it was armed for.
fn never_early(what: &str) {
    let r = reference();
    let rf = r.freq().unwrap();
    let bits = r.bits();
    // How far the reference may run slow against the clock.
    let clock_ppb = match clock::kind().unwrap() {
        Clock::Tsc | Clock::TscNotInvariant => time::refine::tsc_bound_ppb(),
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
        let (_, e) = fires();
        if e != early0 {
            crate::println!(
                "ktest: {what}: {} early interrupts by {d} ns timers, the last {} ns early",
                e - early0,
                super::LAST_EARLY.load(Ordering::SeqCst)
            );
        }
    }
    set_hook(None);
    REF_KIND.store(0, Ordering::SeqCst);
    if let Some(b) = pit_b {
        pit_stop(b);
    }
    let (_, early1) = fires();
    assert_eq!(
        early1,
        early0,
        "{what}: the hardware fired before it was armed for, the last time {} ns early",
        super::LAST_EARLY.load(Ordering::SeqCst)
    );
    // Lateness depends on the host (a loaded one deschedules the vCPU),
    // so it is reported, and only bounded loosely here.
    assert!(worst_late < 1_000_000_000, "{what}: {worst_late} ns late");
    crate::println!("ktest: {what}: never early, at worst {worst_late} ns late");
}

/// Force each way of arming in turn that this machine has, and run `f`
/// with its name; then put the boot choice back.
fn each_source(f: impl Fn(&str, Events)) -> u32 {
    let t = get().unwrap();
    let was = t.events.expect("timer events on QEMU");
    let mut n = 0;
    for (e, what) in [
        (Events::TscDeadline, "tsc-deadline"),
        (Events::LapicOneShot, "lapic one-shot"),
        (Events::HpetOneShot, "hpet one-shot"),
        (Events::PitOneShot, "pit one-shot"),
    ] {
        match force_events(e) {
            Ok(_) => {
                f(what, e);
                n += 1;
            }
            Err(why) => crate::println!("ktest: {what}: not on this machine ({why})"),
        }
    }
    force_events(was).expect("the boot choice still works");
    release_unused();
    n
}

/// `what` in several armings: cap each at 3 ms, so 20 ms takes seven.
fn never_early_in_pieces(what: &str) {
    CHUNK_CAP.store(3_000_000, Ordering::SeqCst);
    let (f0, _) = fires();
    let p0 = super::pieces();
    never_early(&alloc::format!("{what} in 3 ms pieces"));
    CHUNK_CAP.store(u64::MAX, Ordering::SeqCst);
    let (f1, _) = fires();
    let pieces = super::pieces() - p0;
    // 21 timers, each run from an interrupt, and the six longer than one
    // piece (5 and 20 ms) start with a piece each. Not more interrupts: a
    // piece that fires late (a loaded host deschedules the vCPU) can leave
    // the deadline already passed, and then no further piece is needed.
    assert!(f1 - f0 >= 21, "{what}: only {} interrupts", f1 - f0);
    assert!(pieces >= 6, "{what}: only {pieces} armings in pieces");
    crate::println!(
        "ktest: {what}: {} interrupts and {pieces} armings in pieces for 21 timers in 3 ms pieces",
        f1 - f0
    );
}

ktest!(every_event_mode_never_fires_early, {
    if no_lapic() {
        return;
    }
    let (r0, m0) = super::hpet_retries();
    let modes = each_source(|what, e| {
        never_early(what);
        if e != Events::TscDeadline {
            never_early_in_pieces(what);
        }
    });
    // Every machine the tests run on has at least the PIT, and the HPET
    // on q35.
    assert!(modes >= 2, "only {modes} ways of arming");
    let (r1, m1) = super::hpet_retries();
    assert_eq!(m1, m0, "an HPET arming gave up");
    crate::println!(
        "ktest: {modes} ways of arming checked; {} HPET comparator retries",
        r1 - r0
    );
});

// The PIT covers at most 54.9 ms per arming: 150 ms takes at least
// three, and is still never early. Checked against the clock only (a
// PIT reference wraps every 55 ms).
ktest!(pit_reaches_long_deadlines_in_pieces, {
    if no_lapic() {
        return;
    }
    let was = get().unwrap().events.unwrap();
    if let Err(why) = force_events(Events::PitOneShot) {
        crate::println!("ktest: no PIT events here ({why})");
        return;
    }
    set_hook(Some(hook));
    let (_, early0) = fires();
    let p0 = super::pieces();
    for d in [60_000_000u64, 150_000_000] {
        HIT_NS.store(0, Ordering::SeqCst);
        let start = clock::now_ns();
        arm_at(start + d);
        assert!(
            wait_for_hook(5_000_000_000),
            "a {d} ns PIT timer never fired"
        );
        let at = HIT_NS.load(Ordering::SeqCst) - 1;
        assert!(
            at >= start + d,
            "{d} ns PIT timer ran at +{} ns",
            at - start
        );
    }
    set_hook(None);
    let pieces = super::pieces() - p0;
    let (_, early1) = fires();
    force_events(was).unwrap();
    release_unused();
    assert_eq!(early1, early0, "the PIT fired before it was armed for");
    // 60 ms is two armings (one piece), 150 ms at least three (two).
    assert!(pieces >= 3, "only {pieces} armings in pieces");
    crate::println!("ktest: pit: 60 and 150 ms timers in {pieces} pieces, never early");
});

// Every one-shot source the tests set up is given back cleanly: after
// the tests above, only the boot choice holds anything.
ktest!(fallbacks_given_back, {
    if no_lapic() {
        return;
    }
    let t = get().unwrap();
    let a = ARMED.lock();
    assert_eq!(a.hpet.is_some(), t.events == Some(Events::HpetOneShot));
    assert_eq!(a.pit, t.events == Some(Events::PitOneShot));
    assert_eq!(a.rates.hpet.is_some(), a.hpet.is_some());
    assert_eq!(a.rates.pit.is_some(), a.pit);
    drop(a);
    let h = time::get().unwrap().hpet.as_ref();
    if let Some(h) = h {
        let want = t.hpet.map_or(0, |(n, _, _)| 1u32 << n);
        assert_eq!(h.claimed(), want);
    }
});

ktest!(cancel_and_past_deadlines, {
    if no_lapic() {
        return;
    }
    let n = each_source(|what, _| {
        cancel_and_past(what);
        crate::println!("ktest: {what}: cancel, past and replaced deadlines");
    });
    assert!(n >= 2);
});

fn cancel_and_past(what: &str) {
    set_hook(Some(hook));
    HITS.store(0, Ordering::SeqCst);
    HIT_NS.store(0, Ordering::SeqCst);
    // Cancelled before it is due: never runs.
    arm_at(clock::now_ns() + 5_000_000);
    cancel();
    assert!(!wait_for_hook(30_000_000), "{what}: a cancelled timer ran");
    // A deadline already past runs at once (well within 100 ms).
    arm_at(0);
    assert!(
        wait_for_hook(100_000_000),
        "{what}: a past deadline did not run"
    );
    // A new deadline replaces the old one: only the second runs.
    HIT_NS.store(0, Ordering::SeqCst);
    let now = clock::now_ns();
    arm_at(now + 200_000_000);
    arm_at(now + 2_000_000);
    assert!(
        wait_for_hook(100_000_000),
        "{what}: the replacing deadline did not run"
    );
    assert!(!wait_for_hook(0) || HITS.load(Ordering::SeqCst) == 2);
    HIT_NS.store(0, Ordering::SeqCst);
    assert!(
        !wait_for_hook(300_000_000),
        "{what}: the replaced deadline ran too"
    );
    set_hook(None);
    assert_eq!(HITS.load(Ordering::SeqCst), 2, "{what}");
}

ktest!(periodic_never_runs_fast, {
    if no_lapic() {
        return;
    }
    if get().unwrap().skip.lapic {
        crate::println!("ktest: the LAPIC timer is skipped (notimer=lapic)");
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
