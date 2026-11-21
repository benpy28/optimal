//! Optimal kernel.
//!
//! Boot order: serial (so we can report anything), CPU tables and exception
//! handling, hardware protections, then whatever the command line asks for:
//! the normal boot, the in-kernel test suite, or a deliberate crash test.

#![no_std]
#![no_main]

mod arch;
mod backtrace;
mod boot;
mod bootinfo;
mod crashtest;
#[cfg(feature = "ktest")]
mod ktest;
mod mm;
mod serial;
mod sync;
#[cfg(feature = "ktest")]
mod tests;

use core::panic::PanicInfo;

/// The line CI looks for to decide the kernel booted.
pub const BOOT_OK: &str = "OPTIMAL: kernel booted";

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    crate::println!();
    crate::println!("Optimal kernel {}", env!("CARGO_PKG_VERSION"));

    // SAFETY: first thing on the boot CPU, interrupts are still disabled.
    let hardening = unsafe { arch::init_cpu() };
    crate::println!(
        "cpu: tables loaded; NX {} WP {} SMEP {} SMAP {} UMIP {}",
        on(hardening.nx),
        on(hardening.wp),
        on(hardening.smep),
        on(hardening.smap),
        on(hardening.umip)
    );

    if !boot::BASE_REVISION.is_supported() {
        panic!("bootloader does not support the Limine base revision we need");
    }
    if let Some(info) = boot::BOOTLOADER_INFO.get_response() {
        crate::println!("bootloader: {} {}", info.name(), info.version());
    }
    let info = bootinfo::capture();
    report_memory(info);
    paint_screen();

    let cmdline = info.cmdline();
    if let Some(name) = boot::arg(cmdline, "crashtest") {
        crashtest::run(name);
    }

    #[cfg(feature = "ktest")]
    ktest::run_all();

    crate::println!("{BOOT_OK}");
    arch::cpu::halt_forever();
}

fn on(b: bool) -> &'static str {
    if b { "on" } else { "unsupported" }
}

fn report_memory(info: &bootinfo::BootInfo) {
    let usable: u64 = info
        .memory_map()
        .iter()
        .filter(|e| e.kind == bootinfo::Kind::Usable)
        .map(|e| e.len)
        .sum();
    crate::println!(
        "memory: {} MiB usable in {} map entries",
        usable / (1024 * 1024),
        info.memory_map().len()
    );
    let s = mm::frames::init(info);
    crate::println!(
        "frames: {} MiB free in {} zone(s), {} KiB of allocator metadata",
        s.free_bytes / (1024 * 1024),
        s.zones,
        s.metadata_bytes / 1024
    );
}

/// Fill the screen with a dark blue and draw a lighter band across the
/// middle, so a real PC visibly shows the kernel is in control.
fn paint_screen() {
    let Some(fb) = boot::FRAMEBUFFER
        .get_response()
        .and_then(|r| r.framebuffers().next())
    else {
        crate::println!("screen: none (headless)");
        return;
    };
    crate::println!("screen: {}x{} @ {} bpp", fb.width(), fb.height(), fb.bpp());
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
            // SAFETY: (x, y) is inside the framebuffer Limine gave us, and each
            // pixel is 4 bytes at `pitch` bytes per row.
            unsafe { base.add(offset).cast::<u32>().write_volatile(colour) };
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(feature = "ktest")]
    if let Some(test) = ktest::current_name() {
        crate::println!("KTEST FAIL {test}");
    }
    crate::println!("OPTIMAL: KERNEL PANIC: {info}");
    backtrace::print_here();
    arch::cpu::halt_forever();
}
