//! Deliberate crashes, chosen with `crashtest=<name>` on the kernel command
//! line. CI boots each one and checks the crash report names the right fault,
//! which proves every exception path works before real code depends on it.

use core::arch::asm;

use crate::println;

/// A higher-half address nothing is ever mapped at.
const UNMAPPED: u64 = crate::mm::paging::NEVER_MAPPED;

/// Recurse with a 1 KiB frame until the boot stack runs into its guard page.
#[inline(never)]
fn recurse(depth: u64) -> u64 {
    let mut pad = [0u8; 1024];
    pad[0] = depth as u8;
    let pad = core::hint::black_box(pad);
    if core::hint::black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1) + u64::from(pad[(depth % 1024) as usize])
}

/// A pointer the relocations fill in, so it lives in the RELRO region.
static RELRO_POINTER: &u64 = &0x5eed;

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
            "stack_overflow" => {
                core::hint::black_box(recurse(0));
            }
            "write_relro" => {
                // RELRO: relocated pointers are read-only once the kernel's
                // own page tables are on.
                asm!("mov qword ptr [{}], 0", in(reg) &raw const RELRO_POINTER);
            }
            "write_text" => {
                // Kernel code must not be writable.
                asm!("mov byte ptr [{}], 0xc3", in(reg) recurse as *const () as u64);
            }
            "heap_double_free" => {
                let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
                // black_box: the compiler may otherwise drop an unused
                // allocation and its frees altogether.
                let p = core::hint::black_box(alloc::alloc::alloc(layout));
                alloc::alloc::dealloc(p, layout);
                alloc::alloc::dealloc(core::hint::black_box(p), layout);
            }
            "heap_bad_pointer" => {
                // Free a pointer into the middle of an object.
                let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
                let p = core::hint::black_box(alloc::alloc::alloc(layout));
                alloc::alloc::dealloc(core::hint::black_box(p.add(8)), layout);
            }
            "heap_wild_pointer" => {
                // Free a pointer the heap never handed out (kernel data).
                static NOT_HEAP: [u8; 64] = [0; 64];
                let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
                alloc::alloc::dealloc(&raw const NOT_HEAP as *mut u8, layout);
            }
            "execute_heap" => {
                // Heap memory is no-execute.
                let code = alloc::boxed::Box::new([0xc3u8; 16]);
                asm!("call {}", in(reg) code.as_ptr(), clobber_abi("C"));
            }
            "execute_direct_map" => {
                // The direct map is no-execute: put a `ret` in a fresh frame
                // and call it through the direct map.
                let f = crate::mm::frames::alloc(0).expect("no frame for the test");
                let p = crate::mm::frames::phys_to_virt(f) as *mut u8;
                p.write_volatile(0xc3);
                asm!("call {}", in(reg) p, clobber_abi("C"));
            }
            other => println!("OPTIMAL: unknown crashtest {other}"),
        }
    }
    println!("OPTIMAL: crashtest {name} did NOT fault");
}
