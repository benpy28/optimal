use optimal_acpi::madt::{Cpu, IoApic, LapicNmi, Madt, NmiSource, Override, Polarity, Trigger};
use proptest::prelude::*;

use crate::ioapic::{Delivery, Redirection};
use crate::lapic::{self, Lvt};
use crate::plan::*;
use crate::vectors;

fn cpu(apic_id: u32, uid: u32, enabled: bool) -> Cpu {
    Cpu {
        apic_id,
        uid,
        enabled,
        online_capable: false,
        x2apic: false,
    }
}

fn io(id: u8, address: u32, gsi_base: u32) -> IoApic {
    IoApic {
        id,
        address,
        gsi_base,
    }
}

fn ov(source: u8, gsi: u32, polarity: Polarity, trigger: Trigger) -> Override {
    Override {
        source,
        gsi,
        polarity,
        trigger,
    }
}

fn empty() -> Madt {
    Madt {
        checksum_ok: true,
        lapic_address: 0xFEE0_0000,
        pcat_compat: true,
        cpus: vec![],
        ioapics: vec![],
        overrides: vec![],
        lapic_nmis: vec![],
        nmi_sources: vec![],
        unknown_entries: 0,
        bad_entries: 0,
        truncated: false,
    }
}

/// QEMU's MADT (pc and q35): one CPU (APIC 0, UID 0), one 24-pin I/O APIC
/// at 0xFEC00000 from GSI 0, IRQ 0 moved to GSI 2, IRQs 5, 9, 10 and 11
/// level triggered active high (the PCI link IRQs), LINT1 NMI on all CPUs.
fn qemu() -> Madt {
    Madt {
        cpus: vec![cpu(0, 0, true)],
        ioapics: vec![io(0, 0xFEC0_0000, 0)],
        overrides: vec![
            ov(0, 2, Polarity::Bus, Trigger::Bus),
            ov(5, 5, Polarity::High, Trigger::Level),
            ov(9, 9, Polarity::High, Trigger::Level),
            ov(10, 10, Polarity::High, Trigger::Level),
            ov(11, 11, Polarity::High, Trigger::Level),
        ],
        lapic_nmis: vec![LapicNmi {
            uid: None,
            lint: 1,
            polarity: Polarity::Bus,
            trigger: Trigger::Bus,
        }],
        ..empty()
    }
}

fn masked_isa(irq: u8, active_low: bool, level: bool, dest: u8) -> Redirection {
    Redirection {
        vector: vectors::isa(irq),
        delivery: Delivery::Fixed,
        logical: false,
        active_low,
        level,
        masked: true,
        destination: dest,
    }
}

const NMI_LVT: Lvt = Lvt {
    vector: 0,
    delivery: lapic::Delivery::Nmi,
    active_low: false,
    level: false,
    masked: false,
};

#[test]
fn qemu_hand_worked() {
    let m = qemu();
    let p = Plan::build(Inputs {
        madt: &m,
        bsp_apic_id: 0,
        ioapic_max_entry: &[Some(23)],
    });
    assert_eq!(p.warnings, []);
    assert_eq!(
        p.ioapics,
        [IoApicPlan {
            id: 0,
            address: 0xFEC0_0000,
            gsi_base: 0,
            pins: 24,
            status: IoApicStatus::Used
        }]
    );
    assert_eq!(p.destination, Some(0));
    // IRQ 0: GSI 2, pin 2, vector 0x30, ISA defaults (bus-conforming).
    assert_eq!(
        p.isa[0],
        Ok(Route {
            gsi: 2,
            ioapic: 0,
            pin: 2,
            overridden: true,
            entry: masked_isa(0, false, false, 0)
        })
    );
    assert_eq!(p.isa[2], Err(NotRouted::Cascade));
    for irq in [1u8, 3, 4, 6, 7, 8, 12, 13, 14, 15] {
        assert_eq!(
            p.isa[usize::from(irq)],
            Ok(Route {
                gsi: u32::from(irq),
                ioapic: 0,
                pin: irq,
                overridden: false,
                entry: masked_isa(irq, false, false, 0)
            }),
            "IRQ {irq}"
        );
    }
    for irq in [5u8, 9, 10, 11] {
        let r = p.isa_route(irq).unwrap();
        assert_eq!((r.gsi, r.pin, r.overridden), (u32::from(irq), irq, true));
        assert_eq!(r.entry, masked_isa(irq, false, true, 0));
    }
    assert_eq!(p.lint, [Lvt::masked(vectors::UNROUTED), NMI_LVT]);
    assert_eq!(p.nmi_pins, []);
    let t = p.table(0);
    assert_eq!(t.len(), 24);
    // Pin 0 has no source (IRQ 0 moved away); pin 2 is the timer.
    assert_eq!(t[0], unrouted(0));
    assert_eq!(t[2], masked_isa(0, false, false, 0));
    assert_eq!(t[9], masked_isa(9, false, true, 0));
    assert!(t[16..].iter().all(|&e| e == unrouted(0)));
    assert_eq!(p.table(1), []);
    assert_eq!(p.isa_route(2), None);
    assert_eq!(p.isa_route(16), None);
}

