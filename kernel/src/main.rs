//! Optimal kernel.
//!
//! Phase 0: boot through Limine on UEFI or BIOS, say hello over serial, show
//! that we own the screen, report how much memory the firmware handed us, then
//! halt cleanly. Everything else in ROADMAP.md is built on top of this.

#![no_std]
#![no_main]

mod serial;

use core::arch::asm;
use core::panic::PanicInfo;

use limine::BaseRevision;
use limine::memory_map::EntryType;
use limine::request::{
    BootloaderInfoRequest, FramebufferRequest, MemoryMapRequest, RequestsEndMarker,
    RequestsStartMarker,
};

// Limine reads these requests out of the kernel file before jumping to kmain.
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static BOOTLOADER_INFO: BootloaderInfoRequest = BootloaderInfoRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// The line CI looks for to decide the kernel booted.
pub const BOOT_OK: &str = "OPTIMAL: kernel booted";

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    println!();
    println!("Optimal kernel {}", env!("CARGO_PKG_VERSION"));

    if !BASE_REVISION.is_supported() {
        panic!("bootloader does not support Limine base revision we need");
    }

    if let Some(info) = BOOTLOADER_INFO.get_response() {
        println!("bootloader: {} {}", info.name(), info.version());
    }

    if let Some(map) = MEMORY_MAP.get_response() {
        let usable: u64 = map
            .entries()
            .iter()
            .filter(|e| e.entry_type == EntryType::USABLE)
            .map(|e| e.length)
            .sum();
        println!(
            "memory: {} MiB usable in {} map entries",
            usable / (1024 * 1024),
            map.entries().len()
        );
    }

    if let Some(fb) = FRAMEBUFFER
        .get_response()
        .and_then(|r| r.framebuffers().next())
    {
        println!("screen: {}x{} @ {} bpp", fb.width(), fb.height(), fb.bpp());
        paint_screen(&fb);
    } else {
        println!("screen: none (headless)");
    }

    println!("{BOOT_OK}");
    halt_forever();
}

/// Fill the screen with a dark blue and draw a lighter band across the
/// middle, so a real PC visibly shows the kernel is in control.
fn paint_screen(fb: &limine::framebuffer::Framebuffer) {
    if fb.bpp() != 32 {
        return;
    }
    let (w, h, pitch) = (fb.width(), fb.height(), fb.pitch());
    let base = fb.addr();
    for y in 0..h {
        let colour: u32 = if y > h * 9 / 20 && y < h * 11 / 20 {
            0x3d7eff
        } else {
            0x0b1626
        };
        for x in 0..w {
            let offset = (y * pitch + x * 4) as usize;
            unsafe { base.add(offset).cast::<u32>().write_volatile(colour) };
        }
    }
}

fn halt_forever() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("OPTIMAL: KERNEL PANIC: {info}");
    halt_forever();
}
