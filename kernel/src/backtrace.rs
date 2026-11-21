//! Stack traces by walking frame pointers (the kernel is built with
//! `-C force-frame-pointers=yes`, so every function saves RBP).

use crate::println;

const MAX_FRAMES: usize = 32;

/// The kernel image lives in the top 2 GiB; anything else isn't one of our
/// frames, so the walk stops instead of reading wild memory.
fn plausible(addr: u64) -> bool {
    addr >= 0xffff_ffff_8000_0000 && addr.is_multiple_of(8)
}

fn plausible_stack(addr: u64) -> bool {
    // Higher-half addresses only (kernel stacks live in the HHDM or the image).
    addr >= 0xffff_8000_0000_0000 && addr.is_multiple_of(8)
}

pub fn print_from(rip: u64, mut rbp: u64) {
    println!("backtrace:");
    println!("  #0  {rip:#018x}");
    for i in 1..MAX_FRAMES {
        if !plausible_stack(rbp) {
            break;
        }
        // SAFETY: rbp points at a saved [previous rbp, return address] pair
        // pushed by a function prologue; the address was range-checked above.
        let (next, ret) = unsafe {
            let p = rbp as *const u64;
            (p.read_volatile(), p.add(1).read_volatile())
        };
        if ret == 0 || !plausible(ret) {
            break;
        }
        println!("  #{i:<2} {ret:#018x}");
        if next <= rbp {
            break; // stacks grow down, so callers' frames are at higher addresses
        }
        rbp = next;
    }
}

/// Print a trace of the current call stack (used by the panic handler).
pub fn print_here() {
    let rbp: u64;
    // SAFETY: reading RBP has no side effects.
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags))
    };
    print_from(print_here as *const () as u64, rbp);
}
