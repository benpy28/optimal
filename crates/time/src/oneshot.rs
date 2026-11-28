//! One-shot timer events from the HPET and the 8254 PIT, the fallbacks
//! when the local APIC timer cannot be used (ROADMAP 1c: "timer events
//! from the LAPIC TSC-deadline mode, falling back to LAPIC one-shot,
//! HPET, then PIT").
//!
//! The arming rule is the LAPIC timer's (crate::lapic): an event asked
//! for `ns` must not fire before `ns` have passed, whatever the source's
//! true rate is within its proven bound, so tick counts are worked out at
//! the fastest rate the bound allows ([`crate::lapic::ticks_at_least`]),
//! plus whatever the source's own phase uncertainty needs:
//!
//! - HPET (IA-PC HPET 1.0a, 2.3.9): the comparator interrupts when the
//!   main counter becomes equal to it. The counter value `now` the kernel
//!   read became current up to one tick period before the read, so a
//!   comparator at `now + t` matches between `t - 1` and `t` periods after
//!   the read: one extra tick ([`hpet_ticks`]). Equal, not past: a value
//!   the counter has already gone by never matches until the counter
//!   wraps, so [`hpet_program`] checks after each write and moves the
//!   comparator further ahead when the counter got there first.
//! - PIT channel 0 in mode 0 (Intel 8254 datasheet, "Mode 0: Interrupt on
//!   Terminal Count"): writing the count's low byte drops OUT and stops
//!   counting, the high byte starts it; the count is loaded on the next
//!   CLK pulse and OUT rises N pulses after that, so between N and N + 1
//!   periods after the write. N periods are enough ([`pit_count`]). The
//!   count is 16 bits, 0 meaning 65,536, so one arming covers at most
//!   65,536 / 1,193,181.8 Hz = 54.925 ms.
//!
//! A wait longer than one arming covers is reached in several, each no
//! longer than [`hpet_longest_ns`] or [`pit_longest_ns`].

use crate::freq::Freq;
use crate::hpet;
use crate::lapic::ticks_at_least;

/// The most ticks one HPET arming may cover: under half the comparator's
/// width, so [`hpet::passed`]'s signed compare in that width is right
/// from the arming until the deadline: 2^31 - 1 for a 32-bit comparator
/// (21.4 s at 100 MHz, 2.9 min at 14.318 MHz), 2^62 for a 64-bit one
/// (over a thousand years at any HPET rate).
pub fn hpet_max_ticks(timer_config: u64) -> u64 {
    if timer_config & hpet::T_64BIT_CAPABLE != 0 {
        1 << 62
    } else {
        (1 << 31) - 1
    }
}

/// The comparator distance, in HPET ticks, that is sure to take at least
/// `ns` from the counter read it is added to, for an HPET at `freq`
/// within `ppb` of the clock: [`ticks_at_least`] plus one (see the module
/// documentation). `None` when it is past [`hpet_max_ticks`]: arm for
/// less ([`hpet_longest_ns`]) and again later.
pub fn hpet_ticks(freq: Freq, ppb: u64, ns: u64, timer_config: u64) -> Option<u64> {
    let t = ticks_at_least(freq, ppb, ns) + 1;
    u64::try_from(t)
        .ok()
        .filter(|&t| t <= hpet_max_ticks(timer_config))
}

/// The longest `ns` [`hpet_ticks`] accepts at this rate and bound.
pub fn hpet_longest_ns(freq: Freq, ppb: u64, timer_config: u64) -> u64 {
    longest(|ns| hpet_ticks(freq, ppb, ns, timer_config).is_some())
}

/// A comparator, as [`hpet_program`] drives it.
pub trait Comparator {
    /// Read the main counter.
    fn counter(&mut self) -> u64;
    /// Write the comparator value.
    fn set(&mut self, value: u64);
    /// Turn the comparator's interrupt on (Tn_INT_ENB_CNF).
    fn enable(&mut self);
}

/// Each retry moves the comparator this much further than the last, and
/// doubles it: after `n` retries the extra distance is
/// `step * (2^n - 1)`.
pub const MAX_RETRIES: u32 = 16;

/// What [`hpet_program`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Programmed {
    /// The counter read the value was worked out from.
    pub start: u64,
    /// The comparator value.
    pub value: u64,
    /// Ticks past the first counter read: at least the `ticks` asked for.
    pub distance: u64,
    /// Writes the counter got past first.
    pub retries: u32,
}

/// The counter passed the comparator after every one of
/// [`MAX_RETRIES`] + 1 writes. With `step` a few microseconds that is a
/// counter racing far ahead of its own reads, or a broken one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missed {
    /// The last value written.
    pub value: u64,
}

/// Arm comparator `c` (configured as `timer_config`) `ticks` ahead of the
/// counter: read the counter, write the comparator, and read the counter
/// again; if it has already reached the value, the match may have been
/// missed, so write again further ahead, adding `step`, then twice that,
/// and so on, never past [`hpet_max_ticks`]. `ticks` is at most that
/// ([`hpet_ticks`]), and `step` at least 1.
///
/// With `enable`, the comparator's interrupt is off and is turned on
/// right after the first write, before the check: the value it holds
/// before that is one the counter has passed, which the specification
/// says never matches but QEMU fires at once when the interrupt is
/// turned on; and a match between the write and the enable would be
/// lost, which the check after it catches.
pub fn hpet_program(
    c: &mut impl Comparator,
    ticks: u64,
    step: u64,
    timer_config: u64,
    enable: bool,
) -> Result<Programmed, Missed> {
    let max = hpet_max_ticks(timer_config);
    let step = step.max(1);
    let mut distance = ticks.min(max);
    let mut value = 0;
    let mut enable = enable;
    for retries in 0..=MAX_RETRIES {
        let start = c.counter();
        value = hpet::deadline(start, distance, timer_config);
        c.set(value);
        if enable {
            c.enable();
            enable = false;
        }
        if !hpet::passed(c.counter(), value, timer_config) {
            return Ok(Programmed {
                start,
                value,
                distance,
                retries,
            });
        }
        let extra = step.saturating_mul(1 << retries.min(62));
        distance = distance.saturating_add(extra).min(max);
    }
    Err(Missed { value })
}

/// The PIT's largest count: 0 in the count register means 65,536.
pub const PIT_MAX_COUNT: u32 = 1 << 16;

/// The mode 0 count, 1 to 65,536, that is sure to take at least `ns`
/// after it is written, for a PIT at `freq` within `ppb` of the clock:
/// [`ticks_at_least`], and at least 1 (the datasheet's smallest mode 0
/// count). `None` past [`PIT_MAX_COUNT`].
pub fn pit_count(freq: Freq, ppb: u64, ns: u64) -> Option<u32> {
    u32::try_from(ticks_at_least(freq, ppb, ns).max(1))
        .ok()
        .filter(|&n| n <= PIT_MAX_COUNT)
}

/// The longest `ns` [`pit_count`] accepts at this rate and bound.
pub fn pit_longest_ns(freq: Freq, ppb: u64) -> u64 {
    longest(|ns| pit_count(freq, ppb, ns).is_some())
}

/// The largest `ns` for which `fits` holds, given that it holds for 0
/// and, once it fails, fails for every larger `ns`.
fn longest(fits: impl Fn(u64) -> bool) -> u64 {
    if fits(u64::MAX) {
        return u64::MAX;
    }
    // Invariant: fits(lo) and !fits(hi).
    let (mut lo, mut hi) = (0u64, u64::MAX);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}