/// Two I/O APICs: 24 pins from GSI 0 and 32 from GSI 24. IRQ 3 moved to
/// GSI 30, active low, level: second I/O APIC, pin 30 - 24 = 6.
#[test]
fn two_ioapics() {
    let m = Madt {
        cpus: vec![cpu(1, 0, true)],
        ioapics: vec![io(0, 0xFEC0_0000, 0), io(1, 0xFEC0_1000, 24)],
        overrides: vec![ov(3, 30, Polarity::Low, Trigger::Level)],
        ..empty()
    };
    let p = Plan::build(Inputs {
        madt: &m,
        bsp_apic_id: 1,
        ioapic_max_entry: &[Some(23), Some(31)],
    });
    assert_eq!(p.warnings, []);
    let r = p.isa_route(3).unwrap();
    assert_eq!((r.ioapic, r.pin, r.gsi), (1, 6, 30));
    assert_eq!(r.entry, masked_isa(3, true, true, 1));
    assert_eq!(p.table(1).len(), 32);
    assert_eq!(p.table(1)[6], r.entry);
    assert_eq!(p.table(0)[3], unrouted(1));
    assert!(p.ioapics[1].serves(55) && !p.ioapics[1].serves(56) && !p.ioapics[1].serves(23));
    // No LAPIC NMI entries: both LINT pins stay masked.
    assert_eq!(p.lint, [Lvt::masked(vectors::UNROUTED); 2]);
}

fn build(m: &Madt, bsp: u32, max: &[Option<u8>]) -> Plan {
    Plan::build(Inputs {
        madt: m,
        bsp_apic_id: bsp,
        ioapic_max_entry: max,
    })
}

