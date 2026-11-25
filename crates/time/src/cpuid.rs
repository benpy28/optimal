//! What CPUID says about the time-stamp counter (Intel SDM Vol. 2A,
//! CPUID; Vol. 3B section 18.17).

use crate::freq::Freq;

/// The TSC features, from leaf 01h and leaf 80000007h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tsc {
    /// Leaf 01h EDX bit 4: there is a TSC.
    pub present: bool,
    /// Leaf 80000007h EDX bit 8: the TSC runs at a constant rate in every
    /// P-, C- and T-state, so it can be the clock.
    pub invariant: bool,
    /// Leaf 01h ECX bit 24: the local APIC timer has TSC-deadline mode.
    pub deadline: bool,
}

impl Tsc {
    /// `leaf1_ecx` and `leaf1_edx` from leaf 01h; `ext7_edx` from leaf
    /// 80000007h, or `None` when the highest extended leaf is below it.
    pub const fn decode(leaf1_ecx: u32, leaf1_edx: u32, ext7_edx: Option<u32>) -> Tsc {
        Tsc {
            present: leaf1_edx & (1 << 4) != 0,
            invariant: match ext7_edx {
                Some(edx) => edx & (1 << 8) != 0,
                None => false,
            },
            deadline: leaf1_ecx & (1 << 24) != 0,
        }
    }
}

/// Where a frequency CPUID gave came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Leaf 15h: crystal frequency times EBX / EAX. Exact.
    Crystal,
    /// Leaf 40000010h, the hypervisor timing leaf (VMware's, also given
    /// by KVM): a whole number of kHz.
    Hypervisor,
    /// Leaf 16h: the processor's base frequency in whole MHz. Not always
    /// the TSC's frequency; never trusted except as a last resort.
    BaseFrequency,
}

/// A TSC frequency CPUID gave, and how coarse it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub freq: Freq,
    pub source: Source,
    /// Half the unit it was given in, in ppb of `freq`, rounded up: how far
    /// the true value can be from it from rounding alone. 0 when exact.
    pub resolution_ppb: u64,
}

/// Half of `unit_hz`, in ppb of `hz`, rounded up.
fn half_unit_ppb(unit_hz: u64, hz: u64) -> u64 {
    // unit / 2 / hz * 10^9; unit <= 10^6 so the product fits in a u64.
    (unit_hz * 1_000_000_000).div_ceil(2 * hz)
}

/// Leaf 15h (EAX, EBX, ECX). `None` when it does not give the TSC
/// frequency: EAX or EBX is 0 (the ratio is not enumerated) or ECX is 0
/// (the crystal frequency is not enumerated; some processors document it
/// by model instead, which the kernel does not guess at).
pub fn crystal(eax: u32, ebx: u32, ecx: u32) -> Option<Claimed> {
    // ecx * ebx < 2^64; eax <= 2^32 - 1 is a valid denominator. Freq::new
    // refuses a zero numerator or denominator, which is every "not
    // enumerated" case.
    let freq = Freq::new(u64::from(ecx) * u64::from(ebx), u64::from(eax))?;
    Some(Claimed {
        freq,
        source: Source::Crystal,
        resolution_ppb: 0,
    })
}

/// Leaf 40000010h EAX, in kHz. `None` when it is 0. The caller checks that
/// the hypervisor's highest leaf (40000000h EAX) reaches 40000010h.
pub fn hypervisor(eax: u32) -> Option<Claimed> {
    // At most 2^32 * 1000 Hz, and Freq::hz refuses 0.
    let hz = u64::from(eax) * 1000;
    Some(Claimed {
        freq: Freq::hz(hz)?,
        source: Source::Hypervisor,
        resolution_ppb: half_unit_ppb(1000, hz),
    })
}

/// Leaf 16h EAX bits 15:0, in MHz. `None` when it is 0.
pub fn base_frequency(eax: u32) -> Option<Claimed> {
    let hz = u64::from(eax & 0xFFFF) * 1_000_000;
    Some(Claimed {
        freq: Freq::hz(hz)?,
        source: Source::BaseFrequency,
        resolution_ppb: half_unit_ppb(1_000_000, hz),
    })
}

/// The best claim: leaf 15h, then the hypervisor's, then leaf 16h.
pub fn best(
    crystal: Option<Claimed>,
    hypervisor: Option<Claimed>,
    base: Option<Claimed>,
) -> Option<Claimed> {
    crystal.or(hypervisor).or(base)
}
