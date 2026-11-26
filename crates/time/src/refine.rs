//! The 10 s refinement of the TSC's frequency (ROADMAP 1c).
//!
//! At boot the TSC's frequency is either measured for at most 100 ms or
//! taken from an exact CPUID claim (leaf 15h or a hypervisor's leaf), so
//! boot spends no time on it. Over the next 10 s the kernel measures the
//! TSC again against the HPET or the PM timer, from one bracketed sample
//! at the start of the window to one at the end ([`crate::calibrate`]),
//! with timer events in between, and [`decide`] says what to do with the
//! result:
//!
//! - A measured boot value is replaced by the 10 s one when that is
//!   tighter, which takes the clock from tens of ppm to well under one.
//!   The two must agree: the true frequency lies inside both bounds if
//!   the TSC's rate stayed the same, so two bounds that do not overlap
//!   prove it changed.
//! - A CPUID claim is checked against the measurement, and kept when it
//!   agrees: the claim is exact to the crystal it is derived from, which
//!   is no worse than the reference's own crystal, so a measurement
//!   against the reference cannot improve on it. A claim that disagrees
//!   is replaced by the measurement. (Chosen: keep a confirmed claim.
//!   Rejected: always switch to the measurement, which would only trade
//!   the TSC's crystal error for the reference's.)
//!
//! Narrow references wrap during the window: a 24-bit PM timer every
//! 4.7 s. The kernel reads the reference every eighth of its wrap period
//! ([`poll_ns`]), and works out the ticks between reads from the raw
//! difference and the TSC ([`expected`], [`unwrap`]): the TSC says
//! roughly how much time passed, and only one whole number of wraps fits
//! that. When more than one could (the vCPU was stopped for longer than
//! a wrap, say), the refinement refuses instead of guessing.
//!
//! When the clock's frequency changes, everything measured against the
//! clock changes by the same factor: [`rescale`] carries the LAPIC timer's
//! rate across exactly.

use crate::calibrate::{Estimate, MAX_TOLERANCE_PPB, MAX_WINDOW, ceil_ppb};
use crate::freq::{CAL_DEN, Freq};

/// How long the refinement measures for, in nanoseconds of the clock.
pub const WINDOW_NS: u64 = 10_000_000_000;

/// How often a `bits`-bit reference at `freq` must be read so that no
/// wrap goes unseen: an eighth of its wrap period, so that even a TSC
/// whose frequency is off by a factor of 2 (a lying CPUID claim, which
/// the refinement exists to catch) still reads it within a quarter wrap.
/// `u64::MAX` for a 64-bit counter, which never wraps.
pub fn poll_ns(freq: Freq, bits: u32) -> u64 {
    if !(4..64).contains(&bits) {
        return u64::MAX;
    }
    freq.ticks_to_ns(1 << (bits - 3))
}

/// The reference ticks that `tsc_ticks` TSC ticks can be, as a range
/// `(lo, hi)`: from half to twice what the TSC's frequency says. Only
/// used to count wraps, so it is deliberately wide: it holds whenever the
/// TSC's frequency is within a factor of 2 of `tsc`. `hi` saturates at
/// u64::MAX, which no count can pass. `None` past [`MAX_WINDOW`] TSC
/// ticks, when even `lo` is past u64::MAX, or when the products do not
/// fit in 128 bits (they always do for real counters, whose frequency
/// numerators are far below 2^56).
pub fn expected(tsc_ticks: u64, tsc: Freq, reference: Freq) -> Option<(u64, u64)> {
    if tsc_ticks > MAX_WINDOW {
        return None;
    }
    // ticks * (rn / rd) / (tn / td) = ticks * rn * td / (rd * tn).
    let n = u128::from(tsc_ticks)
        .checked_mul(u128::from(reference.num()))?
        .checked_mul(u128::from(tsc.den()))?;
    let d = u128::from(reference.den()).checked_mul(u128::from(tsc.num()))?;
    let lo = n / d.checked_mul(2)?;
    let hi = n.checked_mul(2)?.div_ceil(d);
    Some((
        u64::try_from(lo).ok()?,
        u64::try_from(hi).unwrap_or(u64::MAX),
    ))
}

/// The one count `t` in `lo..=hi` with `t` = `raw_delta` modulo 2^`bits`
/// (1 to 64), that is the ticks between two reads of a `bits`-bit counter
/// whose raw difference was `raw_delta`. `None` when there is none, or
/// when more than one fits: then the number of wraps is unknown.
pub fn unwrap(raw_delta: u64, bits: u32, lo: u64, hi: u64) -> Option<u64> {
    assert!((1..=64).contains(&bits));
    if lo > hi {
        return None;
    }
    let m = 1u128 << bits;
    let lo = u128::from(lo);
    // The smallest t >= lo with t = raw_delta (mod m). (Below 2^65, so
    // nothing overflows.)
    let t = lo + (u128::from(raw_delta) + m - lo % m) % m;
    if t > u128::from(hi) || t + m <= u128::from(hi) {
        return None;
    }
    u64::try_from(t).ok()
}