#[test]
fn ioapic_refusals() {
    let base = |ioapics| Madt {
        cpus: vec![cpu(0, 0, true)],
        ioapics,
        ..empty()
    };
    // The second covers GSIs 16 to 39 and overlaps the first (0 to 23).
    let m = base(vec![io(0, 0xFEC0_0000, 0), io(1, 0xFEC0_1000, 16)]);
    let p = build(&m, 0, &[Some(23), Some(23)]);
    assert_eq!(p.ioapics[1].status, IoApicStatus::Overlaps(0));
    assert_eq!(p.ioapics[1].pins, 0);
    assert!(p.warnings.contains(&Warning::IoApic {
        index: 1,
        status: IoApicStatus::Overlaps(0)
    }));
    assert_eq!(p.table(1), []);
    // Touching ranges do not overlap: the second starts at 24.
    let m = base(vec![io(0, 0xFEC0_0000, 0), io(1, 0xFEC0_1000, 24)]);
    assert_eq!(
        build(&m, 0, &[Some(23), Some(0)]).ioapics[1].status,
        IoApicStatus::Used
    );
    // The same the other way round: the second (GSIs 0 to 23) ends exactly
    // where the first (from 24) starts. One more pin and they share GSI 24.
    let m = base(vec![io(0, 0xFEC0_0000, 24), io(1, 0xFEC0_1000, 0)]);
    let p = build(&m, 0, &[Some(23), Some(23)]);
    assert_eq!(p.ioapics[1].status, IoApicStatus::Used);
    let p = build(&m, 0, &[Some(23), Some(24)]);
    assert_eq!(p.ioapics[1].status, IoApicStatus::Overlaps(0));
    // An overlap with one that is not used does not count.
    let m = base(vec![io(0, 0xFEC0_0008, 0), io(1, 0xFEC0_1000, 0)]);
    let p = build(&m, 0, &[Some(23), Some(23)]);
    assert_eq!(p.ioapics[0].status, IoApicStatus::BadAddress);
    assert_eq!(p.ioapics[1].status, IoApicStatus::Used);
    assert_eq!(p.isa_route(1).unwrap().ioapic, 1);

    let m = base(vec![io(0, 0xFEC0_0000, 0)]);
    let p = build(&m, 0, &[None]);
    assert_eq!(p.ioapics[0].status, IoApicStatus::Unreachable);
    // Every ISA IRQ but the cascade is refused: 15 warnings plus the I/O APIC.
    let no = p
        .warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                Warning::Isa {
                    why: NotRouted::NoIoApic { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(no, 15);
    assert_eq!(p.warnings.len(), 16);
    // Fewer entries than I/O APICs: the missing one is unreachable.
    assert_eq!(
        build(&m, 0, &[]).ioapics[0].status,
        IoApicStatus::Unreachable
    );

    // 120 pins is the most IOREGSEL can reach; 121 is refused.
    assert_eq!(build(&m, 0, &[Some(119)]).ioapics[0].pins, 120);
    assert_eq!(
        build(&m, 0, &[Some(120)]).ioapics[0].status,
        IoApicStatus::TooManyPins
    );

    // 0xFFFFFFE8 + 24 = 2^32 exactly: fits. 0xFFFFFFF0 + 24 does not.
    let m = base(vec![io(0, 0xFEC0_0000, 0xFFFF_FFE8)]);
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(p.ioapics[0].status, IoApicStatus::Used);
    assert!(p.ioapics[0].serves(u32::MAX));
    let m = base(vec![io(0, 0xFEC0_0000, 0xFFFF_FFF0)]);
    assert_eq!(
        build(&m, 0, &[Some(23)]).ioapics[0].status,
        IoApicStatus::RangeOverflow
    );
}

#[test]
fn override_conflicts() {
    let mut m = qemu();
    // A second override onto GSI 2: IRQ 8 loses to IRQ 0.
    m.overrides.push(ov(8, 2, Polarity::High, Trigger::Edge));
    // A reserved polarity on IRQ 4: refused, and GSI 4 stays free.
    m.overrides
        .push(ov(4, 4, Polarity::Reserved, Trigger::Edge));
    // IRQ 6 moved onto IRQ 7's GSI: IRQ 7 is shadowed, not a warning.
    m.overrides.push(ov(6, 7, Polarity::High, Trigger::Edge));
    // An override on the cascade is ignored, and one above 15 is warned.
    m.overrides.push(ov(2, 20, Polarity::High, Trigger::Edge));
    m.overrides.push(ov(17, 21, Polarity::High, Trigger::Edge));
    // Trigger reserved on IRQ 12.
    m.overrides
        .push(ov(12, 12, Polarity::High, Trigger::Reserved));
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(p.isa[8], Err(NotRouted::GsiTaken { gsi: 2, by: 0 }));
    assert_eq!(p.isa[4], Err(NotRouted::ReservedFlags { gsi: 4 }));
    assert_eq!(p.isa[12], Err(NotRouted::ReservedFlags { gsi: 12 }));
    assert_eq!(p.isa[7], Err(NotRouted::Shadowed { gsi: 7, by: 6 }));
    assert_eq!(p.isa_route(6).unwrap().pin, 7);
    assert_eq!(p.isa[2], Err(NotRouted::Cascade));
    assert_eq!(
        p.warnings,
        [
            Warning::OverrideNotIsa { source: 17 },
            Warning::Isa {
                irq: 4,
                why: NotRouted::ReservedFlags { gsi: 4 }
            },
            Warning::Isa {
                irq: 8,
                why: NotRouted::GsiTaken { gsi: 2, by: 0 }
            },
            Warning::Isa {
                irq: 12,
                why: NotRouted::ReservedFlags { gsi: 12 }
            },
        ]
    );
    let t = p.table(0);
    assert_eq!(t[20], unrouted(0));
    assert_eq!(t[21], unrouted(0));
    assert_eq!(t[4], unrouted(0));
}

#[test]
fn nmi_sources() {
    let mut m = qemu();
    m.nmi_sources = vec![
        NmiSource {
            gsi: 1,
            polarity: Polarity::Low,
            trigger: Trigger::Level,
        },
        NmiSource {
            gsi: 1,
            polarity: Polarity::High,
            trigger: Trigger::Edge,
        },
        NmiSource {
            gsi: 100,
            polarity: Polarity::High,
            trigger: Trigger::Edge,
        },
        NmiSource {
            gsi: 13,
            polarity: Polarity::Reserved,
            trigger: Trigger::Edge,
        },
    ];
    let p = build(&m, 0, &[Some(23)]);
    // GSI 1: NMI, unmasked, active low, and edge even though the MADT
    // says level (NMI delivery is always edge).
    let nmi = Redirection {
        vector: 0,
        delivery: Delivery::Nmi,
        logical: false,
        active_low: true,
        level: false,
        masked: false,
        destination: 0,
    };
    assert_eq!(
        p.nmi_pins,
        [NmiPin {
            gsi: 1,
            ioapic: 0,
            pin: 1,
            entry: nmi
        }]
    );
    assert_eq!(p.isa[1], Err(NotRouted::NmiSource { gsi: 1 }));
    assert_eq!(p.table(0)[1], nmi);
    assert_eq!(p.table(0)[13], masked_isa(13, false, false, 0));
    assert_eq!(
        p.warnings,
        [
            Warning::NmiSourceDuplicate { gsi: 1 },
            Warning::NmiSourceNoIoApic { gsi: 100 },
            Warning::NmiSourceReservedFlags { gsi: 13 },
            Warning::Isa {
                irq: 1,
                why: NotRouted::NmiSource { gsi: 1 }
            },
        ]
    );
}

/// A broken entry (reserved polarity) is skipped with a warning, and a later
/// good entry for the same GSI is still used: it is not a duplicate of an
/// entry that was never accepted. (Found by the random-MADT property.)
#[test]
fn nmi_source_after_a_broken_one() {
    let mut m = qemu();
    m.nmi_sources = vec![
        NmiSource {
            gsi: 13,
            polarity: Polarity::Reserved,
            trigger: Trigger::Bus,
        },
        NmiSource {
            gsi: 13,
            polarity: Polarity::Low,
            trigger: Trigger::Bus,
        },
    ];
    let p = build(&m, 0, &[Some(23)]);
    let nmi = Redirection {
        vector: 0,
        delivery: Delivery::Nmi,
        logical: false,
        active_low: true,
        level: false,
        masked: false,
        destination: 0,
    };
    assert_eq!(
        p.nmi_pins,
        [NmiPin {
            gsi: 13,
            ioapic: 0,
            pin: 13,
            entry: nmi
        }]
    );
    assert_eq!(p.table(0)[13], nmi);
    assert_eq!(p.isa[13], Err(NotRouted::NmiSource { gsi: 13 }));
    assert!(
        p.warnings
            .contains(&Warning::NmiSourceReservedFlags { gsi: 13 })
    );
    assert!(
        !p.warnings
            .contains(&Warning::NmiSourceDuplicate { gsi: 13 })
    );
}

/// An x2APIC boot CPU with ID 300 cannot be named by an I/O APIC entry
/// (8 bits) without interrupt remapping: every route is refused.
#[test]
fn destination_too_wide() {
    let mut m = qemu();
    m.cpus = vec![cpu(300, 0, true)];
    m.nmi_sources = vec![NmiSource {
        gsi: 1,
        polarity: Polarity::High,
        trigger: Trigger::Edge,
    }];
    let p = build(&m, 300, &[Some(23)]);
    assert_eq!(p.destination, None);
    assert_eq!(
        p.isa[0],
        Err(NotRouted::DestinationTooWide { apic_id: 300 })
    );
    let wide = p
        .warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                Warning::Isa {
                    why: NotRouted::DestinationTooWide { .. },
                    ..
                }
            )
        })
        .count();
    // 16 IRQs less the cascade, less IRQ 1, which loses nothing: with the
    // NMI source refused its GSI is free, so it too is refused for width.
    assert_eq!(wide, 15);
    assert!(
        p.warnings
            .contains(&Warning::NmiSourceDestinationTooWide { gsi: 1 })
    );
    assert_eq!(p.table(0), vec![unrouted(0); 24]);
    // 255 still fits.
    m.cpus = vec![cpu(255, 0, true)];
    assert_eq!(build(&m, 255, &[Some(23)]).destination, Some(255));
}

