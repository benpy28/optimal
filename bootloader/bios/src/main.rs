//! Optimal Boot, BIOS stage: what runs on machines without UEFI (boot tier
//! B4 in design/boot.md).
//!
//! Stage 1 (stage1.s, the MBR) loads stage 2 from the sectors the installer
//! recorded. Stage 2 (stage2.s) switches to long mode and calls
//! `stage2_main`, which from then on works like the UEFI loader: find the
//! boot volume, verify the signed config, verify every file against its
//! hash, and load the kernel. BIOS services are reached through a thunk to
//! real mode (bios.rs).

#![no_std]
#![no_main]

extern crate alloc;

mod bios;
mod boot;
mod console;
mod disk;
mod hw;
mod idt;
mod mem;
mod menu;

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("stage1.s"), options(att_syntax));
core::arch::global_asm!(include_str!("stage2.s"), options(att_syntax));

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

pub const NAME: &str = "Optimal Boot";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stops the boot with a message, waits for a key (or 30 seconds), then
/// hands back to the BIOS with INT 18h so it can try the next boot device.
pub fn fatal(msg: core::fmt::Arguments) -> ! {
    println!();
    println!("OPTIMAL-BOOT: FATAL: {msg}");
    println!("The system was not booted. Nothing unverified was run.");
    println!("Press any key to try the next boot device (or wait 30 seconds).");
    menu::wait_key(30_000);
    bios::next_boot_device()
}

#[unsafe(no_mangle)]
extern "sysv64" fn stage2_main(drive: u32) -> ! {
    let t_init = hw::rdtsc();
    // First, before any static is written: is stage 2 what was installed?
    let self_check = boot::check_self();
    idt::init();
    console::init();
    boot::run(drive as u8, self_check, t_init)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    fatal(format_args!("internal error: {info}"))
}
