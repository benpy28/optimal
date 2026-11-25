use crate::cpuid::*;
use crate::freq::Freq;

#[test]
fn tsc_bits_one_at_a_time() {
    let none = Tsc {
        present: false,
        invariant: false,
        deadline: false,
    };
    assert_eq!(Tsc::decode(0, 0, None), none);
    assert_eq!(Tsc::decode(0, 0, Some(0)), none);
    assert_eq!(
        Tsc::decode(0, 1 << 4, None),
        Tsc {
            present: true,
            ..none
        }
    );
    assert_eq!(
        Tsc::decode(0, 0, Some(1 << 8)),
        Tsc {
            invariant: true,
            ..none
        }
    );
    assert_eq!(
        Tsc::decode(1 << 24, 0, None),
        Tsc {
            deadline: true,
            ..none
        }
    );
    // Neighbouring bits mean nothing here.
    assert_eq!(Tsc::decode(!(1 << 24), !(1 << 4), Some(!(1 << 8))), none);
    assert_eq!(
        Tsc::decode(u32::MAX, u32::MAX, Some(u32::MAX)),
        Tsc {
            present: true,
            invariant: true,
            deadline: true
        }
    );
}

#[test]
fn crystal_hand_worked() {
    // A 24 MHz crystal with TSC/crystal = 250/2: 3 GHz exactly.
    let c = crystal(2, 250, 24_000_000).unwrap();
    assert_eq!(c.freq, Freq::hz(3_000_000_000).unwrap());
    assert_eq!(c.source, Source::Crystal);
    assert_eq!(c.resolution_ppb, 0);
    // 25 MHz with 3:184 is 1,533,333,333.33... Hz, kept exact.
    let c = crystal(3, 184, 25_000_000).unwrap();
    assert_eq!((c.freq.num(), c.freq.den()), (4_600_000_000, 3));
    // Not enumerated.
    assert_eq!(crystal(0, 250, 24_000_000), None);
    assert_eq!(crystal(2, 0, 24_000_000), None);
    assert_eq!(crystal(2, 250, 0), None);
    // Absurd: 4 GHz crystal times 2^32 - 1.
    assert_eq!(crystal(1, u32::MAX, 4_000_000_000), None);
}

#[test]
fn hypervisor_hand_worked() {
    // 2,995,200 kHz: half a kHz in 2.9952 GHz is 166.93 ppb, so 167.
    let c = hypervisor(2_995_200).unwrap();
    assert_eq!(c.freq, Freq::hz(2_995_200_000).unwrap());
    assert_eq!(c.source, Source::Hypervisor);
    assert_eq!(c.resolution_ppb, 167);
    assert_eq!(hypervisor(0), None);
    // 1 kHz: half a kHz is half of it.
    assert_eq!(hypervisor(1).unwrap().resolution_ppb, 500_000_000);
    // u32::MAX kHz is 4.3 THz: past 2^40 Hz.
    assert_eq!(hypervisor(u32::MAX), None);
}

#[test]
fn base_frequency_hand_worked() {
    // 3000 MHz: half a MHz is 166,666.67 ppb, so 166,667.
    let c = base_frequency(3000).unwrap();
    assert_eq!(c.freq, Freq::hz(3_000_000_000).unwrap());
    assert_eq!(c.source, Source::BaseFrequency);
    assert_eq!(c.resolution_ppb, 166_667);
    // Only bits 15:0 count.
    assert_eq!(base_frequency(0xABCD_0000 | 3000), Some(c));
    assert_eq!(base_frequency(0xFFFF_0000), None);
    assert_eq!(base_frequency(0), None);
}

#[test]
fn best_prefers_exact() {
    let a = crystal(2, 250, 24_000_000);
    let b = hypervisor(2_995_200);
    let c = base_frequency(3000);
    assert_eq!(best(a, b, c), a);
    assert_eq!(best(None, b, c), b);
    assert_eq!(best(None, None, c), c);
    assert_eq!(best(None, None, None), None);
    assert_eq!(best(a, None, None), a);
}
