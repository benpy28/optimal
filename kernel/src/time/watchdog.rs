//! The clocksource watchdog (ROADMAP 1c), driven by timer events.
//! optimal_time::watchdog makes every decision; this file reads the
//! counters and says what to do.
//!
//! 1. [`start`] (crate::timer, the timer's lock held), once the 10 s
//!    refinement has finished: when the clock is the TSC and has a
//!    fallback (the reference it was refined against, the HPET or the PM
//!    timer), take the first reference read.
//! 2. [`poll`] (crate::timer::interrupt, interrupts off, the timer's lock
//!    held): every optimal_time::watchdog::CHECK_NS, and at once when a
//!    timer event comes early by the clock (a clock that fell behind the
//!    timer's own count, which a clock that went back would otherwise
//!    hide by delaying the check), read the clock and 3 bracketed
//!    reference reads and check them.
//! 3. When the TSC is unstable, [`poll`] says so and crate::timer falls
//!    back: crate::clock::switch to the reference, LAPIC one-shot events
//!    instead of TSC-deadline, the LAPIC timer's rate carried across
//!    (optimal_time::watchdog::carry), all under the timer's lock. Then
//!    [`fired`] records the outcome.
//!
//! The clock is not watched once it is no longer the TSC: the HPET and
//! the PM timer run from fixed chipset crystals with nothing like the
//! TSC's dependence on CPU power states and firmware, and a bad HPET is
//! caught at boot by checking it against the PM timer (crate::time).
//! (Chosen: watch only the TSC, as ROADMAP 1c's fallback order needs.
//! Rejected: watching the HPET against the PM timer after a fallback,
//! which would read two slow counters every second to guard against a
//! fault the boot check already covers.)
//!
//! Nothing here allocates or prints in interrupt context; [`report`]
//! prints from the idle loop and the tests.

use core::fmt::Write;
use core::sync::atomic::{AtomicU8, Ordering};

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use optimal_time::select::{Clock, Events, Reference};
use optimal_time::watchdog::{self as wd, READS, Read, Unstable, Verdict, Watch};

use super::Counter;
use super::refine::{self, Skip};
use crate::clock;
use crate::sync::IrqSpinLock;

/// Added on both sides when counting the reference's wraps, for reads
/// that straddle a clock tick: 1 us. It only widens the range, so it can
/// make a check restart, never be wrong.
const READ_SLACK_NS: u64 = 1_000;

/// Why the watchdog does not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Idle {
    /// The clock is not the TSC (or there is none): nothing to watch.
    ClockNotTsc(Option<Clock>),
    /// No HPET or PM timer to watch the TSC against (and fall back to).
    NoReference,
    /// No timer events to run the checks.
    NoEvents,
    /// The refinement did not finish, so the TSC's frequency was never
    /// checked against the reference.
    NotRefined,
}

/// Why it stopped without finding the TSC unstable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The reference (64 bits) read below an earlier read.
    ReferenceBackwards,
    /// The reference gave no consistent reading.
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotStarted,
    Idle(Idle),
    Watching {
        against: Reference,
    },
    Stopped {
        against: Reference,
        why: Stop,
    },
    /// The TSC was found unstable at clock time `at_ns` (the new clock's,
    /// after a fall-back).
    Fired {
        against: Reference,
        why: Unstable,
        /// For [`Unstable::Backwards`]: by how much, in ns of the clock.
        back_ns: u64,
        at_ns: u64,
        /// What the clock fell back to, and where its timeline starts;
        /// `None` if the fallback gave no reading (the TSC stays).
        to: Option<(Clock, u64)>,
        /// How events are programmed now.
        events: Option<Events>,
    },
}

/// What crate::timer must do after a check.
pub enum Poll {
    /// Check again at this clock time.
    Next(u64),
    /// Stop: nothing more to check.
    Stop,
    /// The TSC is unstable: fall back, starting the new timeline at
    /// `floor_ns` or later, and widen the LAPIC timer's rate by
    /// `bound_ppb` (optimal_time::watchdog::carry).
    Fall { floor_ns: u64, bound_ppb: u64 },
}

