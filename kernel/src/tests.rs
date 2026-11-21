//! Kernel tests for Phase 1a: CPU tables, exceptions and the security baseline.

use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::arch::{cpu, gdt, idt};
use crate::ktest;

ktest!(code_segment_is_kernel_code, {
    let cs: u16;
    // SAFETY: reading CS has no side effects.
    unsafe { asm!("mov {:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
    assert_eq!(cs, gdt::KERNEL_CODE);
});

ktest!(data_segments_are_kernel_data, {
    let (ds, ss): (u16, u16);
    // SAFETY: reading segment registers has no side effects.
    unsafe {
        asm!("mov {:x}, ds", out(reg) ds, options(nomem, nostack, preserves_flags));
        asm!("mov {:x}, ss", out(reg) ss, options(nomem, nostack, preserves_flags));
    }
    assert_eq!(ds, gdt::KERNEL_DATA);
    assert_eq!(ss, gdt::KERNEL_DATA);
});

ktest!(task_register_points_at_tss, {
    let tr: u16;
    // SAFETY: STR only reads the task register.
    unsafe { asm!("str {:x}", out(reg) tr, options(nomem, nostack, preserves_flags)) };
    assert_eq!(tr, gdt::TSS);
});

ktest!(idt_covers_all_256_vectors, {
    let mut ptr = [0u8; 10];
    // SAFETY: SIDT writes exactly 10 bytes to the given buffer.
    unsafe { asm!("sidt [{}]", in(reg) ptr.as_mut_ptr(), options(nostack, preserves_flags)) };
    let limit = u16::from_le_bytes([ptr[0], ptr[1]]);
    assert_eq!(limit as usize + 1, 256 * 16);
});

ktest!(breakpoint_handler_returns, {
    let before = idt::BREAKPOINTS.load(Ordering::SeqCst);
    // SAFETY: int3 traps into our handler, which counts it and returns.
    unsafe { asm!("int3") };
    unsafe { asm!("int3") };
    assert_eq!(idt::BREAKPOINTS.load(Ordering::SeqCst), before + 2);
});

ktest!(registers_survive_an_interrupt, {
    // Every general-purpose register must come back unchanged after a trap.
    let out: [u64; 6];
    // SAFETY: int3 returns with all registers restored by isr_common.
    unsafe {
        let (a, b, c, d, e, f): (u64, u64, u64, u64, u64, u64);
        asm!(
            "mov rsi, 0x1111111111111111",
            "mov r12, 0x2222222222222222",
            "mov r13, 0x3333333333333333",
            "mov r14, 0x4444444444444444",
            "mov r15, 0x5555555555555555",
            "mov r11, 0x6666666666666666",
            "int3",
            out("rsi") a, out("r12") b, out("r13") c, out("r14") d, out("r15") e, out("r11") f,
        );
        out = [a, b, c, d, e, f];
    }
    assert_eq!(
        out,
        [
            0x1111111111111111,
            0x2222222222222222,
            0x3333333333333333,
            0x4444444444444444,
            0x5555555555555555,
            0x6666666666666666
        ]
    );
});

ktest!(hardening_matches_cpu_features, {
    let f = cpu::features();
    let cr4 = cpu::read_cr4();
    assert!(cpu::read_cr0() & (1 << 16) != 0, "CR0.WP off");
    assert_eq!(cr4 & (1 << 20) != 0, f.smep, "SMEP state wrong");
    assert_eq!(cr4 & (1 << 21) != 0, f.smap, "SMAP state wrong");
    assert_eq!(cr4 & (1 << 11) != 0, f.umip, "UMIP state wrong");
    let efer = cpu::read_msr(cpu::MSR_EFER);
    assert_eq!(efer & (1 << 11) != 0, f.nx, "NX state wrong");
});

ktest!(cmdline_parsing, {
    use crate::boot::arg;
    assert_eq!(arg("a=1 crashtest=divide b", "crashtest"), Some("divide"));
    assert_eq!(arg("crashtests=x", "crashtest"), None);
    assert_eq!(arg("", "crashtest"), None);
    assert_eq!(arg("crashtest=", "crashtest"), Some(""));
});

ktest!(exception_names_cover_every_vector, {
    for v in 0..256u64 {
        assert!(!idt::exception_name(v).is_empty());
    }
    assert_eq!(idt::exception_name(14), "#PF page fault");
});
