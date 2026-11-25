use proptest::prelude::*;

use crate::ioapic::*;

#[test]
fn registers() {
    assert_eq!(redirection_regs(0), Some((0x10, 0x11)));
    assert_eq!(redirection_regs(23), Some((0x3E, 0x3F)));
    // 0x10 + 2 * 119 = 0xFE: the last pair IOREGSEL can select.
    assert_eq!(redirection_regs(119), Some((0xFE, 0xFF)));
    assert_eq!(redirection_regs(120), None);
    assert_eq!(redirection_regs(255), None);
    assert_eq!((IOREGSEL, IOWIN, REG_ID, REG_VERSION), (0, 0x10, 0, 1));
}

#[test]
fn addresses() {
    assert!(address_ok(0xFEC0_0000));
    assert!(address_ok(0xFEC0_1000));
    assert!(!address_ok(0));
    assert!(!address_ok(0xFEC0_0008));
    // 0xFE0 + 0x14 = 0xFF4, inside the page; 0xFF0 + 0x14 crosses it.
    assert!(address_ok(0xFEC0_0FE0));
    assert!(!address_ok(0xFEC0_0FF0));
}

/// QEMU's I/O APIC version register reads 0x00170020: version 0x20,
/// max entry 0x17 = 23, so 24 pins.
#[test]
fn version_and_id() {
    let v = Version::decode(0x0017_0020);
    assert_eq!(
        v,
        Version {
            version: 0x20,
            max_entry: 23
        }
    );
    assert_eq!(v.pins(), 24);
    assert_eq!(Version::decode(0x00FF_0011).pins(), 256);
    assert!(id_matches(0, 0));
    assert!(id_matches(0x0100_0000, 1));
    assert!(id_matches(0x0F00_0000, 0x1F));
    assert!(id_matches(0xF100_0000, 1));
    assert!(!id_matches(0x0200_0000, 1));
    assert!(!id_matches(0x0000_0001, 1));
}

/// IRQ 0 moved to GSI 2 on vector 0x30: fixed, physical, active high, edge,
/// masked (bit 16): low 0x10030, high 0. The SCI on IRQ 9 is level
/// (bit 15): 0x18039, and active low adds bit 13: 0x1A039.
#[test]
fn redirection_hand_worked() {
    let timer = Redirection {
        vector: 0x30,
        delivery: Delivery::Fixed,
        logical: false,
        active_low: false,
        level: false,
        masked: true,
        destination: 0,
    };
    assert_eq!(timer.encode(), (0x1_0030, 0));
    let sci = Redirection {
        vector: 0x39,
        level: true,
        ..timer
    };
    assert_eq!(sci.encode(), (0x1_8039, 0));
    assert_eq!(
        Redirection {
            active_low: true,
            ..sci
        }
        .encode(),
        (0x1_A039, 0)
    );
    let nmi = Redirection {
        vector: 0,
        delivery: Delivery::Nmi,
        masked: false,
        destination: 1,
        ..timer
    };
    assert_eq!(nmi.encode(), (0x400, 0x0100_0000));
    assert_eq!(
        Redirection {
            logical: true,
            delivery: Delivery::LowestPriority,
            ..timer
        }
        .encode(),
        (0x1_0930, 0)
    );
    for (d, bits) in [
        (Delivery::Fixed, 0),
        (Delivery::LowestPriority, 1),
        (Delivery::Smi, 2),
        (Delivery::Nmi, 4),
        (Delivery::Init, 5),
        (Delivery::ExtInt, 7),
    ] {
        let r = Redirection {
            delivery: d,
            ..timer
        };
        assert_eq!(r.encode().0 >> 8 & 7, bits);
        assert_eq!(Redirection::decode(r.encode().0, 0).entry.delivery, d);
    }
    // 0x5030: pending (bit 12) and remote IRR (bit 14) on vector 0x30.
    let b = Redirection::decode(0x5030, 0xAB00_0000);
    assert!(b.pending && b.remote_irr && !b.entry.masked);
    assert_eq!((b.entry.vector, b.entry.destination), (0x30, 0xAB));
    assert_eq!(
        Redirection::decode(0x300, 0).entry.delivery,
        Delivery::Reserved(3)
    );
    assert_eq!(
        Redirection::decode(0x600, 0).entry.delivery,
        Delivery::Reserved(6)
    );
    assert_eq!(
        timer.writes(2),
        Some([(0x14, 0x1_0030), (0x15, 0), (0x14, 0x1_0030)])
    );
    assert_eq!(
        nmi.writes(13),
        Some([(0x2A, 0x1_0400), (0x2B, 0x0100_0000), (0x2A, 0x400)])
    );
    assert_eq!(nmi.writes(120), None);
}

fn entry(
    vector: u8,
    d: u32,
    logical: bool,
    active_low: bool,
    level: bool,
    masked: bool,
    dest: u8,
) -> Redirection {
    let low = u32::from(vector)
        | d << 8
        | u32::from(logical) << 11
        | u32::from(active_low) << 13
        | u32::from(level) << 15
        | u32::from(masked) << 16;
    Redirection::decode(low, u32::from(dest) << 24).entry
}

proptest! {
    #![proptest_config(super::config(5000))]

    #[test]
    fn redirection_decode_keeps_defined_bits(low: u32, high: u32) {
        let r = Redirection::decode(low, high);
        prop_assert_eq!(r.entry.encode(), (low & 0x1_AFFF, high & 0xFF00_0000));
        prop_assert_eq!(r.pending, low & 0x1000 != 0);
        prop_assert_eq!(r.remote_irr, low & 0x4000 != 0);
    }

    /// Model the two registers of a pin. Whatever was there, after the
    /// writes the pin holds the new entry, and at every step where the pin
    /// is unmasked its two halves are both old or both new.
    #[test]
    fn writes_never_expose_a_torn_entry(
        old_low: u32, old_high: u32,
        vector: u8, d in 0u32..8, logical: bool, active_low: bool, level: bool, masked: bool, dest: u8,
        pin in 0u8..120,
    ) {
        let new = entry(vector, d, logical, active_low, level, masked, dest);
        let (lo_reg, hi_reg) = redirection_regs(pin).unwrap();
        let (mut lo, mut hi) = (old_low, old_high);
        for (reg, v) in new.writes(pin).unwrap() {
            if reg == lo_reg { lo = v } else { prop_assert_eq!(reg, hi_reg); hi = v }
            if lo & MASKED == 0 {
                prop_assert!((lo, hi) == (old_low, old_high) || (lo, hi) == new.encode());
            }
        }
        prop_assert_eq!((lo, hi), new.encode());
    }
}
