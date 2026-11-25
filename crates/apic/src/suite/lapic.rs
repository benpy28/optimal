use proptest::prelude::*;

use crate::lapic::*;
use crate::vectors;

/// Every x2APIC MSR, from the SDM's x2APIC register table (11.12.1.2).
const SDM_MSRS: &[u32] = &[
    0x802, 0x803, 0x808, 0x80A, 0x80B, 0x80D, 0x80F, // ID .. SVR
    0x810, 0x811, 0x812, 0x813, 0x814, 0x815, 0x816, 0x817, // ISR
    0x818, 0x819, 0x81A, 0x81B, 0x81C, 0x81D, 0x81E, 0x81F, // TMR
    0x820, 0x821, 0x822, 0x823, 0x824, 0x825, 0x826, 0x827, // IRR
    0x828, 0x82F, 0x830, // ESR, CMCI, ICR
    0x832, 0x833, 0x834, 0x835, 0x836, 0x837, // LVTs
    0x838, 0x839, 0x83E, 0x83F, // timer counts, divide, SELF IPI
];

#[test]
fn x2apic_msrs_hand_worked() {
    assert_eq!(x2apic_msr(ID), Some(0x802));
    assert_eq!(x2apic_msr(EOI), Some(0x80B));
    assert_eq!(x2apic_msr(SVR), Some(0x80F));
    assert_eq!(x2apic_msr(ISR + 0x70), Some(0x817));
    assert_eq!(x2apic_msr(ICR_LOW), Some(0x830));
    assert_eq!(x2apic_msr(LVT_LINT1), Some(0x836));
    assert_eq!(x2apic_msr(SELF_IPI), Some(0x83F));
    for gone in [DFR, APR, RRD, ICR_HIGH, 0x10, 0x40, 0x108, 0x290, 0x3A0] {
        assert_eq!(x2apic_msr(gone), None, "{gone:#x}");
    }
}

/// Over every offset in the 4 KiB page, the MSRs produced are exactly the
/// SDM's list, each from one offset, and xAPIC has every one of those
/// registers except SELF IPI, plus DFR, APR, RRD and ICR high.
#[test]
fn x2apic_msrs_match_the_sdm() {
    let mut seen = Vec::new();
    for off in 0..0x1000u16 {
        if let Some(m) = x2apic_msr(off) {
            assert_eq!(m, 0x800 + u32::from(off >> 4));
            assert_eq!(off % 16, 0);
            seen.push(m);
        }
    }
    assert_eq!(seen, SDM_MSRS);
    let xapic: Vec<u16> = (0..0x1000u16).filter(|&o| xapic_register(o)).collect();
    let mut want: Vec<u16> = SDM_MSRS
        .iter()
        .map(|m| ((m - 0x800) << 4) as u16)
        .filter(|&o| o != SELF_IPI)
        .chain([DFR, APR, RRD, ICR_HIGH])
        .collect();
    want.sort();
    assert_eq!(xapic, want);
}

#[test]
fn id_by_mode() {
    assert_eq!(id(0x0300_0000, false), 3);
    assert_eq!(id(0x0000_0103, true), 0x103);
    assert_eq!(id(0xFF00_0000, false), 0xFF);
}

/// QEMU's local APIC version register reads 0x00050014: version 0x14,
/// max LVT 5 (six entries: timer, thermal, perf, LINT0, LINT1, error).
#[test]
fn version_hand_worked() {
    let v = Version::decode(0x0005_0014);
    assert_eq!(
        v,
        Version {
            version: 0x14,
            max_lvt: 5,
            eoi_broadcast_suppression: false
        }
    );
    assert!(v.integrated());
    assert!(v.has_lvt(LVT_THERMAL) && v.has_lvt(LVT_PERF) && !v.has_lvt(LVT_CMCI));
    let v = Version::decode(0x0106_0015);
    assert!(v.eoi_broadcast_suppression && v.has_lvt(LVT_CMCI));
    let v = Version::decode(0x0004_0010);
    assert!(v.has_lvt(LVT_PERF) && !v.has_lvt(LVT_THERMAL));
    let v = Version::decode(0x0003_0010);
    assert!(!v.has_lvt(LVT_PERF));
    for r in [LVT_TIMER, LVT_LINT0, LVT_LINT1, LVT_ERROR] {
        assert!(v.has_lvt(r));
    }
    assert!(!v.has_lvt(SVR));
    assert!(!Version::decode(0x0002_0004).integrated());
    assert!(!Version::decode(0x0002_0024).integrated());
}

