//! Level-triggered I/O APIC lines, and the pins whose Remote IRR stays
//! set.
//!
//! A level line is held masked from the moment its interrupt arrives
//! until its handler says it is done (design/drivers.md, `IrqHandle`), so
//! a device that keeps the line asserted cannot storm the CPU:
//!
//! 1. The interrupt arrives: the pin is masked, the handler runs.
//! 2. The local APIC gets its EOI. In broadcast mode that EOI reaches the
//!    I/O APIC by itself; in directed mode (optimal_apic::eoi) the kernel
//!    writes the vector to the I/O APIC's EOI register. Either way Remote
//!    IRR clears while the pin is masked.
//! 3. When the handler is done ([`Done::Now`] at once, or [`Line::done`]
//!    later), the pin is unmasked through optimal_apic::eoi::settle,
//!    which reads the entry first: a Remote IRR still set there (the EOI
//!    never matched, or the local APIC took the interrupt as edge) is
//!    cleared by the EOI register or the trigger toggle, and a pin that
//!    will not clear is left masked and reported stuck.
//!
//! Cost per interrupt: one I/O APIC write to mask, one read and one write
//! to unmask (two MMIO accesses each, IOREGSEL then IOWIN), plus the EOI
//! register write in directed mode. MSIs, which later phases use for
//! every device that has them, need none of this.

#![cfg_attr(
    not(feature = "ktest"),
    expect(
        dead_code,
        reason = "the claim API's first users outside tests are the ACPI SCI (AML hosting) and legacy INTx drivers"
    )
)]

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use optimal_apic::eoi::{self, Method, Mode, Outcome};
use optimal_apic::ioapic::{self, Delivery, Redirection};
use optimal_apic::plan::{IoApicStatus, NotRouted};
use optimal_apic::vectors;

use super::{Controllers, HANDLERS, IOAPIC_LOCK, controllers};
use crate::arch::apic::{IoApicRegs, Lapic};
use crate::arch::idt::InterruptFrame;
use crate::sync::IrqSpinLock;

/// Extra reads after an EOI register write or a toggle before the next
/// method is tried. An I/O APIC on the far side of a chipset bus may show
/// the cleared bit a little after the write; 64 reads is tens of
/// microseconds on real hardware, far longer than any bus latency.
pub const REREADS: u32 = 64;

/// What a level handler tells the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// The device no longer asserts the line: unmask it now.
    Now,
    /// Still being handled: the line stays masked until [`Line::done`].
    Later,
}

pub type LevelHandler = fn(&mut InterruptFrame) -> Done;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Interrupts are off on this machine.
    Off,
    /// Not in the device range (0x40 to 0xDF).
    NotDeviceVector(u8),
    /// Something already handles this vector.
    VectorBusy(u8),
    /// No used I/O APIC serves this GSI.
    NoIoApic { gsi: u32 },
    /// The routing plan gives this GSI to an ISA IRQ or an NMI source.
    PlanUses { gsi: u32 },
    /// Another level line has this GSI.
    PinBusy { gsi: u32 },
    /// The boot CPU's APIC ID does not fit an I/O APIC entry.
    NoDestination,
    /// claim_isa: the ISA IRQ has no route.
    NotRouted(NotRouted),
    /// claim_isa: the ISA IRQ is edge-triggered.
    NotLevel { irq: u8 },
}

/// The pin's Remote IRR is set and nothing clears it, so it is masked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stuck;

/// A claimed level line. Starts masked: [`Line::unmask`] it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub gsi: u32,
    /// MADT index of the I/O APIC.
    pub ioapic: usize,
    pub pin: u8,
    pub vector: u8,
    /// The entry when unmasked.
    pub entry: Redirection,
    /// The plan's entry for the pin, put back by [`Line::release`].
    plan_entry: Redirection,
}

/// By vector: 0, or VALID | ioapic << 8 | pin.
static LINES: [AtomicU32; 256] = [const { AtomicU32::new(0) }; 256];
/// By vector: the unmasked entry's low dword.
static LOWS: [AtomicU32; 256] = [const { AtomicU32::new(0) }; 256];
/// By vector: the handler, a `LevelHandler` as usize.
static FNS: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
/// By vector: IDLE, HELD (masked until done) or STUCK.
static STATES: [AtomicU8; 256] = [const { AtomicU8::new(IDLE) }; 256];
const VALID: u32 = 1 << 31;
const IDLE: u8 = 0;
const HELD: u8 = 1;
const STUCK: u8 = 2;