struct Run {
    watch: Watch,
    reference: Counter<'static>,
    against: Reference,
    /// The TSC's bound against the reference (from the refinement).
    bound_ppb: u64,
    /// The unstable verdict, until [`fired`] records the outcome.
    why: Option<(Unstable, u64)>,
}

// SAFETY (for IrqSpinLock's Send bound): Counter holds shared references
// to the HPET and PM timer descriptions, which live in a Once forever
// and are only read.
static RUN: IrqSpinLock<Option<Run>> = IrqSpinLock::new(None);
static STATUS: IrqSpinLock<Status> = IrqSpinLock::new(Status::NotStarted);
/// Bit 0: the first line is printed; bit 1: the outcome line is.
static REPORTED: AtomicU8 = AtomicU8::new(0);

fn set(s: Status) {
    *STATUS.lock() = s;
}

pub fn status() -> Status {
    *STATUS.lock()
}

/// Checks made, and how many restarted the window (the tests read them).
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn checks() -> (u64, u64) {
    RUN.lock()
        .as_ref()
        .map_or((0, 0), |r| (r.watch.checks(), r.watch.restarts()))
}

/// Start watching, at clock time `now`, if there is anything to watch.
/// Called once the refinement has finished or been skipped, with the
/// timer's lock held; `events` says whether timer events run. Returns when
/// [`poll`] is first due.
pub fn start(now: u64, events: bool) -> Option<u64> {
    let idle = |i| {
        set(Status::Idle(i));
        None
    };
    let (bound_ppb, against) = match refine::status() {
        refine::Status::Done {
            boot,
            measured,
            outcome,
            against,
            ..
        } => (wd::tsc_bound_ppb(boot, measured, outcome), against),
        refine::Status::Skipped(Skip::ClockNotTsc(k)) => return idle(Idle::ClockNotTsc(k)),
        refine::Status::Skipped(Skip::Pit(_) | Skip::NoReference) => {
            return idle(Idle::NoReference);
        }
        refine::Status::Skipped(Skip::NoEvents) => return idle(Idle::NoEvents),
        refine::Status::Failed(_) => return idle(Idle::NotRefined),
        refine::Status::NotStarted | refine::Status::Running => {
            unreachable!("the watchdog starts after the refinement")
        }
    };
    // A TSC that is not invariant only becomes the clock with no HPET or
    // PM timer, so it has nothing to fall back to.
    if clock::kind() != Some(Clock::Tsc) {
        return idle(Idle::ClockNotTsc(clock::kind()));
    }
    if clock::fallback().is_none() {
        return idle(Idle::NoReference);
    }
    if !events {
        return idle(Idle::NoEvents);
    }
    let reference = super::get()
        .and_then(|t| t.reference_counter())
        .expect("the refinement ran against it");
    let rf = reference.freq().expect("a reference has a frequency");
    let freq = clock::freq().expect("the clock is running");
    let pre = clock::tsc_raw().expect("the clock is the TSC");
    let Some(raw) = reference.raw() else {
        set(Status::Stopped {
            against,
            why: Stop::Unreadable,
        });
        return None;
    };
    let stamp = clock::tsc_raw().expect("the clock is the TSC");
    let watch = Watch::new(
        freq,
        rf,
        reference.bits(),
        rf.ns_to_ticks_ceil(READ_SLACK_NS),
        pre,
        raw,
        stamp,
    )
    .expect("a counter is 1 to 64 bits wide");
    #[cfg(feature = "ktest")]
    super::fake::arm(crate::arch::cpu::rdtsc_ordered(), freq);
    *RUN.lock() = Some(Run {
        watch,
        reference,
        against,
        bound_ppb,
        why: None,
    });
    set(Status::Watching { against });
    Some(now.saturating_add(wd::CHECK_NS))
}

