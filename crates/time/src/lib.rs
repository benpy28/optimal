//! Time keeping logic for the Optimal kernel.
//!
//! Everything here is pure: frequencies, conversions, register layouts and
//! the decisions about which clock to trust, with no hardware access. The
//! kernel reads the counters, hands the readings to this crate, and acts
//! on what it is told, so every decision is tested on the host.
//!
//! Sources: the IA-PC HPET specification 1.0a, ACPI 6.5 section 4.8.3.3
//! (the PM timer), the Intel 8254 datasheet (the PIT), and the Intel SDM
//! Vol. 2A (CPUID leaves 01h, 06h, 15h, 16h and 80000007h), Vol. 3A
//! section 11.5.4 (the local APIC timer) and Vol. 3B section 18.17 (the
//! time-stamp counter).
//!
//! Policy, as in optimal-acpi and optimal-apic: never guess. A frequency is
//! an exact fraction, a measurement carries its proven error bound, and a
//! clock that cannot be trusted is refused with a named reason.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod calibrate;
pub mod counter;
pub mod cpuid;
pub mod freq;
pub mod hpet;
pub mod lapic;
pub mod oneshot;
pub mod pit;
pub mod pm;
pub mod refine;
pub mod select;
pub mod timeline;
pub mod watchdog;

pub use freq::{Freq, Scale};

/// Nanoseconds in a second.
pub const NS_PER_SEC: u64 = 1_000_000_000;

/// Property-test case count: `PROPTEST_CASES` if set (CI runs far more),
/// otherwise the given default.
#[cfg(test)]
pub(crate) fn test_cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod suite;
