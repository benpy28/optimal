//! The HPET description table (IA-PC HPET specification 1.0a, section 3.2.4).

use crate::{Error, Gas, HEADER_LEN, Sdt, u8_at, u16_at, u32_at};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hpet {
    pub checksum_ok: bool,
    /// Physical base of the 1 KiB register block.
    pub address: u64,
    pub hardware_rev: u8,
    /// Number of comparators (timers), 1 to 32.
    pub comparators: u8,
    /// The main counter is 64 bits wide (else 32).
    pub counter_64bit: bool,
    pub legacy_replacement: bool,
    pub pci_vendor: u16,
    pub number: u8,
    /// Minimum periodic tick the firmware says is safe, in counter ticks.
    pub min_tick: u16,
}

impl Hpet {
    pub fn parse(bytes: &[u8]) -> Result<Hpet, Error> {
        let sdt = Sdt::new(bytes, b"HPET", HEADER_LEN + 20)?;
        let b = sdt.bytes;
        let id = u32_at(b, 36).ok_or(Error::TooShort)?;
        let gas = Gas::at(b, 40).ok_or(Error::TooShort)?;
        // The registers are memory-mapped and the block is 1 KiB aligned;
        // anything else can't be an HPET.
        if gas.space != Gas::SYSTEM_MEMORY || gas.address == 0 || gas.address % 1024 != 0 {
            return Err(Error::BadField);
        }
        Ok(Hpet {
            checksum_ok: sdt.checksum_ok,
            address: gas.address,
            hardware_rev: id as u8,
            comparators: ((id >> 8) & 0x1f) as u8 + 1,
            counter_64bit: id & (1 << 13) != 0,
            legacy_replacement: id & (1 << 15) != 0,
            pci_vendor: (id >> 16) as u16,
            number: u8_at(b, 52).ok_or(Error::TooShort)?,
            min_tick: u16_at(b, 53).ok_or(Error::TooShort)?,
        })
    }
}
