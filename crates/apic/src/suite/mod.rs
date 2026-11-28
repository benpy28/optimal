//! Tests: hand-worked register values (with the working in comments),
//! small reference models of the hardware, and random inputs.

mod base;
mod eoi;
mod ioapic;
mod lapic;
mod msi;
mod pic;
mod plan;
mod vectors;

use proptest::test_runner::Config;

pub(crate) fn config(default: u32) -> Config {
    Config::with_cases(crate::test_cases(default))
}
