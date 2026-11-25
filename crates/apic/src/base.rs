//! IA32_APIC_BASE (MSR 0x1B): where the local APIC lives and which mode it
//! is in (Intel SDM Vol. 3A 11.4.4 and 11.12.1, x2APIC state transitions in
//! 11.12.5).
//!
//! The mode is two bits, EN (11) and EXTD (10):
//!
//! | EN | EXTD | mode                                |
//! |----|------|-------------------------------------|
//! | 0  | 0    | disabled (globally)                 |
//! | 0  | 1    | invalid: writing it faults (#GP)    |
//! | 1  | 0    | xAPIC: registers are MMIO at base   |
//! | 1  | 1    | x2APIC: registers are MSRs 0x800+   |
//!
//! Legal writes: disabled to xAPIC, xAPIC to x2APIC, and either enabled
//! mode to disabled. x2APIC straight to xAPIC faults, so it goes through
//! disabled, which resets the local APIC. Disabled straight to x2APIC is
//! avoided too: the SDM's state diagram only shows the way through xAPIC.

pub const MSR: u32 = 0x1B;
pub const BSP: u64 = 1 << 8;
pub const EXTD: u64 = 1 << 10;
pub const EN: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Disabled,
    Invalid,
    XApic,
    X2Apic,
}

impl Mode {
    pub fn of(raw: u64) -> Mode {
        match (raw & EN != 0, raw & EXTD != 0) {
            (false, false) => Mode::Disabled,
            (false, true) => Mode::Invalid,
            (true, false) => Mode::XApic,
            (true, true) => Mode::X2Apic,
        }
    }

    fn bits(self) -> u64 {
        match self {
            Mode::Disabled => 0,
            Mode::Invalid => EXTD,
            Mode::XApic => EN,
            Mode::X2Apic => EN | EXTD,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Disabled => "disabled",
            Mode::Invalid => "invalid (EN clear, EXTD set)",
            Mode::XApic => "xAPIC",
            Mode::X2Apic => "x2APIC",
        }
    }
}

/// The xAPIC register page's physical address: bits 12 up to the CPU's
/// physical address width (`maxphyaddr`, CPUID 0x80000008 EAX[7:0]).
pub fn address(raw: u64, maxphyaddr: u8) -> u64 {
    let width = u32::from(maxphyaddr.clamp(32, 52));
    raw & ((1u64 << width) - 1) & !0xFFF
}

pub fn is_bsp(raw: u64) -> bool {
    raw & BSP != 0
}

/// The writes that take IA32_APIC_BASE from `raw` to mode `want`, in
/// order: none if it is already there, at most two otherwise. Every other
/// bit (the base address, BSP) is kept as read. Asking for `Invalid` is
/// refused (None).
pub fn transition(raw: u64, want: Mode) -> Option<Transition> {
    if want == Mode::Invalid {
        return None;
    }
    let keep = raw & !(EN | EXTD);
    let at = |m: Mode| keep | m.bits();
    let mut t = Transition::default();
    let mut now = Mode::of(raw);
    // Leaving x2APIC for xAPIC, or an invalid state for anything, goes
    // through disabled.
    if now == Mode::Invalid || (now == Mode::X2Apic && want == Mode::XApic) {
        t.push(at(Mode::Disabled));
        now = Mode::Disabled;
    }
    match (now, want) {
        (a, b) if a == b => {}
        (_, Mode::Disabled) => t.push(at(Mode::Disabled)),
        (Mode::Disabled, Mode::XApic) => t.push(at(Mode::XApic)),
        (Mode::Disabled, Mode::X2Apic) => {
            t.push(at(Mode::XApic));
            t.push(at(Mode::X2Apic));
        }
        (Mode::XApic, Mode::X2Apic) => t.push(at(Mode::X2Apic)),
        _ => unreachable!("every other pair was handled above"),
    }
    Some(t)
}

/// A short list of IA32_APIC_BASE values to write, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transition {
    writes: [u64; 3],
    len: usize,
}

impl Transition {
    fn push(&mut self, v: u64) {
        self.writes[self.len] = v;
        self.len += 1;
    }

    pub fn writes(&self) -> &[u64] {
        &self.writes[..self.len]
    }

    /// True if any write passes through disabled, which resets the local
    /// APIC's registers.
    pub fn resets(&self) -> bool {
        self.writes().iter().any(|&w| Mode::of(w) == Mode::Disabled)
    }
}

/// Why the kernel runs the local APIC in the mode it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Firmware already switched to x2APIC. Leaving it would need a reset
    /// through disabled, and firmware may have chosen it because some APIC
    /// IDs are above 255, so the kernel keeps it.
    FirmwareX2Apic,
    /// The CPU supports x2APIC: MSR access, no MMIO page, 32-bit IDs.
    Supported,
    /// The CPU has no x2APIC.
    Unsupported,
    /// The DMAR table sets X2APIC_OPT_OUT.
    FirmwareOptOut,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::FirmwareX2Apic => "firmware left it on",
            Reason::Supported => "CPU supports it",
            Reason::Unsupported => "CPU has no x2APIC",
            Reason::FirmwareOptOut => "firmware opted out of x2APIC (DMAR)",
        }
    }
}

/// The mode the kernel runs the local APIC in: x2APIC whenever the CPU has
/// it, unless firmware opted out; but a local APIC firmware already put in
/// x2APIC mode stays there.
pub fn choose(current: Mode, cpu_x2apic: bool, opt_out: bool) -> (Mode, Reason) {
    if current == Mode::X2Apic {
        (Mode::X2Apic, Reason::FirmwareX2Apic)
    } else if !cpu_x2apic {
        (Mode::XApic, Reason::Unsupported)
    } else if opt_out {
        (Mode::XApic, Reason::FirmwareOptOut)
    } else {
        (Mode::X2Apic, Reason::Supported)
    }
}
