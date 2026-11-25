use proptest::prelude::*;

use crate::Error;
use crate::build_table;
use crate::madt::*;

fn madt(lapic: u32, flags: u32, entries: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&lapic.to_le_bytes());
    body.extend_from_slice(&flags.to_le_bytes());
    for e in entries {
        body.extend_from_slice(e);
    }
    build_table(b"APIC", 4, &body)
}

fn lapic(uid: u8, id: u8, flags: u32) -> Vec<u8> {
    let mut e = vec![0, 8, uid, id];
    e.extend_from_slice(&flags.to_le_bytes());
    e
}

fn x2apic(id: u32, flags: u32, uid: u32) -> Vec<u8> {
    let mut e = vec![9, 16, 0, 0];
    e.extend_from_slice(&id.to_le_bytes());
    e.extend_from_slice(&flags.to_le_bytes());
    e.extend_from_slice(&uid.to_le_bytes());
    e
}

fn ioapic(id: u8, addr: u32, gsi: u32) -> Vec<u8> {
    let mut e = vec![1, 12, id, 0];
    e.extend_from_slice(&addr.to_le_bytes());
    e.extend_from_slice(&gsi.to_le_bytes());
    e
}

fn iso(bus: u8, src: u8, gsi: u32, flags: u16) -> Vec<u8> {
    let mut e = vec![2, 10, bus, src];
    e.extend_from_slice(&gsi.to_le_bytes());
    e.extend_from_slice(&flags.to_le_bytes());
    e
}

fn nmi_source(flags: u16, gsi: u32) -> Vec<u8> {
    let mut e = vec![3, 8];
    e.extend_from_slice(&flags.to_le_bytes());
    e.extend_from_slice(&gsi.to_le_bytes());
    e
}

fn lapic_nmi(uid: u8, flags: u16, lint: u8) -> Vec<u8> {
    let mut e = vec![4, 6, uid];
    e.extend_from_slice(&flags.to_le_bytes());
    e.push(lint);
    e
}

fn x2apic_nmi(flags: u16, uid: u32, lint: u8) -> Vec<u8> {
    let mut e = vec![10, 12];
    e.extend_from_slice(&flags.to_le_bytes());
    e.extend_from_slice(&uid.to_le_bytes());
    e.extend_from_slice(&[lint, 0, 0, 0]);
    e
}

fn lapic_override(addr: u64) -> Vec<u8> {
    let mut e = vec![5, 12, 0, 0];
    e.extend_from_slice(&addr.to_le_bytes());
    e
}

/// What QEMU's q35 machine with two CPUs gives (checked against a dump).
fn qemu() -> Vec<u8> {
    madt(
        0xfee0_0000,
        1,
        &[
            lapic(0, 0, 1),
            lapic(1, 1, 1),
            ioapic(0, 0xfec0_0000, 0),
            iso(0, 0, 2, 0),
            iso(0, 5, 5, 0xd),
            iso(0, 9, 9, 0xd),
            iso(0, 10, 10, 0xd),
            iso(0, 11, 11, 0xd),
            lapic_nmi(0xff, 0, 1),
        ],
    )
}

#[test]
fn qemu_madt_hand_worked() {
    let m = Madt::parse(&qemu()).unwrap();
    assert!(m.checksum_ok);
    assert_eq!(m.lapic_address, 0xfee0_0000);
    assert!(m.pcat_compat);
    assert_eq!(
        m.cpus,
        vec![
            Cpu {
                apic_id: 0,
                uid: 0,
                enabled: true,
                online_capable: false,
                x2apic: false
            },
            Cpu {
                apic_id: 1,
                uid: 1,
                enabled: true,
                online_capable: false,
                x2apic: false
            },
        ]
    );
    assert_eq!(
        m.ioapics,
        vec![IoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0
        }]
    );
    assert_eq!(m.overrides.len(), 5);
    // 0xd = 0b11_01: polarity 01 (high), trigger 11 (level).
    assert_eq!(
        m.overrides[1],
        Override {
            source: 5,
            gsi: 5,
            polarity: Polarity::High,
            trigger: Trigger::Level
        }
    );
    assert_eq!(
        m.lapic_nmis,
        vec![LapicNmi {
            uid: None,
            lint: 1,
            polarity: Polarity::Bus,
            trigger: Trigger::Bus
        }]
    );
    assert_eq!(
        (m.unknown_entries, m.bad_entries, m.truncated),
        (0, 0, false)
    );
    // ISA IRQ routing: the timer moves to GSI 2, bus defaults become
    // active high and edge, and IRQs with no override map to themselves.
    assert_eq!(m.isa_irq(0), (2, Polarity::High, Trigger::Edge));
    assert_eq!(m.isa_irq(9), (9, Polarity::High, Trigger::Level));
    assert_eq!(m.isa_irq(4), (4, Polarity::High, Trigger::Edge));
}

