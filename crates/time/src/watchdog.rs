//! The clocksource watchdog (ROADMAP 1c): is the TSC still keeping time?
//!
//! Once the 10 s refinement has finished, the kernel checks the clock (the
//! TSC) against the reference (the HPET, else the PM timer) every
//! [`CHECK_NS`]. It detects only instability, never a small, steady
//! frequency error (that is the refinement's job):
//!
//! - **Going backwards**: a clock reading below one taken before it.
//! - **500 ppm or more over 10 checks**: across the last [`CHECKS`] check
//!   intervals the clock provably ran at least [`LIMIT_PPB`] from its own
//!   frequency, measured against the reference. "Provably" means the whole
//!   error bound of the measurement ([`crate::calibrate::estimate`]) lies
//!   500 ppm or more away ([`deviates`]), so a noisy check (an SMI, a
//!   descheduled vCPU) can never make the watchdog fire.
//! - **A jump between two reads**: counting a narrow reference's wraps
//!   needs the clock to be within a factor of 2 of right over the gap
//!   between two reads (as in the refinement). When no wrap count fits,
//!   the clock was off by more than that, which is 500 ppm many times
//!   over.
//!
//! Each check reads the clock (the lead), then takes [`READS`] bracketed
//! reads (reference, clock, reference, clock) and keeps the tightest, the
//! "minimum of 3 bracketed reads" of ROADMAP 1c. So every reference read
//! has a clock read just before and just after it, and the reference
//! ticks between two reference reads lie between the clock's time from
//! the read after the first to the read before the second, and from the
//! read before the first to the read after the second. That holds
//! whatever stalls the reads (an SMI, a vCPU stopped by its host): a stall
//! only widens the range. Counted against half to twice those clock times
//! ([`count`]), a 24-bit PM timer, which wraps every 4.7 s, is extended
//! exactly.
//!
//! What cannot be measured is said so, never guessed. When the clock says
//! so much time passed between two reads that more than one wrap count
//! fits (about a wrap for a 24-bit PM timer: 4.7 s, which a vCPU stopped
//! by its host can cause), the window starts again from that check
//! ([`Verdict::Restarted`]). With a narrow reference, then, a clock that
//! jumped forward by several seconds at once inside such a gap is not
//! caught, and neither is one that jumped by within a few milliseconds of
//! a whole number of wraps between two reads (that looks like time
//! passing); with the HPET (64 bits, or 32 bits wrapping every 5 minutes
//! at 14.3 MHz) neither can happen. A clock that stops entirely also stops
//! the timer events that run the checks (they are timed by the clock);
//! falling back to HPET timer events is a later step of ROADMAP 1c.
//!
//! When the watchdog fires, the kernel falls back to the reference as its
//! clock ([`crate::select::clock`]'s next choice). The new timeline starts
//! where the old clock was ([`crate::timeline::Timeline`]), except after
//! going backwards, when it starts at [`Watch::floor_ns`] or later, so it
//! never gives less than any reading made while the clock still worked.
//! The LAPIC timer's rate, measured against the TSC, is carried across
//! with the TSC's own bound against the reference added ([`carry`]).

use crate::calibrate::{self, Estimate, MAX_TOLERANCE_PPB, Sample};
use crate::counter::{self, Extend};
use crate::freq::Freq;
use crate::refine::{Boot, Outcome, expected};

/// How often the clock is checked, in nanoseconds of the clock: 1 s.
/// (Chosen: 1 s, so the window of [`CHECKS`] checks is the refinement's
/// 10 s and an idle machine wakes once a second for it. Rejected: 0.5 s,
/// which halves the window and doubles the wake-ups for no extra
/// instability caught: a backwards step shows at the next check either
/// way.)
pub const CHECK_NS: u64 = 1_000_000_000;

/// Check intervals the 500 ppm rule is measured over.
pub const CHECKS: usize = 10;

/// The rate rule's limit: 500 ppm.
pub const LIMIT_PPB: u64 = 500_000;

/// Bracketed reads per check; the tightest is used.
pub const READS: usize = 3;

/// The reference ticks between two reads of a counter of `bits` bits
/// (1 to 64) whose raw difference was `raw_delta`, when the truth is known
/// to lie in `lo..=hi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// Exactly one count fits.
    One(u64),
    /// None fits: the truth is not in `lo..=hi`.
    None,
    /// More than one fits: the number of wraps is unknown.
    Many,
}

