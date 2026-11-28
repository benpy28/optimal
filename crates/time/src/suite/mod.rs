//! Tests: hand-worked values (with the working in comments), small
//! reference models, and random inputs checked against exact arithmetic.

mod calibrate;
mod counter;
mod cpuid;
mod freq;
mod hpet;
mod lapic;
mod oneshot;
mod pit_pm;
mod refine;
mod select;
mod timeline;
mod watchdog;

use proptest::test_runner::Config;

pub(crate) fn config(default: u32) -> Config {
    Config::with_cases(crate::test_cases(default))
}

/// x * y as a 256-bit number (high, low), for checking bounds exactly
/// where a u128 would overflow.
pub(crate) fn mul_wide(x: u128, y: u128) -> (u128, u128) {
    const M: u128 = u64::MAX as u128;
    let (x0, x1, y0, y1) = (x & M, x >> 64, y & M, y >> 64);
    let p00 = x0 * y0;
    let p01 = x0 * y1;
    let p10 = x1 * y0;
    let p11 = x1 * y1;
    let mid = (p00 >> 64) + (p01 & M) + (p10 & M);
    let lo = (p00 & M) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (hi, lo)
}

#[test]
fn mul_wide_hand_worked() {
    assert_eq!(mul_wide(u128::MAX, u128::MAX), (u128::MAX - 1, 1));
    assert_eq!(mul_wide(1 << 100, 1 << 100), (1 << 72, 0));
    assert_eq!(mul_wide(3, 5), (0, 15));
}