#[test]
fn isa_irq_keeps_explicit_modes() {
    // 0xf = low, level; 0x7 = low, edge (0b01_11).
    let m = Madt::parse(&madt(0, 0, &[iso(0, 9, 20, 0xf), iso(0, 3, 3, 0x7)])).unwrap();
    assert_eq!(m.isa_irq(9), (20, Polarity::Low, Trigger::Level));
    assert_eq!(m.isa_irq(3), (3, Polarity::Low, Trigger::Edge));
    assert!(!m.pcat_compat);
}

#[test]
fn inti_flags_every_combination() {
    // Bits 0-1 polarity, bits 2-3 trigger (Table 5.26).
    let pol = [
        Polarity::Bus,
        Polarity::High,
        Polarity::Reserved,
        Polarity::Low,
    ];
    let trig = [
        Trigger::Bus,
        Trigger::Edge,
        Trigger::Reserved,
        Trigger::Level,
    ];
    for flags in 0u16..16 {
        let m = Madt::parse(&madt(0, 0, &[iso(0, 1, 1, flags | 0xfff0)])).unwrap();
        let o = m.overrides[0];
        assert_eq!(o.polarity, pol[(flags & 3) as usize], "flags {flags:#x}");
        assert_eq!(o.trigger, trig[(flags >> 2) as usize], "flags {flags:#x}");
    }
}

#[test]
fn cpu_flags_and_x2apic() {
    let m = Madt::parse(&madt(
        0xfee0_0000,
        0,
        &[
            lapic(0, 0, 1),           // enabled
            lapic(1, 1, 2),           // online capable, not enabled
            lapic(2, 2, 3),           // enabled; online-capable bit ignored
            lapic(3, 3, 0),           // neither
            lapic(4, 0xff, 1),        // broadcast ID: skipped
            lapic(5, 1, 1),           // duplicate APIC ID: skipped
            x2apic(0x1_0000, 1, 300), // a CPU only x2APIC can address
            x2apic(2, 1, 9),          // also listed as xAPIC: skipped
            x2apic(0xff, 1, 7),       // 0xff is a normal x2APIC ID
        ],
    ))
    .unwrap();
    let got: Vec<_> = m
        .cpus
        .iter()
        .map(|c| (c.apic_id, c.uid, c.enabled, c.online_capable, c.x2apic))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, 0, true, false, false),
            (1, 1, false, true, false),
            (2, 2, true, false, false),
            (3, 3, false, false, false),
            (0x1_0000, 300, true, false, true),
            (0xff, 7, true, false, true),
        ]
    );
    assert_eq!(m.bad_entries, 3);
}

#[test]
fn ioapics_and_their_duplicates() {
    let m = Madt::parse(&madt(
        0,
        0,
        &[
            ioapic(0, 0xfec0_0000, 0),
            ioapic(1, 0xfec0_1000, 24),
            ioapic(1, 0xfec0_2000, 48), // same ID
            ioapic(2, 0xfec0_3000, 24), // same GSI base
        ],
    ))
    .unwrap();
    assert_eq!(m.ioapics.len(), 2);
    assert_eq!(
        m.ioapics[1],
        IoApic {
            id: 1,
            address: 0xfec0_1000,
            gsi_base: 24
        }
    );
    assert_eq!(m.bad_entries, 2);
}

#[test]
fn overrides_on_other_buses_or_twice_are_skipped() {
    let m = Madt::parse(&madt(
        0,
        0,
        &[iso(0, 0, 2, 0), iso(1, 4, 4, 0), iso(0, 0, 7, 0)],
    ))
    .unwrap();
    assert_eq!(m.overrides.len(), 1);
    assert_eq!(m.overrides[0].gsi, 2);
    assert_eq!(m.bad_entries, 2);
}

