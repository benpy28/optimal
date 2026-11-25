use proptest::prelude::*;

use super::config;
use crate::freq::Freq;
use crate::hpet::*;

/// QEMU's HPET: period 10^7 fs (100 MHz), vendor 0x8086, then 0xA201:
/// bit 15 legacy replacement, bit 13 64-bit counter, NUM_TIM_CAP = 2 (so
/// 3 comparators), REV_ID 1.
const QEMU: u64 = (10_000_000 << 32) | 0x8086_A201;

#[test]
fn qemu_hand_worked() {
    let c = Capabilities::decode(QEMU).unwrap();
    assert_eq!(
        c,
        Capabilities {
            revision: 1,
            comparators: 3,
            counter_64bit: true,
            legacy_replacement: true,
            vendor: 0x8086,
            period_fs: 10_000_000,
        }
    );
    assert_eq!(c.freq(), Freq::hz(100_000_000).unwrap());
    assert_eq!(c.counter_bits(), 64);
}

#[test]
fn ich_hand_worked() {
    // An Intel ICH: period 0x0429B17F fs (14.318 MHz), 0x8086, 0x0701:
    // 32-bit counter, no legacy capability bit, NUM_TIM_CAP 7 (8
    // comparators), REV_ID 1.
    let c = Capabilities::decode(0x0429_B17F_8086_0701).unwrap();
    assert_eq!(c.comparators, 8);
    assert!(!c.counter_64bit);
    assert!(!c.legacy_replacement);
    assert_eq!(c.counter_bits(), 32);
    assert_eq!(c.period_fs, 69_841_279);
    assert_eq!(c.freq().hz_rounded(), 14_318_180);
}

#[test]
fn fields_one_bit_at_a_time() {
    let base = (10_000_000u64 << 32) | 1;
    let d = |bits: u64| Capabilities::decode(base | bits).unwrap();
    assert_eq!(d(0).comparators, 1);
    assert_eq!(d(0x1f << 8).comparators, 32);
    assert_eq!(d(1 << 8).comparators, 2);
    assert!(d(1 << 13).counter_64bit && !d(1 << 13).legacy_replacement);
    assert!(d(1 << 15).legacy_replacement && !d(1 << 15).counter_64bit);
    // Bit 14 is reserved and changes nothing.
    assert_eq!(d(1 << 14), d(0));
    assert_eq!(d(0xFFFF << 16).vendor, 0xFFFF);
    assert_eq!(d(0xFE).revision, 0xFF);
}

#[test]
fn refusals() {
    let with = |period: u64, rev: u64| Capabilities::decode((period << 32) | rev);
    assert_eq!(Capabilities::decode(u64::MAX), Err(Bad::NothingThere));
    assert_eq!(with(10_000_000, 0), Err(Bad::Revision));
    assert_eq!(with(0, 1), Err(Bad::ZeroPeriod));
    assert_eq!(with(100_000_001, 1), Err(Bad::PeriodTooLong(100_000_001)));
    assert!(with(100_000_000, 1).is_ok());
    assert_eq!(with(909, 1), Err(Bad::PeriodTooShort(909)));
    assert_eq!(
        with(910, 1).unwrap().freq(),
        Freq::from_period_fs(910).unwrap()
    );
    // All ones except one bit is a real (if odd) register: decoded, then
    // refused for its period.
    assert_eq!(
        Capabilities::decode(u64::MAX - 1),
        Err(Bad::PeriodTooLong(u32::MAX))
    );
}

#[test]
fn table_check_hand_worked() {
    let c = Capabilities::decode(QEMU).unwrap();
    assert_eq!(check_table(c, 3, true), TableCheck::default());
    assert_eq!(
        check_table(c, 8, false),
        TableCheck {
            comparators: Some((8, 3)),
            counter_bits: Some((32, 64)),
        }
    );
    let ich = Capabilities::decode(0x0429_B17F_8086_0701).unwrap();
    assert_eq!(
        check_table(ich, 8, true),
        TableCheck {
            comparators: None,
            counter_bits: Some((64, 32)),
        }
    );
}

