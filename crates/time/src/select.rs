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
}

/// TSC-deadline mode when the CPU has it and the TSC is invariant with a
/// known frequency; otherwise one-shot when the LAPIC timer's rate is
/// known. A TSC that is not invariant can change rate with the CPU's
/// power state, while the LAPIC timer's bus or crystal clock does not, so
/// then the LAPIC timer's own count is the better measure. (ROADMAP 1c
/// falls back further to the HPET and then the PIT; those come with their
/// own step.)
pub fn events(
    deadline_mode: bool,
    tsc_calibrated: bool,
    tsc_invariant: bool,
    lapic_calibrated: bool,
) -> Option<Events> {
    if deadline_mode && tsc_calibrated && tsc_invariant {
        Some(Events::TscDeadline)
    } else if lapic_calibrated {
        Some(Events::LapicOneShot)
    } else {
        None
    }
}
