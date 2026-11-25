//! Exact frequencies and conversions between counter ticks and nanoseconds.
//!
//! A clock's frequency is kept as an exact fraction of hertz, because the
//! classic PC clocks are not whole numbers of hertz: the PM timer runs at
//! 315/88 MHz (3,579,545.45... Hz) and the 8254 PIT at 315/264 MHz
//! (1,193,181.81... Hz), both derived from the NTSC colour-burst crystal of
//! 315/22 MHz. Rounding those to whole hertz would put a fixed 0.13 ppm and
//! 0.15 ppm error into everything calibrated against them.

use crate::NS_PER_SEC;

/// Largest denominator a [`Freq`] may have. Every source the kernel uses
/// fits: an HPET period in femtoseconds is at most 10^8 (specification
/// 1.0a, section 2.3.4), and calibrated frequencies use [`CAL_DEN`].
/// Keeping it at 2^32 means `ticks * 10^9 * den` always fits in a u128.
pub const MAX_DEN: u64 = 1 << 32;

/// Slowest and fastest clocks accepted: 1 Hz and 2^40 Hz (about 1.1 THz).
/// Anything outside that is a misread register, not a clock.
pub const MIN_HZ: u64 = 1;
pub const MAX_HZ: u64 = 1 << 40;

/// Denominator for calibrated frequencies: 1/1024 Hz resolution, which is
/// under one part in 10^12 of any clock the kernel calibrates (1 MHz or
/// faster), far below what any calibration can measure.
pub const CAL_DEN: u64 = 1024;

/// A frequency of exactly `num / den` hertz, in lowest terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Freq {
    num: u64,
    den: u64,
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl Freq {
    /// `num / den` Hz, or `None` if `den` is 0 or above [`MAX_DEN`] after
    /// reducing, or the value is outside [`MIN_HZ`] to [`MAX_HZ`].
    pub const fn new(num: u64, den: u64) -> Option<Freq> {
        if num == 0 || den == 0 {
            return None;
        }
        let g = gcd(num, den);
        let (num, den) = (num / g, den / g);
        // num/den >= MIN_HZ and num/den <= MAX_HZ, without overflow
        // (den <= 2^32 and MAX_HZ = 2^40, so the products fit in a u128).
        if den > MAX_DEN
            || (num as u128) < MIN_HZ as u128 * den as u128
            || (num as u128) > MAX_HZ as u128 * den as u128
        {
            return None;
        }
        Some(Freq { num, den })
    }

    /// A whole number of hertz.
    pub const fn hz(hz: u64) -> Option<Freq> {
        Freq::new(hz, 1)
    }

    /// The frequency of a counter that ticks once every `fs` femtoseconds
    /// (the HPET's COUNTER_CLK_PERIOD): exactly 10^15 / fs Hz.
    pub const fn from_period_fs(fs: u64) -> Option<Freq> {
        Freq::new(1_000_000_000_000_000, fs)
    }

    pub const fn num(self) -> u64 {
        self.num
    }

    pub const fn den(self) -> u64 {
        self.den
    }

    /// Whole hertz, rounded to the nearest (halves up).
    pub const fn hz_rounded(self) -> u64 {
        ((self.num as u128 * 2 + self.den as u128) / (self.den as u128 * 2)) as u64
    }

    /// Millihertz, rounded to the nearest (halves up). At most 2^40 * 1000,
    /// so it always fits.
    pub const fn millihertz(self) -> u64 {
        ((self.num as u128 * 2000 + self.den as u128) / (self.den as u128 * 2)) as u64
    }

    /// Exactly how many whole nanoseconds `ticks` ticks take (rounded down),
    /// saturating at u64::MAX. Uses a 128-bit division; see [`Scale`] for
    /// the fast path.
    pub fn ticks_to_ns(self, ticks: u64) -> u64 {
        // ticks < 2^64 and 10^9 * den < 2^62, so the product is < 2^126.
        let ns = u128::from(ticks) * u128::from(NS_PER_SEC) * u128::from(self.den)
            / u128::from(self.num);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }

    /// The fewest whole ticks that take at least `ns` nanoseconds (rounded
    /// up), so a timer armed with it never fires early. Saturates at
    /// u64::MAX.
    pub fn ns_to_ticks_ceil(self, ns: u64) -> u64 {
        // ns * num < 2^128; 10^9 * den < 2^62.
        let n = u128::from(ns) * u128::from(self.num);
        let d = u128::from(NS_PER_SEC) * u128::from(self.den);
        u64::try_from(n.div_ceil(d)).unwrap_or(u64::MAX)
    }

    /// How far `other` is from `self`, in parts per billion of `self`
    /// (positive when `other` is faster), rounded toward zero.
    pub fn ppb_from(self, other: Freq) -> i64 {
        // Each cross product is below 2^64 * 2^32 = 2^96, and times 10^9
        // (under 2^30) below 2^126, so i128 holds it with room to spare.
        let a = i128::from(other.num) * i128::from(self.den);
        let b = i128::from(self.num) * i128::from(other.den);
        let ppb = (a - b) * 1_000_000_000 / b;
        // a >= 0, so ppb >= -10^9; it can be as large as about 2^40 * 10^9
        // (MAX_HZ against MIN_HZ), which saturates.
        i64::try_from(ppb).unwrap_or(i64::MAX)
    }

    /// The fast ticks-to-nanoseconds conversion for this frequency.
    pub fn scale(self) -> Scale {
        Scale::new(self)
    }
}

