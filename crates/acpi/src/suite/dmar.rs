use proptest::prelude::*;

use crate::Error;
use crate::build_table;
use crate::dmar::*;

fn dmar(width: u8, flags: u8, extra: &[u8]) -> Vec<u8> {
    let mut body = vec![width, flags];
    body.extend_from_slice(&[0; 10]);
    body.extend_from_slice(extra);
    build_table(b"DMAR", 1, &body)
}

/// A DMAR with width 39 bits (stored 38 = 0x26) and flags
/// INTR_REMAP | X2APIC_OPT_OUT = 0b011. (QEMU's intel-iommu sets only
/// INTR_REMAP, even with eim=off.)
#[test]
fn hand_worked_qemu_style() {
    let d = Dmar::parse(&dmar(0x26, 0b011, &[])).unwrap();
    assert_eq!(
        d,
        Dmar {
            checksum_ok: true,
            host_address_width: 39,
            interrupt_remapping: true,
            x2apic_opt_out: true,
            dma_ctrl_platform_opt_in: false,
        }
    );
}

/// Each flag bit lands in its own field, and the reserved bits 3 to 7 are
/// ignored: 0b1111_1100 sets only DMA_CTRL_PLATFORM_OPT_IN.
#[test]
fn each_flag_alone() {
    let d = Dmar::parse(&dmar(47, 0b001, &[])).unwrap();
    assert!(d.interrupt_remapping && !d.x2apic_opt_out && !d.dma_ctrl_platform_opt_in);
    let d = Dmar::parse(&dmar(47, 0b010, &[])).unwrap();
    assert!(!d.interrupt_remapping && d.x2apic_opt_out && !d.dma_ctrl_platform_opt_in);
    let d = Dmar::parse(&dmar(47, 0b1111_1100, &[])).unwrap();
    assert!(!d.interrupt_remapping && !d.x2apic_opt_out && d.dma_ctrl_platform_opt_in);
    assert_eq!(d.host_address_width, 48);
}

#[test]
fn too_short_and_bad_width() {
    // 47 bytes: one short of the fixed part.
    let t = dmar(38, 0, &[]);
    let mut short = t.clone();
    short.pop();
    let len = short.len() as u32;
    short[4..8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(Dmar::parse(&short), Err(Error::BadLength));
    // Exactly 48 bytes is fine; remapping structures after it are allowed.
    assert!(Dmar::parse(&t).is_ok());
    assert!(Dmar::parse(&dmar(38, 0, &[0; 24])).is_ok());
    assert_eq!(Dmar::parse(&dmar(255, 0, &[])), Err(Error::BadField));
    assert_eq!(
        Dmar::parse(&dmar(254, 0, &[])).unwrap().host_address_width,
        255
    );
    assert_eq!(
        Dmar::parse(&build_table(b"DMAX", 1, &[0; 12])),
        Err(Error::BadSignature)
    );
    let mut bad = t.clone();
    bad[40] ^= 1;
    assert!(!Dmar::parse(&bad).unwrap().checksum_ok);
}

proptest! {
    #![proptest_config(super::config(3000))]

    #[test]
    fn random_dmar_never_panics(body in proptest::collection::vec(any::<u8>(), 0..40)) {
        let _ = Dmar::parse(&build_table(b"DMAR", 1, &body));
    }

    #[test]
    fn dmar_round_trip(width in 0u8..255, flags: u8) {
        let d = Dmar::parse(&dmar(width, flags, &[])).unwrap();
        prop_assert_eq!(d.host_address_width, width + 1);
        prop_assert_eq!(d.interrupt_remapping, flags & 1 != 0);
        prop_assert_eq!(d.x2apic_opt_out, flags & 2 != 0);
        prop_assert_eq!(d.dma_ctrl_platform_opt_in, flags & 4 != 0);
    }
}