/// A check at clock time `now` (crate::timer::interrupt, the timer's lock
/// held).
pub fn poll(now: u64) -> Poll {
    let mut guard = RUN.lock();
    let Some(run) = guard.as_mut() else {
        return Poll::Stop;
    };
    let tsc = || clock::tsc_raw().expect("watched only while the clock is the TSC");
    let blank = Read {
        before: 0,
        clock: 0,
        after: 0,
        stamp: 0,
    };
    let mut reads = [blank; READS];
    let lead = tsc();
    for r in reads.iter_mut() {
        let (Some(before), clock, Some(after), stamp) =
            (run.reference.raw(), tsc(), run.reference.raw(), tsc())
        else {
            set(Status::Stopped {
                against: run.against,
                why: Stop::Unreadable,
            });
            *guard = None;
            return Poll::Stop;
        };
        *r = Read {
            before,
            clock,
            after,
            stamp,
        };
    }
    match run.watch.check(lead, &reads) {
        Verdict::Steady | Verdict::Restarted => Poll::Next(now.saturating_add(wd::CHECK_NS)),
        Verdict::ReferenceBackwards => {
            set(Status::Stopped {
                against: run.against,
                why: Stop::ReferenceBackwards,
            });
            *guard = None;
            Poll::Stop
        }
        Verdict::Unstable(why) => {
            let (floor_ns, back_ns) = match why {
                Unstable::Backwards(ticks) => {
                    // From the newest sample, which the old timeline
                    // converts, plus the most time since by a fresh
                    // reference read.
                    let last_ns = run
                        .watch
                        .last()
                        .and_then(|s| clock::ns_at(s.target))
                        .unwrap_or(now);
                    let raw = run.reference.raw().unwrap_or(0);
                    let back = clock::freq().map_or(0, |f| f.ticks_to_ns(ticks));
                    (run.watch.floor_ns(last_ns, raw).max(now), back)
                }
                Unstable::Rate(_) | Unstable::Jumped => (0, 0),
            };
            run.why = Some((why, back_ns));
            Poll::Fall {
                floor_ns,
                bound_ppb: run.bound_ppb,
            }
        }
    }
}

/// After [`Poll::Fall`]: record what crate::timer did, at clock time
/// `now`, and stop.
pub fn fired(now: u64, to: Option<(Clock, u64)>, events: Option<Events>) {
    let mut guard = RUN.lock();
    let Some(run) = guard.as_ref() else { return };
    let (why, back_ns) = run.why.expect("fired only after an unstable verdict");
    set(Status::Fired {
        against: run.against,
        why,
        back_ns,
        // On the new clock: after going back, the old one's time is
        // behind.
        at_ns: to.map_or(now, |(_, base)| base),
        to,
        events,
    });
    *guard = None;
}

/// Is a line waiting to be printed by [`report`]?
pub fn pending() -> bool {
    let r = REPORTED.load(Ordering::SeqCst);
    match status() {
        Status::NotStarted => false,
        Status::Idle(_) | Status::Watching { .. } => r & 1 == 0,
        Status::Stopped { .. } | Status::Fired { .. } => r != 3,
    }
}

