//! The MCFG (PCI Firmware Specification 3.3, section 4.1.2): where each PCI
//! segment group's memory-mapped configuration space (ECAM) is.
//!
//! The kernel needs it now for one thing: ECAM is device registers, and
//! firmware memory maps call it reserved, the same kind of memory SeaBIOS
//! keeps ACPI tables in. So a corrupt table pointer into ECAM would pass
//! the memory-map check and make the kernel read PCI configuration
//! registers. [`Mcfg::refused`] gives every range the ACPI reader must
//! refuse for that reason. PCI enumeration will use the same entries later.
//!
//! Each entry gives the ECAM base for bus 0 of its segment group and the
//! bus range it decodes; bus `b` takes the 1 MiB at `base + (b << 20)`
//! (PCI Express Base Specification 6.0, section 7.2.2).

use alloc::vec::Vec;

use crate::reach::Span;
use crate::{Error, HEADER_LEN, Sdt, u8_at, u16_at, u64_at};

/// Entries start after the header and 8 reserved bytes; each is 16 bytes.
const ENTRIES: usize = HEADER_LEN + 8;
const ENTRY_LEN: usize = 16;

/// One bus takes 1 MiB of ECAM (32 devices x 8 functions x 4 KiB).
pub const BUS_BYTES: u64 = 1 << 20;

/// One MCFG entry, exactly as the table gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ecam {
    /// Base address for bus 0 of the segment group, even if `start_bus` is
    /// higher.
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl Ecam {
    /// The physical range the entry's buses decode, `None` if it can't be
    /// represented (it would run past the top of the address space).
    pub fn span(&self) -> Option<Span> {
        let start = self
            .base
            .checked_add(u64::from(self.start_bus) * BUS_BYTES)?;
        let end = self
            .base
            .checked_add((u64::from(self.end_bus) + 1) * BUS_BYTES)?;
        Some(Span { start, end })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mcfg {
    pub checksum_ok: bool,
    /// Entries that are usable for PCI: a non-zero, 1 MiB aligned base, a
    /// bus range that isn't reversed and fits the address space, and no
    /// overlap with an earlier usable entry of the same segment group.
    pub entries: Vec<Ecam>,
    /// Entries that failed one of those checks. Their ranges are still
    /// refused (see [`Mcfg::refused`]) unless the base is zero.
    pub bad: Vec<Ecam>,
    /// The table's length leaves a partial entry at the end (ignored).
    pub partial_entry: bool,
}

impl Mcfg {
    pub fn parse(bytes: &[u8]) -> Result<Mcfg, Error> {
        let sdt = Sdt::new(bytes, b"MCFG", ENTRIES)?;
        let b = sdt.bytes;
        let mut m = Mcfg {
            checksum_ok: sdt.checksum_ok,
            entries: Vec::new(),
            bad: Vec::new(),
            partial_entry: !(b.len() - ENTRIES).is_multiple_of(ENTRY_LEN),
        };
        for e in b[ENTRIES..].as_chunks::<ENTRY_LEN>().0 {
            // Each chunk is 16 bytes, so these reads succeed.
            let ecam = Ecam {
                base: u64_at(e, 0).unwrap_or(0),
                segment: u16_at(e, 8).unwrap_or(0),
                start_bus: u8_at(e, 10).unwrap_or(0),
                end_bus: u8_at(e, 11).unwrap_or(0),
            };
            let usable = ecam.base != 0
                && ecam.base.is_multiple_of(BUS_BYTES)
                && ecam.start_bus <= ecam.end_bus
                && ecam.span().is_some()
                && !m.entries.iter().any(|o| {
                    o.segment == ecam.segment
                        && o.start_bus <= ecam.end_bus
                        && ecam.start_bus <= o.end_bus
                });
            if usable {
                m.entries.push(ecam);
            } else {
                m.bad.push(ecam);
            }
        }
        Ok(m)
    }

    /// Every range that may be PCI configuration space, for the ACPI
    /// reader to refuse. A usable entry refuses exactly its buses. A bad
    /// entry may still describe real ECAM with one field wrong, so it
    /// refuses all 256 buses its base could decode (cut at the top of the
    /// address space), whatever its bus numbers say. The one exception is a
    /// zero base: ECAM can never be there (it is the real-mode interrupt
    /// table), and refusing it would refuse the legacy BIOS area where
    /// BIOS firmware keeps the RSDP.
    pub fn refused(&self) -> Vec<Span> {
        let mut v: Vec<Span> = self.entries.iter().filter_map(Ecam::span).collect();
        v.extend(self.bad.iter().filter(|e| e.base != 0).map(|e| Span {
            start: e.base,
            end: e.base.saturating_add(256 * BUS_BYTES),
        }));
        v
    }
}
