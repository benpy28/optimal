//! The MP request: starting the application processors (APs) and parking
//! them where the kernel can release them (Limine protocol, "MP
//! (Multiprocessor) Feature"; Intel SDM Vol. 3A 8.4.4 for the start-up
//! sequence and 11.11.8 for MTRR synchronisation).
//!
//! An AP's path, one AP at a time, after ExitBootServices:
//! 1. INIT, then two start-up IPIs, send it to the start page below 1 MiB in
//!    real mode. That code (below) goes to long mode on a copy of the
//!    firmware's top-level page table, which identity-maps everything, so
//!    loader code can run there.
//! 2. `ap_main` (Rust, the AP's own stack): the same APIC mode as the BSP,
//!    the BSP's MTRRs, and from base revision 5 the same local APIC set-up
//!    the BSP got (platform::init_lapic).
//! 3. The handoff trampoline (handoff.rs), with this AP's own Block: the
//!    final page tables, CR0/CR4/EFER/PAT, a GDT of its own (its TSS
//!    descriptor goes busy when loaded, so it cannot be shared), and then the
//!    parking loop in the handoff page, the one executable page outside the
//!    kernel. There it stores its stack pointer in `reserved` (which tells
//!    the BSP it is parked) and waits for `goto_address`.
//!
//! The BSP removes the identity mappings only after every AP has parked, so
//! no AP is ever running from a page that is about to disappear. A parked
//! AP reloads CR3 before jumping, which drops any TLB entries it still holds
//! for those identity pages.

use crate::efi;
use crate::handoff::{self, Block};
use crate::hw::{self, mmio_read32, mmio_write32};
use crate::memory;
use crate::platform::{self, Platform};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
use optimal_boot_core::mp::{self as cmp, Cpu};

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_EN: u64 = 1 << 11;
const APIC_EXTD: u64 = 1 << 10;
const PAGE: u64 = 4096;

// ---------------------------------------------------------------- APIC mode

/// The BSP's APIC id, in whatever mode the APIC is in now.
fn own_apic_id() -> u32 {
    // SAFETY: IA32_APIC_BASE exists wherever there is an APIC (the caller
    // checked for a MADT or CPUID.APIC).
    let base = unsafe { hw::rdmsr(IA32_APIC_BASE) };
    if base & APIC_EXTD != 0 {
        // SAFETY: x2APIC mode is on, so the ID MSR exists.
        unsafe { hw::rdmsr(0x802) as u32 }
    } else if base & APIC_EN != 0 {
        // SAFETY: the firmware identity-maps the xAPIC page.
        unsafe { mmio_read32((base & 0x000f_ffff_ffff_f000) + 0x20) >> 24 }
    } else {
        // A disabled APIC: CPUID's initial APIC id is what it will report.
        hw::cpuid(1, 0).1 >> 24
    }
}

/// Puts this CPU's local APIC in x2APIC mode (`x2`) or xAPIC mode, with
/// the legal transitions only (SDM 10.12.5: x2APIC can only be left by
/// disabling the APIC). Returns whether the CPU now reports that mode.
pub fn set_mode(x2: bool) -> bool {
    // SAFETY: IA32_APIC_BASE exists on every CPU with an APIC, and each
    // write below is a transition the SDM allows.
    unsafe {
        let mut b = hw::rdmsr(IA32_APIC_BASE);
        if b & APIC_EN == 0 {
            b |= APIC_EN;
            hw::wrmsr(IA32_APIC_BASE, b);
        }
        let on = b & APIC_EXTD != 0;
        if x2 && !on {
            hw::wrmsr(IA32_APIC_BASE, b | APIC_EXTD);
        } else if !x2 && on {
            hw::wrmsr(IA32_APIC_BASE, b & !(APIC_EN | APIC_EXTD));
            hw::wrmsr(IA32_APIC_BASE, (b & !APIC_EXTD) | APIC_EN);
        }
        let now = hw::rdmsr(IA32_APIC_BASE);
        now & APIC_EN != 0 && (now & APIC_EXTD != 0) == x2
    }
}

/// IA32_XAPIC_DISABLE_STATUS.LEGACY_XAPIC_DISABLED: x2APIC is locked on.
fn x2apic_locked() -> bool {
    if hw::max_leaf() < 7 || hw::cpuid(7, 0).3 & (1 << 29) == 0 {
        return false;
    }
    // SAFETY: CPUID.7.0:EDX[29] enumerates IA32_ARCH_CAPABILITIES.
    let caps = unsafe { hw::rdmsr(0x10a) };
    // SAFETY: ARCH_CAPABILITIES bit 21 enumerates IA32_XAPIC_DISABLE_STATUS.
    caps & (1 << 21) != 0 && unsafe { hw::rdmsr(0xbd) } & 1 != 0
}

