use core::fmt::{self, Write};

const COM1: u16 = 0x3f8;

fn outb(p: u16, v: u8) {
    // SAFETY: COM1 port I/O.
    unsafe { core::arch::asm!("out dx, al", in("dx") p, in("al") v) };
}
pub fn inb(p: u16) -> u8 {
    let v: u8;
    // SAFETY: port read.
    unsafe { core::arch::asm!("in al, dx", in("dx") p, out("al") v) };
    v
}

pub fn init() {
    outb(COM1 + 1, 0);
    outb(COM1 + 3, 0x80);
    outb(COM1, 1);
    outb(COM1 + 1, 0);
    outb(COM1 + 3, 3);
    outb(COM1 + 2, 0xc7);
    outb(COM1 + 4, 3);
}

struct S;
impl Write for S {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                put(b'\r');
            }
            put(b);
        }
        Ok(())
    }
}
fn put(b: u8) {
    for _ in 0..100_000 {
        if inb(COM1 + 5) & 0x20 != 0 {
            break;
        }
    }
    outb(COM1, b);
}
pub fn print(a: fmt::Arguments) {
    let _ = S.write_fmt(a);
}
#[macro_export]
macro_rules! println {
    ($($t:tt)*) => ($crate::serial::print(format_args!("{}\n", format_args!($($t)*))));
}
