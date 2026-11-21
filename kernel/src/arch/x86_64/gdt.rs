//! Global Descriptor Table and Task State Segment.
//!
//! Long mode barely uses segmentation, but the CPU still needs code/data
//! descriptors for each privilege level, and a TSS that names the stacks used
//! for privilege changes and for the Interrupt Stack Table (IST). Faults that
//! can strike when the current stack is broken (double fault, NMI, machine
//! check) each get their own known-good IST stack.

use core::arch::asm;
use core::mem::size_of;
use core::ptr::addr_of;

/// Selector layout. Order matters for SYSCALL/SYSRET later:
/// SYSRET loads user CS from STAR+16 and user SS from STAR+8.
pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
#[expect(dead_code, reason = "used by SYSCALL/SYSRET setup in Phase 3")]
pub const USER_DATA: u16 = 0x18 | 3;
#[expect(dead_code, reason = "used by SYSCALL/SYSRET setup in Phase 3")]
pub const USER_CODE: u16 = 0x20 | 3;
pub const TSS: u16 = 0x28;

/// IST slots (1-based, as the IDT encodes them).
pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

const IST_STACK_SIZE: usize = 16 * 1024;

#[repr(C, packed(4))]
struct TaskStateSegment {
    _reserved0: u32,
    rsp: [u64; 3],
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

#[repr(C, align(16))]
struct Stack([u8; IST_STACK_SIZE]);

// One CPU for now; these become per-CPU in Phase 2 (SMP).
static mut IST_STACKS: [Stack; 3] = [const { Stack([0; IST_STACK_SIZE]) }; 3];
static mut TSS_STORE: TaskStateSegment = TaskStateSegment {
    _reserved0: 0,
    rsp: [0; 3],
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    // No I/O permission bitmap: user code gets no port access.
    iomap_base: size_of::<TaskStateSegment>() as u16,
};
static mut GDT: [u64; 7] = [0; 7];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

const fn segment(code: bool, dpl: u64) -> u64 {
    let present = 1 << 47;
    let s = 1 << 44; // code/data, not system
    let dpl = dpl << 45;
    let rw = 1 << 41;
    if code {
        let exec = 1 << 43;
        let long = 1 << 53;
        present | s | dpl | exec | rw | long
    } else {
        present | s | dpl | rw
    }
}

fn tss_descriptor(base: u64) -> (u64, u64) {
    let limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let low = (limit & 0xFFFF)
        | ((base & 0xFF_FFFF) << 16)
        | (0b1001 << 40) // 64-bit TSS, available
        | (1 << 47) // present
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    let high = base >> 32;
    (low, high)
}

/// Build and load the GDT and TSS on this CPU.
///
/// # Safety
/// Must be called once per CPU, early in boot, with interrupts disabled.
pub unsafe fn init() {
    // SAFETY: single-threaded early boot; nothing else references these
    // statics yet, so taking raw pointers and writing through them is sound.
    unsafe {
        let tss = &raw mut TSS_STORE;
        let stacks = &raw mut IST_STACKS;
        for (i, slot) in [IST_DOUBLE_FAULT, IST_NMI, IST_MACHINE_CHECK]
            .iter()
            .enumerate()
        {
            // The stack grows down, so the IST entry is its top end.
            let top = (&raw mut (*stacks)[i]) as u64 + IST_STACK_SIZE as u64;
            (*tss).ist[(*slot - 1) as usize] = top;
        }

        let (tss_low, tss_high) = tss_descriptor(tss as u64);
        let gdt = &raw mut GDT;
        *gdt = [
            0,
            segment(true, 0),
            segment(false, 0),
            segment(false, 3),
            segment(true, 3),
            tss_low,
            tss_high,
        ];

        let ptr = DescriptorTablePointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: gdt as u64,
        };
        asm!("lgdt [{}]", in(reg) addr_of!(ptr), options(readonly, nostack, preserves_flags));

        // Reload CS with a far return, then the data segments.
        asm!(
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov ss, {ds:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            cs = in(reg) u64::from(KERNEL_CODE),
            ds = in(reg) u64::from(KERNEL_DATA),
            tmp = out(reg) _,
            options(preserves_flags),
        );

        asm!("ltr {:x}", in(reg) TSS, options(nostack, preserves_flags));
    }
}