/// Serialises claims and releases.
static CLAIM: IrqSpinLock<()> = IrqSpinLock::new(());

/// Remote IRRs found set and cleared, by method, and pins left stuck,
/// since boot (setup's own sweep included).
pub static CLEARED_EOI: AtomicU64 = AtomicU64::new(0);
pub static CLEARED_TOGGLE: AtomicU64 = AtomicU64::new(0);
pub static STUCK_PINS: AtomicU64 = AtomicU64::new(0);

/// Test builds: make every pin act as if it ignores both the EOI register
/// and the trigger toggle, to prove a pin that stays stuck is handled.
#[cfg(feature = "ktest")]
pub static NO_CLEAR: AtomicBool = AtomicBool::new(false);
#[cfg(not(feature = "ktest"))]
static NO_CLEAR: AtomicBool = AtomicBool::new(false);

/// One pin, for optimal_apic::eoi::settle. The I/O APIC lock is held.
pub(super) struct IoPin {
    pub regs: IoApicRegs,
    pub pin: u8,
}

impl eoi::Pin for IoPin {
    fn read_low(&mut self) -> u32 {
        self.regs.read(
            ioapic::redirection_regs(self.pin)
                .expect("pin below MAX_PINS")
                .0,
        )
    }

    fn write_low(&mut self, value: u32) {
        let value = if NO_CLEAR.load(Ordering::Relaxed) {
            // An I/O APIC that keeps Remote IRR through a toggle acts as
            // if the edge write never happened.
            value | 1 << 15
        } else {
            value
        };
        self.regs.write(
            ioapic::redirection_regs(self.pin)
                .expect("pin below MAX_PINS")
                .0,
            value,
        );
    }

    fn eoi(&mut self, vector: u8) {
        if !NO_CLEAR.load(Ordering::Relaxed) {
            self.regs.eoi(vector);
        }
    }
}

/// settle() one pin and count what it found. The I/O APIC lock is held.
pub(super) fn settle(
    regs: IoApicRegs,
    pin: u8,
    wanted: Redirection,
    eoi_register: bool,
) -> Outcome {
    let out = eoi::settle(&mut IoPin { regs, pin }, wanted, eoi_register, REREADS);
    match out {
        Outcome::Clear => {}
        Outcome::Cleared(Method::EoiRegister) => {
            CLEARED_EOI.fetch_add(1, Ordering::Relaxed);
        }
        Outcome::Cleared(Method::Toggle) => {
            CLEARED_TOGGLE.fetch_add(1, Ordering::Relaxed);
        }
        Outcome::Stuck => {
            STUCK_PINS.fetch_add(1, Ordering::Relaxed);
        }
    }
    out
}

pub fn method_name(m: Method) -> &'static str {
    match m {
        Method::EoiRegister => "the EOI register",
        Method::Toggle => "the trigger toggle",
    }
}

/// Is `gsi` free for a level line? Ok((ioapic, pin)) if so.
pub fn free(c: &Controllers, gsi: u32) -> Result<(usize, u8), ClaimError> {
    let (i, io) = c
        .plan
        .ioapics
        .iter()
        .enumerate()
        .find(|(_, io)| io.status == IoApicStatus::Used && io.serves(gsi))
        .ok_or(ClaimError::NoIoApic { gsi })?;
    let pin = (gsi - io.gsi_base) as u8;
    let planned = c
        .plan
        .isa
        .iter()
        .flatten()
        .any(|r| r.ioapic == i && r.pin == pin)
        || c.plan
            .nmi_pins
            .iter()
            .any(|n| n.ioapic == i && n.pin == pin);
    if planned {
        return Err(ClaimError::PlanUses { gsi });
    }
    if LINES
        .iter()
        .any(|l| l.load(Ordering::SeqCst) == line_word(i, pin))
    {
        return Err(ClaimError::PinBusy { gsi });
    }
    Ok((i, pin))
}