/// NMI is delivery mode 4 (bits 8-10): 4 << 8 = 0x400. ExtINT is 7: 0x700.
/// Mask is bit 16 (0x10000), level bit 15 (0x8000), active low bit 13 (0x2000).
#[test]
fn lvt_hand_worked() {
    let nmi = Lvt {
        vector: 0,
        delivery: Delivery::Nmi,
        active_low: false,
        level: false,
        masked: false,
    };
    assert_eq!(nmi.encode(), 0x400);
    assert_eq!(
        Lvt {
            active_low: true,
            ..nmi
        }
        .encode(),
        0x2400
    );
    assert_eq!(
        Lvt {
            delivery: Delivery::ExtInt,
            ..nmi
        }
        .encode(),
        0x700
    );
    assert_eq!(Lvt::masked(0xF0).encode(), 0x100F0);
    assert_eq!(
        Lvt {
            vector: 0x31,
            delivery: Delivery::Fixed,
            active_low: false,
            level: true,
            masked: true
        }
        .encode(),
        0x18031
    );
    assert_eq!(
        Lvt {
            delivery: Delivery::Smi,
            ..nmi
        }
        .encode(),
        0x200
    );
    assert_eq!(
        Lvt {
            delivery: Delivery::Init,
            ..nmi
        }
        .encode(),
        0x500
    );
    assert_eq!(Lvt::decode(0x1_0400).delivery, Delivery::Nmi);
    assert_eq!(Lvt::decode(0x100).delivery, Delivery::Reserved(1));
    assert_eq!(Lvt::decode(0x300).delivery, Delivery::Reserved(3));
    assert_eq!(Lvt::decode(0x600).delivery, Delivery::Reserved(6));
}

#[test]
fn svr_and_esr() {
    assert_eq!(svr(0xFF, true), 0x1FF);
    assert_eq!(svr(0xFF, false), 0xFF);
    assert!(svr_enabled(0x1EF) && !svr_enabled(0xEF));
    assert_eq!(esr_errors(0).count(), 0);
    assert_eq!(
        esr_errors(0x41).collect::<Vec<_>>(),
        ["send checksum", "received illegal vector"]
    );
    assert_eq!(esr_errors(0xFFFF_FFFF).count(), 8);
    assert_eq!(
        esr_errors(0x80).collect::<Vec<_>>(),
        ["illegal register address"]
    );
}

/// A self IPI on 0x80: vector 0x80, assert (bit 14 = 0x4000), shorthand
/// self (1 << 18 = 0x40000): 0x44080. INIT level assert to APIC 3:
/// 5 << 8 | 0x4000 | 0x8000 = 0xC500, destination 3 << 24 in xAPIC.
#[test]
fn ipi_hand_worked() {
    let s = Ipi::to_self(0x80);
    assert_eq!(s.x2apic(), 0x44080);
    assert_eq!(s.xapic(), Some((0, 0x44080)));
    let init = Ipi {
        vector: 0,
        delivery: IpiDelivery::Init,
        logical: false,
        assert: true,
        level_triggered: true,
        shorthand: Shorthand::None,
        destination: 3,
    };
    assert_eq!(init.xapic(), Some((0x0300_0000, 0xC500)));
    assert_eq!(init.x2apic(), 0x3_0000_C500);
    let wide = Ipi {
        destination: 0x1_0000,
        ..init
    };
    assert_eq!(wide.xapic(), None);
    assert_eq!(wide.x2apic(), 0x1_0000_0000_C500);
    let all = Ipi {
        delivery: IpiDelivery::StartUp,
        vector: 0x08,
        level_triggered: false,
        logical: true,
        shorthand: Shorthand::AllExcludingSelf,
        ..init
    };
    // 0x08 | 6 << 8 | 1 << 11 | 1 << 14 | 3 << 18 = 0xC4E08.
    assert_eq!(all.x2apic() as u32, 0xC4E08);
    let modes = [
        (IpiDelivery::Fixed, 0),
        (IpiDelivery::LowestPriority, 1),
        (IpiDelivery::Smi, 2),
        (IpiDelivery::Nmi, 4),
        (IpiDelivery::Init, 5),
        (IpiDelivery::StartUp, 6),
    ];
    for (d, bits) in modes {
        let i = Ipi { delivery: d, ..s };
        assert_eq!((i.x2apic() >> 8) & 7, bits);
    }
    let shorts = [
        (Shorthand::None, 0),
        (Shorthand::ToSelf, 1),
        (Shorthand::AllIncludingSelf, 2),
        (Shorthand::AllExcludingSelf, 3),
    ];
    for (sh, bits) in shorts {
        let i = Ipi { shorthand: sh, ..s };
        assert_eq!((i.x2apic() >> 18) & 3, bits);
    }
    assert_eq!(ICR_PENDING, 0x1000);
    assert!(!legal_fixed_vector(15) && legal_fixed_vector(16) && legal_fixed_vector(0xFF));
}

