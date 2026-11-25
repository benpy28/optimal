use proptest::prelude::*;

use super::config;
use crate::counter::{Extend, delta, in_order, mask};

#[test]
fn hand_worked() {
    assert_eq!(mask(1), 1);
    assert_eq!(mask(16), 0xFFFF);
    assert_eq!(mask(24), 0xFF_FFFF);
    assert_eq!(mask(64), u64::MAX);
    // A 24-bit PM timer wrapping: 0xFFFFF0 to 0x000010 is 0x20 ticks.
    assert_eq!(delta(0xFF_FFF0, 0x10, 24), 0x20);
    // Bits above the width are ignored.
    assert_eq!(delta(0xAB00_0005, 0xCD00_0007, 24), 2);
    assert_eq!(delta(5, 5, 32), 0);
    assert_eq!(delta(u64::MAX, 0, 64), 1);
    // The 16-bit PIT counter.
    assert_eq!(delta(0xFFFF, 0x0001, 16), 2);
}

#[test]
#[should_panic]
fn zero_bits_is_a_bug() {
    let _ = mask(0);
}

#[test]
#[should_panic]
fn too_many_bits_is_a_bug() {
    let _ = mask(65);
}

#[test]
fn extend_hand_worked() {
    // 24 bits from 0xFFFFFE: +1, +2 (wrap), +0x1000.
    let mut e = Extend::new(24, 0xFF_FFFE);
    assert_eq!(e.update(0xFF_FFFF), 1);
    assert_eq!(e.update(0x00_0001), 3);
    assert_eq!(e.update(0x00_1001), 0x1003);
    assert_eq!(e.total(), 0x1003);
    assert_eq!(Extend::new(24, 5).total(), 0);
    // High junk bits in readings are ignored, the first one too.
    let mut e = Extend::new(16, 0xDEAD_0010);
    assert_eq!(e.update(0xBEEF_0012), 2);
}

#[test]
fn in_order_hand_worked() {
    assert!(in_order(10, 11, 12, 24, 1));
    assert!(!in_order(10, 11, 13, 24, 1));
    assert!(!in_order(10, 12, 13, 24, 1));
    // A glitch that reads back far ahead, or behind (which is a huge
    // step forward modulo the width).
    assert!(!in_order(10, 0x80_0000, 12, 24, 100));
    assert!(!in_order(10, 9, 12, 24, 100));
    // Across the wrap.
    assert!(in_order(0xFF_FFFF, 0, 1, 24, 1));
    assert!(in_order(7, 7, 7, 32, 0));
}

proptest! {
    #![proptest_config(config(2000))]

    #[test]
    fn extend_matches_a_wide_counter(
        bits in 1u32..=64,
        start: u64,
        steps in proptest::collection::vec(any::<u64>(), 1..64),
    ) {
        // A 64-bit true count, read through a `bits`-wide window, with each
        // step below one wrap.
        let mut truth = start;
        let mut e = Extend::new(bits, truth);
        let mut total = 0u64;
        for s in steps {
            let s = s & mask(bits);
            truth = truth.wrapping_add(s);
            total = total.wrapping_add(s);
            prop_assert_eq!(e.update(truth), total);
            prop_assert_eq!(e.total(), total);
        }
    }

    #[test]
    fn delta_inverts_adding(bits in 1u32..=64, then: u64, d: u64) {
        let d = d & mask(bits);
        prop_assert_eq!(delta(then, then.wrapping_add(d), bits), d);
    }
}
