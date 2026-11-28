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

/// QEMU's q35 comparators: route capability 0x00FF0104 (inputs 2, 8 and
/// 16 to 23), 64-bit and periodic capable (bits 4 and 5: 0x30). Level
/// one-shot on input 16 with the interrupt on: 0x30 | 16 << 9 (0x2000) |
/// level 0x2 | enable 0x4 = 0x2036, capability kept.
#[test]
fn level_one_shot_hand_worked() {
    let q35 = (0x00FF_0104u64 << 32) | T_PERIODIC_CAPABLE | T_64BIT_CAPABLE;
    assert_eq!(route_cap(q35), 0x00FF_0104);
    assert_eq!(
        level_one_shot(q35, 16, true),
        Some((0x00FF_0104 << 32) | 0x2036)
    );
    assert_eq!(
        level_one_shot(q35, 16, false),
        Some((0x00FF_0104 << 32) | 0x2032)
    );
    // Input 2 is 2 << 9 = 0x400.
    assert_eq!(
        level_one_shot(q35, 2, true),
        Some((0x00FF_0104 << 32) | 0x436)
    );
    // Not in the capability, or past input 31.
    assert_eq!(level_one_shot(q35, 3, true), None);
    assert_eq!(level_one_shot(q35, 24, true), None);
    assert_eq!(level_one_shot(u64::MAX, 32, true), None);
    // Firmware left it periodic, by FSB, 32-bit forced, on input 8 with
    // VAL_SET: all of that goes, the new route replaces the old.
    let dirty =
        q35 | T_PERIODIC | T_FSB_ENABLE | T_32BIT_MODE | T_VAL_SET | (8 << 9) | T_INT_ENABLE;
    assert_eq!(
        level_one_shot(dirty, 17, false),
        Some((0x00FF_0104 << 32) | 0x30 | 17 << 9 | 0x2)
    );
    assert_eq!((T_ROUTE_SHIFT, T_ROUTE), (9, 0x3E00));
}

#[test]
fn deadline_hand_worked() {
    let wide = T_64BIT_CAPABLE;
    assert_eq!(deadline(1000, 100_000, wide), 101_000);
    assert_eq!(deadline(u64::MAX - 9, 20, wide), 10);
    // A 32-bit comparator: 0x1_FFFF_FFF0 + 0x20 = 0x2_0000_0010, low half 0x10.
    assert_eq!(deadline(0x1_FFFF_FFF0, 0x20, 0), 0x10);
    assert_eq!(deadline(5, 7, 0), 12);
}

#[test]
fn passed_hand_worked() {
    let wide = T_64BIT_CAPABLE;
    assert!(passed(100, 100, wide));
    assert!(passed(101, 100, wide));
    assert!(!passed(99, 100, wide));
    // Across the 64-bit wrap: the deadline 10 is 20 ticks after
    // 2^64 - 10, and passed once the counter reaches it.
    assert!(!passed(u64::MAX - 9, 10, wide));
    assert!(passed(10, u64::MAX - 9, wide));
    // A 32-bit comparator: only the low halves count, so 0x1_0000_0005
    // has passed 0x3 and not 0x7.
    assert!(passed(0x1_0000_0005, 0x3, 0));
    assert!(!passed(0x1_0000_0005, 0x7, 0));
    assert!(passed(0x5, 0xFFFF_FFF0, 0));
    assert!(!passed(0xFFFF_FFF0, 0x5, 0));
    // In 64-bit mode the high half counts.
    assert!(!passed(0x1_0000_0005, 0x2_0000_0003, wide));
}

#[test]
fn pick_level_hand_worked() {
    let q35 = 0x00FF_0104u64 << 32;
    let pc = 0x0000_0004u64 << 32;
    // On q35 with inputs 2 and 8 taken by ISA IRQs: comparator 2, input 16.
    let free = |g: u32| g != 2 && g != 8;
    assert_eq!(pick_level(&[q35, q35, q35], free), Some((2, 16)));
    // On pc only input 2 exists, and it is taken.
    assert_eq!(pick_level(&[pc, pc, pc], free), None);
    // A later comparator that reaches nothing free is passed over.
    assert_eq!(pick_level(&[q35, q35, pc], free), Some((1, 16)));
    assert_eq!(pick_level(&[], free), None);
    assert_eq!(pick_level(&[pc], |_| true), Some((0, 2)));
}