/// Setup on QEMU's APIC (six LVTs, no CMCI), with LINT0 masked and LINT1
/// NMI as QEMU's MADT asks.
#[test]
fn setup_hand_worked() {
    let lint0 = Lvt::masked(vectors::UNROUTED);
    let lint1 = Lvt {
        vector: 0,
        delivery: Delivery::Nmi,
        active_low: false,
        level: false,
        masked: false,
    };
    assert_eq!(
        setup(Version::decode(0x0005_0014), lint0, lint1),
        [
            (SVR, 0x1FF),
            (LVT_TIMER, 0x100F0),
            (LVT_THERMAL, 0x100F1),
            (LVT_PERF, 0x100F2),
            (LVT_LINT0, 0x100EE),
            (LVT_LINT1, 0x400),
            (LVT_ERROR, 0xFE),
            (TIMER_INITIAL, 0),
            (ESR, 0),
            (ESR, 0),
            (TPR, 0),
        ]
    );
    let w = setup(Version::decode(0x0006_0015), lint0, lint1);
    assert!(w.contains(&(LVT_CMCI, 0x100F3)));
    // Four LVTs only: SVR, timer, LINT0, LINT1, error, count, ESR twice, TPR.
    let w = setup(Version::decode(0x0003_0014), lint0, lint1);
    assert_eq!(w.len(), 9);
}

proptest! {
    #![proptest_config(super::config(5000))]

    #[test]
    fn lvt_round_trip(vector: u8, d in 0u32..8, active_low: bool, level: bool, masked: bool) {
        let raw = u32::from(vector) | d << 8
            | u32::from(active_low) << 13 | u32::from(level) << 15 | u32::from(masked) << 16;
        let l = Lvt::decode(raw);
        prop_assert_eq!(l.encode(), raw);
        prop_assert_eq!(Lvt::decode(l.encode()), l);
        prop_assert_eq!((l.vector, l.active_low, l.level, l.masked), (vector, active_low, level, masked));
    }

    /// Decoding keeps exactly the bits an LVT entry defines here: vector,
    /// delivery, polarity, trigger, mask (0x1A7FF).
    #[test]
    fn lvt_decode_keeps_defined_bits(raw: u32) {
        prop_assert_eq!(Lvt::decode(raw).encode(), raw & 0x1_A7FF);
    }

    /// For any version: SVR comes first with the enable bit, each LVT the
    /// APIC has is written once (and no other), the error LVT is the only
    /// unmasked one besides the LINT pins, and TPR 0 comes last.
    #[test]
    fn setup_invariants(raw: u32, l0: u32, l1: u32) {
        let v = Version::decode(raw);
        let (l0, l1) = (Lvt::decode(l0), Lvt::decode(l1));
        let w = setup(v, l0, l1);
        prop_assert_eq!(w[0].0, SVR);
        prop_assert!(svr_enabled(w[0].1));
        prop_assert_eq!(w[0].1 & 0xFF, u32::from(vectors::SPURIOUS));
        prop_assert_eq!(*w.last().unwrap(), (TPR, 0));
        for reg in [LVT_TIMER, LVT_THERMAL, LVT_PERF, LVT_CMCI, LVT_LINT0, LVT_LINT1, LVT_ERROR] {
            let n = w.iter().filter(|x| x.0 == reg).count();
            prop_assert_eq!(n, usize::from(v.has_lvt(reg)));
        }
        for &(reg, val) in &w {
            match reg {
                LVT_LINT0 => prop_assert_eq!(val, l0.encode()),
                LVT_LINT1 => prop_assert_eq!(val, l1.encode()),
                LVT_ERROR => prop_assert_eq!(val, u32::from(vectors::LAPIC_ERROR)),
                LVT_TIMER | LVT_THERMAL | LVT_PERF | LVT_CMCI => {
                    prop_assert!(Lvt::decode(val).masked)
                }
                _ => {}
            }
        }
        let esr = w.iter().filter(|x| **x == (ESR, 0)).count();
        prop_assert_eq!(esr, 2);
        prop_assert!(w.contains(&(TIMER_INITIAL, 0)));
    }
}