/// See [`Count`]. Like optimal_time::refine::unwrap, but says why there is
/// no answer.
pub fn count(raw_delta: u64, bits: u32, lo: u64, hi: u64) -> Count {
    assert!((1..=64).contains(&bits));
    // (An empty range, lo > hi, needs no case of its own: t >= lo > hi.)
    let m = 1u128 << bits;
    let (lo, hi) = (u128::from(lo), u128::from(hi));
    // The smallest t >= lo with t = raw_delta (mod m); below 2^65.
    let t = lo + (u128::from(raw_delta & counter::mask(bits)) + m - lo % m) % m;
    if t > hi {
        Count::None
    } else if t + m <= hi {
        Count::Many
    } else {
        // t <= hi < 2^64.
        Count::One(t as u64)
    }
}

/// Did the clock provably run `limit_ppb` or more from `nominal` over the
/// measurement `e` (its frequency against the reference, with a bound)?
/// Then `Some` of how far `e` is from `nominal`, in ppb of `nominal`
/// (positive when the clock ran fast). Exact.
///
/// The true frequency T lies within e.ppb of F = e.freq. Every such T is
/// at least `limit` from E = nominal, on the fast side, when
/// F (1 - p) >= E (1 + L), and on the slow side when F (1 + p) <=
/// E (1 - L). With F = fn/fd and E = en/ed that is fn ed (10^9 - p) >=
/// en fd (10^9 + L), and so on: cross products below 2^96, times at most
/// 10^9 + 2^31 < 2^32, so below 2^128.
pub fn deviates(e: Estimate, nominal: Freq, limit_ppb: u64) -> Option<i64> {
    let g = 1_000_000_000u128;
    let p = u128::from(e.ppb.min(MAX_TOLERANCE_PPB));
    let l = u128::from(limit_ppb.min(MAX_TOLERANCE_PPB));
    let f = u128::from(e.freq.num()) * u128::from(nominal.den());
    let n = u128::from(nominal.num()) * u128::from(e.freq.den());
    let fast = f * g.saturating_sub(p) >= n * (g + l);
    let slow = f * (g + p) <= n * g.saturating_sub(l);
    (fast || slow).then(|| nominal.ppb_from(e.freq))
}

/// How far the TSC's frequency, as the clock now uses it, can be from the
/// truth measured against the reference, in ppb, after the refinement
/// decided `outcome` for `boot` and its measurement `measured`:
///
/// - It took the measurement: the measurement's bound.
/// - It kept a measured boot value: that value's bound (measured against
///   the same reference).
/// - It kept a CPUID claim C the measurement M confirmed `off` ppb away
///   (in ppb of M): the truth T is within p = measured.ppb of M, so
///   |T - C| <= (|off| + p) M, and since |M - C| = |off| M, M / C <=
///   1 / (1 - |off|) <= 1 + 2 |off| for |off| <= 1/2. So T is within
///   q (1 + 2 |off|) of C, with q = |off| + p: q + ceil(2 q |off| / 10^9)
///   ppb. (An agreeing claim is within 200 ppm and a bit of M, far inside
///   1/2.)
pub fn tsc_bound_ppb(boot: Boot, measured: Estimate, outcome: Outcome) -> u64 {
    match (outcome, boot) {
        (Outcome::KeptBoot, Boot::Measured(b)) => b.ppb,
        (Outcome::Confirmed(off), _) => {
            let o = u128::from(off.unsigned_abs());
            let q = o + u128::from(measured.ppb);
            let bound = q + (2 * q * o).div_ceil(1_000_000_000);
            u64::try_from(bound).unwrap_or(u64::MAX)
        }
        _ => measured.ppb,
    }
}

/// The LAPIC timer's rate `rate`, measured against a clock that is itself
/// within `bound_ppb` of the reference, restated against the reference:
/// the same frequency, with the bounds combined exactly. Two ratios each
/// within (1 +/- a) and (1 +/- b) of 1 multiply to within
/// 1 +/- (a + b + ab), so the new bound is a + b + ceil(ab / 10^9).
pub fn carry(rate: Estimate, bound_ppb: u64) -> Estimate {
    let a = u128::from(rate.ppb);
    let b = u128::from(bound_ppb);
    let ab = (a * b).div_ceil(1_000_000_000);
    let ppb = u64::try_from(a + b + ab).unwrap_or(u64::MAX);
    Estimate { ppb, ..rate }
}

/// One bracketed read: the reference (raw, as the counter gives it), the
/// clock, the reference again, then the clock once more, back to back.
/// With the check's lead (a clock read first), every reference read has a
/// clock read on each side, which is what counts the reference's wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Read {
    pub before: u64,
    pub clock: u64,
    pub after: u64,
    pub stamp: u64,
}