#[test]
fn lint_pins() {
    let entry = |uid, lint, polarity| LapicNmi {
        uid,
        lint,
        polarity,
        trigger: Trigger::Edge,
    };
    let mut m = qemu();
    m.cpus = vec![cpu(0, 7, true), cpu(1, 8, true)];
    m.lapic_nmis = vec![
        entry(Some(8), 0, Polarity::High), // another CPU: ignored
        entry(Some(7), 0, Polarity::Low),  // ours, LINT0
        entry(None, 1, Polarity::High),    // everyone, LINT1
        entry(Some(7), 1, Polarity::High), // same again: no warning
    ];
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(
        p.lint,
        [
            Lvt {
                active_low: true,
                ..NMI_LVT
            },
            NMI_LVT
        ]
    );
    assert_eq!(p.warnings, []);
    // A disagreeing second entry is warned; the first stays.
    m.lapic_nmis.push(entry(None, 1, Polarity::Low));
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(p.lint[1], NMI_LVT);
    assert_eq!(p.warnings, [Warning::LintConflict { lint: 1 }]);
    // A reserved polarity: masked, not guessed.
    m.lapic_nmis = vec![entry(None, 1, Polarity::Reserved)];
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(p.lint[1], Lvt::masked(vectors::UNROUTED));
    assert_eq!(p.warnings, [Warning::LintReservedFlags { lint: 1 }]);
    // Boot CPU not in the MADT: only all-processor entries apply.
    m.lapic_nmis = vec![
        entry(Some(7), 0, Polarity::High),
        entry(None, 1, Polarity::High),
    ];
    let p = build(&m, 5, &[Some(23)]);
    assert_eq!(p.lint, [Lvt::masked(vectors::UNROUTED), NMI_LVT]);
    assert_eq!(p.warnings, [Warning::BspNotInMadt { apic_id: 5 }]);
    // Listed but disabled.
    m.cpus = vec![cpu(0, 7, false)];
    let p = build(&m, 0, &[Some(23)]);
    assert_eq!(p.warnings, [Warning::BspNotEnabled { apic_id: 0 }]);
    assert_eq!(p.lint[0], NMI_LVT);
}

