//! Direct CPU and chipset access for the BIOS stage: CPUID, port I/O, the
//! TSC and the RNG instructions.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count, _rdtsc};

pub fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let r = __cpuid_count(leaf, sub);
    (r.eax, r.ebx, r.ecx, r.edx)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub huge_1g: bool,
    pub pat: bool,
    pub rdrand: bool,
    pub rdseed: bool,
}

pub fn features() -> Features {
    let max = __cpuid(0).eax;
    let max_ext = __cpuid(0x8000_0000).eax;
    let (_, _, c1, d1) = cpuid(1, 0);
    let (_, b7, _, _) = if max >= 7 { cpuid(7, 0) } else { (0, 0, 0, 0) };
    let (_, _, _, de) = if max_ext >= 0x8000_0001 {
        cpuid(0x8000_0001, 0)
    } else {
        (0, 0, 0, 0)
    };
    Features {
        huge_1g: de & (1 << 26) != 0,
        pat: d1 & (1 << 16) != 0,
        rdrand: c1 & (1 << 30) != 0,
        rdseed: b7 & (1 << 18) != 0,
    }
}

pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: port reads have no memory effects; we touch standard PC ports.
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)) };
    v
}

pub fn outb(port: u16, v: u8) {
    // SAFETY: as inb; writes go only to standard PC ports.
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)) };
}

pub fn rdtsc() -> u64 {
    // SAFETY: RDTSC is always available on x86-64.
    unsafe { _rdtsc() }
}

pub fn rdrand() -> Option<u64> {
    for _ in 0..10 {
        let v: u64;
        let ok: u8;
        // SAFETY: only called when CPUID reports RDRAND.
        unsafe {
            asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack))
        };
        if ok != 0 {
            return Some(v);
        }
    }
    None
}

pub fn rdseed() -> Option<u64> {
    for _ in 0..100 {
        let v: u64;
        let ok: u8;
        // SAFETY: only called when CPUID reports RDSEED.
        unsafe {
            asm!("rdseed {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack))
        };
        if ok != 0 {
            return Some(v);
        }
        core::hint::spin_loop();
    }
    None
}

/// Resets the machine through the keyboard controller, then by a triple
/// fault if that did nothing.
pub fn reboot() -> ! {
    for _ in 0..100_000 {
        if inb(0x64) & 2 == 0 {
            break;
        }
    }
    outb(0x64, 0xfe);
    // SAFETY: loading an empty IDT and faulting resets the CPU.
    unsafe {
        let zero = [0u8; 10];
        asm!("lidt [{}]", "int3", in(reg) zero.as_ptr(), options(noreturn));
    }
}
