//! The interrupt routing plan: which I/O APIC pin each ISA IRQ arrives on,
//! with what polarity and trigger, which pins carry NMI, and how the boot
//! CPU's LINT pins are set, all from the MADT (ACPI 6.5 section 5.2.12).
//!
//! Rules, in order:
//! 1. I/O APICs are taken in MADT order. One is not used if its address
//!    cannot hold the registers, the kernel could not reach it, it claims
//!    more pins than can exist, its GSI range runs past 2^32, or the range
//!    overlaps one already taken.
//! 2. NMI sources (MADT type 3) claim their pins first, as NMI delivery,
//!    unmasked, because the firmware says those inputs are NMIs.
//! 3. ISA IRQs with an Interrupt Source Override go to its GSI, with its
//!    polarity and trigger (a "conforms to the bus" field means ISA's
//!    defaults, active high and edge).
//! 4. Every other ISA IRQ goes to the GSI of the same number, active high,
//!    edge, unless an override already took that GSI (QEMU and most PCs
//!    move the timer from IRQ 0 to GSI 2, which takes IRQ 2's pin).
//! 5. IRQ 2 is never routed: it is the 8259 cascade, no device uses it.
//! 6. Anything that cannot be routed safely is refused with a reason:
//!    a GSI no I/O APIC serves, a reserved polarity or trigger, a GSI
//!    already taken, or a boot CPU whose APIC ID an I/O APIC cannot name.
//!
//! Every route is installed masked on vector `vectors::isa(irq)`; a driver
//! unmasks its line when it is ready. Pins nothing uses are masked on
//! `vectors::UNROUTED`.

use alloc::vec::Vec;

use optimal_acpi::madt::{Madt, Polarity, Trigger};

use crate::ioapic::{self, Delivery, Redirection};
use crate::lapic::{self, Lvt};
use crate::vectors;