#[test]
fn bit_values() {
    // Specification 1.0a, sections 2.3.5 and 2.3.8.
    assert_eq!((ENABLE, LEGACY_ROUTE), (0x1, 0x2));
    assert_eq!(
        [
            T_LEVEL,
            T_INT_ENABLE,
            T_PERIODIC,
            T_PERIODIC_CAPABLE,
            T_64BIT_CAPABLE,
            T_VAL_SET,
            T_32BIT_MODE,
            T_FSB_ENABLE,
            T_FSB_CAPABLE
        ],
        [0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x100, 0x4000, 0x8000]
    );
}

#[test]
fn offsets() {
    assert_eq!(timer_config(0), 0x100);
    assert_eq!(timer_comparator(0), 0x108);
    assert_eq!(timer_config(2), 0x140);
    assert_eq!(timer_comparator(31), 0x4E8);
    assert_eq!(timer_config(31), 0x4E0);
}

#[test]
fn start_hand_worked() {
    // Firmware left the counter running with legacy replacement on, and
    // comparator 0 periodic with its interrupt on and VAL_SET, comparator 1
    // delivering by FSB. Capability bits (4, 5, 15 and the route mask)
    // and the route and level bits are written back as they were.
    let config = 0x3 | 0xF0;
    let t0 = T_INT_ENABLE
        | T_PERIODIC
        | T_PERIODIC_CAPABLE
        | T_64BIT_CAPABLE
        | T_VAL_SET
        | (2 << 9)
        | (0xF0_0000 << 32);
    let t1 = T_FSB_ENABLE | T_FSB_CAPABLE | T_LEVEL | T_32BIT_MODE;
    let w: Vec<_> = start(config, &[t0, t1]).collect();
    assert_eq!(
        w,
        vec![
            (CONFIG, 0xF0),
            (
                0x100,
                T_PERIODIC_CAPABLE | T_64BIT_CAPABLE | (2 << 9) | (0xF0_0000 << 32)
            ),
            (0x120, T_FSB_CAPABLE | T_LEVEL | T_32BIT_MODE),
            (CONFIG, 0xF1),
        ]
    );
    // No comparators: halt and run.
    assert_eq!(
        start(0, &[]).collect::<Vec<_>>(),
        vec![(CONFIG, 0), (CONFIG, 1)]
    );
}

proptest! {
    #![proptest_config(config(5000))]

    #[test]
    fn config_bits_only_touch_their_own(raw: u64) {
        prop_assert_eq!(halted(raw), raw & !3);
        prop_assert_eq!(running(raw), (raw & !3) | 1);
        prop_assert_eq!(quiet(raw), raw & !((1 << 2) | (1 << 3) | (1 << 6) | (1 << 14)));
    }

    #[test]
    fn decode_round_trip(rev in 1u8.., n in 0u8..32, wide: bool, legacy: bool, vendor: u16, period in 910u32..=100_000_000) {
        let raw = u64::from(rev)
            | u64::from(n) << 8
            | u64::from(wide) << 13
            | u64::from(legacy) << 15
            | u64::from(vendor) << 16
            | u64::from(period) << 32;
        let c = Capabilities::decode(raw).unwrap();
        prop_assert_eq!(c, Capabilities {
            revision: rev,
            comparators: n + 1,
            counter_64bit: wide,
            legacy_replacement: legacy,
            vendor,
            period_fs: period,
        });
        prop_assert_eq!(c.freq(), Freq::from_period_fs(u64::from(period)).unwrap());
    }

    #[test]
    fn decode_never_panics(raw: u64) {
        if let Ok(c) = Capabilities::decode(raw) {
            let _ = c.freq();
        }
    }
}
