//! Kernel tests for the clocksource watchdog: it watches the TSC (or says
//! why not) and leaves a steady one alone, and a faked fault
//! (crate::time::fake: `skew+N`, `back`) is caught, with the clock falling
//! back to the reference without going backwards and the timer moving
//! with it.

use core::sync::atomic::{AtomicU64, Ordering};

use optimal_time::select::{Clock, Events, Reference};
use optimal_time::watchdog::{self as wd, CHECK_NS, CHECKS, LIMIT_PPB, Unstable};

use super::{Idle, Status, checks, report, status, wait_start};
use crate::clock;
use crate::ktest;
use crate::time::fake::{self, BACK_NS, FAULT_AFTER_NS, Fault};
use crate::time::refine;
use crate::timer;

/// The highest clock reading made while the watchdog was still watching,
/// as [`wait_for_the_fault`] saw it.
static HIGHEST: AtomicU64 = AtomicU64::new(0);

/// Is the faked fault one the watchdog must leave alone (a skew under
/// its limit)?
fn tolerated(f: Fault) -> bool {
    matches!(f, Fault::Skew(ppm) if ppm.unsigned_abs() * 1000 < LIMIT_PPB)
}

/// main.rs, with a faked fault, before the tests: spin (interrupts on)
/// until the watchdog has fired or stopped or, for a fault it must leave
/// alone, made a full window of checks plus two after the fault began.
/// Keeps the highest clock reading made while it was still watching.
pub fn wait_for_the_fault() {
    let fault = fake::current().fault.expect("a fault is faked");
    let limit = refine::WAIT_NS + FAULT_AFTER_NS + (CHECKS as u64 + 5) * CHECK_NS;
    let t0 = clock::now_ns();
    let mut high = t0;
    let mut first = None;
    loop {
        let n = clock::now_ns();
        match status() {
            Status::Fired { .. } | Status::Stopped { .. } => break,
            Status::Idle(i) => panic!("the watchdog does not run: {i:?}"),
            // Only readings known to be from before any fall-back.
            Status::Watching { .. } => high = high.max(n),
            Status::NotStarted => {}
        }
        if tolerated(fault) && fake::begun() {
            let c = checks().0;
            let c0 = *first.get_or_insert(c);
            if c >= c0 + CHECKS as u64 + 2 {
                break;
            }
        }
        assert!(
            high.saturating_sub(t0) <= limit,
            "the watchdog did not deal with the fault in {limit} ns"
        );
        core::hint::spin_loop();
    }
    HIGHEST.store(high, Ordering::SeqCst);
    report();
}

/// The reference's clock: what the TSC falls back to.
fn fallback_of(r: Reference) -> Clock {
    match r {
        Reference::Hpet => Clock::Hpet,
        Reference::PmTimer => Clock::PmTimer,
        Reference::Pit => unreachable!("never watched against the PIT"),
    }
}

/// When the watchdog fell back: the bound it carried the LAPIC timer's
/// rate across with (crate::time::refine's tests and the timer's use it).
pub fn fell_back() -> Option<u64> {
    let Status::Fired { to: Some(_), .. } = status() else {
        return None;
    };
    match refine::status() {
        refine::Status::Done {
            boot,
            measured,
            outcome,
            ..
        } => Some(wd::tsc_bound_ppb(boot, measured, outcome)),
        s => unreachable!("fired after a refinement that gave {s:?}"),
    }
}

