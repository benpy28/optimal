//! Measuring one counter's frequency against another, with a proven bound
//! on the error.
//!
//! The kernel reads the reference counter, then the target, then the
//! reference again: a [`Sample`]. The target read happened somewhere
//! between the two reference reads. It does that at the start and at the
//! end of a window, and [`estimate`] turns the two samples into a
//! frequency and the most it can be wrong by.
//!
//! The bound. A reading r of a counter means the true count was in
//! [r, r + 1) when it was taken. So the true reference time between the two
//! target reads, in reference ticks, is more than
//! `min = end.before - start.after - 1` and less than
//! `max = end.after - start.before + 1`, and the true target count between
//! them, with `dt = end.target - start.target`, is more than dt - 1 and
//! less than dt + 1. The target frequency is therefore more than
//! `lo = (dt - 1) * ref / max` and less than `hi = (dt + 1) * ref / min`.
//! The estimate F is the midpoint reading, `2 * dt * ref / sum` with
//! `sum = (end.before + end.after) - (start.before + start.after)`, rounded
//! to 1/1024 Hz, and the bound given is `max(hi - F, F - lo) / F`, rounded
//! up: the true frequency is always closer to F than that.
//!
//! The bound is relative to the reference. How far the reference itself is
//! from its nominal frequency (a crystal is typically within 50 ppm) is a
//! separate question; see [`agrees`] for checking one clock against
//! another.
//!
//! Interrupts, SMIs and virtual machine exits between the three reads only
//! widen a sample, which widens the bound; they can never make it wrong.
//! Taking the tightest of several back-to-back samples ([`tightest`])
//! keeps the bound small.

use crate::freq::{CAL_DEN, Freq};

/// A target counter read, with reference counter reads just before and
/// just after it. Both counters are already extended to 64 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub before: u64,
    pub target: u64,
    pub after: u64,
}

impl Sample {
    /// Reference ticks between the two reference reads, or `None` if the
    /// reference went backwards.
    pub const fn width(self) -> Option<u64> {
        self.after.checked_sub(self.before)
    }
}

/// The sample with the smallest width (the first of equals), skipping
/// any whose reference went backwards.
pub fn tightest(samples: &[Sample]) -> Option<Sample> {
    let mut best: Option<(u64, Sample)> = None;
    for &s in samples {
        if let Some(w) = s.width()
            && best.is_none_or(|(bw, _)| w < bw)
        {
            best = Some((w, s));
        }
    }
    best.map(|(_, s)| s)
}

/// The most ticks a calibration window may span on either counter: 2^40
/// (about 3 hours at 100 MHz, 3.7 minutes at 5 GHz). Keeps every product
/// below inside a u128.
pub const MAX_WINDOW: u64 = 1 << 40;

/// Why two samples give no frequency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A reference read was smaller than one before it.
    ReferenceBackwards,
    /// The target's end reading is below its start reading.
    TargetBackwards,
    /// Under two reference ticks between the samples: no bound exists.
    WindowTooShort,
    /// The target moved under two ticks: it is stopped or far too slow.
    TargetStopped,
    /// More than [`MAX_WINDOW`] ticks on either counter.
    WindowTooLong,
    /// The result is outside what a [`Freq`] can be (1 Hz to 2^40 Hz).
    OutOfRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub freq: Freq,
    /// The true frequency is within this many parts per billion of `freq`
    /// (relative to the reference).
    pub ppb: u64,
    /// Reference ticks between the two target reads (midpoint reading).
    pub window: u64,
}

