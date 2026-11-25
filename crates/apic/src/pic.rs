//! The legacy 8259A PIC pair (Intel 8259A datasheet).
//!
//! The kernel uses the APICs, but a PC-compatible machine still has two
//! 8259s cascaded on IRQ 2. Firmware often leaves them at their reset
//! vectors 0x08 to 0x0F, which are CPU exception vectors, and sometimes
//! unmasked. So the kernel reprograms them onto vectors 0x20 to 0x2F and
//! masks every line: if one ever does get through (an 8259 raises a
//! spurious IRQ 7 or 15 even when masked), it lands on a vector the kernel
//! recognises instead of looking like a double fault.

pub const MASTER_CMD: u16 = 0x20;
pub const MASTER_DATA: u16 = 0x21;
pub const SLAVE_CMD: u16 = 0xA0;
pub const SLAVE_DATA: u16 = 0xA1;

/// ICW1: initialise, ICW4 follows, cascade mode, edge triggered.
pub const ICW1_INIT_ICW4: u8 = 0x11;
/// ICW3 on the master: a slave sits on IRQ 2 (bit mask).
pub const ICW3_MASTER: u8 = 1 << 2;
/// ICW3 on the slave: its cascade identity is 2.
pub const ICW3_SLAVE: u8 = 2;
/// ICW4: 8086 mode, normal (not automatic) EOI.
pub const ICW4_8086: u8 = 0x01;
/// OCW3: the next command-port read returns the in-service register.
pub const OCW3_READ_ISR: u8 = 0x0B;
/// OCW2: non-specific end of interrupt.
pub const EOI: u8 = 0x20;
/// Every line masked.
pub const ALL_MASKED: u8 = 0xFF;

/// Mask value written to the master to see if an 8259 is there: every
/// line masked except the cascade. Reading it back exactly means a PIC
/// answered; with no PIC the port reads back 0xFF (or anything else).
pub const PROBE: u8 = !ICW3_MASTER;

/// The port writes that remap the pair onto `master_base` and `slave_base`
/// and mask every line, in order. None if a base is below 0x20 (exception
/// vectors) or not a multiple of 8 (the 8259 puts the IRQ number in the
/// low three bits).
///
/// Both chips are masked first. ICW1 clears a chip's mask register, and it
/// cannot be set again until ICW4 is done, so each chip is masked by the
/// very next write after its ICW4, before the other chip is touched. The
/// kernel runs this with interrupts off and LINT0 already masked, so that
/// one-write gap cannot deliver anything.
pub fn remap_and_mask(master_base: u8, slave_base: u8) -> Option<[(u16, u8); 12]> {
    if master_base < 0x20
        || slave_base < 0x20
        || !master_base.is_multiple_of(8)
        || !slave_base.is_multiple_of(8)
    {
        return None;
    }
    Some([
        (MASTER_DATA, ALL_MASKED),
        (SLAVE_DATA, ALL_MASKED),
        (MASTER_CMD, ICW1_INIT_ICW4),
        (MASTER_DATA, master_base),
        (MASTER_DATA, ICW3_MASTER),
        (MASTER_DATA, ICW4_8086),
        (MASTER_DATA, ALL_MASKED),
        (SLAVE_CMD, ICW1_INIT_ICW4),
        (SLAVE_DATA, slave_base),
        (SLAVE_DATA, ICW3_SLAVE),
        (SLAVE_DATA, ICW4_8086),
        (SLAVE_DATA, ALL_MASKED),
    ])
}

/// The vector an 8259 uses for its spurious interrupt: its lowest-priority
/// line, IRQ 7 on the master and IRQ 15 (7 on the slave).
pub fn spurious_vector(base: u8) -> u8 {
    base + 7
}