ktest!(watchdog_watches_or_says_why, {
    assert!(
        wait_start(refine::WAIT_NS + 2 * CHECK_NS),
        "the watchdog did not start"
    );
    report();
    let fault = fake::current().fault;
    match status() {
        Status::NotStarted => unreachable!(),
        Status::Idle(Idle::ClockNotTsc(k)) => {
            assert_eq!(k, clock::kind());
            assert_ne!(k, Some(Clock::Tsc));
        }
        Status::Idle(Idle::NoReference) => assert_eq!(clock::fallback(), None),
        Status::Idle(Idle::NoEvents) => assert_eq!(timer::events(), None),
        Status::Idle(Idle::NotRefined) => panic!("the refinement did not finish"),
        Status::Watching { against } => {
            assert!(fault.is_none_or(tolerated), "{fault:?} was not caught");
            assert_eq!(clock::kind(), Some(Clock::Tsc));
            assert_eq!(Some(against), crate::time::get().unwrap().reference);
            assert_eq!(clock::fallback(), Some(fallback_of(against)));
            // Two more checks, and still nothing wrong with a steady TSC.
            let (c0, _) = checks();
            let t0 = clock::now_ns();
            while checks().0 < c0 + 2 {
                assert!(clock::now_ns() - t0 < 5 * CHECK_NS, "no checks for 5 s");
                core::hint::spin_loop();
            }
            assert!(
                matches!(status(), Status::Watching { .. }),
                "{:?}",
                status()
            );
            let (c, restarts) = checks();
            crate::println!("ktest: watchdog: {c} checks, {restarts} restarts, steady");
        }
        Status::Stopped { why, .. } => panic!("the watchdog stopped: {why:?}"),
        Status::Fired { .. } => {
            assert!(
                fault.is_some_and(|f| !tolerated(f)),
                "the watchdog fired on a steady TSC: {:?}",
                status()
            );
        }
    }
});

ktest!(watchdog_deals_with_a_faked_fault, {
    let Some(fault) = fake::current().fault else {
        crate::println!("ktest: no faked TSC fault");
        return;
    };
    if tolerated(fault) {
        let Status::Watching { .. } = status() else {
            panic!("a {fault:?} under the limit gave {:?}", status());
        };
        assert_eq!(clock::kind(), Some(Clock::Tsc));
        assert!(fake::begun());
        crate::println!("ktest: watchdog: left the {fault:?} alone");
        return;
    }
    let Status::Fired {
        against,
        why,
        back_ns,
        to,
        events,
        ..
    } = status()
    else {
        panic!("{fault:?} gave {:?}", status());
    };
    match (fault, why) {
        (Fault::Skew(ppm), Unstable::Rate(off)) => {
            // It fires once the window's average reaches 500 ppm: between
            // that and the whole skew (plus the measurement's bound).
            let ppb = ppm * 1000;
            assert_eq!(off.signum(), ppb.signum());
            assert!(
                off.unsigned_abs() >= LIMIT_PPB
                    && off.unsigned_abs() <= ppb.unsigned_abs() + 20_000,
                "{off} ppb for a skew of {ppb}"
            );
        }
        (Fault::Back, Unstable::Backwards(_)) => {
            // 3 s back, less the time since the last check (at most one
            // interval, plus a little for a late event).
            assert!(
                back_ns <= BACK_NS && back_ns + 2 * CHECK_NS >= BACK_NS,
                "back by {back_ns} ns"
            );
        }
        (f, w) => panic!("{f:?} was found as {w:?}"),
    }
    let (k, base) = to.expect("the fallback gave a reading");
    assert_eq!(k, fallback_of(against));
    assert_eq!(clock::kind(), Some(k));
    assert_eq!(clock::fallback(), None);
    let t = crate::time::get().unwrap();
    let nominal = match k {
        Clock::Hpet => t.hpet.as_ref().unwrap().caps.freq(),
        Clock::PmTimer => optimal_time::pm::FREQ,
        _ => unreachable!(),
    };
    assert_eq!(clock::freq(), Some(nominal));
    // The new timeline starts at or past every reading made before it,
    // even after the TSC went back.
    let high = HIGHEST.load(Ordering::SeqCst);
    assert!(
        base >= high,
        "the clock fell back to {base} ns, below {high}"
    );
    assert!(clock::now_ns() >= high);
    // Events: no longer TSC-deadline, which the TSC times.
    assert_eq!(timer::events(), events);
    let tm = timer::get().unwrap();
    if tm.lapic.is_some() {
        assert_eq!(events, Some(Events::LapicOneShot));
        assert_eq!(timer::rates().tsc, None);
    }
    // A narrow fallback is kept extended.
    if k == Clock::PmTimer {
        assert!(clock::housekeeping_ns().is_some());
    }
    crate::println!(
        "ktest: watchdog: {why:?}; fell back to the {k:?} at {base} ns, events {events:?}"
    );
});
