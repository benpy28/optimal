//! The MADT (ACPI 6.5 section 5.2.12): interrupt controllers and CPUs.

use alloc::vec::Vec;

use crate::{Error, HEADER_LEN, Sdt, u8_at, u16_at, u32_at, u64_at};

/// One CPU's local APIC, from a Processor Local APIC entry (type 0) or a
/// Processor Local x2APIC entry (type 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    /// The ACPI processor UID (matches the processor's _UID in the DSDT).
    pub uid: u32,
    /// Usable now.
    pub enabled: bool,
    /// Not enabled now, but the OS may bring it online later (hot-add).
    pub online_capable: bool,
    /// Came from an x2APIC entry.
    pub x2apic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoApic {
    pub id: u8,
    pub address: u32,
    /// First global system interrupt this I/O APIC handles.
    pub gsi_base: u32,
}

/// Interrupt polarity and trigger mode (MPS INTI flags, Table 5.26).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// Conforms to the bus: active high for ISA.
    Bus,
    High,
    Low,
    /// Value 2, which the specification reserves.
    Reserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Conforms to the bus: edge for ISA.
    Bus,
    Edge,
    Level,
    Reserved,
}

fn inti(flags: u16) -> (Polarity, Trigger) {
    let p = match flags & 3 {
        0 => Polarity::Bus,
        1 => Polarity::High,
        2 => Polarity::Reserved,
        _ => Polarity::Low,
    };
    let t = match (flags >> 2) & 3 {
        0 => Trigger::Bus,
        1 => Trigger::Edge,
        2 => Trigger::Reserved,
        _ => Trigger::Level,
    };
    (p, t)
}

/// An Interrupt Source Override (type 2): ISA IRQ `source` arrives on
/// `gsi` instead of the identity-mapped one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Override {
    pub source: u8,
    pub gsi: u32,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

/// A local APIC NMI (type 4 or 10): which LINT pin carries NMI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LapicNmi {
    /// Processor UID, or `None` for all processors (0xff in type 4,
    /// 0xffff_ffff in type 10).
    pub uid: Option<u32>,
    pub lint: u8,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

/// An NMI Source (type 3): a GSI that carries NMI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NmiSource {
    pub gsi: u32,
    pub polarity: Polarity,
    pub trigger: Trigger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Madt {
    pub checksum_ok: bool,
    /// Local APIC base: the 64-bit override (type 5) if present, else the
    /// 32-bit field in the header.
    pub lapic_address: u64,
    /// PCAT_COMPAT: the machine also has dual 8259 PICs, which the OS must
    /// mask before using the APICs.
    pub pcat_compat: bool,
    pub cpus: Vec<Cpu>,
    pub ioapics: Vec<IoApic>,
    pub overrides: Vec<Override>,
    pub lapic_nmis: Vec<LapicNmi>,
    pub nmi_sources: Vec<NmiSource>,
    /// Entries of a type this parser doesn't use (GIC, SAPIC, ...).
    pub unknown_entries: usize,
    /// Entries that were skipped: shorter than their type needs, a second
    /// CPU with an APIC ID already seen, an xAPIC entry with the broadcast
    /// ID 0xff, a second I/O APIC with the same ID or GSI base, or a second
    /// override for the same ISA IRQ.
    pub bad_entries: usize,
    /// The entry list ended in the middle of an entry (or with a zero
    /// length, which would loop forever); everything before it was used.
    pub truncated: bool,
}

/// Minimum entry length for each type this parser reads.
fn min_len(kind: u8) -> Option<usize> {
    Some(match kind {
        0 => 8,
        1 => 12,
        2 => 10,
        3 => 8,
        4 => 6,
        5 => 12,
        9 => 16,
        10 => 12,
        _ => return None,
    })
}

