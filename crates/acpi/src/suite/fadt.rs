use proptest::prelude::*;

use crate::build_table;
use crate::fadt::{Fadt, PmTimer};
use crate::{Error, HEADER_LEN};

/// A FADT of total length `len`, with fields set at their offsets.
fn fadt(len: usize, set: &[(usize, &[u8])]) -> Vec<u8> {
    let mut body = vec![0u8; len - HEADER_LEN];
    for (off, bytes) in set {
        body[off - HEADER_LEN..off - HEADER_LEN + bytes.len()].copy_from_slice(bytes);
    }
    build_table(b"FACP", if len > 116 { 6 } else { 1 }, &body)
}

const LEGACY: usize = 76;
const LEN: usize = 91;
const CENTURY: usize = 108;
const BOOT_ARCH: usize = 109;
const FLAGS: usize = 112;
const X: usize = 208;
const FACS32: usize = 36;
const DSDT32: usize = 40;
const XFACS: usize = 132;
const XDSDT: usize = 140;

fn gas(space: u8, addr: u64) -> Vec<u8> {
    let mut g = vec![space, 32, 0, 3];
    g.extend_from_slice(&addr.to_le_bytes());
    g
}

#[test]
fn acpi_1_fadt_hand_worked() {
    // 116 bytes, PM timer at port 0x608, 4 bytes long, century register 0x32.
    let t = fadt(
        116,
        &[
            (LEGACY, &0x608u32.to_le_bytes()),
            (LEN, &[4]),
            (CENTURY, &[0x32]),
        ],
    );
    assert_eq!(t.len(), 116);
    let f = Fadt::parse(&t).unwrap();
    assert_eq!(
        f,
        Fadt {
            checksum_ok: true,
            pm_timer: Some(PmTimer::Io(0x608)),
            pm_timer_32bit: false,
            pm_timer_mismatch: false,
            hw_reduced: false,
            boot_arch: 0,
            century: Some(0x32),
            dsdt: None,
            dsdt_mismatch: false,
            facs: None,
            facs_mismatch: false,
        }
    );
}

