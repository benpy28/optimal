use proptest::prelude::*;

use crate::vectors::*;
use crate::{highest, is_set, vector_bit};

#[test]
fn isa_vectors() {
    assert_eq!(isa(0), 0x30);
    assert_eq!(isa(9), 0x39);
    assert_eq!(isa(15), 0x3F);
}

/// The map's promises: nothing on an exception vector, ranges in order and
/// disjoint, the spurious vector's low nibble all ones, and every local
/// APIC vector in a class above every device vector.
const _: () = {
    assert!(PIC_MASTER >= 0x20);
    assert!(PIC_SLAVE == PIC_MASTER + 8);
    assert!(PIC_SLAVE + 8 <= ISA_BASE);
    assert!(ISA_BASE + 16 <= DEVICE_FIRST);
    assert!(DEVICE_FIRST <= DEVICE_LAST && DEVICE_LAST < UNROUTED);
    assert!(UNROUTED != LAPIC_TIMER);
    assert!(SPURIOUS & 0xF == 0xF);
};

/// The map's promises (the constant ones are checked at compile time
/// above): nothing on an exception vector, ranges in order and disjoint,
/// the spurious vector's low nibble all ones, and every local APIC vector
/// in a class above every device vector.
#[test]
fn vector_map_is_consistent() {
    let local = [
        LAPIC_TIMER,
        LAPIC_THERMAL,
        LAPIC_PERF,
        LAPIC_CMCI,
        LAPIC_ERROR,
        SPURIOUS,
    ];
    for (i, &a) in local.iter().enumerate() {
        assert!(a >> 4 > DEVICE_LAST >> 4, "{a:#x}");
        for &b in &local[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// Vector 0x30 = 48 = register 1 (32..63), bit 16. 0xFF = register 7, bit 31.
#[test]
fn vector_bit_hand_worked() {
    assert_eq!(vector_bit(0), (0, 0));
    assert_eq!(vector_bit(0x30), (1, 16));
    assert_eq!(vector_bit(0x3F), (1, 31));
    assert_eq!(vector_bit(0x40), (2, 0));
    assert_eq!(vector_bit(0xFF), (7, 31));
}

#[test]
fn highest_hand_worked() {
    let mut r = [0u32; 8];
    assert_eq!(highest(&r), None);
    r[1] = 1 << 16;
    assert_eq!(highest(&r), Some(0x30));
    r[1] |= 1 << 17;
    assert_eq!(highest(&r), Some(0x31));
    r[0] = 1;
    assert_eq!(highest(&r), Some(0x31));
    r[7] = 1 << 31;
    assert_eq!(highest(&r), Some(0xFF));
    let mut z = [0u32; 8];
    z[0] = 1;
    assert_eq!(highest(&z), Some(0));
}

proptest! {
    #![proptest_config(super::config(5000))]

    /// highest and is_set agree with a bit-by-bit scan.
    #[test]
    fn highest_matches_scan(regs: [u32; 8]) {
        let naive = (0..=255u8).rev().find(|&v| regs[(v / 32) as usize] >> (v % 32) & 1 == 1);
        prop_assert_eq!(highest(&regs), naive);
        for v in 0..=255u8 {
            prop_assert_eq!(is_set(&regs, v), regs[(v / 32) as usize] >> (v % 32) & 1 == 1);
        }
    }
}
