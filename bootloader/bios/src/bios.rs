//! Calling BIOS interrupts from long mode, through the real-mode thunk in
//! stage2.s. Each call switches to real mode, runs `INT n` with the
//! registers given, and switches back; interrupts are enabled only while
//! the BIOS runs.

use core::ptr::{addr_of, addr_of_mut};

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Regs {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub eflags: u32,
    pub ds: u16,
    pub es: u16,
}

const CF: u32 = 1;

impl Regs {
    pub fn carry(&self) -> bool {
        self.eflags & CF != 0
    }
    pub fn ah(&self) -> u8 {
        (self.eax >> 8) as u8
    }
}

unsafe extern "sysv64" {
    fn ob_bios_int(vector: u32);
    static mut ob_regs: Regs;
    static mut ob_low_buf: [u8; LOW_BUF_LEN];
}

pub const LOW_BUF_LEN: usize = 1024;
/// Where the thunk's buffers live: a real-mode-addressable scratch area.
pub fn low_buf() -> u64 {
    addr_of!(ob_low_buf) as u64
}

/// Disk transfers go through this 64 KiB bounce buffer below 1 MiB.
pub const BOUNCE: u64 = 0x70000;
pub const BOUNCE_LEN: usize = 0x10000;

/// Runs `INT vector` with `r`, and returns the registers the BIOS left.
pub fn int(vector: u8, r: Regs) -> Regs {
    // SAFETY: single-threaded, interrupts off; the thunk reads and writes
    // ob_regs, and saves and restores every callee-saved register.
    unsafe {
        addr_of_mut!(ob_regs).write(r);
        ob_bios_int(vector as u32);
        addr_of!(ob_regs).read()
    }
}

/// Real-mode segment:offset for an address below 1 MiB.
pub fn seg_off(addr: u64) -> (u16, u16) {
    debug_assert!(addr < 0x10_0000);
    ((addr >> 4) as u16, (addr & 0xf) as u16)
}

/// Waits `us` microseconds with INT 15h AH=86h (the BIOS keeps time with
/// interrupts on while it waits). False if the BIOS does not support it.
pub fn wait_us(us: u32) -> bool {
    let r = int(
        0x15,
        Regs {
            eax: 0x8600,
            ecx: us >> 16,
            edx: us & 0xffff,
            ..Default::default()
        },
    );
    !r.carry()
}

/// A key if one is waiting (INT 16h AH=01h, then AH=00h to take it):
/// (scan code, ASCII).
pub fn read_key() -> Option<(u8, u8)> {
    let r = int(
        0x16,
        Regs {
            eax: 0x0100,
            ..Default::default()
        },
    );
    if r.eflags & (1 << 6) != 0 {
        return None; // ZF: no key
    }
    let r = int(0x16, Regs::default());
    Some(((r.eax >> 8) as u8, r.eax as u8))
}

/// INT 18h: the BIOS tries the next boot device (or shows its own
/// message). If it returns, reboot through the keyboard controller.
pub fn next_boot_device() -> ! {
    int(0x18, Regs::default());
    int(0x19, Regs::default());
    crate::hw::reboot()
}
