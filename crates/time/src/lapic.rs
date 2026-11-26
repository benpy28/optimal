//! The local APIC timer: its register encodings, and how to arm it so it
//! never fires early (Intel SDM Vol. 3A section 11.5.4, "APIC Timer", and
//! 11.5.4.1, "TSC-Deadline Mode"; AMD APM Vol. 2 section 16.4.1).
//!
//! The timer counts down from the initial count at the bus (or crystal)
//! clock divided by 1 to 128, and interrupts when it reaches zero. In
//! one-shot mode it then stops; in periodic mode it reloads the initial
//! count. In TSC-deadline mode (CPUID 01h ECX bit 24) the count registers
//! are ignored and it interrupts once the TSC reaches the value written to
//! IA32_TSC_DEADLINE.
//!
//! Arming rule: a timer asked for `ns` nanoseconds must not fire before
//! `ns` have passed, whatever the true count rate is within its proven
//! error bound. So the tick count is worked out at the fastest rate the
//! bound allows ([`ticks_at_least`]), and the divided count gets one more
//! tick, because the phase of the divider when the count is written is
//! not specified: the first decrement can come anywhere up to one divided
//! tick later, so `c` decrements take more than `c - 1` divided ticks
//! ([`arm`]).

use crate::freq::Freq;

/// The timer's LVT entry: vector in bits 0 to 7, mask in bit 16, mode in
/// bits 17 and 18 (SDM Figure 11-8).
pub const LVT_MASKED: u32 = 1 << 16;
const MODE_SHIFT: u32 = 17;
const MODE_MASK: u32 = 0b11 << MODE_SHIFT;

/// IA32_TSC_DEADLINE (SDM Vol. 4, MSR 6E0H). Writing 0 disarms it.
pub const TSC_DEADLINE_MSR: u32 = 0x6E0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    OneShot,
    Periodic,
    TscDeadline,
}

impl Mode {
    const fn bits(self) -> u32 {
        (match self {
            Mode::OneShot => 0b00,
            Mode::Periodic => 0b01,
            Mode::TscDeadline => 0b10,
        }) << MODE_SHIFT
    }
}

/// The LVT timer entry for `vector` in `mode`, fixed delivery.
pub const fn lvt(vector: u8, mode: Mode, masked: bool) -> u32 {
    vector as u32 | mode.bits() | if masked { LVT_MASKED } else { 0 }
}

/// The mode an LVT timer entry selects; `None` for the reserved 0b11.
pub const fn lvt_mode(raw: u32) -> Option<Mode> {
    match (raw & MODE_MASK) >> MODE_SHIFT {
        0b00 => Some(Mode::OneShot),
        0b01 => Some(Mode::Periodic),
        0b10 => Some(Mode::TscDeadline),
        _ => None,
    }
}

/// The largest divider is 2^7 = 128.
pub const MAX_DIVIDE_LOG2: u32 = 7;

/// The divide configuration register value for dividing by 2^`log2`
/// (0 to 7). The SDM's Figure 11-10 encodes the divider in bits 0, 1 and
/// 3 (bit 2 is reserved, zero) as a 3-bit value v = log2 - 1 modulo 8:
/// 0b000 divides by 2, 0b001 by 4, ..., 0b110 by 128 and 0b111 by 1.
pub const fn dcr(log2: u32) -> u32 {
    assert!(log2 <= MAX_DIVIDE_LOG2);
    let v = (log2 + 7) & 7;
    (v & 0b11) | (v & 0b100) << 1
}

/// The divider a DCR value selects, as log2 (bit 2 and bits 4 up are
/// ignored, as the hardware ignores them).
pub const fn dcr_log2(raw: u32) -> u32 {
    let v = (raw & 0b11) | (raw >> 1) & 0b100;
    (v + 1) & 7
}

/// The fewest ticks of a counter running at `freq`, known to within `ppb`
/// parts per billion, that are sure to take at least `ns` nanoseconds:
/// exactly ceil(ns * f * (1 + ppb / 10^9)), with f in ticks per
/// nanosecond, because the counter may be running as fast as that.
/// `ppb` is capped at 10^9 (100%); a bound wider than that is no bound
/// and is treated as 100%.
pub fn ticks_at_least(freq: Freq, ppb: u64, ns: u64) -> u128 {
    const G: u128 = 1_000_000_000;
    // The value is x * m / d with x = ns * num (under 2^128),
    // m = 10^9 + ppb (at most 2 * 10^9, under 2^31) and
    // d = 10^9 * 10^9 * den (under 2^92). x * m can overflow a u128, so
    // split x = q * d + r: then x * m / d = q * m + r * m / d, where
    // q < 2^128 / 10^18 < 2^69, so q * m < 2^100, and r < d, so
    // r * m < 2^123. Both fit, and the result is exact.
    let x = u128::from(ns) * u128::from(freq.num());
    let m = G + u128::from(ppb.min(1_000_000_000));
    let d = G * G * u128::from(freq.den());
    (x / d) * m + (x % d * m).div_ceil(d)
}

