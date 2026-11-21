//! Deliberate crashes, chosen with `crashtest=<name>` on the kernel command
//! line. CI boots each one and checks the crash report names the right fault,
//! which proves every exception path works before real code depends on it.

use core::arch::asm;

use crate::println;

/// A higher-half address nothing is mapped at (far beyond any RAM in the
/// direct map).
const UNMAPPED: u64 = 0xffff_9fff_dead_0000;

pub fn run(name: &str) {
    println!("OPTIMAL: crashtest {name}");
    // SAFETY (all arms): each one deliberately executes a faulting
    // instruction; the exception handler reports it and halts, so control
    // never returns into an invalid state.
    unsafe {
        match name {
            "divide" => {
                asm!("xor edx, edx", "xor eax, eax", "xor ecx, ecx", "div ecx", out("eax") _, out("edx") _, out("ecx") _)
            }
            "invalid_opcode" => asm!("ud2"),
            "breakpoint_then_halt" => {
                asm!("int3");
                let n = crate::arch::idt::BREAKPOINTS.load(core::sync::atomic::Ordering::SeqCst);
                println!("OPTIMAL: breakpoint returned, count {n}");
                return;
            }
            // Memory faults use raw instructions rather than Rust pointer
            // reads, so the compiler can't treat them as undefined behaviour
            // and optimise them away.
            "general_protection" => {
                // Non-canonical address: always #GP, never #PF.
                asm!("mov rax, [{}]", in(reg) 0x8000_0000_0000_0000u64, out("rax") _);
            }
            "page_fault_read" => {
                asm!("mov rax, [{}]", in(reg) UNMAPPED, out("rax") _);
            }
            "page_fault_write" => {
                asm!("mov qword ptr [{}], 1", in(reg) UNMAPPED);
            }
            "null_deref" => {
                asm!("mov rax, [{}]", in(reg) 0u64, out("rax") _);
            }
            "write_rodata" => {
                // W^X / CR0.WP check: kernel read-only data must not be writable.
                static RO: u64 = 0x5eed;
                asm!("mov qword ptr [{}], 0", in(reg) &raw const RO);
            }
            "execute_data" => {
                // NX check: jumping into a data page must fault on instruction fetch.
                static mut CODE: [u8; 16] = [0xC3; 16]; // `ret`
                asm!("call {}", in(reg) &raw mut CODE, clobber_abi("C"));
            }
            "double_fault" => {
                // Point RSP at unmapped memory, then fault: the CPU can't push
                // the #PF frame, which escalates to #DF on its own IST stack.
                asm!("mov rsp, {}", "push rax", in(reg) UNMAPPED, options(noreturn));
            }
            "panic" => panic!("deliberate panic from crashtest"),
            other => println!("OPTIMAL: unknown crashtest {other}"),
        }
    }
    println!("OPTIMAL: crashtest {name} did NOT fault");
}
