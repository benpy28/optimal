//! COM1 serial port (16550 UART). The kernel's first voice: it works before
//! memory management, interrupts or graphics exist, and QEMU and CI read it.

use core::fmt::{self, Write};

use crate::arch::port::{inb, outb};

const COM1: u16 = 0x3F8;

/// Set COM1 to 115200 baud, 8 data bits, no parity, 1 stop bit.
pub fn init() {
    // SAFETY: COM1 is the standard PC serial port; these writes follow the
    // 16550 programming sequence and touch nothing else.
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
    // SAFETY: reading the line status register and writing the data register
    // of COM1 are the documented way to send a byte.
    unsafe {
        // Bounded wait: with no UART present (some real PCs), give up rather
        // than hang the kernel forever.
        let mut spins = 0u32;
        while inb(COM1 + 5) & 0x20 == 0 && spins < 100_000 {
            core::hint::spin_loop();
            spins += 1;
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
