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
pub unsafe fn write_cr4(v: u64) {
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
pub const MSR_PAT: u32 = 0x277;
const CR4_PGE: u64 = 1 << 7;
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
    /// 1 GiB pages (PDPE1GB).
    pub huge_1g: bool,
    pub rdrand: bool,
    pub pat: bool,
    pub pge: bool,
}

pub fn features() -> Features {
    let max_ext = __cpuid_count(0x8000_0000, 0).eax;
    let ext1 = if max_ext >= 0x8000_0001 {
        __cpuid_count(0x8000_0001, 0).edx
    } else {
        0
    };
    let max_basic = __cpuid_count(0, 0).eax;
    let leaf1 = __cpuid_count(1, 0);
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
        huge_1g: ext1 & (1 << 26) != 0,
        rdrand: leaf1.ecx & (1 << 30) != 0,
        pat: leaf1.edx & (1 << 16) != 0,
        pge: leaf1.edx & (1 << 13) != 0,
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

#[cfg(feature = "ktest")]
pub fn interrupts_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading RFLAGS has no side effects.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };
    flags & RFLAGS_IF != 0
}

/// Is the no-execute bit in force (EFER.NXE)?
pub fn nx_enabled() -> bool {
    read_msr(MSR_EFER) & EFER_NXE != 0
}

/// Switch to the page tables rooted at `root`, then flush every TLB entry,
/// global ones included, and turn on global pages if the CPU has them.
///
/// # Safety
/// The new tables must map the code running now, its stack, and
/// everything it uses next, at the same virtual addresses.
pub unsafe fn load_page_tables(root: u64) {
    let cr4 = read_cr4();
    // SAFETY: forwarded from the caller; toggling PGE flushes global entries.
    unsafe {
        asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
        write_cr4(cr4 & !CR4_PGE);
        let pge = if features().pge { CR4_PGE } else { 0 };
        write_cr4((cr4 & !CR4_PGE) | pge);
    }
}

/// Flush the TLB entry for one page on this CPU.
pub fn flush_page(virt: u64) {
    // SAFETY: invlpg only drops a cached translation.
    unsafe { asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
}

/// A hardware random number (RDRAND), or None if the CPU has no RDRAND or
/// it kept failing (the SDM suggests 10 retries).
pub fn rdrand() -> Option<u64> {
    if !features().rdrand {
        return None;
    }
    for _ in 0..10 {
        let v: u64;
        let ok: u8;
        // SAFETY: RDRAND is supported (CPUID); it only writes the operands.
        unsafe {
            asm!("rdrand {}", "setc {}", out(reg) v, out(reg_byte) ok, options(nomem, nostack))
        };
        if ok != 0 {
            return Some(v);
        }
    }
    None
}

pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: RDTSC has no side effects (CR4.TSD is clear at ring 0 anyway).
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    (u64::from(hi) << 32) | u64::from(lo)
}
