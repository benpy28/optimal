//! The ACPI PM timer (ACPI 6.5 section 4.8.3.3): a free-running up-counter,
//! 24 or 32 bits wide (FADT TMR_VAL_EXT), at 315/88 MHz.

use crate::freq::Freq;

/// 315/88 MHz = 3,579,545.4545... Hz, in lowest terms 39,375,000 / 11.
pub const FREQ: Freq = match Freq::new(39_375_000, 11) {
    Some(f) => f,
    None => panic!("the PM timer frequency is a valid Freq"),
};

/// Counter width in bits.
pub const fn bits(wide: bool) -> u32 {
    if wide { 32 } else { 24 }
}
