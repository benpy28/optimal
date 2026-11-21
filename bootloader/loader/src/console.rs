//! Text output. While boot services run, everything goes through the
//! firmware console (screen and, on most firmware, serial). After
//! ExitBootServices only the COM1 serial port is left, for fatal errors.

use crate::efi;
use crate::hw;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

static SYSTEM_TABLE: AtomicPtr<efi::SystemTable> = AtomicPtr::new(core::ptr::null_mut());
static BOOT_SERVICES_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn init(st: *mut efi::SystemTable) {
    SYSTEM_TABLE.store(st, Ordering::SeqCst);
    BOOT_SERVICES_ACTIVE.store(true, Ordering::SeqCst);
}

pub fn system_table() -> &'static efi::SystemTable {
    // SAFETY: set once in init() from the pointer the firmware passed to
    // efi_main, which stays valid for the whole boot.
    unsafe { &*SYSTEM_TABLE.load(Ordering::SeqCst) }
}

pub fn boot_services() -> &'static efi::BootServices {
    assert!(boot_services_active(), "boot services used after exit");
    // SAFETY: valid until ExitBootServices, which the assert above guards.
    unsafe { &*system_table().boot_services }
}

pub fn runtime_services() -> &'static efi::RuntimeServices {
    // SAFETY: runtime services stay mapped (identity) until the kernel runs.
    unsafe { &*system_table().runtime_services }
}

pub fn boot_services_active() -> bool {
    BOOT_SERVICES_ACTIVE.load(Ordering::SeqCst)
}

pub fn boot_services_exited() {
    BOOT_SERVICES_ACTIVE.store(false, Ordering::SeqCst);
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if boot_services_active() {
            let out = system_table().con_out;
            if out.is_null() {
                return Ok(());
            }
            let mut buf = [0u16; 128];
            let mut n = 0;
            let flush = |buf: &mut [u16; 128], n: &mut usize| {
                buf[*n] = 0;
                // SAFETY: con_out is the firmware console protocol and buf is
                // NUL-terminated UCS-2.
                unsafe { ((*out).output_string)(out, buf.as_ptr()) };
                *n = 0;
            };
            for c in s.chars() {
                if n + 3 >= buf.len() {
                    flush(&mut buf, &mut n);
                }
                if c == '\n' {
                    buf[n] = b'\r' as u16;
                    n += 1;
                }
                buf[n] = if (c as u32) < 0x80 { c as u16 } else { b'?' as u16 };
                n += 1;
            }
            if n > 0 {
                flush(&mut buf, &mut n);
            }
        } else {
            for b in s.bytes() {
                if b == b'\n' {
                    hw::serial_write(b'\r');
                }
                hw::serial_write(b);
            }
        }
        Ok(())
    }
}

pub fn print(args: fmt::Arguments) {
    let _ = Console.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Lowercase hex for byte strings.
pub struct Hex<'a>(pub &'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
