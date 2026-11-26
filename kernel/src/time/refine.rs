//! The 10 s refinement of the TSC's frequency (ROADMAP 1c), driven by
//! timer events. optimal_time::refine makes every decision; this file
//! reads the counters and applies the result.
//!
//! 1. [`start`] (crate::timer::init): when the clock is the TSC and the
//!    reference is the HPET or the PM timer (ROADMAP 1c refines against
//!    those two only), take a bracketed start sample, reference against
//!    TSC, the tightest of [`TRIES`].
//! 2. [`poll`] (crate::timer::interrupt, interrupts off, the timer's lock
//!    held): every optimal_time::refine::poll_ns of a narrow reference,
//!    read it, counting wraps from the TSC (optimal_time::refine::unwrap)
//!    and refusing if the count is not certain. After 10 s of the clock,
//!    take the end sample and estimate the TSC's frequency.
//! 3. optimal_time::refine::decide: keep the boot value, or rebase the
//!    clock (crate::clock::rebase) to the new frequency and carry the
//!    LAPIC timer's rate across (optimal_time::refine::rescale), all
//!    under the timer's lock so no arming mixes old and new.
//!
//! Anything that goes wrong is a [`Failure`]: the TSC keeps its boot
//! value and [`report`] gives a warning. Nothing here allocates or
//! prints, because it runs in interrupt context; the ktests print the
//! result.

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use optimal_time::calibrate::{self, Estimate, Refused, Sample};
use optimal_time::counter::{self, Extend};
use optimal_time::cpuid::Source;
use optimal_time::freq::Freq;
use optimal_time::lapic as lt;
use optimal_time::refine::{self as rf, Boot, Outcome};
use optimal_time::select::{Clock, Reference};

#[cfg(feature = "ktest")]
use super::fake;
use super::{CPUID_SLACK_PPB, Counter, Mhz, Ppb, Time, TscFreq, source_name};
use crate::arch::cpu;
use crate::clock;
use crate::sync::IrqSpinLock;
use crate::timer::Rates;

/// Samples at each end; the tightest is used.
const TRIES: usize = 8;
/// Added on both sides of the TSC's estimate of the reference ticks
/// between two reads (optimal_time::refine::expected), for the time a
/// read itself takes: 1 ms. It only widens the range the wrap count must
/// be unique in, so it can make the refinement refuse, never be wrong.
const READ_SLACK_NS: u64 = 1_000_000;
/// A refined bound looser than this is a warning: 10 ppm, the boot
/// measurement's own goal (crate::time::GOAL_PPB), so a refinement that
/// cannot beat it is reported. Typically it reaches well under 1 ppm (0.8
/// ppm under TCG, where every HPET read is emulated). The ROADMAP's 1 ppm
/// is a drift bound for the Phase 1 done check on the dedicated KVM host,
/// measured against the host's clock; on hosted runners each HPET read is
/// a nested VM exit, which widens the samples, so it is not a per-boot
/// warning. (Chosen: warn at 10 ppm. Rejected: warn at 1 ppm, which would
/// flag hosted runners rather than the kernel.)
pub const WARN_PPB: u64 = super::GOAL_PPB;

/// Why the refinement did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// The clock is not the TSC (or there is none): nothing to refine.
    ClockNotTsc(Option<Clock>),
    /// The reference is the PIT, which ROADMAP 1c does not refine
    /// against. Holds the boot bound the TSC keeps.
    Pit(u64),
    /// No reference at all (a CPUID claim with no ACPI and no PIT).
    NoReference,
    /// No timer events to time the window.
    NoEvents,
}

/// Why it stopped without a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The reference gave no consistent reading.
    Unreadable,
    /// The number of wraps between two reads of a narrow reference could
    /// not be proven (the vCPU stopped for too long, or the TSC is off by
    /// more than a factor of 2).
    WrapsUnknown,
    /// Every sample at one end had the reference going backwards.
    Backwards,
    Estimate(Refused),
    /// The LAPIC timer's rate could not be carried across.
    Rescale,
    /// The clock refused the new frequency.
    Rebase,
}

#[derive(Clone, Copy, Debug)]
pub enum Status {
    NotStarted,
    Running,
    Skipped(Skip),
    Failed(Failure),
    Done {
        boot: Boot,
        /// Where a claimed boot value came from.
        source: Option<Source>,
        measured: Estimate,
        outcome: Outcome,
        against: Reference,
        /// The window, in milliseconds of the reference.
        ms: u64,
    },
}