/// Why the clock is unstable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unstable {
    /// A clock reading was this many ticks below the one before it.
    Backwards(u64),
    /// Over the last [`CHECKS`] checks the clock ran this many ppb from
    /// its frequency (positive when fast), 500 ppm or more for certain.
    Rate(i64),
    /// Between two reads the clock and the reference disagreed by more
    /// than a factor of 2.
    Jumped,
}

/// What one check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing wrong could be proven (including while the first window
    /// is still filling).
    Steady,
    /// So long passed since the last read that the reference's wraps
    /// cannot be counted: the window starts again from this check.
    Restarted,
    /// A 64-bit reference read below the read before it: the reference,
    /// not the clock, is broken, so the watchdog stops.
    ReferenceBackwards,
    Unstable(Unstable),
}

/// The watchdog's state: the reference extended to 64 bits, and the
/// tightest sample of each of the last [`CHECKS`] + 1 checks.
#[derive(Clone, Debug)]
pub struct Watch {
    clock: Freq,
    reference: Freq,
    bits: u32,
    slack: u64,
    ext: Extend,
    /// The clock right before and right after the last reference read.
    pre: u64,
    stamp: u64,
    ring: [Sample; CHECKS + 1],
    /// Samples in the ring (at most CHECKS + 1).
    len: usize,
    /// Where the next sample goes.
    next: usize,
    checks: u64,
    restarts: u64,
}

const ZERO: Sample = Sample {
    before: 0,
    target: 0,
    after: 0,
};

/// How a reference read moved the extension.
enum Step {
    Ok(u64),
    Stop(Verdict),
}

impl Watch {
    /// Start from a reference read `raw` (raw, as the counter gives it),
    /// with the clock read `pre` right before it and `stamp` right after. `clock` is the clock's
    /// frequency now, `reference` the reference's nominal frequency and
    /// `bits` its width (1 to 64). `slack` reference ticks are added on
    /// both sides of the clock's estimate when counting wraps, for the
    /// time each read takes. `None` for a width outside 1 to 64.
    pub fn new(
        clock: Freq,
        reference: Freq,
        bits: u32,
        slack: u64,
        pre: u64,
        raw: u64,
        stamp: u64,
    ) -> Option<Watch> {
        if !(1..=64).contains(&bits) || stamp < pre {
            return None;
        }
        Some(Watch {
            clock,
            reference,
            bits,
            slack,
            ext: Extend::new(bits, raw),
            pre,
            stamp,
            ring: [ZERO; CHECKS + 1],
            len: 0,
            next: 0,
            checks: 0,
            restarts: 0,
        })
    }

    /// Checks made so far, restarts included.
    pub fn checks(&self) -> u64 {
        self.checks
    }

    /// How many checks restarted the window.
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// The newest sample in the window, if any.
    pub fn last(&self) -> Option<Sample> {
        (self.len > 0).then(|| self.ring[(self.next + CHECKS) % (CHECKS + 1)])
    }

    /// The reference read `raw`, between the clock reads `pre` and
    /// `stamp`, as ticks since the start. The ticks since the last
    /// reference read are more than half the clock's time from the last
    /// read's stamp to `pre` and less than twice its time from the last
    /// read's pre to `stamp`, when the clock is within a factor of 2 of
    /// right.
    /// The clock reads must be in order ([`Watch::check`] made sure).
    fn advance(&mut self, pre: u64, raw: u64, stamp: u64) -> Step {
        let last = self.ext.last();
        if self.bits == 64 && raw < last {
            return Step::Stop(Verdict::ReferenceBackwards);
        }
        // Past calibrate::MAX_WINDOW clock ticks (minutes) the wraps
        // cannot be counted.
        let (Some((lo, _)), Some((_, hi))) = (
            expected(pre - self.stamp, self.clock, self.reference),
            expected(stamp - self.pre, self.clock, self.reference),
        ) else {
            return Step::Stop(Verdict::Restarted);
        };
        let lo = lo.saturating_sub(self.slack);
        let hi = hi.saturating_add(self.slack);
        match count(counter::delta(last, raw, self.bits), self.bits, lo, hi) {
            Count::One(t) => match self.ext.advance(raw, t) {
                Some(total) => {
                    self.pre = pre;
                    self.stamp = stamp;
                    Step::Ok(total)
                }
                // The total passed u64::MAX: centuries of the reference.
                None => Step::Stop(Verdict::Restarted),
            },
            Count::None => Step::Stop(Verdict::Unstable(Unstable::Jumped)),
            Count::Many => Step::Stop(Verdict::Restarted),
        }
    }

