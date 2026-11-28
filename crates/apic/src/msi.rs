//! Message-signalled interrupts: the address and data a device writes to
//! raise an interrupt in a local APIC directly, with no I/O APIC between
//! (Intel SDM Vol. 3A 11.11, "Message Signalled Interrupts"). The HPET's
//! FSB delivery (IA-PC HPET 1.0a 2.3.9, Tn_FSB_INT_ROUTE) uses the same
//! format as PCI MSI and MSI-X.
//!
//! Address (32 bits): 31:20 0xFEE, 19:12 destination APIC ID, 3 RH
//! (redirection hint), 2 DM (destination mode), the rest 0.
//! Data (32 bits): 7:0 vector, 10:8 delivery mode, 14 level (assert),
//! 15 trigger mode (0 edge), the rest 0.
//!
//! Only one form is made here, the one the kernel uses: fixed delivery,
//! edge, physical destination, no redirection hint. That goes to exactly
//! one CPU, and needs no EOI anywhere but that CPU's local APIC.

/// Bits 31:20 of every MSI address.
pub const ADDRESS_BASE: u32 = 0xFEE0_0000;
/// The destination ID field: address bits 19:12.
pub const DESTINATION_SHIFT: u32 = 12;
/// Address bit 3, redirection hint (lowest-priority delivery among the
/// destinations).
pub const REDIRECTION_HINT: u32 = 1 << 3;
/// Address bit 2, destination mode: logical when set.
pub const LOGICAL: u32 = 1 << 2;
/// Data bits 10:8, delivery mode.
pub const DELIVERY_SHIFT: u32 = 8;
/// Data bit 14, level: assert. Ignored for edge.
pub const ASSERT: u32 = 1 << 14;
/// Data bit 15, trigger mode: level when set.
pub const LEVEL: u32 = 1 << 15;

/// An MSI: the device writes `data` (32 bits) to physical address
/// `address`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub address: u32,
    pub data: u32,
}

/// Why no message could be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The address holds an 8-bit destination. Wider APIC IDs (x2APIC
    /// systems with more than 255 CPUs) need interrupt remapping, which
    /// the kernel does not set up yet.
    DestinationTooWide(u32),
    /// Vectors 0x00 to 0x1F are the CPU's exceptions (and 0x00 to 0x0F
    /// are illegal as interrupts: the local APIC drops them and sets
    /// "receive illegal vector" in its ESR, SDM 11.5.3).
    Vector(u8),
}

/// A fixed, edge-triggered message to the CPU whose APIC ID is `apic_id`
/// (physical destination), on `vector`.
pub fn fixed_edge(apic_id: u32, vector: u8) -> Result<Message, Refused> {
    if apic_id > 0xFF {
        return Err(Refused::DestinationTooWide(apic_id));
    }
    if vector < 0x20 {
        return Err(Refused::Vector(vector));
    }
    Ok(Message {
        address: ADDRESS_BASE | apic_id << DESTINATION_SHIFT,
        // Delivery mode 000 (fixed), trigger 0 (edge), level 0.
        data: u32::from(vector),
    })
}
