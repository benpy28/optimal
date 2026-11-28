use proptest::prelude::*;

use crate::eoi::*;
use crate::ioapic::{self, Delivery, Redirection};
use crate::lapic;

fn lapic_version(suppress: bool) -> lapic::Version {
    lapic::Version::decode(0x0005_0014 | if suppress { 1 << 24 } else { 0 })
}

fn io(index: usize, version: u8, address: u64) -> IoApic {
    IoApic {
        index,
        version: ioapic::Version {
            version,
            max_entry: 23,
        },
        address,
    }
}

/// QEMU's local APIC reads 0x00050014 (version 0x14, 6 LVTs, no bit 24);
/// KVM's in x2APIC mode with a userspace I/O APIC adds bit 24:
/// 0x01050014. QEMU's I/O APIC is version 0x20, KVM's in-kernel one 0x11.
#[test]
fn choose_hand_worked() {
    let qemu = io(0, 0x20, 0xFEC0_0000);
    let kvm = io(0, 0x11, 0xFEC0_0000);
    assert_eq!(
        choose(lapic_version(false), &[qemu]),
        Mode::Broadcast(Why::LapicCannotSuppress)
    );
    assert_eq!(choose(lapic_version(true), &[qemu]), Mode::Directed);
    assert_eq!(
        choose(lapic_version(true), &[kvm]),
        Mode::Broadcast(Why::NoEoiRegister {
            index: 0,
            version: 0x11
        })
    );
    assert_eq!(
        choose(lapic_version(true), &[]),
        Mode::Broadcast(Why::NoIoApic)
    );
    // The local APIC is checked first.
    assert_eq!(
        choose(lapic_version(false), &[]),
        Mode::Broadcast(Why::LapicCannotSuppress)
    );
    // One old I/O APIC among new ones rules directed EOI out; the first
    // one found is named.
    assert_eq!(
        choose(
            lapic_version(true),
            &[qemu, io(1, 0x11, 0xFEC0_1000), io(2, 0x11, 0xFEC0_2000)]
        ),
        Mode::Broadcast(Why::NoEoiRegister {
            index: 1,
            version: 0x11
        })
    );
    // 0xFC0 + 0x44 = 0x1004 crosses the page; 0xFB0 + 0x44 = 0xFF4 fits.
    assert_eq!(
        choose(lapic_version(true), &[io(3, 0x20, 0xFEC0_0FC0)]),
        Mode::Broadcast(Why::EoiNotInPage { index: 3 })
    );
    assert_eq!(
        choose(lapic_version(true), &[io(3, 0x21, 0xFEC0_0FB0)]),
        Mode::Directed
    );
    assert!(io(0, 0x20, 0xFEC0_0FB0).has_eoi());
    assert!(!io(0, 0x20, 0xFEC0_0FC0).has_eoi());
    assert!(!io(0, 0x1F, 0xFEC0_0000).has_eoi());
    assert!(!io(0, 0x20, 0).has_eoi());
}

#[test]
fn eoi_register_constants() {
    assert_eq!((ioapic::EOI, ioapic::EOI_VERSION), (0x40, 0x20));
    assert_eq!(ioapic::REMOTE_IRR, 0x4000);
    assert_eq!(lapic::SVR_SUPPRESS_EOI_BROADCAST, 0x1000);
    assert!(ioapic::eoi_in_page(0xFEC0_0000));
    assert!(ioapic::eoi_in_page(0xFEC0_0FB0));
    assert!(!ioapic::eoi_in_page(0xFEC0_0FC0));
    assert!(!ioapic::eoi_in_page(0xFEC0_0008));
    assert!(!ioapic::eoi_in_page(0));
    assert!(ioapic::Version::decode(0x0017_0020).has_eoi());
    assert!(!ioapic::Version::decode(0x0017_0011).has_eoi());
}

proptest! {
    #![proptest_config(super::config(2000))]

    /// Directed exactly when the local APIC can suppress, there is an I/O
    /// APIC, and every one has an EOI register in its page; otherwise the
    /// reason names the first thing that rules it out.
    #[test]
    fn choose_matches_the_rule(
        suppress in any::<bool>(),
        ios in proptest::collection::vec((0x10u8..0x30, 0u64..256), 0..5),
    ) {
        let used: Vec<IoApic> = ios
            .iter()
            .enumerate()
            .map(|(i, &(v, slot))| io(i, v, 0xFEC0_0000 + slot * 16))
            .collect();
        let got = choose(lapic_version(suppress), &used);
        let all = used.iter().all(|i| i.version.version >= 0x20 && i.address % 4096 <= 0xFBC);
        if suppress && !used.is_empty() && all {
            prop_assert_eq!(got, Mode::Directed);
        } else {
            let first_bad = used
                .iter()
                .find(|i| i.version.version < 0x20 || i.address % 4096 > 0xFBC);
            let want = if !suppress {
                Why::LapicCannotSuppress
            } else if used.is_empty() {
                Why::NoIoApic
            } else {
                let b = first_bad.unwrap();
                if b.version.version < 0x20 {
                    Why::NoEoiRegister { index: b.index, version: b.version.version }
                } else {
                    Why::EoiNotInPage { index: b.index }
                }
            };
            prop_assert_eq!(got, Mode::Broadcast(want));
        }
    }
}

