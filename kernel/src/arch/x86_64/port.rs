//! x86 I/O port access.

use core::arch::asm;

/// # Safety
/// Writing to an I/O port can reconfigure hardware; the caller must know what
/// device lives at `port` and that the write is valid for it.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller guarantees the port write is valid.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    };
}

/// # Safety
/// Reading some I/O ports has side effects (for example clearing a status
/// flag); the caller must know the read is valid for the device at `port`.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller guarantees the port read is valid.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    value
}
