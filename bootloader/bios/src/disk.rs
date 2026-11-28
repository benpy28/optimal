//! The boot disk through the INT 13h extensions (EDD 3.0: AH=42h extended
//! read, AH=48h drive parameters). Reads go through the 64 KiB bounce
//! buffer below 1 MiB and are copied to wherever the caller wants them.

use crate::bios::{self, BOUNCE, BOUNCE_LEN, Regs};
use optimal_boot_core::disk::{BlockDevice, valid_sector_size};

pub struct BiosDisk {
    drive: u8,
    sector_size: usize,
    sectors: Option<u64>,
    /// Sectors per request: what fits the bounce buffer, at most 127 (the
    /// most some BIOSes accept).
    per_read: usize,
}

#[derive(Debug)]
pub enum DiskError {
    NoExtensions,
    BadSectorSize(u16),
}

impl core::fmt::Display for DiskError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            DiskError::NoExtensions => write!(f, "the BIOS has no INT 13h extensions for it"),
            DiskError::BadSectorSize(s) => write!(f, "the BIOS reports {s}-byte sectors"),
        }
    }
}

impl BiosDisk {
    pub fn open(drive: u8) -> Result<BiosDisk, DiskError> {
        let r = bios::int(
            0x13,
            Regs {
                eax: 0x4100,
                ebx: 0x55aa,
                edx: drive as u32,
                ..Default::default()
            },
        );
        if r.carry() || r.ebx & 0xffff != 0xaa55 || r.ecx & 1 == 0 {
            return Err(DiskError::NoExtensions);
        }
        // Drive parameters: a 0x1A-byte (EDD 1.1) result is enough for the
        // sector count (offset 0x10) and bytes per sector (0x18).
        let buf = bios::low_buf();
        // SAFETY: the low buffer is ours.
        unsafe {
            core::ptr::write_bytes(buf as *mut u8, 0, 0x42);
            core::ptr::write_unaligned(buf as *mut u16, 0x1e);
        }
        let (seg, off) = bios::seg_off(buf);
        let r = bios::int(
            0x13,
            Regs {
                eax: 0x4800,
                edx: drive as u32,
                esi: off as u32,
                ds: seg,
                ..Default::default()
            },
        );
        let (mut ss, mut total) = (512u16, None);
        if !r.carry() {
            // SAFETY: the BIOS filled the buffer.
            unsafe {
                let s = core::ptr::read_unaligned((buf + 0x18) as *const u16);
                let t = core::ptr::read_unaligned((buf + 0x10) as *const u64);
                if s != 0 {
                    ss = s;
                }
                // Some BIOSes report 0 or all-ones for "unknown".
                if t != 0 && t != u64::MAX {
                    total = Some(t);
                }
            }
        }
        if !valid_sector_size(ss as usize) {
            return Err(DiskError::BadSectorSize(ss));
        }
        Ok(BiosDisk {
            drive,
            sector_size: ss as usize,
            sectors: total,
            per_read: (BOUNCE_LEN / ss as usize).min(127),
        })
    }

    fn read_chunk(&mut self, lba: u64, count: usize) -> bool {
        let dap = bios::low_buf();
        let (bseg, boff) = bios::seg_off(BOUNCE);
        for attempt in 0..3 {
            // SAFETY: the low buffer is ours; the DAP layout is EDD's:
            // size, reserved, count, offset, segment, LBA.
            unsafe {
                let p = dap as *mut u8;
                core::ptr::write_bytes(p, 0, 16);
                *p = 16;
                core::ptr::write_unaligned(p.add(2) as *mut u16, count as u16);
                core::ptr::write_unaligned(p.add(4) as *mut u16, boff);
                core::ptr::write_unaligned(p.add(6) as *mut u16, bseg);
                core::ptr::write_unaligned(p.add(8) as *mut u64, lba);
            }
            let (seg, off) = bios::seg_off(dap);
            let r = bios::int(
                0x13,
                Regs {
                    eax: 0x4200,
                    edx: self.drive as u32,
                    esi: off as u32,
                    ds: seg,
                    ..Default::default()
                },
            );
            // SAFETY: the BIOS may update the DAP's count with what it read.
            let done = unsafe { core::ptr::read_unaligned((dap + 2) as *const u16) };
            if !r.carry() && r.ah() == 0 && done as usize == count {
                return true;
            }
            if attempt < 2 {
                bios::int(
                    0x13,
                    Regs {
                        edx: self.drive as u32,
                        ..Default::default()
                    },
                );
            }
        }
        false
    }
}

impl BlockDevice for BiosDisk {
    fn sector_size(&self) -> usize {
        self.sector_size
    }

    fn sectors(&self) -> Option<u64> {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let ss = self.sector_size;
        if !buf.len().is_multiple_of(ss) {
            return false;
        }
        if let Some(n) = self.sectors
            && lba
                .checked_add((buf.len() / ss) as u64)
                .is_none_or(|e| e > n)
        {
            return false;
        }
        let mut done = 0;
        while done < buf.len() {
            let count = ((buf.len() - done) / ss).min(self.per_read);
            if !self.read_chunk(lba + (done / ss) as u64, count) {
                return false;
            }
            let bytes = count * ss;
            // SAFETY: the BIOS wrote `bytes` bytes into the bounce buffer.
            let src = unsafe { core::slice::from_raw_parts(BOUNCE as *const u8, bytes) };
            buf[done..done + bytes].copy_from_slice(src);
            done += bytes;
        }
        true
    }
}
