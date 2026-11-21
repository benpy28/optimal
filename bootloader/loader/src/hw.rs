//! Direct CPU and chipset access: CPUID, MSRs, port I/O, timers, RNG
//! instructions, and the serial port.

use core::arch::asm;
use core::arch::x86_64::{__cpuid, __cpuid_count, _rdtsc};

pub fn cpuid(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let r = __cpuid_count(leaf, sub);
    (r.eax, r.ebx, r.ecx, r.edx)
}

pub fn max_leaf() -> u32 {
    __cpuid(0).eax
}

pub fn max_ext_leaf() -> u32 {
    __cpuid(0x8000_0000).eax
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub nx: bool,
    pub huge_1g: bool,
    pub la57: bool,
    pub pat: bool,
    pub x2apic: bool,
    pub rdrand: bool,
    pub rdseed: bool,
    pub apic: bool,
}

pub fn features() -> Features {
    let (_, _, c1, d1) = cpuid(1, 0);
    let (_, b7, c7, _) = if max_leaf() >= 7 { cpuid(7, 0) } else { (0, 0, 0, 0) };
    let (_, _, _, de) = if max_ext_leaf() >= 0x8000_0001 {
        cpuid(0x8000_0001, 0)
    } else {
        (0, 0, 0, 0)
    };
    Features {
        nx: de & (1 << 20) != 0,
        huge_1g: de & (1 << 26) != 0,
        la57: c7 & (1 << 16) != 0,
        pat: d1 & (1 << 16) != 0,
        apic: d1 & (1 << 9) != 0,
        x2apic: c1 & (1 << 21) != 0,
        rdrand: c1 & (1 << 30) != 0,
        rdseed: b7 & (1 << 18) != 0,
    }
}

/// # Safety
/// The MSR must exist on this CPU.
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: caller guarantees the MSR exists.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

/// # Safety
/// The MSR must exist and the value must be valid for it.
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: caller guarantees the write is valid.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack))
    };
}

pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: port reads have no memory effects; the ports we touch are
    // standard PC ports (serial, PIC, ACPI PM timer).
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)) };
    v
}

pub fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: as inb.
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") v, options(nomem, nostack)) };
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
        unsafe { asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack)) };
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
        unsafe { asm!("rdseed {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack)) };
        if ok != 0 {
            return Some(v);
        }
        core::hint::spin_loop();
    }
    None
}

pub fn cli() {
    // SAFETY: disabling interrupts is always allowed in ring 0.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: stops the CPU; interrupts are off so it never wakes.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

const COM1: u16 = 0x3f8;

/// Writes a byte to COM1 (only used after the firmware console is gone).
/// The port is set up by the firmware on every machine we test; on machines
/// without one the writes go nowhere.
pub fn serial_write(b: u8) {
    for _ in 0..100_000 {
        if inb(COM1 + 5) & 0x20 != 0 {
            break;
        }
    }
    outb(COM1, b);
}

/// 32-bit MMIO read.
///
/// # Safety
/// `addr` must be a mapped device register.
pub unsafe fn mmio_read32(addr: u64) -> u32 {
    // SAFETY: caller guarantees addr is a mapped register.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// # Safety
/// `addr` must be a mapped device register and `v` valid for it.
pub unsafe fn mmio_write32(addr: u64, v: u32) {
    // SAFETY: caller guarantees addr is a mapped register.
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

/// # Safety
/// `addr` must be a mapped device register.
pub unsafe fn mmio_read64(addr: u64) -> u64 {
    // SAFETY: caller guarantees addr is a mapped register.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// # Safety
/// `addr` must be a mapped device register and `v` valid for it.
pub unsafe fn mmio_write64(addr: u64, v: u64) {
    // SAFETY: caller guarantees addr is a mapped register.
    unsafe { core::ptr::write_volatile(addr as *mut u64, v) }
}