/// Do two measurements of the same frequency agree: can one true
/// frequency lie inside both of their bounds? Exact. `Err` holds how far
/// `b` is from `a`, in ppb of `a` (positive when `b` is faster).
///
/// Each bound covers [F(1 - p), F(1 + p)] (calibrate::estimate bounds
/// both sides by the larger one), so they overlap exactly when
/// A(10^9 - a) <= B(10^9 + b) and B(10^9 - b) <= A(10^9 + a), with a and
/// b in ppb. With A = an/ad and B = bn/bd that is an bd (10^9 - a) <=
/// bn ad (10^9 + b): cross products below 2^96, times at most 10^9 + 2^31
/// < 2^32, so below 2^128.
pub fn consistent(a: Estimate, b: Estimate) -> Result<(), i64> {
    let pa = u128::from(a.ppb.min(MAX_TOLERANCE_PPB));
    let pb = u128::from(b.ppb.min(MAX_TOLERANCE_PPB));
    let g = 1_000_000_000u128;
    let x = u128::from(a.freq.num()) * u128::from(b.freq.den());
    let y = u128::from(b.freq.num()) * u128::from(a.freq.den());
    let a_low_fits = x * g.saturating_sub(pa) <= y * (g + pb);
    let b_low_fits = y * g.saturating_sub(pb) <= x * (g + pa);
    if a_low_fits && b_low_fits {
        Ok(())
    } else {
        Err(a.freq.ppb_from(b.freq))
    }
}

/// What the boot value of the TSC's frequency was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    /// Measured at boot.
    Measured(Estimate),
    /// An exact CPUID claim, trusted to within `slack_ppb` (its rounding
    /// plus the crystal it comes from).
    Claimed { freq: Freq, slack_ppb: u64 },
}

/// What to do with the 10 s measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Use it: it agrees with the measured boot value and is tighter.
    Refined,
    /// Keep the measured boot value: the 10 s one agrees but is no
    /// tighter (only possible when the reference is very coarse or the
    /// boot measurement ran long).
    KeptBoot,
    /// Keep the CPUID claim: the measurement agrees with it. Holds how
    /// far the measurement is from the claim, in ppb of the measurement
    /// (positive when the TSC is faster than claimed).
    Confirmed(i64),
    /// The CPUID claim is wrong by this many ppb of the measurement: use
    /// the measurement.
    ClaimWrong(i64),
    /// The measured boot value and the 10 s one cannot both be right, so
    /// the TSC's rate changed: by this many ppb of the boot value. The
    /// 10 s one is the average over the longer time, so it is used, and
    /// the change is reported.
    Changed(i64),
}

impl Outcome {
    /// Does the clock take the 10 s measurement?
    pub fn adopts(self) -> bool {
        matches!(
            self,
            Outcome::Refined | Outcome::ClaimWrong(_) | Outcome::Changed(_)
        )
    }
}

/// See the module documentation.
pub fn decide(boot: Boot, refined: Estimate) -> Outcome {
    match boot {
        Boot::Claimed { freq, slack_ppb } => {
            match crate::calibrate::agrees(refined, freq, slack_ppb) {
                Ok(()) => Outcome::Confirmed(refined.freq.ppb_from(freq).saturating_neg()),
                Err(off) => Outcome::ClaimWrong(off),
            }
        }
        Boot::Measured(b) => match consistent(b, refined) {
            Err(off) => Outcome::Changed(off),
            Ok(()) if refined.ppb < b.ppb => Outcome::Refined,
            Ok(()) => Outcome::KeptBoot,
        },
    }
}

/// `e` measured against a clock taken to run at `from`, restated for the
/// same clock now taken to run at `to`: the frequency times `to / from`,
/// rounded to 1/1024 Hz, with that rounding added to the bound. The bound
/// is otherwise unchanged, because it was a bound on the ratio between
/// the two counters, which a new value for the clock does not touch.
/// `None` when the result is not a valid [`Freq`] or the arithmetic
/// would not fit.
pub fn rescale(e: Estimate, from: Freq, to: Freq) -> Option<Estimate> {
    // F' = (en / ed) (tn / td) / (fn / fd) = en tn fd / (ed td fn),
    // in 1/1024 Hz: round(en tn fd 1024 / (ed td fn)).
    let n = u128::from(e.freq.num())
        .checked_mul(u128::from(to.num()))?
        .checked_mul(u128::from(from.den()))?
        .checked_mul(u128::from(CAL_DEN))?;
    let d = u128::from(e.freq.den())
        .checked_mul(u128::from(to.den()))?
        .checked_mul(u128::from(from.num()))?;
    let q = n.checked_mul(2)?.checked_add(d)? / d.checked_mul(2)?;
    let big = u64::try_from(q).ok()?;
    let freq = Freq::new(big, CAL_DEN)?;
    // Rounding to the nearest 1/1024 Hz is off by at most 1/2048 Hz,
    // which is 1 / (2 * big) of the result.
    let rounding = if n % d == 0 { 0 } else { ceil_ppb(1, 2 * q) };
    Some(Estimate {
        freq,
        ppb: e.ppb.saturating_add(rounding),
        window: e.window,
    })
}