/// Ticks to nanoseconds as one 64 by 64 bit multiply and a shift:
/// `ns = (ticks * mult) >> shift`.
///
/// `mult` is 10^9 * den / num scaled by 2^shift and rounded to the nearest,
/// with `shift` as large as it can be while `mult` still fits in 64 bits,
/// so mult >= 2^63 - 1. The result is then never more than one nanosecond
/// away from [`Freq::ticks_to_ns`]:
///
/// Let q = 10^9 * den / num (real) and mult = q * 2^shift + e with
/// |e| <= 1/2. Then ticks * mult / 2^shift = ticks * q + ticks * e / 2^shift,
/// and |ticks * e / 2^shift| <= ticks / 2^(shift + 1). Because
/// q * 2^shift >= mult - 1/2 >= 2^63 - 3/2, 2^shift >= (2^63 - 3/2) / q, so
/// the error is at most ticks * q / (2^64 - 3), which is below one as long
/// as the exact result ticks * q is below 2^64 - 3. Two reals less than one
/// apart have floors at most one apart. Results past that saturate.
///
/// It is also monotonic: more ticks never give fewer nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub mult: u64,
    pub shift: u32,
}

impl Scale {
    pub fn new(f: Freq) -> Scale {
        let n = NS_PER_SEC as u128 * f.den as u128;
        let d = f.num as u128;
        // floor(n * 2^s / d) = q with remainder r, starting at s = 0. Since
        // f >= 1 Hz, n / d <= 10^9, so q fits easily.
        let (mut q, mut r, mut s) = (n / d, n % d, 0u32);
        // The rounded value for the current s: q + 1 when r / d >= 1/2.
        // Step s up while the rounded value for s + 1 stays below 2^64.
        // r < d < 2^64, so 2r and 2q + 1 cannot overflow a u128.
        while s < 127 {
            let q2 = 2 * q + (2 * r >= d) as u128;
            let r2 = if 2 * r >= d { 2 * r - d } else { 2 * r };
            let rounded2 = q2 + (2 * r2 >= d) as u128;
            if rounded2 > u64::MAX as u128 {
                break;
            }
            q = q2;
            r = r2;
            s += 1;
        }
        Scale {
            mult: (q + (2 * r >= d) as u128) as u64,
            shift: s,
        }
    }

    /// `ticks` in nanoseconds, saturating at u64::MAX.
    pub fn ticks_to_ns(self, ticks: u64) -> u64 {
        let ns = (u128::from(ticks) * u128::from(self.mult)) >> self.shift;
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}
