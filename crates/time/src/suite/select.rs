use crate::select::*;

#[test]
fn reference_every_case() {
    use HpetCheck::*;
    use Reference::*;
    for hpet in [false, true] {
        for pm in [false, true] {
            for pit in [false, true] {
                for check in [NotChecked, Agrees, Disagrees(1), Disagrees(-1)] {
                    let want = if hpet && !matches!(check, Disagrees(_)) {
                        Some(Hpet)
                    } else if pm {
                        Some(PmTimer)
                    } else if pit {
                        Some(Pit)
                    } else {
                        None
                    };
                    assert_eq!(
                        reference(hpet, pm, pit, check),
                        want,
                        "{hpet} {pm} {pit} {check:?}"
                    );
                }
            }
        }
    }
    // Spelled out: an HPET that disagrees with the PM timer loses to it.
    assert_eq!(
        reference(true, true, true, Disagrees(600_000)),
        Some(PmTimer)
    );
    assert_eq!(reference(true, true, true, Agrees), Some(Hpet));
    assert_eq!(reference(false, false, true, NotChecked), Some(Pit));
}

#[test]
fn clock_every_case() {
    use Clock::*;
    // (calibrated, invariant, hpet, pm) -> clock
    let table = [
        ((true, true, true, true), Some(Tsc)),
        ((true, true, false, false), Some(Tsc)),
        ((true, false, true, true), Some(Hpet)),
        ((true, false, false, true), Some(PmTimer)),
        ((true, false, false, false), Some(TscNotInvariant)),
        ((false, true, true, false), Some(Hpet)),
        ((false, true, false, true), Some(PmTimer)),
        ((false, true, false, false), None),
        ((false, false, false, false), None),
    ];
    for ((c, i, h, p), want) in table {
        assert_eq!(clock(c, i, h, p), want, "{c} {i} {h} {p}");
    }
    for bits in 0..16u8 {
        let (c, i, h, p) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
        let want = if c && i {
            Some(Tsc)
        } else if h {
            Some(Hpet)
        } else if p {
            Some(PmTimer)
        } else if c {
            Some(TscNotInvariant)
        } else {
            None
        };
        assert_eq!(clock(c, i, h, p), want);
    }
}

fn sources(bits: u8) -> Sources {
    Sources {
        deadline_mode: bits & 1 != 0,
        tsc_calibrated: bits & 2 != 0,
        tsc_invariant: bits & 4 != 0,
        lapic_calibrated: bits & 8 != 0,
        hpet: bits & 16 != 0,
        pit: bits & 32 != 0,
    }
}

#[test]
fn events_every_case() {
    use Events::*;
    // All 64 combinations against the rule written out independently.
    for bits in 0..64u8 {
        let s = sources(bits);
        let want = if s.deadline_mode && s.tsc_calibrated && s.tsc_invariant {
            Some(TscDeadline)
        } else if s.lapic_calibrated {
            Some(LapicOneShot)
        } else if s.hpet {
            Some(HpetOneShot)
        } else if s.pit {
            Some(PitOneShot)
        } else {
            None
        };
        assert_eq!(events(s), want, "{s:?}");
    }
}

#[test]
fn events_hand_worked() {
    use Events::*;
    let none = Sources::default();
    // A TSC that is not invariant never drives deadlines.
    let s = Sources {
        deadline_mode: true,
        tsc_calibrated: true,
        lapic_calibrated: true,
        ..none
    };
    assert_eq!(events(s), Some(LapicOneShot));
    let s = Sources {
        deadline_mode: true,
        tsc_calibrated: true,
        ..none
    };
    assert_eq!(events(s), None);
    let s = Sources {
        deadline_mode: true,
        tsc_calibrated: true,
        tsc_invariant: true,
        ..none
    };
    assert_eq!(events(s), Some(TscDeadline));
    // The fallbacks, in order: LAPIC one-shot beats the HPET, which
    // beats the PIT.
    let all_three = Sources {
        lapic_calibrated: true,
        hpet: true,
        pit: true,
        ..none
    };
    assert_eq!(events(all_three), Some(LapicOneShot));
    let s = Sources {
        lapic_calibrated: false,
        ..all_three
    };
    assert_eq!(events(s), Some(HpetOneShot));
    let s = Sources { hpet: false, ..s };
    assert_eq!(events(s), Some(PitOneShot));
    assert_eq!(events(none), None);
    // An invariant TSC without TSC-deadline mode and nothing else.
    let s = Sources {
        tsc_calibrated: true,
        tsc_invariant: true,
        pit: true,
        ..none
    };
    assert_eq!(events(s), Some(PitOneShot));
}