fn line_word(ioapic: usize, pin: u8) -> u32 {
    VALID | (ioapic as u32) << 8 | u32::from(pin)
}

/// Claim `gsi` as a level-triggered line on `vector`, active high or low.
pub fn claim(
    gsi: u32,
    active_low: bool,
    vector: u8,
    handler: LevelHandler,
) -> Result<Line, ClaimError> {
    let c = controllers().ok_or(ClaimError::Off)?;
    if !(vectors::DEVICE_FIRST..=vectors::DEVICE_LAST).contains(&vector) {
        return Err(ClaimError::NotDeviceVector(vector));
    }
    let _g = CLAIM.lock();
    let (ioapic, pin) = free(c, gsi)?;
    let destination = c.plan.destination.ok_or(ClaimError::NoDestination)?;
    let entry = Redirection {
        vector,
        delivery: Delivery::Fixed,
        logical: false,
        active_low,
        level: true,
        masked: false,
        destination,
    };
    install(
        c,
        Line {
            gsi,
            ioapic,
            pin,
            vector,
            entry,
            plan_entry: optimal_apic::plan::unrouted(destination),
        },
        handler,
    )
}

/// Claim ISA IRQ `irq`'s routed pin as a level line, on its ISA vector,
/// as the MADT describes it (the SCI is the usual one).
pub fn claim_isa(irq: u8, handler: LevelHandler) -> Result<Line, ClaimError> {
    let c = controllers().ok_or(ClaimError::Off)?;
    let r = c
        .plan
        .isa
        .get(usize::from(irq))
        .copied()
        .unwrap_or(Err(NotRouted::Cascade))
        .map_err(ClaimError::NotRouted)?;
    if !r.entry.level {
        return Err(ClaimError::NotLevel { irq });
    }
    let _g = CLAIM.lock();
    if LINES
        .iter()
        .any(|l| l.load(Ordering::SeqCst) == line_word(r.ioapic, r.pin))
    {
        return Err(ClaimError::PinBusy { gsi: r.gsi });
    }
    install(
        c,
        Line {
            gsi: r.gsi,
            ioapic: r.ioapic,
            pin: r.pin,
            vector: r.entry.vector,
            entry: Redirection {
                masked: false,
                ..r.entry
            },
            plan_entry: r.entry,
        },
        handler,
    )
}

/// CLAIM is held.
fn install(c: &Controllers, line: Line, handler: LevelHandler) -> Result<Line, ClaimError> {
    let v = usize::from(line.vector);
    if HANDLERS[v].load(Ordering::SeqCst) != 0 || LINES[v].load(Ordering::SeqCst) != 0 {
        return Err(ClaimError::VectorBusy(line.vector));
    }
    let regs = c.ioapics[line.ioapic].expect("a used I/O APIC is mapped");
    let masked = Redirection {
        masked: true,
        ..line.entry
    };
    {
        let _g = IOAPIC_LOCK.lock();
        for (reg, val) in masked.writes(line.pin).expect("pins are below MAX_PINS") {
            regs.write(reg, val);
        }
        // Whatever Remote IRR the pin had is cleared now, while masked.
        settle(regs, line.pin, masked, c.eoi_register[line.ioapic]);
    }
    LOWS[v].store(line.entry.encode().0, Ordering::SeqCst);
    FNS[v].store(handler as usize, Ordering::SeqCst);
    STATES[v].store(HELD, Ordering::SeqCst);
    LINES[v].store(line_word(line.ioapic, line.pin), Ordering::SeqCst);
    Ok(line)
}

impl Line {
    /// Unmask the line, clearing a Remote IRR left set. Err if the pin is
    /// stuck (it stays masked; later calls try again).
    pub fn unmask(&self) -> Result<(), Stuck> {
        let c = controllers().expect("a line exists only while interrupts are on");
        unmask(c, self.vector)
    }

    /// The handler that returned [`Done::Later`] is done.
    pub fn done(&self) -> Result<(), Stuck> {
        self.unmask()
    }

