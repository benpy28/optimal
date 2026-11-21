//! The last step: switch from the firmware's page tables to ours, set the
//! exact CPU state the Limine protocol promises, and jump to the kernel.
//!
//! Changing between 4- and 5-level paging needs paging turned off, which in
//! long mode means dropping to 32-bit protected mode. So the trampoline runs
//! from a page below 4 GiB that is identity mapped in both the firmware's
//! tables and ours, then moves to its HHDM alias, removes that identity
//! mapping, and enters the kernel.

use core::arch::global_asm;

/// Data the trampoline reads; lives at the start of a page below 4 GiB.
#[repr(C)]
pub struct Block {
    pub cr3: u64,           // 0x00
    pub cr4: u64,           // 0x08
    pub efer: u64,          // 0x10
    pub cr0: u64,           // 0x18
    pub pat: u64,           // 0x20
    pub gdtr_low: [u16; 8], // 0x28: limit, base (identity)
    pub gdtr_high: [u16; 8], // 0x38: limit, base (HHDM)
    pub idtr_zero: [u16; 8], // 0x48
    pub stub64: u64,        // 0x58: identity address of the 64-bit re-entry
    pub hhdm: u64,          // 0x60
    pub low_entry: u64,     // 0x68: HHDM address of top-level entry 0
    pub stack_top: u64,     // 0x70
    pub entry: u64,         // 0x78
    pub _pad: [u64; 16],
    pub gdt: [u64; 9],      // 0x100
}

pub const GDT: [u64; 9] = [
    0,
    0x0000_9a00_0000_ffff, // 0x08 16-bit code
    0x0000_9200_0000_ffff, // 0x10 16-bit data
    0x00cf_9a00_0000_ffff, // 0x18 32-bit code
    0x00cf_9200_0000_ffff, // 0x20 32-bit data
    0x00af_9a00_0000_ffff, // 0x28 64-bit code
    0x00cf_9200_0000_ffff, // 0x30 64-bit data
    0x0000_8900_0000_0000, // 0x38 TSS, base 0 limit 0 (low half)
    0,                     //      TSS high half
];

pub fn set_gdtr(slot: &mut [u16; 8], base: u64) {
    slot[0] = (GDT.len() * 8 - 1) as u16;
    slot[1] = base as u16;
    slot[2] = (base >> 16) as u16;
    slot[3] = (base >> 32) as u16;
    slot[4] = (base >> 48) as u16;
}

unsafe extern "C" {
    static ob_tramp_start: u8;
    static ob_tramp_end: u8;
    static ob_tramp_stub64: u8;
}

/// The trampoline's machine code, and the offset of its 64-bit re-entry.
pub fn trampoline() -> (&'static [u8], usize) {
    // SAFETY: the symbols delimit the global_asm block below.
    unsafe {
        let s = &ob_tramp_start as *const u8;
        let e = &ob_tramp_end as *const u8;
        let k = &ob_tramp_stub64 as *const u8;
        (core::slice::from_raw_parts(s, e.offset_from(s) as usize), k.offset_from(s) as usize)
    }
}

// Entered (System V ABI) with rdi = identity address of Block (below 4 GiB), interrupts off.
// The rest of the Block's page is the trampoline's own stack, so the 32-bit
// part never depends on where the firmware's stack was.
global_asm!(
    r#"
    .section .text.ob_tramp, "ax"
    .global ob_tramp_start
    .global ob_tramp_end
    .global ob_tramp_stub64
    .code64
ob_tramp_start:
    cli
    lea rsp, [rdi + 0x1000]
    lgdt [rdi + 0x28]
    lea rax, [rip + 2f]
    push 0x18
    push rax
    retfq
    .code32
2:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov eax, cr0
    and eax, 0x7fffffff
    mov cr0, eax
    mov ecx, 0x277
    mov eax, [edi + 0x20]
    mov edx, [edi + 0x24]
    wrmsr
    mov eax, [edi + 0x08]
    mov cr4, eax
    mov eax, [edi + 0x00]
    mov cr3, eax
    mov ecx, 0xc0000080
    mov eax, [edi + 0x10]
    mov edx, [edi + 0x14]
    wrmsr
    mov eax, [edi + 0x18]
    mov cr0, eax
    push 0x28
    push dword ptr [edi + 0x58]
    retf
    .code64
ob_tramp_stub64:
    mov edi, edi
    mov esp, esp
    mov rax, [rdi + 0x60]
    lea rbx, [rip + 3f]
    add rbx, rax
    add rdi, rax
    jmp rbx
3:
    add rsp, [rdi + 0x60]
    mov rax, [rdi + 0x68]
    mov qword ptr [rax], 0
    mov rax, cr3
    mov cr3, rax
    lgdt [rdi + 0x38]
    lea rax, [rip + 4f]
    push 0x28
    push rax
    retfq
4:
    mov ax, 0x30
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    xor eax, eax
    xor edx, edx
    mov ecx, 0xc0000100
    wrmsr
    mov ecx, 0xc0000101
    wrmsr
    mov ecx, 0xc0000102
    wrmsr
    mov ax, 0x38
    ltr ax
    xor eax, eax
    lldt ax
    lidt [rdi + 0x48]
    mov rsp, [rdi + 0x70]
    mov rax, [rdi + 0x78]
    push 0
    push rax
    push 2
    popfq
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor edi, edi
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    ret
ob_tramp_end:
    .text
"#
);
