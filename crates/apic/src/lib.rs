//! Interrupt controller logic for the Optimal kernel.
//!
//! Everything here is pure: register layouts and the decisions about what
//! to write, with no hardware access. The kernel reads the hardware, asks
//! this crate what to do, and writes what it is told, so every decision is
//! tested on the host. Sources: Intel SDM Vol. 3A chapter 11 (the APIC;
//! chapter 10 in older editions), the Intel 82093AA I/O APIC datasheet, the
//! Intel 8259A datasheet, and ACPI 6.5 section 5.2.12 (the MADT).
//!
//! Policy, as in optimal-acpi: never guess. Anything the plan cannot do
//! safely (a GSI no I/O APIC serves, a reserved polarity, a destination an
//! I/O APIC cannot address) is refused with a named reason the kernel
//! reports, and everything else still works.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod base;
pub mod ioapic;
pub mod lapic;
pub mod pic;
pub mod plan;

/// The kernel's interrupt vector map.
///
/// The local APIC takes vectors in priority classes of 16 (vector >> 4),
/// higher class first, and the TPR blocks whole classes. Exceptions own
/// 0x00 to 0x1F.
pub mod vectors {
    /// The 8259 PICs are remapped here (master 0x20 to 0x27, slave 0x28 to
    /// 0x2F) and fully masked, so a stray or spurious 8259 interrupt can
    /// never land on an exception vector.
    pub const PIC_MASTER: u8 = 0x20;
    pub const PIC_SLAVE: u8 = 0x28;
    /// ISA IRQ n arrives on ISA_BASE + n (0x30 to 0x3F).
    pub const ISA_BASE: u8 = 0x30;
    /// First and last vector for other device interrupts (later phases).
    pub const DEVICE_FIRST: u8 = 0x40;
    pub const DEVICE_LAST: u8 = 0xDF;
    /// Unrouted I/O APIC pins are left masked and point here, so if one
    /// is ever unmasked by mistake the kernel can say which.
    pub const UNROUTED: u8 = 0xEE;
    /// Local APIC timer (Phase 1c3).
    pub const LAPIC_TIMER: u8 = 0xF0;
    /// Local APIC thermal, performance counter and CMCI interrupts: masked
    /// for now, but given vectors of their own.
    pub const LAPIC_THERMAL: u8 = 0xF1;
    pub const LAPIC_PERF: u8 = 0xF2;
    pub const LAPIC_CMCI: u8 = 0xF3;
    /// Local APIC error interrupt.
    pub const LAPIC_ERROR: u8 = 0xFE;
    /// Spurious interrupt vector. The low four bits are all ones because
    /// P6 and Pentium local APICs hard-wire them (SDM 11.9); later CPUs
    /// accept any value, so this one works everywhere.
    pub const SPURIOUS: u8 = 0xFF;

    /// ISA IRQ `irq`'s vector.
    pub const fn isa(irq: u8) -> u8 {
        ISA_BASE + (irq & 0xF)
    }
}

/// Bit `vector` of a 256-bit APIC register array (ISR, TMR, IRR): which
/// 32-bit register (0 to 7) and which bit in it. In xAPIC mode register k is
/// at offset base + 0x10 * k; in x2APIC mode at MSR base + k.
pub const fn vector_bit(vector: u8) -> (usize, u32) {
    ((vector >> 5) as usize, (vector & 31) as u32)
}

/// Whether `vector` is set in a 256-bit register array read as 8 dwords.
pub fn is_set(regs: &[u32; 8], vector: u8) -> bool {
    let (i, b) = vector_bit(vector);
    regs[i] & (1 << b) != 0
}

/// Highest vector set in a 256-bit register array, if any. The local APIC
/// serves (IRR) and retires (ISR, on EOI) the highest one first.
pub fn highest(regs: &[u32; 8]) -> Option<u8> {
    (0..8)
        .rev()
        .find(|&i| regs[i] != 0)
        .map(|i| (i as u32 * 32 + 31 - regs[i].leading_zeros()) as u8)
}

/// Property-test case count: `PROPTEST_CASES` if set (CI runs far more),
/// otherwise the given default.
#[cfg(test)]
pub(crate) fn test_cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod suite;
