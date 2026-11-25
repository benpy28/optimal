use proptest::prelude::*;

use crate::base::*;

#[test]
fn mode_of_each_combination() {
    assert_eq!(Mode::of(0), Mode::Disabled);
    assert_eq!(Mode::of(EXTD), Mode::Invalid);
    assert_eq!(Mode::of(EN), Mode::XApic);
    assert_eq!(Mode::of(EN | EXTD), Mode::X2Apic);
    // Other bits do not matter.
    assert_eq!(Mode::of(0xFEE0_0000 | BSP | EN), Mode::XApic);
}

/// 0xFEE00900 is the reset value on a boot CPU: base 0xFEE00000, EN (bit
/// 11 = 0x800) and BSP (bit 8 = 0x100).
#[test]
fn address_and_bsp() {
    assert_eq!(address(0xFEE0_0900, 36), 0xFEE0_0000);
    assert!(is_bsp(0xFEE0_0900));
    assert!(!is_bsp(0xFEE0_0800));
    // Bits 36 to 39 set (0xF0 << 32): above a 36-bit width they are not
    // address bits; with a 40-bit width they are.
    let raw = 0xF0_FEE0_0900u64;
    assert_eq!(address(raw, 36), 0xFEE0_0000);
    assert_eq!(address(raw, 40), 0xF0_FEE0_0000);
    // A width below 32 or above 52 is clamped to what x86_64 allows.
    assert_eq!(address(raw, 0), 0xFEE0_0000);
    assert_eq!(address(u64::MAX, 63), (1 << 52) - 0x1000);
}

/// Hand-worked transitions from the reset value 0xFEE00900.
/// x2APIC sets EXTD (bit 10 = 0x400): 0xFEE00D00. Disabled clears EN: 0xFEE00100.
#[test]
fn transitions_hand_worked() {
    let w = |raw, want| transition(raw, want).unwrap().writes().to_vec();
    assert_eq!(w(0xFEE0_0900, Mode::XApic), Vec::<u64>::new());
    assert_eq!(w(0xFEE0_0900, Mode::X2Apic), [0xFEE0_0D00]);
    assert_eq!(w(0xFEE0_0100, Mode::XApic), [0xFEE0_0900]);
    assert_eq!(w(0xFEE0_0100, Mode::X2Apic), [0xFEE0_0900, 0xFEE0_0D00]);
    assert_eq!(w(0xFEE0_0D00, Mode::XApic), [0xFEE0_0100, 0xFEE0_0900]);
    assert_eq!(w(0xFEE0_0D00, Mode::Disabled), [0xFEE0_0100]);
    assert_eq!(w(0xFEE0_0500, Mode::XApic), [0xFEE0_0100, 0xFEE0_0900]);
    assert_eq!(
        w(0xFEE0_0500, Mode::X2Apic),
        [0xFEE0_0100, 0xFEE0_0900, 0xFEE0_0D00]
    );
    assert_eq!(w(0xFEE0_0500, Mode::Disabled), [0xFEE0_0100]);
    assert_eq!(transition(0xFEE0_0900, Mode::Invalid), None);
    assert!(transition(0xFEE0_0D00, Mode::XApic).unwrap().resets());
    assert!(!transition(0xFEE0_0900, Mode::X2Apic).unwrap().resets());
    assert!(!transition(0xFEE0_0100, Mode::X2Apic).unwrap().resets());
}

#[test]
fn choose_every_case() {
    use Mode::*;
    for opt_out in [false, true] {
        for cpu in [false, true] {
            assert_eq!(
                choose(X2Apic, cpu, opt_out),
                (X2Apic, Reason::FirmwareX2Apic)
            );
        }
    }
    for current in [Disabled, Invalid, XApic] {
        assert_eq!(choose(current, false, false), (XApic, Reason::Unsupported));
        assert_eq!(choose(current, false, true), (XApic, Reason::Unsupported));
        assert_eq!(choose(current, true, true), (XApic, Reason::FirmwareOptOut));
        assert_eq!(choose(current, true, false), (X2Apic, Reason::Supported));
    }
    assert_eq!(Reason::FirmwareX2Apic.name(), "firmware left it on");
    assert_eq!(Reason::Supported.name(), "CPU supports it");
    assert_eq!(Reason::Unsupported.name(), "CPU has no x2APIC");
    assert_eq!(
        Reason::FirmwareOptOut.name(),
        "firmware opted out of x2APIC (DMAR)"
    );
    assert_eq!(Mode::X2Apic.name(), "x2APIC");
    assert_eq!(Mode::XApic.name(), "xAPIC");
    assert_eq!(Mode::Disabled.name(), "disabled");
    assert!(Mode::Invalid.name().starts_with("invalid"));
}

/// The SDM's legal writes, as a graph: the reference model.
fn legal(from: Mode, to: Mode) -> bool {
    use Mode::*;
    matches!(
        (from, to),
        (Disabled, XApic) | (XApic, X2Apic) | (XApic, Disabled) | (X2Apic, Disabled)
        // Leaving the invalid state (which can only be seen, never set) for
        // disabled is the way out of it.
        | (Invalid, Disabled)
    )
}

/// Shortest legal path length, by breadth-first search over four states.
fn shortest(from: Mode, to: Mode) -> usize {
    use Mode::*;
    let all = [Disabled, Invalid, XApic, X2Apic];
    let mut frontier = vec![from];
    for steps in 0..5 {
        if frontier.contains(&to) {
            return steps;
        }
        frontier = all
            .iter()
            .copied()
            .filter(|&m| frontier.iter().any(|&f| legal(f, m)))
            .collect();
    }
    unreachable!()
}

proptest! {
    #![proptest_config(super::config(5000))]

    /// Every write is a legal step, the last lands on the wanted mode, the
    /// path is as short as the SDM allows, and no other bit ever changes.
    #[test]
    fn transitions_follow_the_sdm(raw: u64, want in 0usize..3) {
        let want = [Mode::Disabled, Mode::XApic, Mode::X2Apic][want];
        let t = transition(raw, want).unwrap();
        let mut now = Mode::of(raw);
        for &w in t.writes() {
            prop_assert!(legal(now, Mode::of(w)), "{:?} -> {:?}", now, Mode::of(w));
            prop_assert_eq!(w & !(EN | EXTD), raw & !(EN | EXTD));
            now = Mode::of(w);
        }
        prop_assert_eq!(now, want);
        prop_assert_eq!(t.writes().len(), shortest(Mode::of(raw), want));
        prop_assert_eq!(t.resets(), t.writes().iter().any(|&w| Mode::of(w) == Mode::Disabled));
    }
}