#[test]
fn not_routed_warning_classes() {
    assert!(!NotRouted::Cascade.is_warning());
    assert!(!NotRouted::Shadowed { gsi: 0, by: 0 }.is_warning());
    for w in [
        NotRouted::NoIoApic { gsi: 0 },
        NotRouted::ReservedFlags { gsi: 0 },
        NotRouted::GsiTaken { gsi: 0, by: 0 },
        NotRouted::NmiSource { gsi: 0 },
        NotRouted::DestinationTooWide { apic_id: 256 },
    ] {
        assert!(w.is_warning());
    }
}

// Random MADTs, checked against the rules in the module documentation.

fn polarity() -> impl Strategy<Value = Polarity> {
    prop_oneof![
        Just(Polarity::Bus),
        Just(Polarity::High),
        Just(Polarity::Low),
        Just(Polarity::Reserved)
    ]
}

fn trigger() -> impl Strategy<Value = Trigger> {
    prop_oneof![
        Just(Trigger::Bus),
        Just(Trigger::Edge),
        Just(Trigger::Level),
        Just(Trigger::Reserved)
    ]
}

fn address() -> impl Strategy<Value = u32> {
    prop_oneof![
        8 => (0u32..16).prop_map(|i| 0xFEC0_0000 + i * 0x1000),
        1 => any::<u32>(),
    ]
}

fn gsi() -> impl Strategy<Value = u32> {
    prop_oneof![8 => 0u32..80, 1 => any::<u32>()]
}

prop_compose! {
    fn random_madt()(
        ioapics in proptest::collection::vec((any::<u8>(), address(), gsi()), 0..4),
        overrides in proptest::collection::vec((0u8..20, gsi(), polarity(), trigger()), 0..12),
        nmis in proptest::collection::vec((gsi(), polarity(), trigger()), 0..3),
        lnmis in proptest::collection::vec((proptest::option::of(0u32..4), 0u8..2, polarity()), 0..4),
        cpus in proptest::collection::vec((0u32..300, 0u32..4, any::<bool>()), 0..4),
    ) -> Madt {
        let mut m = empty();
        m.ioapics = ioapics.into_iter().map(|(i, a, g)| io(i, a, g)).collect();
        // The MADT parser keeps one override per source; so do we.
        for (s, g, p, t) in overrides {
            if !m.overrides.iter().any(|o| o.source == s) {
                m.overrides.push(ov(s, g, p, t));
            }
        }
        m.nmi_sources = nmis.into_iter().map(|(gsi, polarity, trigger)| NmiSource { gsi, polarity, trigger }).collect();
        m.lapic_nmis = lnmis.into_iter().map(|(uid, lint, polarity)| LapicNmi { uid, lint, polarity, trigger: Trigger::Edge }).collect();
        for (id, uid, en) in cpus {
            if !m.cpus.iter().any(|c| c.apic_id == id) {
                m.cpus.push(cpu(id, uid, en));
            }
        }
        m
    }
}