// ---------------------------------------------------------------------------
// settle, against a model pin.

/// A model of one I/O APIC pin. Remote IRR is read-only to software; an
/// EOI register write on the entry's vector clears it if the entry is
/// level-triggered (Intel ICH/PCH I/O APICs; QEMU's ioapic_eoi_broadcast),
/// and an edge-triggered write clears it (QEMU's
/// ioapic_fix_edge_remote_irr, KVM's ioapic_write_indirect). Either can
/// be switched off to model a pin that will not clear, and a clear can
/// take a few reads to show.
#[derive(Debug)]
struct Model {
    low: u32,
    remote_irr: bool,
    has_eoi: bool,
    eoi_works: bool,
    toggle_works: bool,
    /// Reads before a pending clear shows.
    latency: u32,
    pending: Option<u32>,
    writes: Vec<u32>,
    eois: Vec<u8>,
}

const RO: u32 = 1 << 12 | ioapic::REMOTE_IRR;

impl Model {
    fn new(low: u32, remote_irr: bool) -> Model {
        Model {
            low: low & !RO,
            remote_irr,
            has_eoi: true,
            eoi_works: true,
            toggle_works: true,
            latency: 0,
            pending: None,
            writes: Vec::new(),
            eois: Vec::new(),
        }
    }

    fn clear_later(&mut self) {
        if self.remote_irr && self.pending.is_none() {
            self.pending = Some(self.latency);
        }
    }
}

impl Pin for Model {
    fn read_low(&mut self) -> u32 {
        if let Some(n) = self.pending {
            if n == 0 {
                self.remote_irr = false;
                self.pending = None;
            } else {
                self.pending = Some(n - 1);
            }
        }
        self.low
            | if self.remote_irr {
                ioapic::REMOTE_IRR
            } else {
                0
            }
    }

    fn write_low(&mut self, value: u32) {
        self.writes.push(value);
        self.low = value & !RO;
        if self.toggle_works && value & (1 << 15) == 0 {
            self.clear_later();
        }
    }

    fn eoi(&mut self, vector: u8) {
        assert!(self.has_eoi, "EOI register used on an I/O APIC without one");
        self.eois.push(vector);
        if self.eoi_works && self.low & (1 << 15) != 0 && self.low as u8 == vector {
            self.clear_later();
        }
    }
}

fn level(vector: u8, masked: bool) -> Redirection {
    Redirection {
        vector,
        delivery: Delivery::Fixed,
        logical: false,
        active_low: false,
        level: true,
        masked,
        destination: 0,
    }
}

/// Remote IRR clear: one write, the wanted entry (level, vector 0x41,
/// unmasked: 0x8041), and no EOI.
#[test]
fn settle_clear_pin() {
    let mut p = Model::new(0x1_8041, false);
    assert_eq!(settle(&mut p, level(0x41, false), true, 4), Outcome::Clear);
    assert_eq!(p.writes, [0x8041]);
    assert!(p.eois.is_empty());
}

/// Stuck on 0x41 with an EOI register: hold it masked level (0x18041),
/// EOI 0x41, then the wanted entry (0x8041).
#[test]
fn settle_by_eoi_register() {
    let mut p = Model::new(0x8041, true);
    assert_eq!(
        settle(&mut p, level(0x41, false), true, 0),
        Outcome::Cleared(Method::EoiRegister)
    );
    assert_eq!(p.writes, [0x1_8041, 0x8041]);
    assert_eq!(p.eois, [0x41]);
    assert!(!p.remote_irr);
}

/// No EOI register (82093AA, KVM's I/O APIC): hold (0x18041), masked edge
/// (0x10041), masked level again (0x18041), then wanted (0x8041).
#[test]
fn settle_by_toggle() {
    let mut p = Model::new(0x8041, true);
    p.has_eoi = false;
    assert_eq!(
        settle(&mut p, level(0x41, false), false, 0),
        Outcome::Cleared(Method::Toggle)
    );
    assert_eq!(p.writes, [0x1_8041, 0x1_0041, 0x1_8041, 0x8041]);
    assert!(p.eois.is_empty());
}

/// The vector changed while the interrupt was in service: the entry is
/// now on 0x55 (with Remote IRR set from the 0x41 delivery). The EOI
/// register must be given 0x55, the vector the entry has now, and the
/// wanted entry (back on 0x41) is written last.
#[test]
fn settle_uses_the_vector_the_entry_has_now() {
    let mut p = Model::new(0x1_8055, true);
    assert_eq!(
        settle(&mut p, level(0x41, false), true, 0),
        Outcome::Cleared(Method::EoiRegister)
    );
    assert_eq!(p.eois, [0x55]);
    assert_eq!(p.writes, [0x1_8055, 0x8041]);
}

