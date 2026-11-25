//! The DMAR table header (Intel VT-d specification 4.1, section 8.1): only
//! the flags the interrupt code needs. The remapping structures that follow
//! the header are left for the IOMMU work.

use crate::{Error, HEADER_LEN, Sdt, u8_at};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dmar {
    pub checksum_ok: bool,
    /// Physical address width the remapping hardware supports, in bits
    /// (the table holds this minus one).
    pub host_address_width: u8,
    /// INTR_REMAP: the platform supports interrupt remapping.
    pub interrupt_remapping: bool,
    /// X2APIC_OPT_OUT: firmware asks the OS not to switch the local APICs
    /// to x2APIC mode.
    pub x2apic_opt_out: bool,
    /// DMA_CTRL_PLATFORM_OPT_IN: firmware asks for DMA protection to stay
    /// on across the handover.
    pub dma_ctrl_platform_opt_in: bool,
}

impl Dmar {
    pub fn parse(bytes: &[u8]) -> Result<Dmar, Error> {
        // Header, width (1), flags (1), reserved (10).
        let sdt = Sdt::new(bytes, b"DMAR", HEADER_LEN + 12)?;
        let b = sdt.bytes;
        let width = u8_at(b, 36).ok_or(Error::TooShort)?;
        let flags = u8_at(b, 37).ok_or(Error::TooShort)?;
        Ok(Dmar {
            checksum_ok: sdt.checksum_ok,
            // 255 + 1 does not fit a u8, and no address is that wide.
            host_address_width: width.checked_add(1).ok_or(Error::BadField)?,
            interrupt_remapping: flags & 1 != 0,
            x2apic_opt_out: flags & 2 != 0,
            dma_ctrl_platform_opt_in: flags & 4 != 0,
        })
    }
}