#[test]
fn qemu_q35_fadt() {
    // Revision 3-style 244-byte table with both timer fields agreeing, the
    // 32-bit timer flag (bit 8), and boot arch = 8042 present (bit 1).
    let t = fadt(
        244,
        &[
            (LEGACY, &0x608u32.to_le_bytes()),
            (LEN, &[4]),
            (BOOT_ARCH, &2u16.to_le_bytes()),
            (FLAGS, &0x1a5u32.to_le_bytes()),
            (X, &gas(1, 0x608)),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!(f.pm_timer, Some(PmTimer::Io(0x608)));
    assert!(f.pm_timer_32bit);
    assert!(!f.pm_timer_mismatch);
    assert_eq!(f.boot_arch, Fadt::HAS_8042);
    assert_eq!(f.century, None);
}

#[test]
fn extended_timer_wins_and_mismatch_is_reported() {
    let t = fadt(
        276,
        &[
            (LEGACY, &0x408u32.to_le_bytes()),
            (LEN, &[4]),
            (X, &gas(1, 0x1808)),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!(
        (f.pm_timer, f.pm_timer_mismatch),
        (Some(PmTimer::Io(0x1808)), true)
    );
    // Memory-mapped timer.
    let t = fadt(276, &[(X, &gas(0, 0xfe00_0008))]);
    let f = Fadt::parse(&t).unwrap();
    assert_eq!(
        (f.pm_timer, f.pm_timer_mismatch),
        (Some(PmTimer::Memory(0xfe00_0008)), false)
    );
    // An unsupported address space, or an I/O port above 0xffff, falls
    // back to the 32-bit field.
    for g in [gas(2, 0x608), gas(1, 0x1_0000)] {
        let t = fadt(
            276,
            &[(LEGACY, &0x608u32.to_le_bytes()), (LEN, &[4]), (X, &g)],
        );
        let f = Fadt::parse(&t).unwrap();
        assert_eq!(
            (f.pm_timer, f.pm_timer_mismatch),
            (Some(PmTimer::Io(0x608)), false)
        );
    }
    // Zero extended address: the legacy field is used.
    let t = fadt(
        276,
        &[
            (LEGACY, &0x608u32.to_le_bytes()),
            (LEN, &[4]),
            (X, &gas(1, 0)),
        ],
    );
    assert_eq!(Fadt::parse(&t).unwrap().pm_timer, Some(PmTimer::Io(0x608)));
}

#[test]
fn legacy_timer_needs_length_4_and_a_port() {
    for (port, len) in [(0x608u32, 3u8), (0x608, 0), (0, 4), (0x1_0000, 4)] {
        let t = fadt(116, &[(LEGACY, &port.to_le_bytes()), (LEN, &[len])]);
        assert_eq!(
            Fadt::parse(&t).unwrap().pm_timer,
            None,
            "port {port:#x} len {len}"
        );
    }
    let t = fadt(116, &[(LEGACY, &0xffffu32.to_le_bytes()), (LEN, &[4])]);
    assert_eq!(Fadt::parse(&t).unwrap().pm_timer, Some(PmTimer::Io(0xffff)));
}

#[test]
fn hardware_reduced_has_no_pm_timer() {
    let t = fadt(
        276,
        &[
            (LEGACY, &0x608u32.to_le_bytes()),
            (LEN, &[4]),
            (FLAGS, &(1u32 << 20).to_le_bytes()),
            (X, &gas(1, 0x808)),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert!(f.hw_reduced);
    assert_eq!((f.pm_timer, f.pm_timer_mismatch), (None, false));
}

#[test]
fn short_tables() {
    // The extended timer field ends at byte 220: a table of 219 bytes
    // doesn't have it, so only the legacy field counts.
    let t = fadt(219, &[(LEGACY, &0x608u32.to_le_bytes()), (LEN, &[4])]);
    assert_eq!(Fadt::parse(&t).unwrap().pm_timer, Some(PmTimer::Io(0x608)));
    let mut t = fadt(220, &[(X, &gas(1, 0x1808))]);
    assert_eq!(Fadt::parse(&t).unwrap().pm_timer, Some(PmTimer::Io(0x1808)));
    t.truncate(219);
    t[4..8].copy_from_slice(&219u32.to_le_bytes());
    assert_eq!(Fadt::parse(&t).unwrap().pm_timer, None);
    // 115 bytes is shorter than any FADT ever defined.
    let mut t = fadt(116, &[]);
    t.truncate(115);
    t[4..8].copy_from_slice(&115u32.to_le_bytes());
    assert_eq!(Fadt::parse(&t), Err(Error::BadLength));
    let mut t = fadt(116, &[]);
    t[0] = b'F';
    t[3] = b'T';
    assert_eq!(Fadt::parse(&t), Err(Error::BadSignature));
}

#[test]
fn flags_and_boot_arch_bits() {
    let t = fadt(
        244,
        &[
            (BOOT_ARCH, &0x2fu16.to_le_bytes()),
            (FLAGS, &(1u32 << 8).to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!(
        f.boot_arch,
        Fadt::LEGACY_DEVICES
            | Fadt::HAS_8042
            | Fadt::VGA_NOT_PRESENT
            | Fadt::MSI_NOT_SUPPORTED
            | Fadt::CMOS_RTC_NOT_PRESENT
    );
    assert!(f.pm_timer_32bit && !f.hw_reduced);
    let mut bad = t.clone();
    bad[200] ^= 1;
    assert!(!Fadt::parse(&bad).unwrap().checksum_ok);
}

#[test]
fn dsdt_and_facs_hand_worked() {
    // ACPI 1.0 table (116 bytes): only the 32-bit fields exist.
    let t = fadt(
        116,
        &[
            (FACS32, &0x7ffe_0000u32.to_le_bytes()),
            (DSDT32, &0x7ffe_0040u32.to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x7ffe_0040), false));
    assert_eq!((f.facs, f.facs_mismatch), (Some(0x7ffe_0000), false));
    // Both set and equal (QEMU does this): no mismatch.
    let t = fadt(
        244,
        &[
            (FACS32, &0x1000u32.to_le_bytes()),
            (DSDT32, &0x2000u32.to_le_bytes()),
            (XFACS, &0x1000u64.to_le_bytes()),
            (XDSDT, &0x2000u64.to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x2000), false));
    assert_eq!((f.facs, f.facs_mismatch), (Some(0x1000), false));
    // Both set and different: the X_ field wins and the clash is reported.
    let t = fadt(
        244,
        &[
            (FACS32, &0x1000u32.to_le_bytes()),
            (DSDT32, &0x2000u32.to_le_bytes()),
            (XFACS, &0x1_0000_1000u64.to_le_bytes()),
            (XDSDT, &0x1_0000_2000u64.to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x1_0000_2000), true));
    assert_eq!((f.facs, f.facs_mismatch), (Some(0x1_0000_1000), true));
    // Only the X_ fields set: used, no mismatch.
    let t = fadt(
        244,
        &[
            (XFACS, &0x3000u64.to_le_bytes()),
            (XDSDT, &0x4000u64.to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x4000), false));
    assert_eq!((f.facs, f.facs_mismatch), (Some(0x3000), false));
    // Nothing set: none (a HW-reduced machine may have no FACS).
    let f = Fadt::parse(&fadt(244, &[])).unwrap();
    assert_eq!((f.dsdt, f.facs), (None, None));
}

#[test]
fn x_fields_need_the_bytes() {
    // X_FIRMWARE_CTRL is bytes 132..140 and X_DSDT 140..148. A 147-byte
    // table has the first but not the second, so DSDT falls back to the
    // 32-bit field.
    let mut t = fadt(
        148,
        &[
            (DSDT32, &0x2000u32.to_le_bytes()),
            (XFACS, &0x5000u64.to_le_bytes()),
            (XDSDT, &0x6000u64.to_le_bytes()),
        ],
    );
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x6000), true));
    assert_eq!(f.facs, Some(0x5000));
    t.truncate(147);
    t[4..8].copy_from_slice(&147u32.to_le_bytes());
    let f = Fadt::parse(&t).unwrap();
    assert_eq!((f.dsdt, f.dsdt_mismatch), (Some(0x2000), false));
    assert_eq!(f.facs, Some(0x5000));
    t.truncate(139);
    t[4..8].copy_from_slice(&139u32.to_le_bytes());
    assert_eq!(Fadt::parse(&t).unwrap().facs, None);
}

proptest! {
    #![proptest_config(super::config(3000))]

    /// The address rule for both DSDT and FACS: the X_ field when non-zero,
    /// else the 32-bit field when non-zero, else none; a clash is reported
    /// only when both are non-zero and differ.
    #[test]
    fn address_choice_rule(f32: u32, x: u64, d32: u32, xd: u64, zero_mask in 0u8..16) {
        let pick = |bit: u8, v: u64| if zero_mask & bit != 0 { 0 } else { v };
        let (f32, x, d32, xd) = (pick(1, f32.into()) as u32, pick(2, x), pick(4, d32.into()) as u32, pick(8, xd));
        let t = fadt(244, &[(FACS32, &f32.to_le_bytes()), (XFACS, &x.to_le_bytes()), (DSDT32, &d32.to_le_bytes()), (XDSDT, &xd.to_le_bytes())]);
        let f = Fadt::parse(&t).unwrap();
        let rule = |l: u32, x: u64| {
            let addr = if x != 0 { Some(x) } else if l != 0 { Some(u64::from(l)) } else { None };
            (addr, l != 0 && x != 0 && u64::from(l) != x)
        };
        prop_assert_eq!((f.facs, f.facs_mismatch), rule(f32, x));
        prop_assert_eq!((f.dsdt, f.dsdt_mismatch), rule(d32, xd));
    }

    #[test]
    fn random_fadt_never_panics(body in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = Fadt::parse(&build_table(b"FACP", 6, &body));
    }

    /// The timer chosen always follows the rule: none if hardware-reduced,
    /// else the usable 64-bit field, else the usable 32-bit field.
    #[test]
    fn timer_choice_rule(port: u32, len in 0u8..6, space in 0u8..3, x: u64, flags: u32) {
        let t = fadt(276, &[(LEGACY, &port.to_le_bytes()), (LEN, &[len]), (FLAGS, &flags.to_le_bytes()), (X, &gas(space, x))]);
        let f = Fadt::parse(&t).unwrap();
        let legacy = (port != 0 && len == 4 && port <= 0xffff).then_some(PmTimer::Io(port as u16));
        let ext = match space {
            _ if x == 0 => None,
            0 => Some(PmTimer::Memory(x)),
            1 if x <= 0xffff => Some(PmTimer::Io(x as u16)),
            _ => None,
        };
        let hw = flags & (1 << 20) != 0;
        prop_assert_eq!(f.pm_timer, if hw { None } else { ext.or(legacy) });
        prop_assert_eq!(f.pm_timer_mismatch, !hw && legacy.is_some() && ext.is_some() && legacy != ext);
        prop_assert_eq!(f.pm_timer_32bit, flags & (1 << 8) != 0);
    }
}