// ---------------------------------------------------------------- MTRRs

const FIXED_MTRRS: [u32; 11] = [
    0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
];

/// A copy of one CPU's MTRRs.
#[derive(Clone, Copy)]
pub struct Mtrrs {
    present: bool,
    def: u64,
    vcnt: u32,
    var: [u64; 32],
    fixed: Option<[u64; 11]>,
}

fn mtrr_cap() -> Option<u64> {
    // SAFETY: CPUID.1:EDX[12] enumerates the MTRR MSRs.
    (hw::cpuid(1, 0).3 & (1 << 12) != 0).then(|| unsafe { hw::rdmsr(0xfe) })
}

pub fn read_mtrrs() -> Mtrrs {
    let mut m = Mtrrs {
        present: false,
        def: 0,
        vcnt: 0,
        var: [0; 32],
        fixed: None,
    };
    let Some(cap) = mtrr_cap() else { return m };
    m.present = true;
    m.vcnt = (cap & 0xff).min(16) as u32;
    // SAFETY: MTRRcap says these exist.
    unsafe {
        m.def = hw::rdmsr(0x2ff);
        for i in 0..2 * m.vcnt {
            m.var[i as usize] = hw::rdmsr(0x200 + i);
        }
        if cap & (1 << 8) != 0 {
            let mut f = [0; 11];
            for (k, &msr) in FIXED_MTRRS.iter().enumerate() {
                f[k] = hw::rdmsr(msr);
            }
            m.fixed = Some(f);
        }
    }
    m
}

/// Loads `m` into this CPU's MTRRs with caching off, as SDM 11.11.7.2
/// prescribes (CD on, WBINVD, MTRRs off, write, WBINVD, MTRRs on, CD off).
///
/// # Safety
/// Interrupts off, and no other code on this CPU relies on its caches.
unsafe fn load_mtrrs(m: &Mtrrs) {
    let Some(cap) = mtrr_cap().filter(|_| m.present) else {
        return;
    };
    let vcnt = ((cap & 0xff) as u32).min(m.vcnt);
    // SAFETY: as documented; every MSR written is enumerated by MTRRcap.
    unsafe {
        let cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0);
        asm!("mov cr0, {}", in(reg) (cr0 | 1 << 30) & !(1 << 29));
        asm!("wbinvd");
        asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _);
        hw::wrmsr(0x2ff, 0);
        for i in 0..2 * vcnt {
            hw::wrmsr(0x200 + i, m.var[i as usize]);
        }
        if let (Some(f), true) = (m.fixed, cap & (1 << 8) != 0) {
            for (k, &msr) in FIXED_MTRRS.iter().enumerate() {
                hw::wrmsr(msr, f[k]);
            }
        }
        asm!("wbinvd");
        asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _);
        hw::wrmsr(0x2ff, m.def);
        asm!("mov cr0, {}", in(reg) cr0);
    }
}

// ---------------------------------------------------------------- start page

/// The start page's parameters, at START_PARAMS in the page (below 1 MiB).
/// The real-mode code addresses them as DS:offset, the 32- and 64-bit code
/// as the page's address plus offset, so the layout is fixed.
#[repr(C)]
struct StartParams {
    started: u32,    // 0x00: set by the AP as its first act
    _r0: u32,        // 0x04
    gdtr: [u16; 4],  // 0x08: limit, base (24-bit form is enough below 1 MiB)
    far32: [u16; 4], // 0x10: offset (u32), selector 0x18
    entry64: u32,    // 0x18: page + offset of the 64-bit code
    _r1: u32,        // 0x1c
    cr3: u64,        // 0x20: the firmware page tables' root, below 4 GiB
    cr4: u64,        // 0x28
    efer: u64,       // 0x30
    cr0: u64,        // 0x38
    stack: u64,      // 0x40: identity address of this AP's stack top
    arg: u64,        // 0x48: &ApCall
    entry: u64,      // 0x50: ap_main
    gdt: [u64; 7],   // 0x58
}