proptest! {
    #![proptest_config(config(5000))]

    /// level_one_shot changes only the bits it owns, and only when the
    /// route is allowed.
    #[test]
    fn level_one_shot_bits(raw: u64, gsi in 0u32..40, enabled: bool) {
        let got = level_one_shot(raw, gsi, enabled);
        if gsi < 32 && (raw >> 32) & (1 << gsi) != 0 {
            let owned = T_ROUTE | T_32BIT_MODE | T_LEVEL | T_INT_ENABLE | T_PERIODIC | T_FSB_ENABLE | T_VAL_SET;
            let v = got.unwrap();
            prop_assert_eq!(v & !owned, raw & !owned);
            prop_assert_eq!((v & T_ROUTE) >> 9, u64::from(gsi));
            prop_assert_eq!(v & (T_32BIT_MODE | T_PERIODIC | T_FSB_ENABLE | T_VAL_SET), 0);
            prop_assert_eq!(v & T_LEVEL, T_LEVEL);
            prop_assert_eq!(v & T_INT_ENABLE != 0, enabled);
        } else {
            prop_assert_eq!(got, None);
        }
    }

    /// A deadline `ticks` ahead (under half the width) has not passed
    /// before then and has from then on.
    #[test]
    fn deadline_then_passed(now: u64, ticks in 1u64..(1 << 31), wide: bool, late in 0u64..(1 << 30)) {
        let c = if wide { T_64BIT_CAPABLE } else { 0 };
        let d = deadline(now, ticks, c);
        prop_assert!(!passed(now, d, c));
        prop_assert!(!passed(now.wrapping_add(ticks - 1), d, c));
        prop_assert!(passed(now.wrapping_add(ticks), d, c));
        prop_assert!(passed(now.wrapping_add(ticks + late), d, c));
    }

    /// pick_level against a brute-force search.
    #[test]
    fn pick_level_matches_search(caps in proptest::collection::vec(any::<u32>(), 0..6), taken: u32) {
        let configs: Vec<u64> = caps.iter().map(|&c| u64::from(c) << 32).collect();
        let free = |g: u32| taken & (1 << g) == 0;
        let mut want = None;
        for n in (0..caps.len()).rev() {
            if let Some(g) = (0..32).find(|&g| caps[n] & (1 << g) != 0 && free(g)) {
                want = Some((n as u8, g));
                break;
            }
        }
        prop_assert_eq!(pick_level(&configs, free), want);
    }
}

#[test]
fn fsb_hand_worked() {
    // Comparator 2's FSB route register: 0x110 + 0x40 = 0x150.
    assert_eq!(timer_fsb_route(0), 0x110);
    assert_eq!(timer_fsb_route(2), 0x150);
    assert_eq!(timer_fsb_route(31), 0x110 + 0x20 * 31);
    // The message to APIC 0 on vector 0xED: address high, data low.
    assert_eq!(fsb_route(0xFEE0_0000, 0xED), 0xFEE0_0000_0000_00ED);
    assert_eq!(fsb_route(0, 0), 0);
    assert_eq!(fsb_route(u32::MAX, 0), 0xFFFF_FFFF_0000_0000);
    assert_eq!(fsb_route(0, u32::MAX), 0x0000_0000_FFFF_FFFF);
    // QEMU's q35 comparator with msi=on: route cap 0x00ff0104, FSB cap
    // (bit 15), 64-bit capable (bit 5), periodic capable (bit 4) on
    // comparator 0 only. Firmware left it level, 32-bit forced, periodic,
    // enabled, on input 8: 0x8000 | 0x30 | 0x2 | 0x100 | 0x8 | 0x4 | 8 << 9
    // = 0x913E. The FSB one-shot keeps the caps and the route field and
    // sets FSB_EN (0x4000): 0x8000 | 0x30 | 8 << 9 (0x1000) | 0x4000 =
    // 0xD030, plus INT_ENB (0x4) when enabled.
    let caps = 0x00FF_0104u64 << 32;
    let fw = caps | 0x913E;
    assert_eq!(
        fw & 0xFFFF,
        0x8000 | 0x30 | 0x2 | 0x100 | 0x8 | 0x4 | 8 << 9
    );
    assert_eq!(fsb_one_shot(fw, false), Some(caps | 0xD030));
    assert_eq!(fsb_one_shot(fw, true), Some(caps | 0xD034));
    // Without the cap (msi=off), no FSB.
    assert_eq!(fsb_one_shot(fw & !T_FSB_CAPABLE, true), None);
    assert!(fsb_capable(T_FSB_CAPABLE));
    assert!(!fsb_capable(!T_FSB_CAPABLE));
}

