//! A long-mode IDT for stage 2 itself: any CPU exception prints what
//! happened and stops, instead of the triple fault (an instant reboot) an
//! empty IDT would give.

use crate::println;
use core::ptr::addr_of_mut;

core::arch::global_asm!(
    r#"
.section .text.ob_isr, "ax"
.code64
.irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
ob_isr_\n:
.if (\n == 8) || (\n >= 10 && \n <= 14) || (\n == 17) || (\n == 21) || (\n == 29) || (\n == 30)
.else
    pushq $0
.endif
    pushq $\n
    jmp ob_isr_common
.endr
ob_isr_common:
    mov (%rsp), %rdi
    mov 8(%rsp), %rsi
    mov 16(%rsp), %rdx
    mov %cr2, %rcx
    and $-16, %rsp
    call ob_exception
1:  cli
    hlt
    jmp 1b
.section .rodata.ob_isr_table, "a"
.balign 8
.global ob_isr_table
ob_isr_table:
.irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    .quad ob_isr_\n
.endr
"#,
    options(att_syntax)
);

unsafe extern "C" {
    static ob_isr_table: [u64; 32];
    static mut ob_idt_desc: [u8; 16];
}

static mut IDT: [[u64; 2]; 32] = [[0; 2]; 32];

pub fn init() {
    // SAFETY: single-threaded setup before anything can fault; the gates
    // point at the stubs above in the 64-bit code segment (0x08).
    unsafe {
        let idt = &mut *addr_of_mut!(IDT);
        for (i, gate) in idt.iter_mut().enumerate() {
            let a = ob_isr_table[i];
            gate[0] = (a & 0xffff) | 0x08 << 16 | 0x8e00 << 32 | ((a >> 16) & 0xffff) << 48;
            gate[1] = a >> 32;
        }
        let desc = &mut *addr_of_mut!(ob_idt_desc);
        let limit = (core::mem::size_of_val(idt) - 1) as u16;
        desc[0..2].copy_from_slice(&limit.to_le_bytes());
        desc[2..10].copy_from_slice(&(idt.as_ptr() as u64).to_le_bytes());
        core::arch::asm!("lidt [{}]", in(reg) desc.as_ptr(), options(nostack));
    }
}

#[unsafe(no_mangle)]
extern "sysv64" fn ob_exception(vector: u64, error: u64, rip: u64, cr2: u64) -> ! {
    crate::console::init();
    println!();
    println!(
        "OPTIMAL-BOOT: FATAL: CPU exception {vector} (error {error:#x}) at {rip:#x}, cr2 {cr2:#x}"
    );
    println!("The system was not booted. Nothing unverified was run.");
    loop {
        // SAFETY: stop here; interrupts are off.
        unsafe { core::arch::asm!("cli; hlt") };
    }
}
