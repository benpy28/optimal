use proptest::prelude::*;

use crate::Error;
use crate::build_table;
use crate::hpet::Hpet;

fn hpet(id: u32, space: u8, addr: u64, number: u8, min_tick: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&id.to_le_bytes());
    b.extend_from_slice(&[space, 64, 0, 0]);
    b.extend_from_slice(&addr.to_le_bytes());
    b.push(number);
    b.extend_from_slice(&min_tick.to_le_bytes());
    b.push(0); // page protection
    build_table(b"HPET", 1, &b)
}

#[test]
fn qemu_hpet_hand_worked() {
    // QEMU: Event Timer Block ID 0x8086_a201.
    //   bits 0-7 = 0x01 hardware revision
    //   bits 8-12 = 0b00010 = 2, so 3 comparators
    //   bit 13 = 1 (0xa201 has 0x2000): 64-bit counter
    //   bit 15 = 1 (0x8000): legacy replacement capable
    //   bits 16-31 = 0x8086 PCI vendor
    let h = Hpet::parse(&hpet(0x8086_a201, 0, 0xfed0_0000, 0, 128)).unwrap();
    assert_eq!(
        h,
        Hpet {
            checksum_ok: true,
            address: 0xfed0_0000,
            hardware_rev: 1,
            comparators: 3,
            counter_64bit: true,
            legacy_replacement: true,
            pci_vendor: 0x8086,
            number: 0,
            min_tick: 128,
        }
    );
}

#[test]
fn hpet_id_bits_one_at_a_time() {
    let p = |id| Hpet::parse(&hpet(id, 0, 0xfed0_0000, 3, 0x1234)).unwrap();
    let h = p(0);
    assert_eq!(
        (
            h.hardware_rev,
            h.comparators,
            h.counter_64bit,
            h.legacy_replacement,
            h.pci_vendor
        ),
        (0, 1, false, false, 0)
    );
    assert_eq!(p(0x1f00).comparators, 32);
    assert_eq!(p(0x2000).comparators, 1);
    assert!(p(0x2000).counter_64bit);
    assert!(!p(0x4000).counter_64bit && !p(0x4000).legacy_replacement);
    assert!(p(0x8000).legacy_replacement);
    assert_eq!(p(0xffff_0000).pci_vendor, 0xffff);
    assert_eq!(p(0xff).hardware_rev, 0xff);
    assert_eq!((h.number, h.min_tick), (3, 0x1234));
}

#[test]
fn hpet_bad_addresses() {
    // Port I/O space, zero, and not 1 KiB aligned.
    assert_eq!(
        Hpet::parse(&hpet(0, 1, 0xfed0_0000, 0, 0)),
        Err(Error::BadField)
    );
    assert_eq!(Hpet::parse(&hpet(0, 0, 0, 0, 0)), Err(Error::BadField));
    assert_eq!(
        Hpet::parse(&hpet(0, 0, 0xfed0_0200, 0, 0)),
        Err(Error::BadField)
    );
    assert!(Hpet::parse(&hpet(0, 0, 0xfed0_0400, 0, 0)).is_ok());
    assert!(Hpet::parse(&hpet(0, 0, 0x1_0000_0000_0400, 0, 0)).is_ok());
}

#[test]
fn hpet_lengths() {
    let t = hpet(0, 0, 0xfed0_0000, 0, 0);
    assert_eq!(t.len(), 56);
    let mut short = t[..55].to_vec();
    short[4..8].copy_from_slice(&55u32.to_le_bytes());
    assert_eq!(Hpet::parse(&short), Err(Error::BadLength));
    let mut bad = t.clone();
    bad[55] ^= 1;
    assert!(!Hpet::parse(&bad).unwrap().checksum_ok);
}

proptest! {
    #![proptest_config(super::config(3000))]

    #[test]
    fn random_hpet_never_panics(body in proptest::collection::vec(any::<u8>(), 0..40)) {
        let _ = Hpet::parse(&build_table(b"HPET", 1, &body));
    }

    /// Fields come back exactly as written.
    #[test]
    fn hpet_round_trip(id: u32, page in 1u64..1 << 42, number: u8, min_tick: u16) {
        let h = Hpet::parse(&hpet(id, 0, page << 10, number, min_tick)).unwrap();
        prop_assert_eq!(h.address, page << 10);
        prop_assert_eq!(h.comparators, ((id >> 8) & 0x1f) as u8 + 1);
        prop_assert_eq!((h.number, h.min_tick, h.pci_vendor), (number, min_tick, (id >> 16) as u16));
    }
}
