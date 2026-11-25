//! The FADT (ACPI 6.5 section 5.2.9), signature "FACP": the fixed hardware
//! the kernel needs now (the ACPI PM timer and boot architecture flags).
//!
//! The FADT has grown with every ACPI revision, so each field is read only
//! if the table is long enough to hold it; a missing field reads as absent.

use crate::{Error, Gas, Sdt, u8_at, u16_at, u32_at};

/// Where the ACPI PM timer (3.579545 MHz) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmTimer {
    Io(u16),
    Memory(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    pub checksum_ok: bool,
    pub pm_timer: Option<PmTimer>,
    /// The PM timer counts 32 bits (TMR_VAL_EXT), else 24.
    pub pm_timer_32bit: bool,
    /// Both the 32-bit and the 64-bit (X_PM_TMR_BLK) timer fields were set
    /// and disagree; the 64-bit one was used, as the specification says.
    pub pm_timer_mismatch: bool,
    /// HW_REDUCED_ACPI: no fixed hardware (no PM timer, no PM1 blocks).
    pub hw_reduced: bool,
    /// IAPC_BOOT_ARCH flags (0 if the table is too old to have them).
    pub boot_arch: u16,
    /// RTC century register index, if any.
    pub century: Option<u8>,
}

/// Field offsets (ACPI 6.5 Table 5.9).
const PM_TMR_BLK: usize = 76;
const PM_TMR_LEN: usize = 91;
const CENTURY: usize = 108;
const IAPC_BOOT_ARCH: usize = 109;
const FLAGS: usize = 112;
const X_PM_TMR_BLK: usize = 208;

const TMR_VAL_EXT: u32 = 1 << 8;
const HW_REDUCED_ACPI: u32 = 1 << 20;

impl Fadt {
    /// IAPC_BOOT_ARCH bits.
    pub const LEGACY_DEVICES: u16 = 1; // bit 0
    pub const HAS_8042: u16 = 1 << 1;
    pub const VGA_NOT_PRESENT: u16 = 1 << 2;
    pub const MSI_NOT_SUPPORTED: u16 = 1 << 3;
    pub const CMOS_RTC_NOT_PRESENT: u16 = 1 << 5;

    pub fn parse(bytes: &[u8]) -> Result<Fadt, Error> {
        // ACPI 1.0's FADT, the shortest that exists, is 116 bytes.
        let sdt = Sdt::new(bytes, b"FACP", FLAGS + 4)?;
        let b = sdt.bytes;
        let flags = u32_at(b, FLAGS).ok_or(Error::TooShort)?;
        let hw_reduced = flags & HW_REDUCED_ACPI != 0;
        // The 32-bit field, valid only with the length the spec fixes.
        let legacy = u32_at(b, PM_TMR_BLK)
            .filter(|&p| p != 0 && u8_at(b, PM_TMR_LEN) == Some(4))
            .and_then(|p| u16::try_from(p).ok())
            .map(PmTimer::Io);
        let extended = Gas::at(b, X_PM_TMR_BLK).and_then(|g| match g.space {
            _ if g.address == 0 => None,
            Gas::SYSTEM_IO => u16::try_from(g.address).ok().map(PmTimer::Io),
            Gas::SYSTEM_MEMORY => Some(PmTimer::Memory(g.address)),
            _ => None,
        });
        let pm_timer = if hw_reduced {
            None
        } else {
            extended.or(legacy)
        };
        Ok(Fadt {
            checksum_ok: sdt.checksum_ok,
            pm_timer,
            pm_timer_32bit: flags & TMR_VAL_EXT != 0,
            pm_timer_mismatch: !hw_reduced
                && matches!((legacy, extended), (Some(l), Some(x)) if l != x),
            hw_reduced,
            boot_arch: u16_at(b, IAPC_BOOT_ARCH).unwrap_or(0),
            century: u8_at(b, CENTURY).filter(|&c| c != 0),
        })
    }
}