    pub fn mask(&self) {
        let c = controllers().expect("a line exists only while interrupts are on");
        let regs = c.ioapics[self.ioapic].expect("a used I/O APIC is mapped");
        let _g = IOAPIC_LOCK.lock();
        write_low(regs, self.pin, self.entry.encode().0 | ioapic::MASKED);
        STATES[usize::from(self.vector)].store(HELD, Ordering::SeqCst);
    }

    /// Is the line stuck (masked because its Remote IRR will not clear)?
    pub fn stuck(&self) -> bool {
        STATES[usize::from(self.vector)].load(Ordering::SeqCst) == STUCK
    }

    /// Give the line up: the pin goes back to the plan's (masked) entry.
    pub fn release(self) {
        let c = controllers().expect("a line exists only while interrupts are on");
        let regs = c.ioapics[self.ioapic].expect("a used I/O APIC is mapped");
        let _c = CLAIM.lock();
        let v = usize::from(self.vector);
        {
            let _g = IOAPIC_LOCK.lock();
            write_low(regs, self.pin, self.entry.encode().0 | ioapic::MASKED);
            let back = Redirection {
                masked: true,
                ..self.plan_entry
            };
            settle(regs, self.pin, back, c.eoi_register[self.ioapic]);
            for (reg, val) in back.writes(self.pin).expect("pins are below MAX_PINS") {
                regs.write(reg, val);
            }
        }
        LINES[v].store(0, Ordering::SeqCst);
        FNS[v].store(0, Ordering::SeqCst);
        LOWS[v].store(0, Ordering::SeqCst);
        STATES[v].store(IDLE, Ordering::SeqCst);
    }
}

fn write_low(regs: IoApicRegs, pin: u8, value: u32) {
    regs.write(
        ioapic::redirection_regs(pin).expect("pin below MAX_PINS").0,
        value,
    );
}

fn unmask(c: &Controllers, vector: u8) -> Result<(), Stuck> {
    let v = usize::from(vector);
    let word = LINES[v].load(Ordering::SeqCst);
    assert!(word != 0, "vector {vector:#x} is not a level line");
    let (ioapic, pin) = (((word >> 8) & 0xFF) as usize, word as u8);
    let regs = c.ioapics[ioapic].expect("a used I/O APIC is mapped");
    let wanted = Redirection::decode(LOWS[v].load(Ordering::SeqCst), 0).entry;
    let _g = IOAPIC_LOCK.lock();
    match settle(regs, pin, wanted, c.eoi_register[ioapic]) {
        Outcome::Stuck => {
            STATES[v].store(STUCK, Ordering::SeqCst);
            Err(Stuck)
        }
        _ => {
            STATES[v].store(IDLE, Ordering::SeqCst);
            Ok(())
        }
    }
}

/// Is `vector` a level line? Called by dispatch for every device vector.
pub(super) fn is_line(vector: u8) -> bool {
    LINES[usize::from(vector)].load(Ordering::Relaxed) != 0
}

/// A level line's interrupt (dispatch checked [`is_line`]).
pub(super) fn interrupt(c: &Controllers, lapic: Lapic, frame: &mut InterruptFrame) {
    let vector = frame.vector as u8;
    let v = usize::from(vector);
    let word = LINES[v].load(Ordering::SeqCst);
    let (ioapic, pin) = (((word >> 8) & 0xFF) as usize, word as u8);
    let regs = c.ioapics[ioapic].expect("a used I/O APIC is mapped");
    {
        let _g = IOAPIC_LOCK.lock();
        write_low(regs, pin, LOWS[v].load(Ordering::SeqCst) | ioapic::MASKED);
    }
    STATES[v].store(HELD, Ordering::SeqCst);
    // SAFETY: only install stores here, from a LevelHandler.
    let f: LevelHandler =
        unsafe { core::mem::transmute::<usize, LevelHandler>(FNS[v].load(Ordering::SeqCst)) };
    let done = f(frame);
    // The vector came from the I/O APIC (fixed delivery), so it is in
    // service.
    lapic.eoi();
    if c.eoi == Mode::Directed {
        regs.eoi(vector);
    }
    if done == Done::Now {
        // Stuck is recorded in the line's state and the counters; the
        // owner sees it from Line::stuck.
        let _ = unmask(c, vector);
    }
}

#[cfg(feature = "ktest")]
mod tests;