#[test]
fn nmis() {
    let m = Madt::parse(&madt(
        0,
        0,
        &[
            nmi_source(0xd, 7),
            lapic_nmi(3, 0x5, 0),
            lapic_nmi(0xff, 0, 2), // LINT2 doesn't exist: skipped
            x2apic_nmi(0xf, u32::MAX, 1),
            x2apic_nmi(0, 300, 0),
        ],
    ))
    .unwrap();
    assert_eq!(
        m.nmi_sources,
        vec![NmiSource {
            gsi: 7,
            polarity: Polarity::High,
            trigger: Trigger::Level
        }]
    );
    assert_eq!(
        m.lapic_nmis,
        vec![
            LapicNmi {
                uid: Some(3),
                lint: 0,
                polarity: Polarity::High,
                trigger: Trigger::Edge
            },
            LapicNmi {
                uid: None,
                lint: 1,
                polarity: Polarity::Low,
                trigger: Trigger::Level
            },
            LapicNmi {
                uid: Some(300),
                lint: 0,
                polarity: Polarity::Bus,
                trigger: Trigger::Bus
            },
        ]
    );
    assert_eq!(m.bad_entries, 1);
}

#[test]
fn lapic_address_override() {
    let m = Madt::parse(&madt(0xfee0_0000, 0, &[lapic_override(0x1_fee0_0000)])).unwrap();
    assert_eq!(m.lapic_address, 0x1_fee0_0000);
    // A zero override is ignored.
    let m = Madt::parse(&madt(0xfee0_0000, 0, &[lapic_override(0)])).unwrap();
    assert_eq!((m.lapic_address, m.bad_entries), (0xfee0_0000, 1));
}

#[test]
fn unknown_short_and_broken_entries() {
    // Unknown type 0x0b (GIC CPU interface) with its real length.
    let mut gicc = vec![0x0b, 80];
    gicc.resize(80, 0);
    // A type 0 entry only 6 bytes long.
    let short = vec![0, 6, 0, 0, 0, 0];
    // A longer than needed entry is fine (future revisions grow them).
    let mut long = lapic(7, 7, 1);
    long[1] = 10;
    long.extend_from_slice(&[0, 0]);
    let m = Madt::parse(&madt(0, 0, &[gicc, short, long])).unwrap();
    assert_eq!(
        (m.unknown_entries, m.bad_entries, m.truncated),
        (1, 1, false)
    );
    assert_eq!(m.cpus.len(), 1);
    assert_eq!(m.cpus[0].apic_id, 7);
}

#[test]
fn entry_lists_that_end_badly() {
    for (tail, why) in [
        (vec![0u8], "one byte left"),
        (vec![0u8, 0], "zero length"),
        (vec![0u8, 1], "length 1"),
        (vec![0u8, 8, 0, 9], "runs past the end"),
    ] {
        let m = Madt::parse(&madt(0, 0, &[lapic(0, 0, 1), tail])).unwrap();
        assert!(m.truncated, "{why}");
        assert_eq!(m.cpus.len(), 1, "{why}");
    }
}

#[test]
fn madt_errors() {
    let t = qemu();
    assert_eq!(
        Madt::parse(&t[..43]),
        Err(Error::TooShort).or(Err(Error::BadLength))
    );
    let mut short = crate::build_table(b"APIC", 1, &[0; 7]);
    assert_eq!(Madt::parse(&short), Err(Error::BadLength));
    short[0] = b'X';
    assert_eq!(Madt::parse(&short), Err(Error::BadSignature));
    // A header and flags, no entries: valid and empty.
    let m = Madt::parse(&madt(0xfee0_0000, 0, &[])).unwrap();
    assert!(m.cpus.is_empty() && !m.truncated);
    // Wrong checksum: still parsed, and reported.
    let mut bad = qemu();
    bad[50] ^= 0x40;
    let m = Madt::parse(&bad).unwrap();
    assert!(!m.checksum_ok);
}

/// Reference model for random round trips.
#[derive(Clone, Debug)]
enum Ent {
    Lapic(u8, u8, u32),
    X2(u32, u32, u32),
    Io(u8, u32, u32),
    Iso(u8, u32, u16),
    Unknown(u8, u8),
}