struct Run {
    reference: Counter<'static>,
    against: Reference,
    rf: Freq,
    bits: u32,
    ext: Extend,
    /// The TSC right after the last reference read.
    last_tsc: u64,
    /// The TSC's frequency at the start, for counting wraps.
    tsc: Freq,
    slack: u64,
    start: Sample,
    boot: Boot,
    source: Option<Source>,
    end_ns: u64,
    poll_ns: u64,
}

// SAFETY (for IrqSpinLock's Send bound): Counter holds shared references
// to the HPET and PM timer descriptions, which live in a Once forever
// and are only read.
static RUN: IrqSpinLock<Option<Run>> = IrqSpinLock::new(None);
static STATUS: IrqSpinLock<Status> = IrqSpinLock::new(Status::NotStarted);
static REPORTED: AtomicBool = AtomicBool::new(false);

impl Run {
    /// The reference extended to 64 bits, with the wraps since the last
    /// read proven from the TSC.
    fn read_ref(&mut self) -> Result<u64, Failure> {
        let raw = self.reference.raw().ok_or(Failure::Unreadable)?;
        let tsc = cpu::rdtsc_ordered();
        let total = if self.bits == 64 {
            self.ext.update(raw)
        } else {
            let d = counter::delta(self.ext.last(), raw, self.bits);
            let (lo, hi) = rf::expected(tsc.wrapping_sub(self.last_tsc), self.tsc, self.rf)
                .ok_or(Failure::WrapsUnknown)?;
            let ticks = rf::unwrap(
                d,
                self.bits,
                lo.saturating_sub(self.slack),
                hi.saturating_add(self.slack),
            )
            .ok_or(Failure::WrapsUnknown)?;
            self.ext.advance(raw, ticks).ok_or(Failure::WrapsUnknown)?
        };
        self.last_tsc = tsc;
        Ok(total)
    }

    /// The tightest of [`TRIES`] samples. Interrupts are off (the
    /// callers hold the timer's lock).
    fn sample(&mut self) -> Result<Sample, Failure> {
        let mut s = [Sample {
            before: 0,
            target: 0,
            after: 0,
        }; TRIES];
        for x in s.iter_mut() {
            let before = self.read_ref()?;
            let target = cpu::rdtsc_ordered();
            let after = self.read_ref()?;
            *x = Sample {
                before,
                target,
                after,
            };
        }
        calibrate::tightest(&s).ok_or(Failure::Backwards)
    }
}

fn set(s: Status) {
    *STATUS.lock() = s;
}

/// Start the window at clock time `now`, when there is anything to
/// refine. Returns when [`poll`] is first due. With the timer's lock held.
pub fn start(t: &'static Time, now: u64, events: bool) -> Option<u64> {
    let kind = clock::kind();
    if !matches!(kind, Some(Clock::Tsc | Clock::TscNotInvariant)) {
        set(Status::Skipped(Skip::ClockNotTsc(kind)));
        return None;
    }
    let (boot, source) = match t.tsc {
        Some(TscFreq::Measured(e)) => (Boot::Measured(e), None),
        Some(TscFreq::Claimed(c)) => (
            Boot::Claimed {
                freq: c.freq,
                slack_ppb: c.resolution_ppb.saturating_add(CPUID_SLACK_PPB),
            },
            Some(c.source),
        ),
        None => unreachable!("the TSC is only the clock with a frequency"),
    };
    let against = match t.reference {
        Some(r @ (Reference::Hpet | Reference::PmTimer)) => r,
        Some(Reference::Pit) => {
            let ppb = t.tsc_estimate().map_or(0, |e| e.ppb);
            set(Status::Skipped(Skip::Pit(ppb)));
            return None;
        }
        None => {
            set(Status::Skipped(Skip::NoReference));
            return None;
        }
    };
    if !events {
        set(Status::Skipped(Skip::NoEvents));
        return None;
    }
    let reference = t.reference_counter().expect("the reference exists");
    let rfreq = reference.freq().expect("a reference has a frequency");
    let bits = reference.bits();
    let Some(first) = reference.raw() else {
        set(Status::Failed(Failure::Unreadable));
        return None;
    };
    let mut run = Run {
        reference,
        against,
        rf: rfreq,
        bits,
        ext: Extend::new(bits, first),
        last_tsc: cpu::rdtsc_ordered(),
        tsc: clock::freq().expect("the clock is running"),
        slack: rfreq.ns_to_ticks_ceil(READ_SLACK_NS),
        start: Sample {
            before: 0,
            target: 0,
            after: 0,
        },
        boot,
        source,
        end_ns: now.saturating_add(rf::WINDOW_NS),
        poll_ns: rf::poll_ns(rfreq, bits),
    };
    match run.sample() {
        Ok(s) => run.start = s,
        Err(f) => {
            set(Status::Failed(f));
            return None;
        }
    }
    let next = now.saturating_add(run.poll_ns).min(run.end_ns);
    *RUN.lock() = Some(run);
    set(Status::Running);
    Some(next)
}

