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