/// A one-shot or periodic arming: the divider (as log2) and the count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arm {
    pub divide_log2: u32,
    pub count: u32,
}

impl Arm {
    /// The DCR value for this arming.
    pub const fn dcr(self) -> u32 {
        dcr(self.divide_log2)
    }

    /// Undivided ticks this arming is sure to last, at least: (count - 1)
    /// whole divided ticks (see the module documentation).
    pub const fn min_ticks(self) -> u64 {
        (self.count as u64 - 1) << self.divide_log2
    }
}

/// How to arm a timer whose undivided rate is `freq` within `ppb` so it
/// fires no earlier than `ns` from now: the finest divider whose count
/// (the divided ticks needed, rounded up, plus one) fits in 32 bits.
/// `None` when even dividing by 128 does not fit: the caller arms for
/// less and re-arms ([`longest_ns`] says how much less).
///
/// Finest first because each divided tick is a step of uncertainty in
/// when the timer fires; and the count is at least 2, so a one-shot armed
/// for 0 ns still waits for one whole divided tick, never zero (a zero
/// initial count stops the timer instead of firing it).
pub fn arm(freq: Freq, ppb: u64, ns: u64) -> Option<Arm> {
    let t = ticks_at_least(freq, ppb, ns);
    (0..=MAX_DIVIDE_LOG2).find_map(|log2| {
        let count = (t.div_ceil(1u128 << log2) + 1).max(2);
        u32::try_from(count).ok().map(|count| Arm {
            divide_log2: log2,
            count,
        })
    })
}

/// The longest `ns` that [`arm`] can arm for in one go at this rate and
/// bound (`arm` succeeds for every ns up to it, and fails for ns + 1).
/// At least 1 second for any rate up to 128 * (2^32 - 2) / 2 Hz, which is
/// about 275 GHz.
pub fn longest_ns(freq: Freq, ppb: u64) -> u64 {
    // arm(ns) succeeds exactly when ticks_at_least(ns) <= 128 * (2^32 - 2)
    // (then ceil(t / 128) + 1 <= 2^32 - 1), and ticks_at_least is
    // monotonic in ns, so a binary search finds the edge.
    if arm(freq, ppb, u64::MAX).is_some() {
        return u64::MAX;
    }
    let (mut lo, mut hi) = (0u64, u64::MAX);
    // Invariant: arm(lo) is Some and arm(hi) is None. arm(0) is always
    // Some: ticks_at_least(0) = 0, so the count is 2.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if arm(freq, ppb, mid).is_some() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The IA32_TSC_DEADLINE value that fires no earlier than `ns` after the
/// TSC read `now`, for a TSC at `freq` within `ppb`. Saturates at
/// u64::MAX (over a century at any real TSC rate), and is never 0, which
/// would disarm the timer instead.
pub fn tsc_deadline(now: u64, freq: Freq, ppb: u64, ns: u64) -> u64 {
    let t = u64::try_from(ticks_at_least(freq, ppb, ns)).unwrap_or(u64::MAX);
    now.saturating_add(t).max(1)
}

/// What the timer's current count register says, as ticks since a
/// one-shot was armed with `initial`: an up-counter for calibration.
/// `None` once the count has reached zero, because a stopped one-shot no
/// longer says how much time has passed.
pub const fn elapsed(initial: u32, current: u32) -> Option<u64> {
    if current == 0 || current > initial {
        None
    } else {
        Some((initial - current) as u64)
    }
}

/// The combined bound when a measurement `a` ppb from its reference is
/// made against a reference itself `b` ppb from the truth: the true value
/// is within (1 + a)(1 + b) - 1 = a + b + ab of the estimate. Rounded up,
/// saturating.
pub fn compose_ppb(a: u64, b: u64) -> u64 {
    let ab = (u128::from(a) * u128::from(b)).div_ceil(1_000_000_000);
    u64::try_from(u128::from(a) + u128::from(b) + ab).unwrap_or(u64::MAX)
}