/// Due at clock time `now` (crate::timer::interrupt, the timer's lock
/// held, so `rates` are the timer's). Returns when it is next due.
pub fn poll(now: u64, rates: &mut Rates) -> Option<u64> {
    let mut guard = RUN.lock();
    let run = guard.as_mut()?;
    if now < run.end_ns {
        if let Err(f) = run.read_ref() {
            *guard = None;
            set(Status::Failed(f));
            return None;
        }
        return Some(now.saturating_add(run.poll_ns).min(run.end_ns));
    }
    let result = finish(run, rates);
    *guard = None;
    set(match result {
        Ok(s) => s,
        Err(f) => Status::Failed(f),
    });
    None
}

fn finish(run: &mut Run, rates: &mut Rates) -> Result<Status, Failure> {
    let end = run.sample()?;
    let measured = calibrate::estimate(run.rf, run.start, end).map_err(Failure::Estimate)?;
    let outcome = rf::decide(run.boot, measured);
    if outcome.adopts() {
        let old = clock::freq().ok_or(Failure::Rebase)?;
        // Everything that can fail is worked out before anything changes.
        let lapic = match rates.lapic {
            Some(l) => Some(rf::rescale(l, old, measured.freq).ok_or(Failure::Rescale)?),
            None => None,
        };
        if !clock::rebase(measured.freq) {
            return Err(Failure::Rebase);
        }
        rates.lapic = lapic;
        if let Some(l) = lapic {
            rates.longest_ns = lt::longest_ns(l.freq, l.ppb);
        }
        if rates.tsc.is_some() {
            rates.tsc = Some(measured.freq);
        }
    }
    Ok(Status::Done {
        boot: run.boot,
        source: run.source,
        measured,
        outcome,
        against: run.against,
        ms: run.rf.ticks_to_ns(measured.window) / 1_000_000,
    })
}

/// How long the tests wait for the refinement: its window plus 5 s for a
/// loaded host.
#[cfg(feature = "ktest")]
pub const WAIT_NS: u64 = rf::WINDOW_NS + 5_000_000_000;

pub fn status() -> Status {
    *STATUS.lock()
}

/// The bound the tests allow on the TSC's frequency against the
/// reference: the boot bound, or the refined one when the clock took it
/// and it is wider. A test window spanning the rebase runs partly at each,
/// so it is within the larger. A faked wrong claim (crate::time::fake) is
/// always replaced before the tests start.
#[cfg(feature = "ktest")]
pub fn tsc_bound_ppb() -> u64 {
    let boot = super::get()
        .and_then(|t| t.tsc_estimate())
        .map_or(0, |e| e.ppb);
    match status() {
        Status::Done {
            measured, outcome, ..
        } if outcome.adopts() => match fake::current().claim_ppm {
            Some(_) => measured.ppb,
            None => boot.max(measured.ppb),
        },
        _ => boot,
    }
}

/// Has it finished, or been skipped?
pub fn finished() -> bool {
    !matches!(status(), Status::NotStarted | Status::Running)
}

