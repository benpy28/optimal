//! Control registers, CPU feature detection and the security baseline.

use core::arch::asm;
use core::arch::x86_64::__cpuid_count;

pub fn read_cr0() -> u64 {
    let v;
    // SAFETY: reading CR0 has no side effects at ring 0.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr2() -> u64 {
    let v;
    // SAFETY: reading CR2 has no side effects at ring 0.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr3() -> u64 {
    let v;
    // SAFETY: reading CR3 has no side effects at ring 0.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr4() -> u64 {
    let v;
    // SAFETY: reading CR4 has no side effects at ring 0.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// The new CR0 must keep paging and protected mode enabled.
unsafe fn write_cr0(v: u64) {
    // SAFETY: the caller keeps the CPU in a valid mode.
    unsafe { asm!("mov cr0, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// # Safety
/// The new CR4 must keep PAE/long-mode requirements and only enable features
/// the CPU reports.
unsafe fn write_cr4(v: u64) {
    // SAFETY: the caller only sets supported bits.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

pub fn read_msr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: only called with architectural MSRs this kernel checked support for.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// # Safety
/// Writing an MSR changes CPU behaviour; the value must be valid for `msr`.
pub unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees the value is valid for this MSR.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nostack, preserves_flags))
    };
}

pub const MSR_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;
const CR0_WP: u64 = 1 << 16;
const CR4_UMIP: u64 = 1 << 11;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub nx: bool,
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
}

pub fn features() -> Features {
    let max_ext = __cpuid_count(0x8000_0000, 0).eax;
    let ext1 = if max_ext >= 0x8000_0001 {
        __cpuid_count(0x8000_0001, 0).edx
    } else {
        0
    };
    let max_basic = __cpuid_count(0, 0).eax;
    let (ebx7, ecx7) = if max_basic >= 7 {
        let r = __cpuid_count(7, 0);
        (r.ebx, r.ecx)
    } else {
        (0, 0)
    };
    Features {
        nx: ext1 & (1 << 20) != 0,
        smep: ebx7 & (1 << 7) != 0,
        smap: ebx7 & (1 << 20) != 0,
        umip: ecx7 & (1 << 2) != 0,
    }
}

/// What the security baseline actually turned on, for the boot log and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hardening {
    pub nx: bool,
    pub wp: bool,
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
}

/// Turn on every hardware protection this CPU supports. Called once on each
/// CPU during early boot, before anything else can run.
pub fn enable_hardening() -> Hardening {
    let f = features();
    let mut h = Hardening::default();

    if f.nx {
        // SAFETY: NXE is supported (CPUID) and only adds the no-execute bit meaning.
        unsafe { write_msr(MSR_EFER, read_msr(MSR_EFER) | EFER_NXE) };
        h.nx = true;
    }

    // SAFETY: CR0.WP makes the kernel respect read-only pages; paging stays on.
    unsafe { write_cr0(read_cr0() | CR0_WP) };
    h.wp = true;

    let mut cr4 = read_cr4();
    if f.smep {
        cr4 |= CR4_SMEP;
        h.smep = true;
    }
    if f.smap {
        cr4 |= CR4_SMAP;
        h.smap = true;
    }
    if f.umip {
        cr4 |= CR4_UMIP;
        h.umip = true;
    }
    // SAFETY: only bits reported as supported by CPUID are set.
    unsafe { write_cr4(cr4) };
    h
}

pub fn halt_forever() -> ! {
    loop {
        // SAFETY: disabling interrupts and halting is always valid at ring 0.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

const RFLAGS_IF: u64 = 1 << 9;

/// Disable interrupts on this CPU. Returns whether they were enabled.
pub fn interrupts_disable() -> bool {
    let flags: u64;
    // SAFETY: reading RFLAGS and clearing IF is always valid at ring 0.
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags, options(nomem, preserves_flags)) };
    flags & RFLAGS_IF != 0
}

/// Enable interrupts on this CPU.
pub fn interrupts_enable() {
    // SAFETY: an IDT that handles every vector is loaded before anything can
    // take a lock (arch::init_cpu runs first).
    unsafe { asm!("sti", options(nomem, nostack)) };
}

pub fn interrupts_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading RFLAGS has no side effects.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };
    flags & RFLAGS_IF != 0
}