fn ent() -> impl Strategy<Value = Ent> {
    prop_oneof![
        (any::<u8>(), any::<u8>(), 0u32..4).prop_map(|(a, b, c)| Ent::Lapic(a, b, c)),
        (any::<u32>(), 0u32..4, any::<u32>()).prop_map(|(a, b, c)| Ent::X2(a, b, c)),
        (0u8..8, any::<u32>(), 0u32..8).prop_map(|(a, b, c)| Ent::Io(a, b, c * 24)),
        (0u8..16, any::<u32>(), any::<u16>()).prop_map(|(a, b, c)| Ent::Iso(a, b, c)),
        (11u8..=255, 2u8..40).prop_map(|(a, b)| Ent::Unknown(a, b)),
    ]
}

fn bytes(e: &Ent) -> Vec<u8> {
    match *e {
        Ent::Lapic(u, i, f) => lapic(u, i, f),
        Ent::X2(i, f, u) => x2apic(i, f, u),
        Ent::Io(i, a, g) => ioapic(i, a, g),
        Ent::Iso(s, g, f) => iso(0, s, g, f),
        Ent::Unknown(k, l) => {
            let mut v = vec![k, l];
            v.resize(l as usize, 0);
            v
        }
    }
}

proptest! {
    #![proptest_config(super::config(3000))]

    /// Random entry lists parse to exactly what a simple model predicts.
    #[test]
    fn round_trip_matches_model(es in proptest::collection::vec(ent(), 0..40)) {
        let t = madt(0xfee0_0000, 1, &es.iter().map(bytes).collect::<Vec<_>>());
        let m = Madt::parse(&t).unwrap();
        let (mut cpus, mut ios, mut isos) = (Vec::new(), Vec::new(), Vec::new());
        let (mut unknown, mut bad) = (0, 0);
        for e in &es {
            match *e {
                Ent::Lapic(u, i, f) => {
                    if i == 0xff || cpus.iter().any(|c: &Cpu| c.apic_id == u32::from(i)) { bad += 1; continue; }
                    cpus.push(Cpu { apic_id: i.into(), uid: u.into(), enabled: f & 1 == 1, online_capable: f == 2, x2apic: false });
                }
                Ent::X2(i, f, u) => {
                    if cpus.iter().any(|c: &Cpu| c.apic_id == i) { bad += 1; continue; }
                    cpus.push(Cpu { apic_id: i, uid: u, enabled: f & 1 == 1, online_capable: f == 2, x2apic: true });
                }
                Ent::Io(i, a, g) => {
                    if ios.iter().any(|x: &IoApic| x.id == i || x.gsi_base == g) { bad += 1; continue; }
                    ios.push(IoApic { id: i, address: a, gsi_base: g });
                }
                Ent::Iso(s, g, f) => {
                    if isos.iter().any(|o: &(u8, u32, u16)| o.0 == s) { bad += 1; continue; }
                    isos.push((s, g, f & 0xf));
                }
                Ent::Unknown(..) => unknown += 1,
            }
        }
        prop_assert_eq!(&m.cpus, &cpus);
        prop_assert_eq!(&m.ioapics, &ios);
        let got: Vec<_> = m.overrides.iter().map(|o| {
            let p = [Polarity::Bus, Polarity::High, Polarity::Reserved, Polarity::Low].iter().position(|&x| x == o.polarity).unwrap();
            let t = [Trigger::Bus, Trigger::Edge, Trigger::Reserved, Trigger::Level].iter().position(|&x| x == o.trigger).unwrap();
            (o.source, o.gsi, (p | t << 2) as u16)
        }).collect();
        prop_assert_eq!(got, isos);
        prop_assert_eq!((m.unknown_entries, m.bad_entries, m.truncated), (unknown, bad, false));
    }

    /// Cutting a valid table anywhere (and fixing its length) never panics,
    /// and the CPUs found are a prefix of the full list.
    #[test]
    fn truncation_gives_a_prefix(es in proptest::collection::vec(ent(), 0..20), cut in 0usize..400) {
        let full = madt(0, 0, &es.iter().map(bytes).collect::<Vec<_>>());
        let all = Madt::parse(&full).unwrap();
        let n = cut.min(full.len());
        let mut t = full[..n].to_vec();
        if n >= 8 { t[4..8].copy_from_slice(&(n as u32).to_le_bytes()); }
        if let Ok(m) = Madt::parse(&t) {
            prop_assert!(m.cpus.len() <= all.cpus.len());
            prop_assert_eq!(&m.cpus[..], &all.cpus[..m.cpus.len()]);
        }
    }

    #[test]
    fn random_bodies_never_panic(body in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = Madt::parse(&build_table(b"APIC", 1, &body));
    }
}