/// Spin (interrupts on) until the refinement has finished or been
/// skipped, for at most `limit_ns` of the clock. True if it finished.
#[cfg(feature = "ktest")]
pub fn wait(limit_ns: u64) -> bool {
    let t0 = clock::now_ns();
    loop {
        if finished() {
            return true;
        }
        if clock::now_ns().saturating_sub(t0) > limit_ns {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// A signed offset in ppb as ppm with one decimal, rounded toward zero.
pub struct Off(pub i64);

impl core::fmt::Display for Off {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let tenths = self.0.unsigned_abs() / 100;
        let sign = if self.0 < 0 { '-' } else { '+' };
        write!(f, "{sign}{}.{}", tenths / 10, tenths % 10)
    }
}

fn clock_name(k: Option<Clock>) -> &'static str {
    match k {
        Some(Clock::Tsc | Clock::TscNotInvariant) => "tsc",
        Some(Clock::Hpet) => "hpet",
        Some(Clock::PmTimer) => "pm-timer",
        None => "none",
    }
}

/// The `refine:` line and its warnings.
pub fn lines(s: Status) -> (String, Vec<String>) {
    let mut w = Vec::new();
    let mut line = String::from("refine: ");
    match s {
        Status::NotStarted | Status::Running => line.push_str("running"),
        Status::Skipped(Skip::ClockNotTsc(k)) => {
            let _ = write!(line, "none (the clock is the {})", clock_name(k));
        }
        Status::Skipped(Skip::Pit(ppb)) => {
            line.push_str("none (the reference is the pit)");
            w.push(format!(
                "the TSC keeps its boot measurement (+/-{} ppm): the refinement needs the HPET or the PM timer",
                Ppb(ppb)
            ));
        }
        Status::Skipped(Skip::NoReference) => {
            line.push_str("none (no reference)");
            w.push(String::from(
                "the TSC keeps its CPUID frequency, unchecked: there is no reference to check it against",
            ));
        }
        Status::Skipped(Skip::NoEvents) => {
            line.push_str("none (no timer events)");
            w.push(String::from(
                "the TSC keeps its boot value: there are no timer events to time the refinement",
            ));
        }
        Status::Failed(f) => {
            line.push_str("failed");
            w.push(format!(
                "{}; the TSC keeps its boot value",
                match f {
                    Failure::Unreadable => String::from("the reference gave no consistent reading"),
                    Failure::WrapsUnknown => String::from(
                        "the reference's wraps between two reads could not be counted for certain",
                    ),
                    Failure::Backwards => String::from("the reference went backwards"),
                    Failure::Estimate(r) => format!("the measurement was refused ({r:?})"),
                    Failure::Rescale =>
                        String::from("the LAPIC timer's rate could not be carried across"),
                    Failure::Rebase => String::from("the clock refused the new frequency"),
                }
            ));
        }
        Status::Done {
            boot,
            source,
            measured,
            outcome,
            against,
            ms,
        } => {
            let _ = write!(
                line,
                "tsc {} +/-{} ppm against the {} over {ms} ms; ",
                Mhz(measured.freq),
                Ppb(measured.ppb),
                match against {
                    Reference::Hpet => "hpet",
                    Reference::PmTimer => "pm-timer",
                    Reference::Pit => "pit",
                }
            );
            let src = source.map_or("", source_name);
            match (outcome, boot) {
                (Outcome::Refined, Boot::Measured(b)) => {
                    let _ = write!(
                        line,
                        "refined the boot measurement {} +/-{} ppm by {} ppm",
                        Mhz(b.freq),
                        Ppb(b.ppb),
                        Off(b.freq.ppb_from(measured.freq))
                    );
                }
                (Outcome::KeptBoot, Boot::Measured(b)) => {
                    let _ = write!(
                        line,
                        "kept the boot measurement {} +/-{} ppm",
                        Mhz(b.freq),
                        Ppb(b.ppb)
                    );
                }
                (Outcome::Confirmed(off), Boot::Claimed { freq, .. }) => {
                    let _ = write!(
                        line,
                        "confirmed CPUID's {} ({src}) to {} ppm",
                        Mhz(freq),
                        Off(off)
                    );
                }
                (Outcome::ClaimWrong(off), Boot::Claimed { freq, .. }) => {
                    let _ = write!(
                        line,
                        "CPUID's {} ({src}) is wrong; using the measurement",
                        Mhz(freq)
                    );
                    w.push(format!(
                        "CPUID's TSC frequency {} ({src}) is wrong: the TSC runs {} ppm from it",
                        Mhz(freq),
                        Off(off)
                    ));
                }
                (Outcome::Changed(off), Boot::Measured(b)) => {
                    let _ = write!(
                        line,
                        "the TSC's rate changed since boot; using the measurement"
                    );
                    w.push(format!(
                        "the TSC measured {} +/-{} ppm at boot but {} ppm from that over 10 s: its rate is not steady",
                        Mhz(b.freq),
                        Ppb(b.ppb),
                        Off(off)
                    ));
                }
                (o, b) => unreachable!("decide gave {o:?} for {b:?}"),
            }
            if measured.ppb > WARN_PPB {
                w.push(format!(
                    "the refinement only reached +/-{} ppm",
                    Ppb(measured.ppb)
                ));
            }
        }
    }
    let _ = write!(line, "; {} warnings", w.len());
    (line, w)
}

/// Print the `refine:` line and its warnings, once (the tests and the
/// idle loop both ask).
pub fn report() {
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (line, w) = lines(status());
    crate::println!("{line}");
    for x in w {
        crate::println!("refine: warning: {x}");
    }
}

#[cfg(feature = "ktest")]
mod tests;
