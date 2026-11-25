//! I/O APIC registers (Intel 82093AA datasheet, section 3).
//!
//! Two MMIO registers: IOREGSEL at +0x00 selects an internal register and
//! IOWIN at +0x10 reads or writes it, 32 bits at a time. The internal
//! registers are the ID (0), version (1), arbitration ID (2) and two
//! dwords per redirection table entry from 0x10.

pub const IOREGSEL: u64 = 0x00;
pub const IOWIN: u64 = 0x10;
pub const REG_ID: u8 = 0x00;
pub const REG_VERSION: u8 = 0x01;

/// IOREGSEL is 8 bits and the table starts at 0x10 with two registers per
/// entry, so an I/O APIC can have at most 120 pins. A version register
/// that claims more is not believed.
pub const MAX_PINS: u16 = 120;

/// The register pair of redirection entry `pin`: (low, high), or None past
/// the last pin IOREGSEL can select.
pub fn redirection_regs(pin: u8) -> Option<(u8, u8)> {
    if u16::from(pin) >= MAX_PINS {
        return None;
    }
    let low = 0x10 + 2 * pin;
    Some((low, low + 1))
}

/// Can an I/O APIC's registers live at `address`? It must be non-zero and
/// 16-byte aligned, and IOREGSEL to the end of IOWIN (0x14 bytes) must fit
/// in one 4 KiB page, so one page mapping covers it.
pub fn address_ok(address: u64) -> bool {
    address != 0 && address.is_multiple_of(16) && address % 4096 <= 4096 - 0x14
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub version: u8,
    /// Highest redirection entry: the I/O APIC has max_entry + 1 pins.
    pub max_entry: u8,
}

impl Version {
    pub fn decode(raw: u32) -> Version {
        Version {
            version: raw as u8,
            max_entry: (raw >> 16) as u8,
        }
    }

    pub fn pins(self) -> u16 {
        u16::from(self.max_entry) + 1
    }
}

/// The I/O APIC ID from its ID register. The 82093AA has 4 bits (27:24);
/// later chipsets widen it to 8, but the MADT ID is compared on the low 4
/// bits only so neither kind is reported wrongly.
pub fn id_matches(id_register: u32, madt_id: u8) -> bool {
    (id_register >> 24) & 0xF == u32::from(madt_id & 0xF)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Fixed,
    LowestPriority,
    Smi,
    Nmi,
    Init,
    ExtInt,
    /// 3 and 6 are reserved.
    Reserved(u8),
}

impl Delivery {
    fn bits(self) -> u32 {
        match self {
            Delivery::Fixed => 0,
            Delivery::LowestPriority => 1,
            Delivery::Smi => 2,
            Delivery::Nmi => 4,
            Delivery::Init => 5,
            Delivery::ExtInt => 7,
            Delivery::Reserved(v) => u32::from(v & 7),
        }
    }

    fn from_bits(v: u32) -> Delivery {
        match v & 7 {
            0 => Delivery::Fixed,
            1 => Delivery::LowestPriority,
            2 => Delivery::Smi,
            4 => Delivery::Nmi,
            5 => Delivery::Init,
            7 => Delivery::ExtInt,
            r => Delivery::Reserved(r as u8),
        }
    }
}

pub const MASKED: u32 = 1 << 16;
const LOGICAL: u32 = 1 << 11;
const PENDING: u32 = 1 << 12;
const ACTIVE_LOW: u32 = 1 << 13;
const REMOTE_IRR: u32 = 1 << 14;
const LEVEL: u32 = 1 << 15;

/// One redirection table entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redirection {
    pub vector: u8,
    pub delivery: Delivery,
    /// Logical destination mode (else physical: `destination` is an APIC ID).
    pub logical: bool,
    pub active_low: bool,
    pub level: bool,
    pub masked: bool,
    /// Bits 63:56. Physical mode can only name APIC IDs 0 to 255; reaching
    /// higher x2APIC IDs needs interrupt remapping.
    pub destination: u8,
}

/// What a redirection entry reads back as: the entry, plus the two
/// read-only status bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readback {
    pub entry: Redirection,
    /// Delivery status: an interrupt is waiting to be sent.
    pub pending: bool,
    /// Remote IRR: a level interrupt was accepted and awaits EOI.
    pub remote_irr: bool,
}

impl Redirection {
    /// (low dword, high dword).
    pub fn encode(self) -> (u32, u32) {
        let low = u32::from(self.vector)
            | self.delivery.bits() << 8
            | if self.logical { LOGICAL } else { 0 }
            | if self.active_low { ACTIVE_LOW } else { 0 }
            | if self.level { LEVEL } else { 0 }
            | if self.masked { MASKED } else { 0 };
        (low, u32::from(self.destination) << 24)
    }

    pub fn decode(low: u32, high: u32) -> Readback {
        Readback {
            entry: Redirection {
                vector: low as u8,
                delivery: Delivery::from_bits(low >> 8),
                logical: low & LOGICAL != 0,
                active_low: low & ACTIVE_LOW != 0,
                level: low & LEVEL != 0,
                masked: low & MASKED != 0,
                destination: (high >> 24) as u8,
            },
            pending: low & PENDING != 0,
            remote_irr: low & REMOTE_IRR != 0,
        }
    }

    /// The register writes that install this entry on `pin` without ever
    /// leaving a half-written entry unmasked: low with the mask bit set,
    /// then high, then low as wanted. None if `pin` is past `MAX_PINS`.
    pub fn writes(self, pin: u8) -> Option<[(u8, u32); 3]> {
        let (lo_reg, hi_reg) = redirection_regs(pin)?;
        let (lo, hi) = self.encode();
        Some([(lo_reg, lo | MASKED), (hi_reg, hi), (lo_reg, lo)])
    }
}
