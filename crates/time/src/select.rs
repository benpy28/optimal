//! Which counter to calibrate against, and which to keep time with.

/// A counter with a known frequency, to calibrate others against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    Hpet,
    PmTimer,
    Pit,
}

/// What the HPET-against-PM-timer check found, when both exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HpetCheck {
    /// Only one of them, or the check could not be made.
    NotChecked,
    Agrees,
    /// The PM timer, measured with the HPET, is off by this many ppb. The
    /// PM timer's frequency is fixed by the ACPI specification, while the
    /// HPET's comes from a register firmware can get wrong, so the HPET
    /// loses.
    Disagrees(i64),
}

/// The calibration reference: the HPET (fastest and finest) unless it
/// disagrees with the PM timer, then the PM timer, then the PIT.
pub fn reference(hpet: bool, pm: bool, pit: bool, check: HpetCheck) -> Option<Reference> {
    if hpet && !matches!(check, HpetCheck::Disagrees(_)) {
        Some(Reference::Hpet)
    } else if pm {
        Some(Reference::PmTimer)
    } else if pit {
        Some(Reference::Pit)
    } else {
        None
    }
}

/// The clock the kernel keeps time with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// Invariant and calibrated: the fastest to read (no I/O at all).
    Tsc,
    Hpet,
    PmTimer,
    /// The TSC as a last resort: calibrated but not invariant, so it may
    /// drift or stop with CPU power states. Reported as a warning.
    TscNotInvariant,
}

/// The TSC when it is invariant and calibrated; otherwise the HPET (when
/// it is trusted), then the PM timer; otherwise a calibrated TSC that is
/// not invariant. The PIT cannot be a clock: it wraps every 55 ms and
/// takes three port accesses to read.
pub fn clock(tsc_calibrated: bool, tsc_invariant: bool, hpet: bool, pm: bool) -> Option<Clock> {
    if tsc_calibrated && tsc_invariant {
        Some(Clock::Tsc)
    } else if hpet {
        Some(Clock::Hpet)
    } else if pm {
        Some(Clock::PmTimer)
    } else if tsc_calibrated {
        Some(Clock::TscNotInvariant)
    } else {
        None
    }
}

/// What programs timer events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Events {
    /// The local APIC timer in TSC-deadline mode: armed with one MSR
    /// write, in TSC ticks, no division and no 32-bit count limit.
    TscDeadline,
    /// The local APIC timer counting down at its own calibrated rate.
    LapicOneShot,
    /// An HPET comparator in one-shot mode (crate::oneshot): armed with
    /// two MMIO reads and one write, and its interrupt comes through an
    /// I/O APIC pin.
    HpetOneShot,
    /// PIT channel 0 in mode 0 on ISA IRQ 0 (crate::oneshot): three port
    /// writes per arming, and at most 54.9 ms per arming.
    PitOneShot,
}

/// What the kernel has to program timer events with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// CPUID 01h ECX bit 24.
    pub deadline_mode: bool,
    /// The TSC's frequency is known.
    pub tsc_calibrated: bool,
    pub tsc_invariant: bool,
    /// The LAPIC timer's rate was measured.
    pub lapic_calibrated: bool,
    /// An HPET comparator is routed to a free I/O APIC input and its rate
    /// against the clock was measured.
    pub hpet: bool,
    /// ISA IRQ 0 is routed and the PIT's rate against the clock was
    /// measured.
    pub pit: bool,
}

/// TSC-deadline mode when the CPU has it and the TSC is invariant with a
/// known frequency; otherwise LAPIC one-shot when the LAPIC timer's rate
/// is known; otherwise an HPET comparator; otherwise the PIT.
///
/// A TSC that is not invariant can change rate with the CPU's power
/// state, while the LAPIC timer's bus or crystal clock does not, so then
/// the LAPIC timer's own count is the better measure. The LAPIC timer
/// beats the HPET because it is per CPU and armed with register writes
/// local to the CPU (an MSR write, or two in x2APIC mode), with no
/// interrupt routing; the HPET beats the PIT because one arming covers
/// seconds rather than 54.9 ms, at a finer tick (10 MHz or faster against
/// 1.19 MHz), with one MMIO write rather than three port writes.
/// (The kernel idles only with `hlt` so far, where the LAPIC timer keeps
/// running with or without ARAT; deeper C-states, when they come, must
/// add ARAT to this choice.)
pub fn events(s: Sources) -> Option<Events> {
    if s.deadline_mode && s.tsc_calibrated && s.tsc_invariant {
        Some(Events::TscDeadline)
    } else if s.lapic_calibrated {
        Some(Events::LapicOneShot)
    } else if s.hpet {
        Some(Events::HpetOneShot)
    } else if s.pit {
        Some(Events::PitOneShot)
    } else {
        None
    }
}
