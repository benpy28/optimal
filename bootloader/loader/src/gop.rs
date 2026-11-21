//! The framebuffer, from the UEFI Graphics Output Protocol.

use crate::console::{boot_services, system_table};
use crate::efi;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

#[derive(Clone, Copy, Default)]
pub struct Format {
    pub bpp: u16,
    pub red: (u8, u8),
    pub green: (u8, u8),
    pub blue: (u8, u8),
}

#[derive(Clone, Copy)]
pub struct Mode {
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u64,
    pub format: Format,
}

pub struct Display {
    pub base: u64,
    pub size: u64,
    pub current: Mode,
    pub modes: Vec<Mode>,
    pub edid: Vec<u8>,
}

fn mask_info(mask: u32) -> (u8, u8) {
    if mask == 0 {
        return (0, 0);
    }
    (mask.count_ones() as u8, mask.trailing_zeros() as u8)
}

fn format(info: &efi::GopModeInfo) -> Option<Format> {
    match info.pixel_format {
        efi::PIXEL_RGBX => Some(Format {
            bpp: 32,
            red: (8, 0),
            green: (8, 8),
            blue: (8, 16),
        }),
        efi::PIXEL_BGRX => Some(Format {
            bpp: 32,
            red: (8, 16),
            green: (8, 8),
            blue: (8, 0),
        }),
        efi::PIXEL_BITMASK => {
            let all = info.red_mask | info.green_mask | info.blue_mask | info.reserved_mask;
            if all == 0 {
                return None;
            }
            let top = 32 - all.leading_zeros();
            let bpp = top.div_ceil(8) * 8;
            Some(Format {
                bpp: bpp as u16,
                red: mask_info(info.red_mask),
                green: mask_info(info.green_mask),
                blue: mask_info(info.blue_mask),
            })
        }
        _ => None, // BltOnly: no linear framebuffer
    }
}

fn mode_from(number: u32, info: &efi::GopModeInfo) -> Option<Mode> {
    let f = format(info)?;
    if info.horizontal_resolution == 0
        || info.vertical_resolution == 0
        || info.pixels_per_scan_line < info.horizontal_resolution
    {
        return None;
    }
    Some(Mode {
        number,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        pitch: info.pixels_per_scan_line as u64 * (f.bpp as u64 / 8),
        format: f,
    })
}

fn find_gop() -> Option<(*mut efi::GraphicsOutput, efi::Handle)> {
    let bs = boot_services();
    // Prefer the GOP on the console's own output device (the active screen).
    let st = system_table();
    let mut p: *mut c_void = ptr::null_mut();
    if !st.console_out_handle.is_null()
        && (bs.handle_protocol)(st.console_out_handle, &efi::GOP_GUID, &mut p) == efi::SUCCESS
        && !p.is_null()
    {
        return Some((p as *mut _, st.console_out_handle));
    }
    let mut count = 0usize;
    let mut handles: *mut efi::Handle = ptr::null_mut();
    if (bs.locate_handle_buffer)(
        efi::LOCATE_BY_PROTOCOL,
        &efi::GOP_GUID,
        ptr::null(),
        &mut count,
        &mut handles,
    ) != efi::SUCCESS
        || count == 0
    {
        return None;
    }
    // SAFETY: the firmware returned `count` handles.
    let h = unsafe { *handles };
    (bs.free_pool)(handles as *mut u8);
    let mut p: *mut c_void = ptr::null_mut();
    ((bs.handle_protocol)(h, &efi::GOP_GUID, &mut p) == efi::SUCCESS && !p.is_null())
        .then_some((p as *mut _, h))
}

/// Finds the display, switching mode if the config asks for a resolution
/// (or if the current mode has no linear framebuffer).
pub fn setup(want: Option<(u32, u32)>) -> Option<Display> {
    let (gop, handle) = find_gop()?;
    // SAFETY: gop is a valid GOP from the firmware for the rest of this function.
    let g = unsafe { &mut *gop };
    let mode = unsafe { &*g.mode };
    let mut modes = Vec::new();
    for n in 0..mode.max_mode.min(512) {
        let mut size = 0usize;
        let mut info: *const efi::GopModeInfo = ptr::null();
        if (g.query_mode)(gop, n, &mut size, &mut info) == efi::SUCCESS && !info.is_null() {
            // SAFETY: the firmware returned a mode info structure.
            if let Some(m) = mode_from(n, unsafe { &*info }) {
                modes.push(m);
            }
        }
    }
    let current = || {
        // SAFETY: mode and its info are firmware-owned and valid.
        let m = unsafe { &*g.mode };
        mode_from(m.mode, unsafe { &*m.info })
    };
    let target = match want {
        Some((w, h)) => modes
            .iter()
            .find(|m| m.width == w && m.height == h)
            .copied(),
        None if current().is_none() => modes
            .iter()
            .max_by_key(|m| m.width as u64 * m.height as u64)
            .copied(),
        None => None,
    };
    if let Some(t) = target
        && current().map(|c| c.number) != Some(t.number)
    {
        (g.set_mode)(gop, t.number);
    }
    let current = current()?;
    // SAFETY: as above.
    let m = unsafe { &*g.mode };
    let size = (m.frame_buffer_size as u64).max(current.pitch * current.height as u64);
    let mut edid = Vec::new();
    for guid in [efi::EDID_ACTIVE_GUID, efi::EDID_DISCOVERED_GUID] {
        let mut p: *mut c_void = ptr::null_mut();
        if (boot_services().handle_protocol)(handle, &guid, &mut p) == efi::SUCCESS && !p.is_null()
        {
            // SAFETY: an EDID protocol from the firmware.
            let e = unsafe { &*(p as *const efi::EdidProtocol) };
            if !e.edid.is_null() && (128..=32768).contains(&e.size_of_edid) {
                // SAFETY: the protocol says `size_of_edid` bytes live at `edid`.
                edid.extend_from_slice(unsafe {
                    core::slice::from_raw_parts(e.edid, e.size_of_edid as usize)
                });
                break;
            }
        }
    }
    if m.frame_buffer_base == 0 {
        return None;
    }
    Some(Display {
        base: m.frame_buffer_base,
        size,
        current,
        modes,
        edid,
    })
}