impl Madt {
    pub fn parse(bytes: &[u8]) -> Result<Madt, Error> {
        let sdt = Sdt::new(bytes, b"APIC", HEADER_LEN + 8)?;
        let b = sdt.bytes;
        let flags = u32_at(b, 40).ok_or(Error::TooShort)?;
        let mut m = Madt {
            checksum_ok: sdt.checksum_ok,
            lapic_address: u64::from(u32_at(b, 36).ok_or(Error::TooShort)?),
            pcat_compat: flags & 1 != 0,
            cpus: Vec::new(),
            ioapics: Vec::new(),
            overrides: Vec::new(),
            lapic_nmis: Vec::new(),
            nmi_sources: Vec::new(),
            unknown_entries: 0,
            bad_entries: 0,
            truncated: false,
        };
        let mut off = HEADER_LEN + 8;
        while off < b.len() {
            let (Some(kind), Some(len)) = (u8_at(b, off), u8_at(b, off + 1)) else {
                m.truncated = true;
                break;
            };
            let len = len as usize;
            if len < 2 || off + len > b.len() {
                m.truncated = true;
                break;
            }
            let e = &b[off..off + len];
            off += len;
            match min_len(kind) {
                None => m.unknown_entries += 1,
                Some(need) if len < need => m.bad_entries += 1,
                Some(_) => {
                    if !m.entry(kind, e) {
                        m.bad_entries += 1;
                    }
                }
            }
        }
        Ok(m)
    }

    /// Adds one entry of a known type, long enough for its type. Returns
    /// false if it must be skipped.
    fn entry(&mut self, kind: u8, e: &[u8]) -> bool {
        // The lengths were checked against min_len, so these reads succeed.
        let r8 = |o| u8_at(e, o).unwrap_or(0);
        let r16 = |o| u16_at(e, o).unwrap_or(0);
        let r32 = |o| u32_at(e, o).unwrap_or(0);
        match kind {
            0 | 9 => {
                let (apic_id, uid, flags) = if kind == 0 {
                    (u32::from(r8(3)), u32::from(r8(2)), r32(4))
                } else {
                    (r32(4), r32(12), r32(8))
                };
                if kind == 0 && apic_id == 0xff {
                    return false;
                }
                if self.cpus.iter().any(|c| c.apic_id == apic_id) {
                    return false;
                }
                self.cpus.push(Cpu {
                    apic_id,
                    uid,
                    enabled: flags & 1 != 0,
                    // Online Capable is only defined when Enabled is clear.
                    online_capable: flags & 1 == 0 && flags & 2 != 0,
                    x2apic: kind == 9,
                });
            }
            1 => {
                let io = IoApic {
                    id: r8(2),
                    address: r32(4),
                    gsi_base: r32(8),
                };
                if self
                    .ioapics
                    .iter()
                    .any(|i| i.id == io.id || i.gsi_base == io.gsi_base)
                {
                    return false;
                }
                self.ioapics.push(io);
            }
            2 => {
                // Bus 0 (ISA) is the only bus the specification defines.
                let source = r8(3);
                if r8(2) != 0 || self.overrides.iter().any(|o| o.source == source) {
                    return false;
                }
                let (polarity, trigger) = inti(r16(8));
                self.overrides.push(Override {
                    source,
                    gsi: r32(4),
                    polarity,
                    trigger,
                });
            }
            3 => {
                let (polarity, trigger) = inti(r16(2));
                self.nmi_sources.push(NmiSource {
                    gsi: r32(4),
                    polarity,
                    trigger,
                });
            }
            4 | 10 => {
                let (uid, flags, lint) = if kind == 4 {
                    let u = r8(2);
                    ((u != 0xff).then_some(u32::from(u)), r16(3), r8(5))
                } else {
                    let u = r32(4);
                    ((u != u32::MAX).then_some(u), r16(2), r8(8))
                };
                if lint > 1 {
                    return false;
                }
                let (polarity, trigger) = inti(flags);
                self.lapic_nmis.push(LapicNmi {
                    uid,
                    lint,
                    polarity,
                    trigger,
                });
            }
            5 => {
                let a = u64_at(e, 4).unwrap_or(0);
                if a == 0 {
                    return false;
                }
                self.lapic_address = a;
            }
            _ => unreachable!("min_len only knows the types above"),
        }
        true
    }

    /// The GSI and mode for ISA IRQ `irq`: its override if there is one,
    /// else the identity mapping with ISA defaults (active high, edge).
    pub fn isa_irq(&self, irq: u8) -> (u32, Polarity, Trigger) {
        match self.overrides.iter().find(|o| o.source == irq) {
            Some(o) => {
                let p = if o.polarity == Polarity::Bus {
                    Polarity::High
                } else {
                    o.polarity
                };
                let t = if o.trigger == Trigger::Bus {
                    Trigger::Edge
                } else {
                    o.trigger
                };
                (o.gsi, p, t)
            }
            None => (u32::from(irq), Polarity::High, Trigger::Edge),
        }
    }
}