/// See the module documentation.
pub fn estimate(reference: Freq, start: Sample, end: Sample) -> Result<Estimate, Refused> {
    if start.width().is_none() || end.width().is_none() || end.before < start.after {
        return Err(Refused::ReferenceBackwards);
    }
    if end.target < start.target {
        return Err(Refused::TargetBackwards);
    }
    // end.before >= start.after + 2, so min >= 1.
    if end.before - start.after < 2 {
        return Err(Refused::WindowTooShort);
    }
    let dt = end.target - start.target;
    if dt < 2 {
        return Err(Refused::TargetStopped);
    }
    if dt > MAX_WINDOW || end.after - start.before > MAX_WINDOW {
        return Err(Refused::WindowTooLong);
    }
    let min = u128::from(end.before - start.after - 1);
    let max = u128::from(end.after - start.before + 1);
    let sum = u128::from(end.before) + u128::from(end.after)
        - u128::from(start.before)
        - u128::from(start.after);
    let dt = u128::from(dt);
    let (rn, rd) = (u128::from(reference.num()), u128::from(reference.den()));
    let cal = u128::from(CAL_DEN);

    // F = round(2 dt rn cal / (sum rd)) / cal. 2 dt rn cal < 2^41 2^64 2^10.
    let n = 2 * dt * rn * cal;
    let d = sum * rd;
    let big_f = (2 * n + d) / (2 * d);
    let freq = u64::try_from(big_f)
        .ok()
        .and_then(|f| Freq::new(f, CAL_DEN))
        .ok_or(Refused::OutOfRange)?;
    // Freq::new may have reduced the fraction; the bound below works with
    // N / cal as computed, which is the same value. N <= 2^40 * 2^10 now
    // that Freq::new accepted it.

    // (hi - F) / F = ((dt + 1) rn cal - N min rd) / (min rd N)
    // (F - lo) / F = (N max rd - (dt - 1) rn cal) / (max rd N)
    // Each term is below 2^122 (N < 2^51, min and max <= 2^40 + 1,
    // rd <= 2^32; dt + 1 <= 2^40 + 1, rn < 2^64, cal = 2^10).
    let up = ((dt + 1) * rn * cal).saturating_sub(big_f * min * rd);
    let down = (big_f * max * rd).saturating_sub((dt - 1) * rn * cal);
    let ppb = ceil_ppb(up, min * rd * big_f).max(ceil_ppb(down, max * rd * big_f));
    Ok(Estimate {
        freq,
        ppb,
        window: (sum / 2) as u64,
    })
}

/// ceil(x / y * 10^9), saturating; an upper bound when the operands are
/// too big to multiply exactly (x is rounded up and y down as they are
/// halved, which can only raise the result). y = 0 gives u64::MAX.
pub fn ceil_ppb(mut x: u128, mut y: u128) -> u64 {
    const LIMIT: u128 = 1 << 96;
    while x >= LIMIT || y >= LIMIT {
        x = (x >> 1) + (x & 1);
        y >>= 1;
    }
    if y == 0 {
        return u64::MAX;
    }
    // x < 2^96 and 10^9 < 2^30, so this fits.
    u64::try_from((x * 1_000_000_000).div_ceil(y)).unwrap_or(u64::MAX)
}

/// The most [`agrees`] tolerates: 2^31 ppb (over 200%). Larger
/// tolerances are treated as this one.
pub const MAX_TOLERANCE_PPB: u64 = 1 << 31;

/// Does a measurement of a clock agree with what the clock should be,
/// within the measurement's own bound plus `slack_ppb`? Exact: it compares
/// |F - E| * 10^9 with (bound + slack) * F in whole numbers. `Err` holds
/// how far off the measurement F is from the expected E, in ppb of F
/// (positive when the measurement is faster), rounded toward zero.
pub fn agrees(measured: Estimate, expected: Freq, slack_ppb: u64) -> Result<(), i64> {
    let f = measured.freq;
    // F = fn/fd and E = en/ed: compare |fn ed - en fd| * 10^9 with
    // tol * fn ed. Cross products < 2^96; times 10^9 or tol (<= 2^31)
    // they stay below 2^127.
    let a = u128::from(f.num()) * u128::from(expected.den());
    let b = u128::from(expected.num()) * u128::from(f.den());
    let tol = measured
        .ppb
        .saturating_add(slack_ppb)
        .min(MAX_TOLERANCE_PPB);
    if a.abs_diff(b) * 1_000_000_000 <= u128::from(tol) * a {
        Ok(())
    } else {
        Err(f.ppb_from(expected).saturating_neg())
    }
}
