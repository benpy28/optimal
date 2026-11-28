//! Local APIC registers (Intel SDM Vol. 3A 11.4 to 11.9, x2APIC in 11.12).
//!
//! Registers are named by their xAPIC MMIO offset. In x2APIC mode the same
//! register is MSR 0x800 + offset / 16, except that DFR, APR and the
//! remote-read register do not exist, the ICR is one 64-bit MSR, and
//! SELF IPI (offset 0x3F0) exists only there.

use alloc::vec::Vec;

use crate::vectors;

pub const ID: u16 = 0x20;
pub const VERSION: u16 = 0x30;
pub const TPR: u16 = 0x80;
pub const APR: u16 = 0x90;
pub const PPR: u16 = 0xA0;
pub const EOI: u16 = 0xB0;
pub const RRD: u16 = 0xC0;
pub const LDR: u16 = 0xD0;
pub const DFR: u16 = 0xE0;
pub const SVR: u16 = 0xF0;
pub const ISR: u16 = 0x100;
pub const TMR: u16 = 0x180;
pub const IRR: u16 = 0x200;
pub const ESR: u16 = 0x280;
pub const LVT_CMCI: u16 = 0x2F0;
pub const ICR_LOW: u16 = 0x300;
pub const ICR_HIGH: u16 = 0x310;
pub const LVT_TIMER: u16 = 0x320;
pub const LVT_THERMAL: u16 = 0x330;
pub const LVT_PERF: u16 = 0x340;
pub const LVT_LINT0: u16 = 0x350;
pub const LVT_LINT1: u16 = 0x360;
pub const LVT_ERROR: u16 = 0x370;
pub const TIMER_INITIAL: u16 = 0x380;
pub const TIMER_CURRENT: u16 = 0x390;
pub const TIMER_DIVIDE: u16 = 0x3E0;
pub const SELF_IPI: u16 = 0x3F0;

/// Is `offset` a register in xAPIC mode?
pub fn xapic_register(offset: u16) -> bool {
    matches!(
        offset,
        ID | VERSION
            | TPR
            | APR
            | PPR
            | EOI
            | RRD
            | LDR
            | DFR
            | SVR
            | ESR
            | LVT_CMCI
            | ICR_LOW
            | ICR_HIGH
            | LVT_TIMER
            | LVT_THERMAL
            | LVT_PERF
            | LVT_LINT0
            | LVT_LINT1
            | LVT_ERROR
            | TIMER_INITIAL
            | TIMER_CURRENT
            | TIMER_DIVIDE
    ) || (offset.is_multiple_of(0x10) && (ISR..IRR + 0x80).contains(&offset))
}

/// The MSR for register `offset` in x2APIC mode, or None if it has none.
pub fn x2apic_msr(offset: u16) -> Option<u32> {
    let exists = match offset {
        APR | RRD | DFR | ICR_HIGH => false,
        SELF_IPI => true,
        o => xapic_register(o),
    };
    exists.then_some(0x800 + u32::from(offset >> 4))
}

/// The ID register: the APIC ID is bits 31:24 in xAPIC mode (only 27:24 on
/// P6 and Pentium) and the whole register in x2APIC mode.
pub fn id(raw: u32, x2apic: bool) -> u32 {
    if x2apic { raw } else { raw >> 24 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub version: u8,
    /// Number of LVT entries minus one.
    pub max_lvt: u8,
    /// The APIC can suppress EOI broadcasts to I/O APICs.
    pub eoi_broadcast_suppression: bool,
}

impl Version {
    pub fn decode(raw: u32) -> Version {
        Version {
            version: raw as u8,
            max_lvt: (raw >> 16) as u8,
            eoi_broadcast_suppression: raw & (1 << 24) != 0,
        }
    }

    /// An integrated local APIC (version 0x1X), as opposed to the 82489DX
    /// (0x0X). Every x86_64 CPU has one.
    pub fn integrated(self) -> bool {
        self.version >> 4 == 1
    }

    /// Does this APIC have LVT register `reg`? Timer, LINT0, LINT1 and error
    /// always exist; the performance counter LVT needs at least 5 entries,
    /// thermal 6 and CMCI 7 (SDM 11.5.1 and 11.6.1, Figure 11-8).
    pub fn has_lvt(self, reg: u16) -> bool {
        match reg {
            LVT_TIMER | LVT_LINT0 | LVT_LINT1 | LVT_ERROR => true,
            LVT_PERF => self.max_lvt >= 4,
            LVT_THERMAL => self.max_lvt >= 5,
            LVT_CMCI => self.max_lvt >= 6,
            _ => false,
        }
    }
}

/// Delivery modes an LVT entry can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Fixed,
    Smi,
    Nmi,
    Init,
    ExtInt,
    /// 1, 3 and 6 are reserved in an LVT.
    Reserved(u8),
}

