//! Tests for the table parsers: hand-worked tables (with the working in
//! comments), random and corrupted tables, and reference-model round trips.

mod fadt;
mod hpet;
mod madt;
mod tables;

use proptest::test_runner::Config;

pub(crate) fn config(default: u32) -> Config {
    Config::with_cases(crate::test_cases(default))
}