/// The EOI register does nothing: the toggle is tried next.
#[test]
fn settle_falls_back_to_toggle() {
    let mut p = Model::new(0x8041, true);
    p.eoi_works = false;
    assert_eq!(
        settle(&mut p, level(0x41, false), true, 2),
        Outcome::Cleared(Method::Toggle)
    );
    assert_eq!(p.eois, [0x41]);
    assert_eq!(p.writes, [0x1_8041, 0x1_0041, 0x1_8041, 0x8041]);
}

/// Nothing clears it: the wanted entry is left masked (0x18041).
#[test]
fn settle_stuck_pin_is_left_masked() {
    let mut p = Model::new(0x8041, true);
    p.eoi_works = false;
    p.toggle_works = false;
    assert_eq!(settle(&mut p, level(0x41, false), true, 3), Outcome::Stuck);
    assert_eq!(p.writes, [0x1_8041, 0x1_0041, 0x1_8041, 0x1_8041]);
    assert!(p.remote_irr);
}

/// An edge entry firmware left with Remote IRR set (the bit is undefined
/// for edge entries, but a pin reprogrammed level later would inherit
/// it): it is held level so the EOI register applies, then made the
/// wanted edge entry (vector 0x30, masked: 0x10030).
#[test]
fn settle_edge_entry_with_remote_irr() {
    let mut p = Model::new(0x0030, true);
    let edge = Redirection {
        level: false,
        ..level(0x30, true)
    };
    assert_eq!(
        settle(&mut p, edge, true, 0),
        Outcome::Cleared(Method::EoiRegister)
    );
    assert_eq!(p.writes, [0x1_8030, 0x1_0030]);
}

/// A clear that takes 3 reads to show is seen with 3 rereads (4 reads),
/// and missed with 2, which then tries the toggle (whose own clear the
/// late EOI one beats).
#[test]
fn settle_rereads() {
    let mut p = Model::new(0x8041, true);
    p.latency = 3;
    assert_eq!(
        settle(&mut p, level(0x41, false), true, 3),
        Outcome::Cleared(Method::EoiRegister)
    );
    let mut p = Model::new(0x8041, true);
    p.latency = 3;
    p.toggle_works = false;
    assert_eq!(
        settle(&mut p, level(0x41, false), true, 2),
        Outcome::Cleared(Method::Toggle)
    );
    let mut p = Model::new(0x8041, true);
    p.latency = 4;
    p.eoi_works = false;
    assert_eq!(settle(&mut p, level(0x41, false), true, 3), Outcome::Stuck);
}

proptest! {
    #![proptest_config(super::config(5000))]

    /// Over random entries, starting states and pins that clear or not:
    /// every write but the last is masked; the last is the wanted entry
    /// (masked only if stuck); the EOI register is used only when there
    /// is one, with the vector the entry had; Remote IRR is clear at the
    /// end unless stuck; and the outcome is the one the pin's behaviour
    /// calls for.
    #[test]
    fn settle_against_the_model(
        start in any::<u32>(),
        remote_irr in any::<bool>(),
        wanted_low in any::<u32>(),
        has_eoi in any::<bool>(),
        eoi_works in any::<bool>(),
        toggle_works in any::<bool>(),
        latency in 0u32..6,
        rereads in 0u32..6,
    ) {
        // Delivery modes other than fixed are fine for the pin logic;
        // only the encodable ones are used.
        let wanted = Redirection::decode(wanted_low & !(RO | 0xFFFE_0000), 0).entry;
        let mut p = Model::new(start & !0xFFFE_0000, remote_irr);
        p.has_eoi = has_eoi;
        p.eoi_works = eoi_works;
        p.toggle_works = toggle_works;
        p.latency = latency;
        let vector = start as u8;
        let out = settle(&mut p, wanted, has_eoi, rereads);

        let (last, rest) = p.writes.split_last().unwrap();
        for w in rest {
            prop_assert!(w & ioapic::MASKED != 0, "unmasked mid-sequence: {:#x}", w);
        }
        match out {
            Outcome::Stuck => {
                prop_assert_eq!(*last, wanted.encode().0 | ioapic::MASKED);
                prop_assert!(p.remote_irr);
            }
            _ => {
                prop_assert_eq!(*last, wanted.encode().0);
            }
        }
        if !has_eoi || !remote_irr {
            prop_assert!(p.eois.is_empty());
        } else {
            prop_assert_eq!(&p.eois, &[vector]);
        }
        // The EOI register's clear shows on the (latency + 1)th read after
        // it; the toggle's own reads come after the EOI's 1 + rereads, and
        // the toggle does not restart a clear the EOI already started.
        let want = if !remote_irr {
            Outcome::Clear
        } else if has_eoi && eoi_works {
            if latency <= rereads {
                Outcome::Cleared(Method::EoiRegister)
            } else if latency <= 2 * rereads + 1 {
                Outcome::Cleared(Method::Toggle)
            } else {
                Outcome::Stuck
            }
        } else if toggle_works && latency <= rereads {
            Outcome::Cleared(Method::Toggle)
        } else {
            Outcome::Stuck
        };
        prop_assert_eq!(out, want);
        if out != Outcome::Stuck {
            prop_assert!(!p.remote_irr);
        }
    }
}
