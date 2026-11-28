//! Text output: COM1 (what CI reads) and the VGA text screen, written
//! directly. Going through INT 10h for every character would cost two mode
//! switches per byte.

use crate::hw;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

const COM1: u16 = 0x3f8;
static SERIAL: AtomicBool = AtomicBool::new(false);
static VGA_BASE: AtomicU64 = AtomicU64::new(0);
static COLS: AtomicU16 = AtomicU16::new(80);
static ROWS: AtomicU16 = AtomicU16::new(25);
static POS: AtomicU16 = AtomicU16::new(0);

pub fn init() {
    // A UART is there if its scratch register holds a value.
    hw::outb(COM1 + 7, 0x5a);
    if hw::inb(COM1 + 7) == 0x5a {
        hw::outb(COM1 + 1, 0x00); // no interrupts
        hw::outb(COM1 + 3, 0x80); // DLAB
        hw::outb(COM1, 0x01); // 115200 baud
        hw::outb(COM1 + 1, 0x00);
        hw::outb(COM1 + 3, 0x03); // 8N1
        hw::outb(COM1 + 2, 0xc7); // FIFO on, cleared
        hw::outb(COM1 + 4, 0x03); // DTR, RTS
        SERIAL.store(true, Ordering::Relaxed);
    }
    // SAFETY: the BIOS data area is identity mapped and always present.
    let (mode, cols, rows, col, row) = unsafe {
        (
            core::ptr::read_volatile(0x449 as *const u8),
            core::ptr::read_volatile(0x44a as *const u16),
            core::ptr::read_volatile(0x484 as *const u8),
            core::ptr::read_volatile(0x450 as *const u8),
            core::ptr::read_volatile(0x451 as *const u8),
        )
    };
    let base = match mode {
        2 | 3 => 0xb8000,
        7 => 0xb0000,
        _ => 0,
    };
    if base != 0 && (40..=132).contains(&cols) {
        let rows = if (24..=59).contains(&rows) {
            rows as u16 + 1
        } else {
            25
        };
        COLS.store(cols, Ordering::Relaxed);
        ROWS.store(rows, Ordering::Relaxed);
        POS.store(
            (row as u16).min(rows - 1) * cols + col as u16,
            Ordering::Relaxed,
        );
        VGA_BASE.store(base, Ordering::Relaxed);
    }
}

fn serial(b: u8) {
    if !SERIAL.load(Ordering::Relaxed) {
        return;
    }
    for _ in 0..100_000 {
        if hw::inb(COM1 + 5) & 0x20 != 0 {
            break;
        }
    }
    hw::outb(COM1, b);
}

fn vga(b: u8) {
    let base = VGA_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }
    let cols = COLS.load(Ordering::Relaxed) as u64;
    let rows = ROWS.load(Ordering::Relaxed) as u64;
    let mut pos = POS.load(Ordering::Relaxed) as u64;
    match b {
        b'\n' => pos = (pos / cols + 1) * cols,
        b'\r' => pos = pos / cols * cols,
        _ => {
            // SAFETY: inside the text buffer (pos < cols * rows).
            unsafe { core::ptr::write_volatile((base + 2 * pos) as *mut u16, 0x0700 | b as u16) };
            pos += 1;
        }
    }
    if pos >= cols * rows {
        // Scroll one line.
        // SAFETY: moves and clears cells inside the text buffer.
        unsafe {
            core::ptr::copy(
                (base + 2 * cols) as *const u16,
                base as *mut u16,
                (cols * (rows - 1)) as usize,
            );
            for i in 0..cols {
                core::ptr::write_volatile((base + 2 * (cols * (rows - 1) + i)) as *mut u16, 0x0720);
            }
        }
        pos -= cols;
    }
    POS.store(pos as u16, Ordering::Relaxed);
}

pub fn put(b: u8) {
    if b == b'\n' {
        serial(b'\r');
    }
    serial(b);
    vga(b);
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            put(if b.is_ascii() { b } else { b'?' });
        }
        Ok(())
    }
}

pub fn print(args: fmt::Arguments) {
    let _ = Console.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
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