fn isa_flags(p: Polarity, t: Trigger) -> Option<(bool, bool)> {
    let low = match p {
        Polarity::Bus | Polarity::High => false,
        Polarity::Low => true,
        Polarity::Reserved => return None,
    };
    let level = match t {
        Trigger::Bus | Trigger::Edge => false,
        Trigger::Level => true,
        Trigger::Reserved => return None,
    };
    Some((low, level))
}

proptest! {
    #![proptest_config(super::config(4000))]

    #[test]
    fn plan_obeys_the_rules(
        m in random_madt(),
        bsp in prop_oneof![4 => 0u32..4, 1 => 250u32..300],
        maxes in proptest::collection::vec(proptest::option::of(prop_oneof![8 => 0u8..40, 1 => any::<u8>()]), 0..4),
    ) {
        let p = build(&m, bsp, &maxes);
        let dest = u8::try_from(bsp).ok();
        prop_assert_eq!(p.destination, dest);

        // I/O APICs: used ones have disjoint ranges; every refusal is warned.
        prop_assert_eq!(p.ioapics.len(), m.ioapics.len());
        for (i, a) in p.ioapics.iter().enumerate() {
            if a.status == IoApicStatus::Used {
                prop_assert_eq!(Some(u16::from(maxes[i].unwrap()) + 1), Some(a.pins));
                prop_assert!(a.pins <= 120);
                prop_assert!(u64::from(a.gsi_base) + u64::from(a.pins) <= 1 << 32);
                prop_assert!(crate::ioapic::address_ok(u64::from(a.address)));
                for b in &p.ioapics[..i] {
                    if b.status == IoApicStatus::Used {
                        let (a0, a1) = (u64::from(a.gsi_base), u64::from(a.gsi_base) + u64::from(a.pins));
                        let (b0, b1) = (u64::from(b.gsi_base), u64::from(b.gsi_base) + u64::from(b.pins));
                        prop_assert!(a1 <= b0 || b1 <= a0);
                    }
                }
                prop_assert_eq!(p.table(i).len(), usize::from(a.pins));
            } else {
                prop_assert_eq!(a.pins, 0);
                let w = Warning::IoApic { index: i, status: a.status };
                prop_assert!(p.warnings.contains(&w));
                prop_assert_eq!(p.table(i).len(), 0);
            }
        }
        let served = |g: u32| p.ioapics.iter().position(|a| a.serves(g));

        // NMI pins: served, unique, edge, unmasked NMI to the boot CPU.
        for (k, n) in p.nmi_pins.iter().enumerate() {
            let i = served(n.gsi);
            prop_assert_eq!(i, Some(n.ioapic));
            prop_assert_eq!(u32::from(n.pin), n.gsi - p.ioapics[n.ioapic].gsi_base);
            prop_assert_eq!(n.entry.delivery, Delivery::Nmi);
            prop_assert!(!n.entry.masked && !n.entry.level);
            prop_assert_eq!(Some(n.entry.destination), dest);
            prop_assert!(p.nmi_pins[..k].iter().all(|o| o.gsi != n.gsi));
            // The first entry for this GSI with usable flags: a broken
            // entry is skipped (with a warning) rather than hiding a good one.
            let src = m
                .nmi_sources
                .iter()
                .find(|s| s.gsi == n.gsi && s.polarity != Polarity::Reserved)
                .unwrap();
            prop_assert_eq!(n.entry.active_low, src.polarity == Polarity::Low);
        }

        // ISA routes.
        let mut gsis = Vec::new();
        for irq in 0..16u8 {
            let o = m.overrides.iter().find(|o| o.source == irq);
            let (want_gsi, flags) = match o {
                Some(o) => (o.gsi, isa_flags(o.polarity, o.trigger)),
                None => (u32::from(irq), Some((false, false))),
            };
            match p.isa[usize::from(irq)] {
                Ok(r) => {
                    prop_assert_ne!(irq, 2);
                    prop_assert_eq!(r.gsi, want_gsi);
                    prop_assert_eq!(r.overridden, o.is_some());
                    prop_assert_eq!(served(r.gsi), Some(r.ioapic));
                    prop_assert_eq!(u32::from(r.pin), r.gsi - p.ioapics[r.ioapic].gsi_base);
                    let (low, level) = flags.unwrap();
                    prop_assert_eq!(r.entry, masked_isa(irq, low, level, dest.unwrap()));
                    prop_assert!(!gsis.contains(&r.gsi));
                    prop_assert!(p.nmi_pins.iter().all(|n| n.gsi != r.gsi));
                    prop_assert_eq!(p.table(r.ioapic)[usize::from(r.pin)], r.entry);
                    gsis.push(r.gsi);
                }
                Err(why) => {
                    match why {
                        NotRouted::Cascade => prop_assert_eq!(irq, 2),
                        NotRouted::NoIoApic { gsi } => {
                            prop_assert_eq!(gsi, want_gsi);
                            prop_assert_eq!(served(gsi), None);
                        }
                        NotRouted::ReservedFlags { gsi } => {
                            prop_assert_eq!(gsi, want_gsi);
                            prop_assert_eq!(flags, None);
                        }
                        NotRouted::GsiTaken { gsi, by } | NotRouted::Shadowed { gsi, by } => {
                            prop_assert_eq!(gsi, want_gsi);
                            prop_assert_eq!(p.isa_route(by).map(|r| r.gsi), Some(gsi));
                            prop_assert_eq!(matches!(why, NotRouted::GsiTaken { .. }), o.is_some());
                        }
                        NotRouted::NmiSource { gsi } => {
                            prop_assert_eq!(gsi, want_gsi);
                            prop_assert!(p.nmi_pins.iter().any(|n| n.gsi == gsi));
                        }
                        NotRouted::DestinationTooWide { apic_id } => {
                            prop_assert_eq!(apic_id, bsp);
                            prop_assert!(bsp > 255);
                        }
                    }
                    prop_assert_eq!(
                        p.warnings.contains(&Warning::Isa { irq, why }),
                        why.is_warning()
                    );
                }
            }
        }
        // Completeness: an IRQ whose GSI is served, with usable flags, a
        // nameable boot CPU, no NMI source on it, and nobody else on its
        // GSI, is routed. Overrides are placed before identity routes.
        for irq in (0..16u8).filter(|&i| i != 2) {
            let o = m.overrides.iter().find(|o| o.source == irq);
            let (g, flags) = match o {
                Some(o) => (o.gsi, isa_flags(o.polarity, o.trigger)),
                None => (u32::from(irq), Some((false, false))),
            };
            let other = (0..16u8).any(|j| j != irq && p.isa_route(j).is_some_and(|r| r.gsi == g));
            if served(g).is_some() && flags.is_some() && dest.is_some()
                && p.nmi_pins.iter().all(|n| n.gsi != g) && !other {
                prop_assert!(p.isa_route(irq).is_some(), "IRQ {} not routed", irq);
            }
        }
        // Every pin in every table is a route, an NMI pin, or unrouted.
        for (i, a) in p.ioapics.iter().enumerate() {
            for (pin, e) in p.table(i).iter().enumerate() {
                let g = a.gsi_base + pin as u32;
                let r = p.isa.iter().flatten().find(|r| r.gsi == g).map(|r| r.entry);
                let n = p.nmi_pins.iter().find(|n| n.gsi == g).map(|n| n.entry);
                prop_assert_eq!(*e, r.or(n).unwrap_or(unrouted(dest.unwrap_or(0))));
            }
        }
        // Overrides above 15 are warned.
        for o in m.overrides.iter().filter(|o| o.source >= 16) {
            let w = Warning::OverrideNotIsa { source: o.source };
            prop_assert!(p.warnings.contains(&w));
        }
        // LINT pins: NMI only where an applicable entry says so.
        let uid = m.cpus.iter().find(|c| c.apic_id == bsp).map(|c| c.uid);
        for l in 0..2u8 {
            let first = m.lapic_nmis.iter().find(|n| n.lint == l && (n.uid.is_none() || (uid.is_some() && n.uid == uid)));
            let want = match first.map(|n| n.polarity) {
                None | Some(Polarity::Reserved) => Lvt::masked(vectors::UNROUTED),
                Some(pol) => Lvt { active_low: pol == Polarity::Low, ..NMI_LVT },
            };
            prop_assert_eq!(p.lint[usize::from(l)], want);
        }
    }
}