#[test]
fn legacy_hand_worked() {
    // The same firmware leftovers as fsb_hand_worked, without the FSB cap:
    // 0x113E. Legacy one-shot clears level, 32-bit, periodic, VAL_SET and
    // FSB_EN and keeps the route field: 0x30 | 0x1000 = 0x1030, plus 0x4.
    let caps = 0x0000_0004u64 << 32;
    let fw = caps | 0x113E;
    assert_eq!(legacy_one_shot(fw, false), caps | 0x1030);
    assert_eq!(legacy_one_shot(fw, true), caps | 0x1034);
    // FSB_EN left on by firmware is cleared too.
    assert_eq!(
        legacy_one_shot(T_FSB_CAPABLE | T_FSB_ENABLE, true),
        T_FSB_CAPABLE | T_INT_ENABLE
    );
    // General configuration: ENABLE and LEG_RT_CNF set, the rest kept.
    assert_eq!(legacy_running(0), 0x3);
    assert_eq!(legacy_running(0x1), 0x3);
    assert_eq!(legacy_running(0xF0), 0xF3);
    // And running() turns it off again.
    assert_eq!(running(legacy_running(0xF0)), 0xF1);
}

#[test]
fn pick_hand_worked() {
    let q35 = 0x00FF_0104u64 << 32;
    let pc = 0x0000_0004u64 << 32;
    let fsb = T_FSB_CAPABLE;
    // Inputs 2 and 8 are ISA IRQs'.
    let free = |g: u32| g != 2 && g != 8;
    let all = Allowed::ALL;
    let no_fsb = Allowed { fsb: false, ..all };
    let legacy_only = Allowed {
        fsb: false,
        ioapic: false,
        legacy: true,
    };
    // q35 with msi=on: FSB on comparator 2.
    let q = [q35 | fsb, q35 | fsb, q35 | fsb];
    assert_eq!(pick(&q, 0, true, all, free), Some((2, Route::Fsb)));
    // Comparator 2 taken: comparator 1.
    assert_eq!(pick(&q, 0b100, true, all, free), Some((1, Route::Fsb)));
    // FSB ruled out: input 16 on comparator 2, as pick_level.
    assert_eq!(
        pick(&q, 0, true, no_fsb, free),
        Some((2, Route::IoApic(16)))
    );
    // A taken comparator is passed over for the I/O APIC route too.
    assert_eq!(
        pick(&q, 0b100, true, no_fsb, free),
        Some((1, Route::IoApic(16)))
    );
    // Legacy only: comparator 0.
    assert_eq!(
        pick(&q, 0, true, legacy_only, free),
        Some((0, Route::Legacy))
    );
    // pc with msi=off: input 2 only, and it is taken, so legacy.
    let p = [pc, pc, pc];
    assert_eq!(pick(&p, 0, true, all, free), Some((0, Route::Legacy)));
    // ... unless the HPET lacks LEG_RT_CAP, legacy is not allowed, or
    // comparator 0 or 1 is taken.
    assert_eq!(pick(&p, 0, false, all, free), None);
    assert_eq!(
        pick(
            &p,
            0,
            true,
            Allowed {
                legacy: false,
                ..all
            },
            free
        ),
        None
    );
    assert_eq!(pick(&p, 0b01, true, all, free), None);
    assert_eq!(pick(&p, 0b10, true, all, free), None);
    assert_eq!(pick(&p, 0b100, true, all, free), Some((0, Route::Legacy)));
    // A single comparator is enough for legacy.
    assert_eq!(pick(&[pc], 0, true, all, free), Some((0, Route::Legacy)));
    // No comparators: nothing, even with LEG_RT_CAP.
    assert_eq!(pick(&[], 0, true, all, |_| true), None);
    // pc with msi=on: FSB on comparator 2.
    assert_eq!(
        pick(&[pc | fsb, pc | fsb, pc | fsb], 0, true, all, free),
        Some((2, Route::Fsb))
    );
    // Only comparator 0 has FSB: it still wins over the I/O APIC route.
    assert_eq!(
        pick(&[q35 | fsb, q35, q35], 0, true, all, free),
        Some((0, Route::Fsb))
    );
    // Nothing allowed: nothing.
    let none = Allowed {
        fsb: false,
        ioapic: false,
        legacy: false,
    };
    assert_eq!(pick(&q, 0, true, none, |_| true), None);
}