/// The tests: spin (interrupts on) until the watchdog has started or said
/// why not, for at most `limit_ns` of the clock. True if it did.
#[cfg(feature = "ktest")]
pub fn wait_start(limit_ns: u64) -> bool {
    let t0 = clock::now_ns();
    while status() == Status::NotStarted {
        if clock::now_ns().saturating_sub(t0) > limit_ns {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn name(c: Clock) -> &'static str {
    match c {
        Clock::Tsc | Clock::TscNotInvariant => "tsc",
        Clock::Hpet => "hpet",
        Clock::PmTimer => "pm-timer",
    }
}

fn reference_name(r: Reference) -> &'static str {
    match r {
        Reference::Hpet => "hpet",
        Reference::PmTimer => "pm-timer",
        Reference::Pit => "pit",
    }
}

/// The first line: watching, or why not.
pub fn first_line(s: Status) -> (String, Vec<String>) {
    let mut w = Vec::new();
    let mut line = String::from("watchdog: ");
    let unwatched = "the TSC is not watched for instability";
    match s {
        Status::NotStarted => line.push_str("not started"),
        Status::Idle(Idle::ClockNotTsc(k)) => {
            let _ = write!(line, "none (the clock is the {})", k.map_or("none", name));
        }
        Status::Idle(Idle::NoReference) => {
            line.push_str("none (no hpet or pm timer to check the tsc against)");
            w.push(String::from(unwatched));
        }
        Status::Idle(Idle::NoEvents) => {
            line.push_str("none (no timer events)");
            w.push(String::from(unwatched));
        }
        Status::Idle(Idle::NotRefined) => {
            line.push_str("none (the refinement did not finish)");
            w.push(String::from(unwatched));
        }
        Status::Watching { against }
        | Status::Stopped { against, .. }
        | Status::Fired { against, .. } => {
            let _ = write!(
                line,
                "watching the tsc against the {} every {} s",
                reference_name(against),
                wd::CHECK_NS / 1_000_000_000
            );
        }
    }
    let _ = write!(line, "; {} warnings", w.len());
    (line, w)
}

/// The outcome line, once there is one.
pub fn outcome_line(s: Status) -> Option<(String, Vec<String>)> {
    let mut w = Vec::new();
    let mut line = String::from("watchdog: ");
    match s {
        Status::Stopped { against, why } => {
            let r = reference_name(against);
            let what = match why {
                Stop::ReferenceBackwards => format!("the {r} went backwards"),
                Stop::Unreadable => format!("the {r} gave no consistent reading"),
            };
            let _ = write!(line, "stopped: {what}");
            w.push(format!("the TSC is no longer watched: {what}"));
        }
        Status::Fired {
            against,
            why,
            back_ns,
            at_ns,
            to,
            events,
        } => {
            match why {
                Unstable::Backwards(_) => {
                    let _ = write!(line, "the tsc went backwards by {back_ns} ns");
                }
                Unstable::Rate(off) => {
                    let _ = write!(
                        line,
                        "the tsc ran {} ppm from its frequency over {} checks",
                        refine::Off(off),
                        wd::CHECKS
                    );
                }
                Unstable::Jumped => {
                    let _ = write!(
                        line,
                        "the tsc and the {} disagreed by more than a factor of 2",
                        reference_name(against)
                    );
                }
            }
            let _ = write!(line, " at {} ms; ", at_ns / 1_000_000);
            match to {
                Some((k, _)) => {
                    let _ = write!(
                        line,
                        "the clock is now the {}, events {}",
                        name(k),
                        crate::timer::events_name(events)
                    );
                    w.push(format!(
                        "the TSC is unstable, so the clock fell back to the {}",
                        name(k)
                    ));
                }
                None => {
                    let _ = write!(
                        line,
                        "the {} gave no reading, so the clock stays the tsc",
                        reference_name(against)
                    );
                    w.push(String::from(
                        "the TSC is unstable, but the clock could not fall back",
                    ));
                }
            }
        }
        _ => return None,
    }
    let _ = write!(line, "; {} warnings", w.len());
    Some((line, w))
}

/// Print whatever has not been printed yet: the first line once the
/// watchdog has started (or said why not), the outcome once there is one.
pub fn report() {
    let s = status();
    if s == Status::NotStarted {
        return;
    }
    if REPORTED.fetch_or(1, Ordering::SeqCst) & 1 == 0 {
        let (line, w) = first_line(s);
        crate::println!("{line}");
        for x in w {
            crate::println!("watchdog: warning: {x}");
        }
    }
    if let Some((line, w)) = outcome_line(s)
        && REPORTED.fetch_or(2, Ordering::SeqCst) & 2 == 0
    {
        crate::println!("{line}");
        for x in w {
            crate::println!("watchdog: warning: {x}");
        }
    }
}

#[cfg(feature = "ktest")]
pub mod tests;
