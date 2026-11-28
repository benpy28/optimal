//! The last step: switch from the firmware's page tables to ours, set the
//! exact CPU state the Limine protocol promises, and jump to the kernel.
//!
//! Changing between 4- and 5-level paging needs paging turned off, which in
//! long mode means dropping to 32-bit protected mode. So the trampoline runs
//! from a page below 4 GiB that is identity mapped in both the firmware's
//! tables and ours, then moves to its handoff alias, removes that identity
//! mapping, and enters the kernel.
//!
//! The handoff alias is the one page of the final address space that is
//! neither the kernel nor the HHDM: the trampoline page mapped read-only and
//! executable just below the kernel's lowest virtual page. Some executable
//! page must survive the switch to run the instructions after it, and keeping
//! it out of the HHDM keeps the whole HHDM non-executable (W^X). It holds
//! only this code, and its memory is bootloader-reclaimable, so it goes away
//! when the kernel moves to its own page tables.

use core::arch::global_asm;

/// Data the trampoline reads; lives at the start of a page below 4 GiB.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Block {
    pub cr3: u64,            // 0x00
    pub cr4: u64,            // 0x08
    pub efer: u64,           // 0x10
    pub cr0: u64,            // 0x18
    pub pat: u64,            // 0x20
    pub gdtr_low: [u16; 8],  // 0x28: limit, base (identity)
    pub gdtr_high: [u16; 8], // 0x38: limit, base (HHDM)
    pub idtr_zero: [u16; 8], // 0x48
    pub stub64: u64,         // 0x58: identity address of the 64-bit re-entry
    pub hhdm: u64,           // 0x60
    pub low_entry: u64,      // 0x68: HHDM address of top-level entry 0
    pub stack_top: u64,      // 0x70
    pub entry: u64,          // 0x78
    pub code_delta: u64,     // 0x80: handoff alias minus identity address of the code page
    pub arg: u64, // 0x88: RDI at `entry` (0 for the kernel; an AP's info for the parking loop)
    pub _pad: [u64; 14],
    pub gdt: [u64; 9], // 0x100
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
    static ob_tramp_park: u8;
}

/// The trampoline's machine code, the offset of its 64-bit re-entry, and
/// the offset of the AP parking loop.
pub fn trampoline() -> (&'static [u8], usize, usize) {
    // SAFETY: the symbols delimit the global_asm block below.
    unsafe {
        let s = &ob_tramp_start as *const u8;
        let e = &ob_tramp_end as *const u8;
        let k = &ob_tramp_stub64 as *const u8;
        let p = &ob_tramp_park as *const u8;
        (
            core::slice::from_raw_parts(s, e.offset_from(s) as usize),
            k.offset_from(s) as usize,
            p.offset_from(s) as usize,
        )
    }
}

// Entered (System V ABI) with rdi = identity address of Block (below 4 GiB), interrupts off.
// The BSP enters it once, last; each AP (mp.rs) enters it with a Block of
// its own whose low_entry is 0 (so it leaves the identity mapping alone),
// whose entry is the parking loop and whose arg is its limine_mp_info.
// The rest of the Block's page is the trampoline's own stack, so the 32-bit
// part never depends on where the firmware's stack was.
global_asm!(
    r#"
    .section .text.ob_tramp, "ax"
    .global ob_tramp_start
    .global ob_tramp_end
    .global ob_tramp_stub64
    .global ob_tramp_park
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
    lea rbx, [rip + 3f]
    add rbx, [rdi + 0x80]
    add rdi, [rdi + 0x60]
    jmp rbx
3:
    add rsp, [rdi + 0x60]
    mov rax, [rdi + 0x68]
    test rax, rax
    jz 5f
    mov qword ptr [rax], 0
5:
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
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    mov rdi, [rdi + 0x88]
    // Last, because every xor above sets flags: RFLAGS = 0x2 exactly.
    push 2
    popfq
    ret
// An AP's entry: rdi = its limine_mp_info (HHDM), rsp = stack top - 8 with
// the 0 return address at [rsp]. `reserved` gets the stack pointer, which
// also tells the BSP this AP is parked; then wait for goto_address (x86
// loads have acquire semantics). CR3 is reloaded before the jump, to drop
// TLB entries for the identity pages the BSP has removed since.
ob_tramp_park:
    mov [rdi + 8], rsp
6:
    mov rax, [rdi + 16]
    test rax, rax
    jnz 7f
    pause
    jmp 6b
7:
    mov rcx, cr3
    mov cr3, rcx
    xor ecx, ecx
    push 2
    popfq
    jmp rax
ob_tramp_end:
    .text
"#
);