    /// Start the window again from the read `raw`, between the clock reads
    /// `pre` and `stamp`.
    fn restart(&mut self, pre: u64, raw: u64, stamp: u64) {
        self.ext = Extend::new(self.bits, raw);
        self.pre = pre;
        self.stamp = stamp;
        self.len = 0;
        self.next = 0;
        self.restarts += 1;
    }

    /// One check: the clock read `lead`, then `reads`. After
    /// [`Verdict::Unstable`] or [`Verdict::ReferenceBackwards`] the
    /// watchdog is done: what it says after that means nothing.
    pub fn check(&mut self, lead: u64, reads: &[Read; READS]) -> Verdict {
        self.checks += 1;
        // Every clock read, in the order taken, from the last check's
        // final stamp on.
        let mut last = self.stamp;
        for c in core::iter::once(lead).chain(reads.iter().flat_map(|r| [r.clock, r.stamp])) {
            if c < last {
                return Verdict::Unstable(Unstable::Backwards(last - c));
            }
            last = c;
        }
        let mut samples = [ZERO; READS];
        let mut pre = lead;
        for (s, r) in samples.iter_mut().zip(reads) {
            let before = match self.advance(pre, r.before, r.clock) {
                Step::Ok(t) => t,
                Step::Stop(v) => return self.stop(v, reads),
            };
            pre = r.stamp;
            let after = match self.advance(r.clock, r.after, r.stamp) {
                Step::Ok(t) => t,
                Step::Stop(v) => return self.stop(v, reads),
            };
            *s = Sample {
                before,
                target: r.clock,
                after,
            };
        }
        // The extension only moves forward, so every width is valid.
        let Some(s) = calibrate::tightest(&samples) else {
            unreachable!("an extended count never goes backwards")
        };
        self.ring[self.next] = s;
        self.next = (self.next + 1) % (CHECKS + 1);
        self.len = (self.len + 1).min(CHECKS + 1);
        if self.len <= CHECKS {
            return Verdict::Steady;
        }
        // Full: the oldest is where the next one goes.
        let oldest = self.ring[self.next];
        match calibrate::estimate(self.reference, oldest, s) {
            Ok(e) => match deviates(e, self.clock, LIMIT_PPB) {
                Some(off) => Verdict::Unstable(Unstable::Rate(off)),
                None => Verdict::Steady,
            },
            // Every gap was counted within a factor of 2 of the clock, so
            // only a window too long to measure (a VM paused for minutes)
            // gets here, and that proves nothing.
            Err(_) => Verdict::Steady,
        }
    }

    fn stop(&mut self, v: Verdict, reads: &[Read; READS]) -> Verdict {
        if v == Verdict::Restarted {
            let r = reads[READS - 1];
            self.restart(r.clock, r.after, r.stamp);
        }
        v
    }

    /// The least the clock's new timeline may start at, in ns, after the
    /// clock went backwards: the clock's time at the newest sample
    /// (`last_ns`, the old timeline at [`Watch::last`]'s target) plus the
    /// most time that can have passed since, from the reference read
    /// `raw` made after the check that fired, plus [`LIMIT_PPB`]: a clock
    /// that has not been caught running fast can have run up to that much
    /// fast. Every reading made before the check that fired, while the
    /// clock still went forward, is below it.
    ///
    /// The reference ticks since the last read are counted as at most one
    /// wrap: the clock that would count them is the one that went
    /// backwards. (A 64-bit reference never wraps; a narrow one would
    /// need over 4.7 s between that read and `raw`, a vCPU stopped by its
    /// host for that long at exactly that moment.) With no sample yet,
    /// `last_ns`.
    pub fn floor_ns(&self, last_ns: u64, raw: u64) -> u64 {
        let Some(s) = self.last() else {
            return last_ns;
        };
        let since = if self.bits == 64 {
            raw.saturating_sub(self.ext.last())
        } else {
            counter::delta(self.ext.last(), raw, self.bits)
        };
        let ticks = u128::from(self.ext.total() - s.before) + u128::from(since);
        // ceil(ticks * 10^9 * den / num), then times (10^9 + L) / 10^9,
        // rounded up. ticks < 2^65 and 10^9 den < 2^62, so below 2^127.
        let ns = (ticks * 1_000_000_000 * u128::from(self.reference.den()))
            .div_ceil(u128::from(self.reference.num()));
        let ns = ns
            .saturating_mul(1_000_000_000 + u128::from(LIMIT_PPB))
            .div_ceil(1_000_000_000);
        last_ns.saturating_add(u64::try_from(ns).unwrap_or(u64::MAX))
    }
}