const START_PARAMS: u64 = 0xe00;
const _: () = {
    assert!(core::mem::offset_of!(StartParams, gdtr) == 0x08);
    assert!(core::mem::offset_of!(StartParams, far32) == 0x10);
    assert!(core::mem::offset_of!(StartParams, entry64) == 0x18);
    assert!(core::mem::offset_of!(StartParams, cr3) == 0x20);
    assert!(core::mem::offset_of!(StartParams, cr4) == 0x28);
    assert!(core::mem::offset_of!(StartParams, efer) == 0x30);
    assert!(core::mem::offset_of!(StartParams, cr0) == 0x38);
    assert!(core::mem::offset_of!(StartParams, stack) == 0x40);
    assert!(core::mem::offset_of!(StartParams, arg) == 0x48);
    assert!(core::mem::offset_of!(StartParams, entry) == 0x50);
    assert!(core::mem::offset_of!(StartParams, gdt) == 0x58);
    assert!(START_PARAMS as usize + core::mem::size_of::<StartParams>() <= 0xf00);
};

unsafe extern "C" {
    static ob_ap16_start: u8;
    static ob_ap16_end: u8;
    static ob_ap32: u8;
    static ob_ap64: u8;
}

// Entered by the start-up IPI at CS = page >> 4, IP = 0, in real mode.
// The top of the page (above the parameters) is the stack for the far
// return into 64-bit mode.
global_asm!(
    r#"
    .section .text.ob_ap16, "ax"
    .global ob_ap16_start
    .global ob_ap16_end
    .global ob_ap32
    .global ob_ap64
    .code16
ob_ap16_start:
    cli
    cld
    mov ax, cs
    mov ds, ax
    mov dword ptr [0xe00], 1
    lgdt [0xe08]
    xor ebx, ebx
    mov bx, cs
    shl ebx, 4
    mov eax, cr0
    or al, 1
    mov cr0, eax
    // jmp far dword [0xe10]: to the 32-bit code, selector 0x18.
    .byte 0x66, 0xff, 0x2e
    .word 0xe10
    .code32
ob_ap32:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    lea esp, [ebx + 0x1000]
    mov eax, [ebx + 0xe28]
    mov cr4, eax
    mov eax, [ebx + 0xe20]
    mov cr3, eax
    mov ecx, 0xc0000080
    mov eax, [ebx + 0xe30]
    mov edx, [ebx + 0xe34]
    wrmsr
    mov eax, [ebx + 0xe38]
    mov cr0, eax
    push 0x28
    push dword ptr [ebx + 0xe18]
    retf
    .code64
ob_ap64:
    mov ebx, ebx
    mov rsp, [rbx + 0xe40]
    mov rdi, [rbx + 0xe48]
    mov rax, [rbx + 0xe50]
    call rax
2:
    cli
    hlt
    jmp 2b
ob_ap16_end:
    .text
"#
);

/// The start-up code, and the offsets of its 32- and 64-bit parts.
fn start_code() -> (&'static [u8], u64, u64) {
    // SAFETY: the symbols delimit the global_asm block above.
    unsafe {
        let s = &raw const ob_ap16_start;
        let e = &raw const ob_ap16_end;
        (
            core::slice::from_raw_parts(s, e.offset_from(s) as usize),
            (&raw const ob_ap32).offset_from(s) as u64,
            (&raw const ob_ap64).offset_from(s) as u64,
        )
    }
}

// ---------------------------------------------------------------- the AP side

/// What every AP's `ap_main` needs; shared, read-only while APs run.
#[repr(C)]
struct ApCall {
    x2: bool,
    rev: u64,
    plat: *const Platform,
    mtrr: Mtrrs,
    /// Identity address of the AP Block (handoff.rs) for the AP starting now.
    block: u64,
    /// Identity address of the handoff trampoline.
    tramp: u64,
}

extern "sysv64" fn ap_main(c: &ApCall) -> ! {
    // Nothing here may allocate, print or panic: boot services are gone and
    // the BSP is busy-waiting for this AP to park.
    set_mode(c.x2);
    // SAFETY: interrupts are off on a freshly started AP.
    unsafe { load_mtrrs(&c.mtrr) };
    if c.rev >= 5 {
        // SAFETY: the BSP's Platform outlives the boot and is only read.
        platform::init_lapic(unsafe { &*c.plat });
    }
    // SAFETY: the trampoline is in place and the Block is filled for us.
    let enter: extern "sysv64" fn(u64) -> ! = unsafe { core::mem::transmute(c.tramp as usize) };
    enter(c.block)
}

// ---------------------------------------------------------------- the BSP side