impl Delivery {
    fn bits(self) -> u32 {
        match self {
            Delivery::Fixed => 0,
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
            2 => Delivery::Smi,
            4 => Delivery::Nmi,
            5 => Delivery::Init,
            7 => Delivery::ExtInt,
            r => Delivery::Reserved(r as u8),
        }
    }
}

pub const LVT_MASKED: u32 = 1 << 16;
const LVT_ACTIVE_LOW: u32 = 1 << 13;
const LVT_LEVEL: u32 = 1 << 15;

/// One local vector table entry. The timer's mode bits (17 and 18) are
/// left zero, which is one-shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lvt {
    pub vector: u8,
    pub delivery: Delivery,
    /// Pin polarity (LINT0 and LINT1 only).
    pub active_low: bool,
    /// Level triggered (LINT0 and LINT1 with fixed delivery only).
    pub level: bool,
    pub masked: bool,
}

impl Lvt {
    /// A masked fixed-delivery entry on `vector`.
    pub const fn masked(vector: u8) -> Lvt {
        Lvt {
            vector,
            delivery: Delivery::Fixed,
            active_low: false,
            level: false,
            masked: true,
        }
    }

    pub fn encode(self) -> u32 {
        u32::from(self.vector)
            | self.delivery.bits() << 8
            | if self.active_low { LVT_ACTIVE_LOW } else { 0 }
            | if self.level { LVT_LEVEL } else { 0 }
            | if self.masked { LVT_MASKED } else { 0 }
    }

    pub fn decode(raw: u32) -> Lvt {
        Lvt {
            vector: raw as u8,
            delivery: Delivery::from_bits(raw >> 8),
            active_low: raw & LVT_ACTIVE_LOW != 0,
            level: raw & LVT_LEVEL != 0,
            masked: raw & LVT_MASKED != 0,
        }
    }
}

/// SVR bit 12: EOIs of level-triggered interrupts are not broadcast to
/// the I/O APICs (SDM 11.8.5). Only settable when the version register
/// says the APIC can ([`Version::eoi_broadcast_suppression`]); the kernel
/// sets it only in directed-EOI mode (crate::eoi).
pub const SVR_SUPPRESS_EOI_BROADCAST: u32 = 1 << 12;

/// The spurious-interrupt vector register: software enable (bit 8) and the
/// spurious vector. Focus checking and EOI-broadcast suppression stay off
/// (crate::eoi adds [`SVR_SUPPRESS_EOI_BROADCAST`] when it chooses to).
pub fn svr(vector: u8, enabled: bool) -> u32 {
    u32::from(vector) | if enabled { 1 << 8 } else { 0 }
}

pub fn svr_enabled(raw: u32) -> bool {
    raw & (1 << 8) != 0
}

/// Error status register bits (SDM 11.5.3).
pub const ESR_BITS: [(u32, &str); 8] = [
    (0x01, "send checksum"),
    (0x02, "receive checksum"),
    (0x04, "send accept"),
    (0x08, "receive accept"),
    (0x10, "redirectable IPI"),
    (0x20, "send illegal vector"),
    (0x40, "received illegal vector"),
    (0x80, "illegal register address"),
];

/// Names of the error bits set in `esr`, lowest bit first.
pub fn esr_errors(esr: u32) -> impl Iterator<Item = &'static str> {
    ESR_BITS
        .iter()
        .filter(move |(b, _)| esr & b != 0)
        .map(|&(_, n)| n)
}

/// Interrupt command register delivery modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiDelivery {
    Fixed,
    LowestPriority,
    Smi,
    Nmi,
    Init,
    StartUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shorthand {
    None,
    ToSelf,
    AllIncludingSelf,
    AllExcludingSelf,
}

