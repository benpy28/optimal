//! COM1 serial port (16550 UART). This is the kernel's first voice: it works
//! before memory management, interrupts or graphics exist, and QEMU and CI can
//! read it.

use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    };
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    value
}

/// Set COM1 to 115200 baud, 8 data bits, no parity, 1 stop bit.
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // interrupts off
        outb(COM1 + 3, 0x80); // enable divisor latch
        outb(COM1, 0x01); // divisor 1 = 115200 baud (low byte)
        outb(COM1 + 1, 0x00); //                     (high byte)
        outb(COM1 + 3, 0x03); // 8N1, latch off
        outb(COM1 + 2, 0xC7); // FIFO on, cleared, 14-byte threshold
        outb(COM1 + 4, 0x03); // DTR + RTS
    }
}

fn write_byte(byte: u8) {
    unsafe {
        // Wait until the transmit buffer is empty.
        while inb(COM1 + 5) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        outb(COM1, byte);
    }
}

pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::serial::Serial, $($arg)*);
    }};
}