/// Everything decided and allocated while boot services still run.
pub struct Plan {
    pub x2: bool,
    pub bsp: u32,
    /// Every processor to report, BSP included, in MADT order.
    pub cpus: Vec<Cpu>,
    /// Why no AP will be started, when one would have been.
    pub no_start: Option<&'static str>,
    start_page: u64,
    root_page: u64,
    stacks: u64,
    stack_size: u64,
    tsc_hz: u64,
    call: Box<ApCall>,
    /// Per CPU: identity addresses of its limine_mp_info and own GDT (the
    /// caller fills them from the response arena), whether it parked, and
    /// room for the response's pointer array. Allocated here because
    /// nothing can be allocated after ExitBootServices.
    pub infos: Vec<u64>,
    pub gdts: Vec<u64>,
    pub ok: Vec<bool>,
    pub up: Vec<u64>,
}

impl Plan {
    /// Bytes of response arena it needs: the response, the pointer array,
    /// and per CPU its info struct and an own GDT.
    pub fn arena_bytes(&self) -> u64 {
        64 + self.cpus.len() as u64 * (8 + 32 + 8 * handoff::GDT.len() as u64 + 8)
    }
}

/// Decides the APIC mode and the processors, and allocates the start page,
/// the AP block and the AP stacks. Err is a reason to refuse the boot.
pub fn prepare(
    plat: &Platform,
    asked_x2: bool,
    stack_size: u64,
    rev: u64,
) -> Result<Plan, &'static str> {
    let has_apic = hw::features().apic || plat.madt.is_some();
    let mut madt_cpus = Vec::new();
    if let Some(m) = &plat.madt {
        m.processors(&platform::Identity, |c| madt_cpus.push(c));
    }
    // SAFETY: as own_apic_id.
    let fw_x2 = has_apic && unsafe { hw::rdmsr(IA32_APIC_BASE) } & APIC_EXTD != 0;
    let cpu_x2 = hw::cpuid(1, 0).2 & (1 << 21) != 0;
    let max_id = madt_cpus
        .iter()
        .filter(|c| c.enabled)
        .map(|c| c.apic_id)
        .max()
        .unwrap_or(0);
    let x2 = has_apic
        && cmp::apic_mode(asked_x2, cpu_x2, fw_x2, fw_x2 && x2apic_locked(), max_id).map_err(
            |_| {
                "the firmware left x2APIC on and it cannot be turned off, but the kernel did not ask for x2APIC (the Limine protocol forbids booting it)"
            },
        )?;
    let bsp = if has_apic { own_apic_id() } else { 0 };
    let mut cpus = alloc::vec![Cpu::default(); madt_cpus.len() + 1];
    let n = cmp::select(madt_cpus.iter().copied(), x2, bsp, &mut cpus);
    cpus.truncate(n);
    let aps = cpus.len() as u64 - 1;
    let tsc_hz = platform::tsc_frequency(plat).unwrap_or_else(|| {
        let t0 = hw::rdtsc();
        (crate::console::boot_services().stall)(10_000);
        (hw::rdtsc() - t0) * 100
    });
    let call = Box::new(ApCall {
        x2,
        rev,
        plat: plat as *const Platform,
        mtrr: read_mtrrs(),
        block: 0,
        tramp: 0,
    });
    let n = cpus.len();
    let mut plan = Plan {
        x2,
        bsp,
        cpus,
        infos: alloc::vec![0; n],
        gdts: alloc::vec![0; n],
        ok: alloc::vec![false; n],
        up: alloc::vec![0; n],
        no_start: None,
        start_page: 0,
        root_page: 0,
        stacks: 0,
        stack_size,
        tsc_hz,
        call,
    };
    if aps == 0 {
        return Ok(plan);
    }
    if !has_apic {
        plan.no_start = Some("no local APIC to send start-up IPIs with");
        return Ok(plan);
    }
    // The start page: anywhere in conventional memory below 1 MiB, but not
    // page 0. Freed again if it lands there.
    let mut low = memory::alloc_pages(1, efi::LOADER_CODE, Some(0x9_ffff));
    if low == Some(0) {
        low = memory::alloc_pages(1, efi::LOADER_CODE, Some(0x9_ffff));
        memory::free_pages(0, 1);
    }
    let root = memory::alloc_pages(1, efi::LOADER_DATA, Some(0xffff_f000));
    let stacks = memory::alloc_pages(aps * stack_size.div_ceil(PAGE), efi::LOADER_DATA, None);
    match (low, root, stacks) {
        (Some(l), Some(r), Some(s)) if cmp::startup_vector(l).is_some() => {
            plan.start_page = l;
            plan.root_page = r;
            plan.stacks = s;
        }
        _ => {
            plan.no_start =
                Some("no free memory below 1 MiB (or for AP stacks) to start them from");
        }
    }
    Ok(plan)
}

