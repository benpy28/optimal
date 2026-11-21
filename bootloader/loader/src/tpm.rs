//! Measured boot through the firmware's EFI_TCG2_PROTOCOL (TPM 2.0).
//!
//! Our PCR use (documented in DESIGN.md):
//!   PCR 8: the config file, then the chosen entry and its command line
//!   PCR 9: the kernel file, then each module, in load order

use crate::console::boot_services;
use crate::efi;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

pub const PCR_CONFIG: u32 = 8;
pub const PCR_IMAGES: u32 = 9;
const EV_IPL: u32 = 0x0000_000d;

pub struct Tpm {
    proto: *mut efi::Tcg2Protocol,
    pub sha256_active: bool,
}

impl Tpm {
    pub fn find() -> Option<Tpm> {
        let mut p: *mut c_void = ptr::null_mut();
        if (boot_services().locate_protocol)(&efi::TCG2_GUID, ptr::null(), &mut p) != efi::SUCCESS || p.is_null() {
            return None;
        }
        let proto = p as *mut efi::Tcg2Protocol;
        let mut cap = efi::Tcg2Capability {
            size: core::mem::size_of::<efi::Tcg2Capability>() as u8,
            ..Default::default()
        };
        // SAFETY: proto is the firmware's TCG2 protocol; cap has its size set.
        if unsafe { ((*proto).get_capability)(proto, &mut cap) } != efi::SUCCESS || cap.tpm_present_flag == 0 {
            return None;
        }
        let active = cap.active_pcr_banks;
        Some(Tpm { proto, sha256_active: active & efi::TCG2_HASH_ALG_SHA256 != 0 })
    }

    /// Hashes `data` into `pcr` and logs an EV_IPL event described by `desc`.
    pub fn measure(&self, pcr: u32, data: &[u8], desc: &str) -> bool {
        // EFI_TCG2_EVENT: Size u32, then header {HeaderSize u32, HeaderVersion
        // u16, PCRIndex u32, EventType u32}, then the event data. Packed.
        let mut ev: Vec<u8> = Vec::with_capacity(18 + desc.len());
        let total = (4 + 14 + desc.len()) as u32;
        ev.extend_from_slice(&total.to_le_bytes());
        ev.extend_from_slice(&14u32.to_le_bytes());
        ev.extend_from_slice(&1u16.to_le_bytes());
        ev.extend_from_slice(&pcr.to_le_bytes());
        ev.extend_from_slice(&EV_IPL.to_le_bytes());
        ev.extend_from_slice(desc.as_bytes());
        // SAFETY: data and ev are valid for the given lengths for the call.
        let s = unsafe {
            ((*self.proto).hash_log_extend_event)(self.proto, 0, data.as_ptr() as u64, data.len() as u64, ev.as_ptr())
        };
        s == efi::SUCCESS
    }

    /// Copies the event log (crypto-agile format). Call after the last
    /// measurement: later events go to the firmware's final-events table.
    pub fn event_log(&self) -> Option<Vec<u8>> {
        let mut first = 0u64;
        let mut last = 0u64;
        let mut truncated = false;
        // SAFETY: valid protocol, out-pointers valid.
        let s = unsafe { ((*self.proto).get_event_log)(self.proto, efi::TCG2_EVENT_LOG_FORMAT_TCG_2, &mut first, &mut last, &mut truncated) };
        if s != efi::SUCCESS || first == 0 || last < first || last - first > 64 << 20 {
            return None;
        }
        // Read the header to learn digest sizes, then size the last event.
        // SAFETY: the firmware's log is readable from `first` through the end
        // of the last event; we read the header (at most 4 KiB) first.
        let head = unsafe { core::slice::from_raw_parts(first as *const u8, 4096.min((last - first) as usize + 4096)) };
        let (hlen, algs, n) = optimal_boot_core::tcg::spec_id(head)?;
        let last_size = if last == first {
            0
        } else {
            // SAFETY: the last event starts at `last`; 64 KiB covers any event we
            // could size (bounded by event2_size against this slice).
            let tail = unsafe { core::slice::from_raw_parts(last as *const u8, 64 * 1024) };
            optimal_boot_core::tcg::event2_size(tail, 0, &algs[..n])?
        };
        let len = if last == first { hlen } else { (last - first) as usize + last_size };
        // SAFETY: [first, first + len) is the firmware's event log.
        Some(unsafe { core::slice::from_raw_parts(first as *const u8, len) }.to_vec())
    }

    /// Reads SHA-256 PCR values with TPM2_PCR_Read (for the CI report).
    pub fn read_pcrs(&self, pcrs: &[u32]) -> Option<Vec<[u8; 32]>> {
        let mut sel = [0u8; 3];
        for &p in pcrs {
            sel[(p / 8) as usize] |= 1 << (p % 8);
        }
        let mut cmd = Vec::new();
        cmd.extend_from_slice(&0x8001u16.to_be_bytes()); // TPM_ST_NO_SESSIONS
        cmd.extend_from_slice(&20u32.to_be_bytes());
        cmd.extend_from_slice(&0x17eu32.to_be_bytes()); // TPM_CC_PCR_Read
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(&0x000bu16.to_be_bytes()); // TPM_ALG_SHA256
        cmd.push(3);
        cmd.extend_from_slice(&sel);
        let mut rsp = [0u8; 1024];
        // SAFETY: buffers valid for the given sizes.
        let s = unsafe { ((*self.proto).submit_command)(self.proto, cmd.len() as u32, cmd.as_ptr(), rsp.len() as u32, rsp.as_mut_ptr()) };
        if s != efi::SUCCESS || u32::from_be_bytes(rsp[6..10].try_into().unwrap()) != 0 {
            return None;
        }
        // tag, size, rc, updateCounter, TPML_PCR_SELECTION(count + 1 sel = 4+2+1+3), TPML_DIGEST
        let mut o = 10 + 4 + 4 + 2 + 1 + 3;
        let count = u32::from_be_bytes(rsp.get(o..o + 4)?.try_into().ok()?) as usize;
        o += 4;
        let mut out = Vec::new();
        for _ in 0..count {
            let size = u16::from_be_bytes(rsp.get(o..o + 2)?.try_into().ok()?) as usize;
            o += 2;
            if size != 32 {
                return None;
            }
            out.push(rsp.get(o..o + 32)?.try_into().ok()?);
            o += 32;
        }
        (out.len() == pcrs.len()).then_some(out)
    }
}
