use proptest::prelude::*;

use crate::msi::*;

/// Worked by hand from SDM Vol. 3A figure 11-24 and 11-25:
/// address = 0xFEE << 20 | id << 12, data = vector (fixed, edge).
#[test]
fn fixed_edge_hand_worked() {
    // APIC 0, the HPET timer vector 0xED: 0xFEE00000, 0x000000ED.
    assert_eq!(
        fixed_edge(0, 0xED),
        Ok(Message {
            address: 0xFEE0_0000,
            data: 0x0000_00ED
        })
    );
    // APIC 3 is 0x3 << 12 = 0x3000.
    assert_eq!(
        fixed_edge(3, 0x41),
        Ok(Message {
            address: 0xFEE0_3000,
            data: 0x41
        })
    );
    // The widest ID that fits: 0xFF << 12 = 0xFF000.
    assert_eq!(
        fixed_edge(0xFF, 0xFF),
        Ok(Message {
            address: 0xFEEF_F000,
            data: 0xFF
        })
    );
    // 0x100 needs bit 20, which is part of 0xFEE.
    assert_eq!(
        fixed_edge(0x100, 0xED),
        Err(Refused::DestinationTooWide(0x100))
    );
    assert_eq!(
        fixed_edge(u32::MAX, 0xED),
        Err(Refused::DestinationTooWide(u32::MAX))
    );
    // Exceptions: 0x1F is the last refused, 0x20 the first accepted.
    assert_eq!(fixed_edge(0, 0x1F), Err(Refused::Vector(0x1F)));
    assert_eq!(fixed_edge(0, 0x00), Err(Refused::Vector(0x00)));
    assert_eq!(fixed_edge(0, 0x20).map(|m| m.data), Ok(0x20));
    // The destination is checked first.
    assert_eq!(
        fixed_edge(0x100, 0x00),
        Err(Refused::DestinationTooWide(0x100))
    );
}

/// The field constants match the figure.
#[test]
fn field_constants() {
    assert_eq!(ADDRESS_BASE >> 20, 0xFEE);
    assert_eq!(REDIRECTION_HINT, 0x8);
    assert_eq!(LOGICAL, 0x4);
    assert_eq!(ASSERT, 0x4000);
    assert_eq!(LEVEL, 0x8000);
    assert_eq!(DELIVERY_SHIFT, 8);
    assert_eq!(DESTINATION_SHIFT, 12);
}

proptest! {
    #![proptest_config(super::config(4096))]

    /// Every accepted message decodes back, field by field, to what was
    /// asked: physical, no hint, fixed, edge, deasserted level bit, and
    /// nothing in the reserved bits.
    #[test]
    fn fixed_edge_decodes(id in 0u32..0x200, vector in any::<u8>()) {
        match fixed_edge(id, vector) {
            Ok(m) => {
                prop_assert!(id <= 0xFF && vector >= 0x20);
                prop_assert_eq!(m.address >> 20, 0xFEE);
                prop_assert_eq!((m.address >> DESTINATION_SHIFT) & 0xFF, id);
                prop_assert_eq!(m.address & (REDIRECTION_HINT | LOGICAL), 0);
                prop_assert_eq!(m.address & 0xFFF, 0);
                prop_assert_eq!(m.data & 0xFF, u32::from(vector));
                prop_assert_eq!((m.data >> DELIVERY_SHIFT) & 0x7, 0);
                prop_assert_eq!(m.data & (LEVEL | ASSERT), 0);
                prop_assert_eq!(m.data >> 8, 0);
            }
            Err(Refused::DestinationTooWide(i)) => {
                prop_assert_eq!(i, id);
                prop_assert!(id > 0xFF);
            }
            Err(Refused::Vector(v)) => {
                prop_assert_eq!(v, vector);
                prop_assert!(id <= 0xFF && vector < 0x20);
            }
        }
    }
}
