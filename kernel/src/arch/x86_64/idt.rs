//! Interrupt Descriptor Table, interrupt entry, and exception crash reports.
//!
//! All 256 vectors enter through generated stubs (see build.rs) that push a
//! uniform frame, so one Rust dispatcher sees every register. Faults the
//! kernel can't recover from produce a full crash report and stop the CPU.

use core::arch::{asm, global_asm};
use core::mem::size_of;
use core::ptr::addr_of;
use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu;
use super::gdt;

global_asm!(include_str!(concat!(env!("OUT_DIR"), "/isr_stubs.s")));

// Common entry: save every general-purpose register, hand the frame to Rust,
// restore and return. The CPU aligns RSP to 16 before pushing its frame, and
// the 22 quadwords pushed here keep it 16-aligned at the `call`.
global_asm!(
    ".section .text.isr, \"ax\"",
    ".global isr_common",
    "isr_common:",
    "    cld",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov rdi, rsp",
    "    call {dispatch}",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop r11",
    "    pop r10",
    "    pop r9",
    "    pop r8",
    "    pop rbp",
    "    pop rdi",
    "    pop rsi",
    "    pop rdx",
    "    pop rcx",
    "    pop rbx",
    "    pop rax",
    "    add rsp, 16", // vector + error code
    "    iretq",
    dispatch = sym dispatch,
);

unsafe extern "C" {
    /// First of the 256 generated entry stubs (see build.rs).
    fn isr_stub_0();
}

/// Each generated stub occupies exactly this many bytes.
pub const STUB_SIZE: u64 = 16;

/// Address of the entry stub for `vector`.
pub fn stub_address(vector: u8) -> u64 {
    isr_stub_0 as *const () as u64 + u64::from(vector) * STUB_SIZE
}

/// True if the CPU pushes an error code for this vector (Intel SDM Vol. 3A, 6.13).
pub fn has_error_code(vector: u8) -> bool {
    matches!(vector, 8 | 10..=14 | 17 | 21 | 29 | 30)
}

/// Everything saved on interrupt entry, lowest address first.
#[repr(C)]
#[derive(Debug)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl Gate {
    const EMPTY: Gate = Gate {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        _reserved: 0,
    };

    /// A present, DPL-0 interrupt gate (interrupts are disabled on entry).
    fn interrupt(handler: u64, ist: u8) -> Gate {
        Gate {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE,
            ist,
            type_attr: 0x8E,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved: 0,
        }
    }
}

static mut IDT: [Gate; 256] = [Gate::EMPTY; 256];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

pub const VEC_NMI: u64 = 2;
pub const VEC_BREAKPOINT: u64 = 3;
pub const VEC_DOUBLE_FAULT: u64 = 8;
pub const VEC_PAGE_FAULT: u64 = 14;
pub const VEC_MACHINE_CHECK: u64 = 18;

/// Build and load the IDT on this CPU.
///
/// # Safety
/// Must be called after `gdt::init` (the gates use its selectors and IST
/// slots), with interrupts disabled.
pub unsafe fn init() {
    // SAFETY: early boot, single CPU, interrupts off: nothing else touches IDT.
    unsafe {
        let idt = &raw mut IDT;
        for vector in 0..256usize {
            let stub = stub_address(vector as u8);
            let ist = match vector as u64 {
                VEC_DOUBLE_FAULT => gdt::IST_DOUBLE_FAULT,
                VEC_NMI => gdt::IST_NMI,
                VEC_MACHINE_CHECK => gdt::IST_MACHINE_CHECK,
                _ => 0,
            };
            (*idt)[vector] = Gate::interrupt(stub, ist);
        }
        let ptr = DescriptorTablePointer {
            limit: (size_of::<[Gate; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!("lidt [{}]", in(reg) addr_of!(ptr), options(readonly, nostack, preserves_flags));
    }
}

/// Count of breakpoints taken, so tests can prove the handler ran and returned.
pub static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

/// Rust side of every interrupt. Called from `isr_common` with the saved frame.
extern "C" fn dispatch(frame: &mut InterruptFrame) {
    match frame.vector {
        VEC_BREAKPOINT => {
            BREAKPOINTS.fetch_add(1, Ordering::SeqCst);
        }
        v if v < 32 => crash(frame),
        // Hardware interrupts are routed here once the APIC is set up (Phase 1c).
        _ => crash(frame),
    }
}

pub fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "#DE divide error",
        1 => "#DB debug",
        2 => "NMI",
        3 => "#BP breakpoint",
        4 => "#OF overflow",
        5 => "#BR bound range exceeded",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        8 => "#DF double fault",
        10 => "#TS invalid TSS",
        11 => "#NP segment not present",
        12 => "#SS stack-segment fault",
        13 => "#GP general protection fault",
        14 => "#PF page fault",
        16 => "#MF x87 floating-point error",
        17 => "#AC alignment check",
        18 => "#MC machine check",
        19 => "#XM SIMD floating-point error",
        20 => "#VE virtualization exception",
        21 => "#CP control protection",
        28 => "#HV hypervisor injection",
        29 => "#VC VMM communication",
        30 => "#SX security exception",
        32..=255 => "unexpected interrupt",
        _ => "reserved exception",
    }
}

fn crash(f: &InterruptFrame) -> ! {
    use crate::println;
    println!();
    println!(
        "OPTIMAL: EXCEPTION {} (vector {})",
        exception_name(f.vector),
        f.vector
    );
    if f.vector < 32 && has_error_code(f.vector as u8) {
        println!("error code {:#x}", f.error_code);
    }
    if f.vector == VEC_PAGE_FAULT {
        let e = f.error_code;
        println!(
            "page fault at {:#018x}: {} {} in {} mode{}{}",
            cpu::read_cr2(),
            if e & 1 != 0 {
                "protection violation"
            } else {
                "page not present"
            },
            if e & (1 << 4) != 0 {
                "on instruction fetch"
            } else if e & 2 != 0 {
                "on write"
            } else {
                "on read"
            },
            if e & 4 != 0 { "user" } else { "kernel" },
            if e & 8 != 0 { ", reserved bit set" } else { "" },
            if e & (1 << 5) != 0 {
                ", protection key"
            } else {
                ""
            },
        );
    }
    println!(
        "rip {:#018x}  cs {:#06x}  rflags {:#010x}",
        f.rip, f.cs, f.rflags
    );
    println!("rsp {:#018x}  ss {:#06x}", f.rsp, f.ss);
    println!(
        "rax {:#018x}  rbx {:#018x}  rcx {:#018x}",
        f.rax, f.rbx, f.rcx
    );
    println!(
        "rdx {:#018x}  rsi {:#018x}  rdi {:#018x}",
        f.rdx, f.rsi, f.rdi
    );
    println!(
        "rbp {:#018x}  r8  {:#018x}  r9  {:#018x}",
        f.rbp, f.r8, f.r9
    );
    println!(
        "r10 {:#018x}  r11 {:#018x}  r12 {:#018x}",
        f.r10, f.r11, f.r12
    );
    println!(
        "r13 {:#018x}  r14 {:#018x}  r15 {:#018x}",
        f.r13, f.r14, f.r15
    );
    println!(
        "cr0 {:#018x}  cr3 {:#018x}  cr4 {:#018x}",
        cpu::read_cr0(),
        cpu::read_cr3(),
        cpu::read_cr4()
    );
    crate::backtrace::print_from(f.rip, f.rbp);
    println!("OPTIMAL: system halted");
    cpu::halt_forever();
}