fn delay_us(hz: u64, us: u64) {
    let t0 = hw::rdtsc();
    let ticks = (hz as u128 * us as u128 / 1_000_000) as u64;
    while hw::rdtsc().wrapping_sub(t0) < ticks {
        core::hint::spin_loop();
    }
}

/// Sends one IPI to `dest` and, in xAPIC mode, waits for it to leave.
fn ipi(x2: bool, apic: u64, dest: u32, low: u32, hz: u64) -> bool {
    let (hi, lo) = cmp::icr(x2, dest, low);
    if x2 {
        // SAFETY: x2APIC mode: the ICR is MSR 0x830, written in one go.
        unsafe { hw::wrmsr(0x830, (hi as u64) << 32 | lo as u64) };
        return true;
    }
    // SAFETY: the firmware identity-maps the xAPIC page.
    unsafe {
        mmio_write32(apic + 0x310, hi);
        mmio_write32(apic + 0x300, lo);
    }
    let t0 = hw::rdtsc();
    // SAFETY: as above.
    while unsafe { mmio_read32(apic + 0x300) } & cmp::ICR_PENDING != 0 {
        if hw::rdtsc().wrapping_sub(t0) > hz / 10 {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Where the parked APs end up, and the CPU state they take with them.
pub struct Parking {
    /// Identity address of the BSP's filled-in Block. Each AP gets a copy
    /// in the same page (the only Block page the final tables identity-map),
    /// one at a time, and the BSP's own is put back afterwards.
    pub block: u64,
    /// Identity address of the trampoline page (code already copied in).
    pub tramp: u64,
    /// The parking loop's address in the handoff alias.
    pub park: u64,
    pub hhdm: u64,
}

/// Starts every AP in `plan.cpus` and parks it, using `plan.infos[i]` and
/// `plan.gdts[i]` for CPU i, and sets `plan.ok[i]` for each CPU that is
/// parked (the BSP counts as parked).
///
/// Runs after ExitBootServices, on the firmware's page tables, with
/// interrupts off. Allocates nothing.
pub fn start_all(plan: &mut Plan, p: &Parking) {
    let n = plan.cpus.len();
    for (i, c) in plan.cpus.iter().enumerate() {
        plan.ok[i] = c.apic_id == plan.bsp;
    }
    if plan.no_start.is_some() || n == 1 {
        return;
    }
    let (info, gdt) = (&plan.infos, &plan.gdts);
    let hz = plan.tsc_hz.max(1_000_000);
    // SAFETY: the BSP's APIC is in plan.x2's mode (set_mode ran).
    let apic = unsafe { hw::rdmsr(IA32_APIC_BASE) } & 0x000f_ffff_ffff_f000;

    // A below-4 GiB root for the APs' first long mode: the firmware's own,
    // or a copy of its top-level table (the lower levels are shared).
    let (cr3, cr4, efer): (u64, u64, u64);
    // SAFETY: reading control registers and EFER.
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3);
        asm!("mov {}, cr4", out(reg) cr4);
        efer = hw::rdmsr(0xc000_0080);
    }
    let fw_root = cr3 & 0x000f_ffff_ffff_f000;
    let root = if fw_root < 0xffff_f000 {
        fw_root
    } else {
        // SAFETY: both pages are identity mapped; root_page is ours.
        unsafe { ptr::copy_nonoverlapping(fw_root as *const u8, plan.root_page as *mut u8, 4096) };
        plan.root_page
    };

    let (code, off32, off64) = start_code();
    let l = plan.start_page;
    let params = (l + START_PARAMS) as *mut StartParams;
    // SAFETY: the start page is ours and identity mapped; CR0.WP is cleared
    // around the copy in case the firmware mapped LoaderCode read-only.
    unsafe {
        let cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0);
        asm!("mov cr0, {}", in(reg) cr0 & !(1 << 16));
        ptr::copy_nonoverlapping(code.as_ptr(), l as *mut u8, code.len());
        let gdt_lin = (l + START_PARAMS + core::mem::offset_of!(StartParams, gdt) as u64) as u32;
        let far = (l + off32) as u32;
        ptr::write(
            params,
            StartParams {
                started: 0,
                _r0: 0,
                gdtr: [7 * 8 - 1, gdt_lin as u16, (gdt_lin >> 16) as u16, 0],
                far32: [far as u16, (far >> 16) as u16, 0x18, 0],
                entry64: (l + off64) as u32,
                _r1: 0,
                cr3: root,
                cr4: 1 << 5 | (cr4 & 1 << 12),
                efer: 1 << 8 | (efer & 1 << 11),
                cr0: 1 | 1 << 4 | 1 << 16 | 1 << 31,
                stack: 0,
                arg: 0,
                entry: ap_main as *const () as u64,
                gdt: [
                    handoff::GDT[0],
                    handoff::GDT[1],
                    handoff::GDT[2],
                    handoff::GDT[3],
                    handoff::GDT[4],
                    handoff::GDT[5],
                    handoff::GDT[6],
                ],
            },
        );
        asm!("mov cr0, {}", in(reg) cr0);
    }
    plan.call.block = p.block;
    // SAFETY: the BSP's Block, filled in and not yet used.
    let saved: Block = unsafe { ptr::read(p.block as *const Block) };
    plan.call.tramp = p.tramp;
    let vector = cmp::startup_vector(l).unwrap() as u32;
    let stack_bytes = plan.stack_size.div_ceil(PAGE) * PAGE;

    let mut k = 0u64;
    for i in 0..n {
        let c = plan.cpus[i];
        if c.apic_id == plan.bsp {
            continue;
        }
        let top = plan.stacks + (k + 1) * stack_bytes;
        k += 1;
        // This AP's Block: the BSP's, with its own stack, GDT, entry
        // (the parking loop), argument (its info, as the kernel sees it),
        // and no identity-map removal (the BSP does that, last).
        // SAFETY: the Block page is ours, identity mapped; no AP is using it.
        let b = unsafe { &mut *(p.block as *mut Block) };
        // SAFETY: Block is plain data.
        unsafe { ptr::copy_nonoverlapping(&saved as *const Block, b, 1) };
        // SAFETY: gdt[i] has room for the GDT (arena, sized by arena_bytes).
        unsafe { ptr::write(gdt[i] as *mut [u64; 9], handoff::GDT) };
        handoff::set_gdtr(&mut b.gdtr_high, gdt[i] + p.hhdm);
        b.low_entry = 0;
        b.stack_top = top + p.hhdm;
        b.entry = p.park;
        b.arg = info[i] + p.hhdm;
        // SAFETY: the params are ours and no AP is running the start code.
        unsafe {
            ptr::write_volatile(&raw mut (*params).started, 0);
            ptr::write_volatile(&raw mut (*params).stack, top);
            ptr::write_volatile(&raw mut (*params).arg, &*plan.call as *const ApCall as u64);
        }
        // SAFETY: `reserved` of this CPU's info; the AP writes it when parked.
        let parked = unsafe { &*((info[i] + 8) as *const AtomicU64) };
        parked.store(0, SeqCst);
        // SAFETY: as above.
        let started =
            unsafe { &*(&raw const (*params).started as *const core::sync::atomic::AtomicU32) };
        core::sync::atomic::fence(SeqCst);

        // SDM 8.4.4.1: INIT, 10 ms, start-up, 200 us, start-up again.
        let mut sent = ipi(plan.x2, apic, c.apic_id, cmp::ICR_INIT, hz);
        delay_us(hz, 10_000);
        sent &= ipi(plan.x2, apic, c.apic_id, cmp::ICR_STARTUP | vector, hz);
        delay_us(hz, 200);
        if started.load(SeqCst) == 0 {
            sent &= ipi(plan.x2, apic, c.apic_id, cmp::ICR_STARTUP | vector, hz);
            delay_us(hz, 200);
        }
        let t0 = hw::rdtsc();
        while sent && parked.load(SeqCst) == 0 && hw::rdtsc().wrapping_sub(t0) < 5 * hz {
            core::hint::spin_loop();
        }
        if parked.load(SeqCst) != 0 {
            plan.ok[i] = true;
        } else {
            // Never let a half-started AP run on: back to wait-for-SIPI.
            ipi(plan.x2, apic, c.apic_id, cmp::ICR_INIT, hz);
            delay_us(hz, 10_000);
        }
    }
    // SAFETY: every AP is parked or stopped; the page is the BSP's again.
    unsafe { ptr::write(p.block as *mut Block, saved) };
}