/// An interprocessor interrupt (SDM 11.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipi {
    pub vector: u8,
    pub delivery: IpiDelivery,
    pub logical: bool,
    /// Level: assert (INIT level de-assert is the only use of clear).
    pub assert: bool,
    pub level_triggered: bool,
    pub shorthand: Shorthand,
    /// Destination APIC ID (ignored with a shorthand).
    pub destination: u32,
}

/// Delivery status (send pending), xAPIC ICR low bit 12. x2APIC has none.
pub const ICR_PENDING: u32 = 1 << 12;

impl Ipi {
    /// A fixed, edge-triggered IPI to this CPU on `vector`.
    pub const fn to_self(vector: u8) -> Ipi {
        Ipi {
            vector,
            delivery: IpiDelivery::Fixed,
            logical: false,
            assert: true,
            level_triggered: false,
            shorthand: Shorthand::ToSelf,
            destination: 0,
        }
    }

    fn low(self) -> u32 {
        let mode = match self.delivery {
            IpiDelivery::Fixed => 0,
            IpiDelivery::LowestPriority => 1,
            IpiDelivery::Smi => 2,
            IpiDelivery::Nmi => 4,
            IpiDelivery::Init => 5,
            IpiDelivery::StartUp => 6,
        };
        let sh = match self.shorthand {
            Shorthand::None => 0,
            Shorthand::ToSelf => 1,
            Shorthand::AllIncludingSelf => 2,
            Shorthand::AllExcludingSelf => 3,
        };
        u32::from(self.vector)
            | mode << 8
            | u32::from(self.logical) << 11
            | u32::from(self.assert) << 14
            | u32::from(self.level_triggered) << 15
            | sh << 18
    }

    /// The x2APIC ICR (one MSR): destination in the high 32 bits.
    pub fn x2apic(self) -> u64 {
        u64::from(self.destination) << 32 | u64::from(self.low())
    }

    /// The xAPIC ICR as (high, low): write high first, since writing low
    /// sends. None if the destination does not fit xAPIC's 8 bits.
    pub fn xapic(self) -> Option<(u32, u32)> {
        let dest = u8::try_from(self.destination).ok()?;
        Some((u32::from(dest) << 24, self.low()))
    }
}

/// Fixed-delivery vectors 0 to 15 are illegal: the APIC refuses them with
/// an illegal-vector error (SDM 11.5.2).
pub fn legal_fixed_vector(vector: u8) -> bool {
    vector >= 16
}

/// The register writes that set up a local APIC, in order, given its
/// version and the two LINT entries the routing plan chose.
///
/// 1. SVR first, with the software enable bit, because while the APIC is
///    software-disabled every LVT mask bit is forced on (SDM 11.4.7.2).
/// 2. Every LVT entry this APIC has: timer, thermal, performance counter
///    and CMCI masked on vectors of their own, LINT0 and LINT1 as planned,
///    error unmasked. Firmware may have left any of them live.
/// 3. Timer initial count 0, which stops a timer firmware left running.
/// 4. ESR written twice: the first write latches errors so far, the second
///    clears them (SDM 11.5.3: write before reading).
/// 5. TPR 0, accept every priority class.
pub fn setup(version: Version, lint0: Lvt, lint1: Lvt) -> Vec<(u16, u32)> {
    let mut w = Vec::with_capacity(12);
    w.push((SVR, svr(vectors::SPURIOUS, true)));
    let lvts = [
        (LVT_TIMER, Lvt::masked(vectors::LAPIC_TIMER)),
        (LVT_THERMAL, Lvt::masked(vectors::LAPIC_THERMAL)),
        (LVT_PERF, Lvt::masked(vectors::LAPIC_PERF)),
        (LVT_CMCI, Lvt::masked(vectors::LAPIC_CMCI)),
        (LVT_LINT0, lint0),
        (LVT_LINT1, lint1),
        (
            LVT_ERROR,
            Lvt {
                masked: false,
                ..Lvt::masked(vectors::LAPIC_ERROR)
            },
        ),
    ];
    for (reg, lvt) in lvts {
        if version.has_lvt(reg) {
            w.push((reg, lvt.encode()));
        }
    }
    w.push((TIMER_INITIAL, 0));
    w.push((ESR, 0));
    w.push((ESR, 0));
    w.push((TPR, 0));
    w
}
