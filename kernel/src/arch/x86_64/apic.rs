//! Register access for the local APIC, the I/O APICs and the 8259 PICs.
//!
//! This module only reads and writes registers. What to write, and in
//! which order, is decided by the host-tested optimal-apic crate; see
//! `crate::irq` for the setup itself.

use optimal_apic::lapic::{self, ICR_PENDING, Ipi};
use optimal_apic::{ioapic, pic};

use super::cpu;
use super::port::{inb, outb};

/// The boot CPU's local APIC, in the mode the kernel put it in.
#[derive(Clone, Copy, Debug)]
pub enum Lapic {
    /// x2APIC: registers are MSRs.
    X2,
    /// xAPIC: registers are uncached MMIO at this virtual address.
    X { regs: u64 },
}

impl Lapic {
    pub fn read(self, reg: u16) -> u32 {
        match self {
            Lapic::X2 => {
                let msr = lapic::x2apic_msr(reg).expect("register has no x2APIC MSR");
                cpu::read_msr(msr) as u32
            }
            Lapic::X { regs } => {
                debug_assert!(lapic::xapic_register(reg));
                // SAFETY: `regs` maps the local APIC's 4 KiB register page
                // uncached (irq::init), and `reg` is a register offset in it.
                unsafe { ((regs + u64::from(reg)) as *const u32).read_volatile() }
            }
        }
    }

    pub fn write(self, reg: u16, value: u32) {
        match self {
            Lapic::X2 => {
                let msr = lapic::x2apic_msr(reg).expect("register has no x2APIC MSR");
                // SAFETY: an x2APIC register MSR, with a value the
                // optimal-apic encoders produced for it.
                unsafe { cpu::write_msr(msr, u64::from(value)) }
            }
            Lapic::X { regs } => {
                debug_assert!(lapic::xapic_register(reg));
                // SAFETY: as in read.
                unsafe { ((regs + u64::from(reg)) as *mut u32).write_volatile(value) }
            }
        }
    }

    /// A 256-bit register array (ISR, TMR or IRR) as 8 dwords.
    pub fn read_array(self, base: u16) -> [u32; 8] {
        core::array::from_fn(|i| self.read(base + 0x10 * i as u16))
    }

    pub fn id(self) -> u32 {
        lapic::id(self.read(lapic::ID), matches!(self, Lapic::X2))
    }

    /// End of interrupt: retires the highest vector in service.
    pub fn eoi(self) {
        // x2APIC requires the value 0 (anything else faults).
        self.write(lapic::EOI, 0);
    }

    /// The error status: written first, as the SDM requires, which also
    /// clears it for next time.
    pub fn esr(self) -> u32 {
        self.write(lapic::ESR, 0);
        self.read(lapic::ESR)
    }

    /// Send an IPI. False if the destination does not fit xAPIC or the
    /// xAPIC never reported the IPI sent.
    #[cfg_attr(
        not(feature = "ktest"),
        expect(dead_code, reason = "first user outside tests is SMP bring-up")
    )]
    pub fn send(self, ipi: Ipi) -> bool {
        match self {
            Lapic::X2 => {
                // SAFETY: the x2APIC ICR MSR, with an encoded ICR value.
                unsafe { cpu::write_msr(0x830, ipi.x2apic()) };
                true
            }
            Lapic::X { .. } => {
                let Some((high, low)) = ipi.xapic() else {
                    return false;
                };
                self.write(lapic::ICR_HIGH, high);
                self.write(lapic::ICR_LOW, low);
                // Delivery takes a few hundred cycles; a million polls is
                // far beyond any real or emulated APIC.
                (0..1_000_000).any(|_| {
                    core::hint::spin_loop();
                    self.read(lapic::ICR_LOW) & ICR_PENDING == 0
                })
            }
        }
    }
}

/// One I/O APIC's registers, mapped uncached. Callers serialise access
/// (IOREGSEL then IOWIN is a pair).
#[derive(Clone, Copy, Debug)]
pub struct IoApicRegs {
    pub regs: u64,
}

impl IoApicRegs {
    pub fn read(self, reg: u8) -> u32 {
        // SAFETY: `regs` maps an I/O APIC's register block (irq::init
        // checked the address with ioapic::address_ok, so IOREGSEL and IOWIN
        // are in the mapped page).
        unsafe {
            ((self.regs + ioapic::IOREGSEL) as *mut u32).write_volatile(u32::from(reg));
            ((self.regs + ioapic::IOWIN) as *const u32).read_volatile()
        }
    }

    pub fn write(self, reg: u8, value: u32) {
        // SAFETY: as in read.
        unsafe {
            ((self.regs + ioapic::IOREGSEL) as *mut u32).write_volatile(u32::from(reg));
            ((self.regs + ioapic::IOWIN) as *mut u32).write_volatile(value);
        }
    }

    /// Redirection entry `pin` as (low, high).
    pub fn entry(self, pin: u8) -> (u32, u32) {
        let (lo, hi) = ioapic::redirection_regs(pin).expect("pin past MAX_PINS");
        (self.read(lo), self.read(hi))
    }
}

/// Give a slow 8259 time between commands: a write to the POST port 0x80,
/// which nothing decodes, takes about a microsecond on the ISA bus.
fn io_wait() {
    // SAFETY: port 0x80 is the POST diagnostic port; writes are harmless.
    unsafe { outb(0x80, 0) };
}

/// Is there an 8259 at the standard ports? Masks everything on both chips
/// as a side effect (with the master's cascade line open while probing).
pub fn pic_probe() -> bool {
    // SAFETY: the 8259 mask registers; masking is always safe, and with no
    // PIC the writes go nowhere.
    unsafe {
        outb(pic::SLAVE_DATA, pic::ALL_MASKED);
        outb(pic::MASTER_DATA, pic::PROBE);
        io_wait();
        let back = inb(pic::MASTER_DATA);
        outb(pic::MASTER_DATA, pic::ALL_MASKED);
        back == pic::PROBE
    }
}

/// The two mask registers, (master, slave).
pub fn pic_masks() -> (u8, u8) {
    // SAFETY: reading the mask registers has no side effects.
    unsafe { (inb(pic::MASTER_DATA), inb(pic::SLAVE_DATA)) }
}

/// Run a port write sequence from optimal_apic::pic.
pub fn pic_write(seq: &[(u16, u8)]) {
    for &(port, value) in seq {
        // SAFETY: the sequence comes from pic::remap_and_mask and only
        // touches the 8259 command and data ports.
        unsafe { outb(port, value) };
        io_wait();
    }
}

/// The in-service registers, (master, slave).
pub fn pic_in_service() -> (u8, u8) {
    // SAFETY: OCW3 selects the ISR for the next command-port read; it
    // changes nothing else.
    unsafe {
        outb(pic::MASTER_CMD, pic::OCW3_READ_ISR);
        outb(pic::SLAVE_CMD, pic::OCW3_READ_ISR);
        (inb(pic::MASTER_CMD), inb(pic::SLAVE_CMD))
    }
}

/// Non-specific EOI to the slave (if asked) and the master.
pub fn pic_eoi(slave: bool) {
    // SAFETY: OCW2 EOI retires the highest in-service line.
    unsafe {
        if slave {
            outb(pic::SLAVE_CMD, pic::EOI);
        }
        outb(pic::MASTER_CMD, pic::EOI);
    }
}
