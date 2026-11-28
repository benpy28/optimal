//! Optimal Boot: the UEFI bootloader for Optimal.
//!
//! Boot flow: verify the signed config, pick an entry, load and verify every
//! file against the hashes in the config, measure everything into the TPM,
//! load the kernel ELF (with KASLR), build the Limine-protocol responses and
//! page tables, exit boot services and hand over. Any failure stops the boot
//! with a clear message; nothing unverified ever runs.

#![no_std]
#![no_main]

extern crate alloc;

mod boot;
mod console;
mod efi;
mod fs;
mod gop;
mod handoff;
mod hw;
mod memory;
mod menu;
mod mp;
mod platform;
mod tpm;

use core::panic::PanicInfo;

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

pub const NAME: &str = "Optimal Boot";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stops the boot with a message. While boot services run, we return to the
/// firmware afterwards so it can try the next boot option.
pub fn fatal(msg: core::fmt::Arguments) -> efi::Status {
    println!();
    println!("OPTIMAL-BOOT: FATAL: {msg}");
    println!("The system was not booted. Nothing unverified was run.");
    if console::boot_services_active() {
        println!("Press any key to return to the firmware (or wait 30 seconds).");
        menu::wait_key(30_000);
    }
    efi::LOAD_ERROR
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: efi::Handle, st: *mut efi::SystemTable) -> efi::Status {
    console::init(st);
    match boot::run(image) {
        Ok(never) => never,
        Err(status) => status,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("OPTIMAL-BOOT: FATAL: internal error: {info}");
    if console::boot_services_active() {
        menu::wait_key(30_000);
    }
    (console::runtime_services().reset_system)(
        efi::RESET_COLD,
        efi::LOAD_ERROR,
        0,
        core::ptr::null(),
    )
}