/// What the plan needs from the hardware.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    pub madt: &'a Madt,
    /// The boot CPU's APIC ID, read from its local APIC.
    pub bsp_apic_id: u32,
    /// For each MADT I/O APIC, in MADT order, its highest redirection entry
    /// as its version register reports it, or None if the kernel did not
    /// reach it (the address was refused or did not answer).
    pub ioapic_max_entry: &'a [Option<u8>],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoApicStatus {
    Used,
    /// The MADT address cannot hold I/O APIC registers (`ioapic::address_ok`).
    BadAddress,
    /// The kernel could not reach it.
    Unreachable,
    /// gsi_base + pins is past 2^32.
    RangeOverflow,
    /// Its version register claims more pins than IOREGSEL can select
    /// (`ioapic::MAX_PINS`).
    TooManyPins,
    /// Its GSIs overlap those of an earlier I/O APIC (by MADT index).
    Overlaps(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoApicPlan {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
    /// Number of pins (0 if it is not used).
    pub pins: u16,
    pub status: IoApicStatus,
}

impl IoApicPlan {
    /// Does this I/O APIC serve `gsi`? Only a used one serves anything.
    pub fn serves(&self, gsi: u32) -> bool {
        self.status == IoApicStatus::Used
            && gsi >= self.gsi_base
            && u64::from(gsi) < u64::from(self.gsi_base) + u64::from(self.pins)
    }
}

/// One ISA IRQ's route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub gsi: u32,
    /// MADT index of the I/O APIC and the pin on it.
    pub ioapic: usize,
    pub pin: u8,
    /// Came from an Interrupt Source Override.
    pub overridden: bool,
    pub entry: Redirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotRouted {
    /// IRQ 2, the 8259 cascade. Not a problem.
    Cascade,
    /// Its identity GSI was taken by another IRQ's override. Not a
    /// problem: it means the IRQ does not exist on its own.
    Shadowed { gsi: u32, by: u8 },
    /// No used I/O APIC serves its GSI.
    NoIoApic { gsi: u32 },
    /// The override's polarity or trigger is the reserved value.
    ReservedFlags { gsi: u32 },
    /// Another override already routed an IRQ to this GSI.
    GsiTaken { gsi: u32, by: u8 },
    /// The GSI is an NMI source.
    NmiSource { gsi: u32 },
    /// The boot CPU's APIC ID is above 255, which an I/O APIC entry
    /// cannot name without interrupt remapping.
    DestinationTooWide { apic_id: u32 },
}

impl NotRouted {
    /// Is this something to warn about, rather than how PCs normally are?
    pub fn is_warning(self) -> bool {
        !matches!(self, NotRouted::Cascade | NotRouted::Shadowed { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NmiPin {
    pub gsi: u32,
    pub ioapic: usize,
    pub pin: u8,
    pub entry: Redirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Warning {
    IoApic {
        index: usize,
        status: IoApicStatus,
    },
    Isa {
        irq: u8,
        why: NotRouted,
    },
    /// An Interrupt Source Override for a source above 15, which is not
    /// an ISA IRQ; it is ignored.
    OverrideNotIsa {
        source: u8,
    },
    /// An NMI source on a GSI no used I/O APIC serves.
    NmiSourceNoIoApic {
        gsi: u32,
    },
    /// An NMI source with the reserved polarity value.
    NmiSourceReservedFlags {
        gsi: u32,
    },
    /// A second NMI source for the same GSI.
    NmiSourceDuplicate {
        gsi: u32,
    },
    /// An NMI source, but the boot CPU's ID is too wide to target.
    NmiSourceDestinationTooWide {
        gsi: u32,
    },
    /// A local APIC NMI entry for this CPU with the reserved polarity: the
    /// pin is left masked rather than guessed.
    LintReservedFlags {
        lint: u8,
    },
    /// Two local APIC NMI entries for the same pin disagree on polarity;
    /// the first one is used.
    LintConflict {
        lint: u8,
    },
    /// The boot CPU's APIC ID is not in the MADT, so only "all processors"
    /// local NMI entries apply to it.
    BspNotInMadt {
        apic_id: u32,
    },
    /// The MADT lists the boot CPU as not enabled.
    BspNotEnabled {
        apic_id: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub ioapics: Vec<IoApicPlan>,
    /// ISA IRQs 0 to 15.
    pub isa: [Result<Route, NotRouted>; 16],
    pub nmi_pins: Vec<NmiPin>,
    /// The boot CPU's LINT0 and LINT1 entries.
    pub lint: [Lvt; 2],
    /// The boot CPU's APIC ID as an I/O APIC destination, if it fits.
    pub destination: Option<u8>,
    pub warnings: Vec<Warning>,
}

fn polarity_low(p: Polarity) -> Option<bool> {
    match p {
        Polarity::Bus | Polarity::High => Some(false),
        Polarity::Low => Some(true),
        Polarity::Reserved => None,
    }
}

fn trigger_level(t: Trigger) -> Option<bool> {
    match t {
        Trigger::Bus | Trigger::Edge => Some(false),
        Trigger::Level => Some(true),
        Trigger::Reserved => None,
    }
}

/// A masked entry for a pin nothing uses.
pub fn unrouted(destination: u8) -> Redirection {
    Redirection {
        vector: vectors::UNROUTED,
        delivery: Delivery::Fixed,
        logical: false,
        active_low: false,
        level: false,
        masked: true,
        destination,
    }
}

impl Plan {
    pub fn build(inp: Inputs<'_>) -> Plan {
        let madt = inp.madt;
        let mut warnings = Vec::new();

        // 1. I/O APICs.
        let mut ioapics: Vec<IoApicPlan> = Vec::with_capacity(madt.ioapics.len());
        for (i, io) in madt.ioapics.iter().enumerate() {
            let max = inp.ioapic_max_entry.get(i).copied().flatten();
            let pins = max.map_or(0, |m| u16::from(m) + 1);
            let end = u64::from(io.gsi_base) + u64::from(pins);
            let status = if !ioapic::address_ok(u64::from(io.address)) {
                IoApicStatus::BadAddress
            } else if max.is_none() {
                IoApicStatus::Unreachable
            } else if pins > ioapic::MAX_PINS {
                IoApicStatus::TooManyPins
            } else if end > 1 << 32 {
                IoApicStatus::RangeOverflow
            } else if let Some(o) = ioapics.iter().position(|p| {
                p.status == IoApicStatus::Used
                    && u64::from(p.gsi_base) < end
                    && u64::from(io.gsi_base) < u64::from(p.gsi_base) + u64::from(p.pins)
            }) {
                IoApicStatus::Overlaps(o)
            } else {
                IoApicStatus::Used
            };
            if status != IoApicStatus::Used {
                warnings.push(Warning::IoApic { index: i, status });
            }
            ioapics.push(IoApicPlan {
                id: io.id,
                address: io.address,
                gsi_base: io.gsi_base,
                pins: if status == IoApicStatus::Used {
                    pins
                } else {
                    0
                },
                status,
            });
        }
        let locate = |gsi: u32| -> Option<(usize, u8)> {
            ioapics
                .iter()
                .position(|p| p.serves(gsi))
                .map(|i| (i, (gsi - ioapics[i].gsi_base) as u8))
        };
        let dest = u8::try_from(inp.bsp_apic_id).ok();

        // 2. NMI sources.
        let mut nmi_pins: Vec<NmiPin> = Vec::new();
        for s in &madt.nmi_sources {
            if nmi_pins.iter().any(|n| n.gsi == s.gsi) {
                warnings.push(Warning::NmiSourceDuplicate { gsi: s.gsi });
                continue;
            }
            let Some((ioapic, pin)) = locate(s.gsi) else {
                warnings.push(Warning::NmiSourceNoIoApic { gsi: s.gsi });
                continue;
            };
            let Some(active_low) = polarity_low(s.polarity) else {
                warnings.push(Warning::NmiSourceReservedFlags { gsi: s.gsi });
                continue;
            };
            let Some(destination) = dest else {
                warnings.push(Warning::NmiSourceDestinationTooWide { gsi: s.gsi });
                continue;
            };
            // NMI delivery ignores the vector and is always edge
            // triggered (82093AA 3.2.4), whatever the MADT says.
            nmi_pins.push(NmiPin {
                gsi: s.gsi,
                ioapic,
                pin,
                entry: Redirection {
                    vector: 0,
                    delivery: Delivery::Nmi,
                    logical: false,
                    active_low,
                    level: false,
                    masked: false,
                    destination,
                },
            });
        }

        // 3 to 5. ISA IRQs: overrides first, then identity.
        let mut isa: [Result<Route, NotRouted>; 16] = [Err(NotRouted::Cascade); 16];
        let mut taken: Vec<(u32, u8)> = Vec::new();
        let mut route = |irq: u8, gsi: u32, p: Polarity, t: Trigger, overridden: bool| {
            if let Some(&(_, by)) = taken.iter().find(|&&(g, _)| g == gsi) {
                return Err(if overridden {
                    NotRouted::GsiTaken { gsi, by }
                } else {
                    NotRouted::Shadowed { gsi, by }
                });
            }
            if nmi_pins.iter().any(|n| n.gsi == gsi) {
                return Err(NotRouted::NmiSource { gsi });
            }
            let (ioapic, pin) = locate(gsi).ok_or(NotRouted::NoIoApic { gsi })?;
            let (Some(active_low), Some(level)) = (polarity_low(p), trigger_level(t)) else {
                return Err(NotRouted::ReservedFlags { gsi });
            };
            let destination = dest.ok_or(NotRouted::DestinationTooWide {
                apic_id: inp.bsp_apic_id,
            })?;
            taken.push((gsi, irq));
            Ok(Route {
                gsi,
                ioapic,
                pin,
                overridden,
                entry: Redirection {
                    vector: vectors::isa(irq),
                    delivery: Delivery::Fixed,
                    logical: false,
                    active_low,
                    level,
                    masked: true,
                    destination,
                },
            })
        };
        for o in &madt.overrides {
            if o.source >= 16 {
                warnings.push(Warning::OverrideNotIsa { source: o.source });
            }
        }
        for o in madt
            .overrides
            .iter()
            .filter(|o| o.source < 16 && o.source != 2)
        {
            isa[o.source as usize] = route(o.source, o.gsi, o.polarity, o.trigger, true);
        }
        for irq in (0..16u8).filter(|&i| i != 2) {
            if !madt.overrides.iter().any(|o| o.source == irq) {
                isa[irq as usize] =
                    route(irq, u32::from(irq), Polarity::High, Trigger::Edge, false);
            }
        }
        for (irq, r) in isa.iter().enumerate() {
            if let Err(why) = *r
                && why.is_warning()
            {
                warnings.push(Warning::Isa {
                    irq: irq as u8,
                    why,
                });
            }
        }

        // The boot CPU's LINT pins.
        let bsp = madt.cpus.iter().find(|c| c.apic_id == inp.bsp_apic_id);
        match bsp {
            None => warnings.push(Warning::BspNotInMadt {
                apic_id: inp.bsp_apic_id,
            }),
            Some(c) if !c.enabled => warnings.push(Warning::BspNotEnabled {
                apic_id: inp.bsp_apic_id,
            }),
            Some(_) => {}
        }
        let uid = bsp.map(|c| c.uid);
        let mut lint = [Lvt::masked(vectors::UNROUTED); 2];
        let mut chosen: [Option<Polarity>; 2] = [None; 2];
        for n in madt
            .lapic_nmis
            .iter()
            .filter(|n| n.uid.is_none() || (uid.is_some() && n.uid == uid))
        {
            let l = usize::from(n.lint & 1);
            if let Some(first) = chosen[l] {
                if polarity_low(first) != polarity_low(n.polarity) {
                    warnings.push(Warning::LintConflict { lint: l as u8 });
                }
                continue;
            }
            chosen[l] = Some(n.polarity);
            match polarity_low(n.polarity) {
                // NMI through a LINT pin is always edge sensitive and its
                // vector is ignored (SDM 11.5.1), so trigger is not used.
                Some(active_low) => {
                    lint[l] = Lvt {
                        vector: 0,
                        delivery: lapic::Delivery::Nmi,
                        active_low,
                        level: false,
                        masked: false,
                    }
                }
                None => warnings.push(Warning::LintReservedFlags { lint: l as u8 }),
            }
        }

        Plan {
            ioapics,
            isa,
            nmi_pins,
            lint,
            destination: dest,
            warnings,
        }
    }

    /// The full redirection table for the I/O APIC at MADT index `index`:
    /// one entry per pin, unrouted pins masked (aimed at the boot CPU if an
    /// entry can name it, else 0). Empty if it is not used.
    pub fn table(&self, index: usize) -> Vec<Redirection> {
        let Some(io) = self.ioapics.get(index) else {
            return Vec::new();
        };
        if io.status != IoApicStatus::Used {
            return Vec::new();
        }
        let mut t = alloc::vec![unrouted(self.destination.unwrap_or(0)); usize::from(io.pins)];
        for r in self.isa.iter().flatten().filter(|r| r.ioapic == index) {
            t[usize::from(r.pin)] = r.entry;
        }
        for n in self.nmi_pins.iter().filter(|n| n.ioapic == index) {
            t[usize::from(n.pin)] = n.entry;
        }
        t
    }

    /// ISA IRQ `irq`'s route, if it has one.
    pub fn isa_route(&self, irq: u8) -> Option<&Route> {
        self.isa.get(usize::from(irq))?.as_ref().ok()
    }
}