proptest! {
    #![proptest_config(config(5000))]

    /// fsb_one_shot changes only the bits it owns.
    #[test]
    fn fsb_one_shot_bits(raw: u64, enabled: bool) {
        match fsb_one_shot(raw, enabled) {
            Some(v) => {
                prop_assert!(raw & T_FSB_CAPABLE != 0);
                let owned = T_32BIT_MODE | T_LEVEL | T_INT_ENABLE | T_PERIODIC | T_FSB_ENABLE | T_VAL_SET;
                prop_assert_eq!(v & !owned, raw & !owned);
                prop_assert_eq!(v & (T_32BIT_MODE | T_LEVEL | T_PERIODIC | T_VAL_SET), 0);
                prop_assert_eq!(v & T_FSB_ENABLE, T_FSB_ENABLE);
                prop_assert_eq!(v & T_INT_ENABLE != 0, enabled);
            }
            None => prop_assert_eq!(raw & T_FSB_CAPABLE, 0),
        }
    }

    /// legacy_one_shot changes only the bits it owns.
    #[test]
    fn legacy_one_shot_bits(raw: u64, enabled: bool) {
        let v = legacy_one_shot(raw, enabled);
        let owned = T_32BIT_MODE | T_LEVEL | T_INT_ENABLE | T_PERIODIC | T_FSB_ENABLE | T_VAL_SET;
        prop_assert_eq!(v & !owned, raw & !owned);
        prop_assert_eq!(v & (T_32BIT_MODE | T_LEVEL | T_PERIODIC | T_VAL_SET | T_FSB_ENABLE), 0);
        prop_assert_eq!(v & T_INT_ENABLE != 0, enabled);
    }

    #[test]
    fn legacy_running_bits(raw: u64) {
        let v = legacy_running(raw);
        prop_assert_eq!(v, raw | ENABLE | LEGACY_ROUTE);
        prop_assert_eq!(halted(v), raw & !(ENABLE | LEGACY_ROUTE));
    }

    #[test]
    fn fsb_route_halves(address: u32, data: u32) {
        let v = fsb_route(address, data);
        prop_assert_eq!(v as u32, data);
        prop_assert_eq!((v >> 32) as u32, address);
    }

    /// pick against a brute-force search in the documented order.
    #[test]
    fn pick_matches_search(
        comps in proptest::collection::vec((any::<u32>(), any::<bool>()), 0..6),
        taken in 0u32..64,
        used: u32,
        legacy_capable: bool,
        a in (any::<bool>(), any::<bool>(), any::<bool>()),
    ) {
        let allowed = Allowed { fsb: a.0, ioapic: a.1, legacy: a.2 };
        let configs: Vec<u64> = comps
            .iter()
            .map(|&(cap, f)| u64::from(cap) << 32 | if f { T_FSB_CAPABLE } else { 0 })
            .collect();
        let free = |g: u32| used & (1 << g) == 0;
        let open = |n: usize| taken & (1 << n) == 0;
        let mut want = None;
        if allowed.fsb {
            for n in (0..comps.len()).rev() {
                if open(n) && comps[n].1 {
                    want = Some((n as u8, Route::Fsb));
                    break;
                }
            }
        }
        if want.is_none() && allowed.ioapic {
            'outer: for n in (0..comps.len()).rev() {
                if !open(n) {
                    continue;
                }
                for g in 0..32 {
                    if comps[n].0 & (1 << g) != 0 && free(g) {
                        want = Some((n as u8, Route::IoApic(g)));
                        break 'outer;
                    }
                }
            }
        }
        if want.is_none() && allowed.legacy && legacy_capable && !comps.is_empty() && open(0) && open(1) {
            want = Some((0, Route::Legacy));
        }
        prop_assert_eq!(pick(&configs, taken, legacy_capable, allowed, free), want);
    }
}
