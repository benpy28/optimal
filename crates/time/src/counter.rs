//! Counters narrower than 64 bits: the PM timer (24 or 32 bits), a 32-bit
//! HPET, and the PIT's 16-bit down counter.

/// Ticks from `then` to `now` on a counter of `bits` bits (1 to 64) that
/// counts up and wraps, assuming it wrapped at most once in between.
pub const fn delta(then: u64, now: u64, bits: u32) -> u64 {
    now.wrapping_sub(then) & mask(bits)
}

/// The low `bits` bits set (1 to 64).
pub const fn mask(bits: u32) -> u64 {
    assert!(bits >= 1 && bits <= 64);
    u64::MAX >> (64 - bits)
}

/// Extends a narrow up-counter to 64 bits. Correct as long as [`update`]
/// sees every wrap, that is it is called at least once per 2^bits ticks.
///
/// [`update`]: Extend::update
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extend {
    bits: u32,
    last: u64,
    total: u64,
}

impl Extend {
    /// Starts at 0 from the raw reading `first`.
    pub const fn new(bits: u32, first: u64) -> Extend {
        Extend {
            bits,
            last: first & mask(bits),
            total: 0,
        }
    }

    /// Takes a new raw reading and returns ticks since the first one.
    pub fn update(&mut self, raw: u64) -> u64 {
        let raw = raw & mask(self.bits);
        self.total = self.total.wrapping_add(delta(self.last, raw, self.bits));
        self.last = raw;
        self.total
    }
}

/// Three back-to-back reads `a`, `b`, `c` of a `bits`-bit up-counter are
/// in order, each within `max_step` ticks of the one before. A counter
/// that is read faster than it can wrap can only fail this if a read came
/// back wrong: some chipsets' PM timers return a glitched value now and
/// then, which must not reach a calibration. Then `b` can be trusted.
pub const fn in_order(a: u64, b: u64, c: u64, bits: u32, max_step: u64) -> bool {
    delta(a, b, bits) <= max_step && delta(b, c, bits) <= max_step
}
