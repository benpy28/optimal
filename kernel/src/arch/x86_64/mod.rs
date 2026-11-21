//! Everything specific to x86_64. Other architectures get a sibling module.

pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod port;

/// Bring this CPU from "Limine handed us control" to "safe to run kernel
/// code": descriptor tables, exception handling and hardware protections.
///
/// # Safety
/// Call once per CPU, first thing, with interrupts disabled.
pub unsafe fn init_cpu() -> cpu::Hardening {
    // SAFETY: forwarded from the caller's contract.
    unsafe {
        gdt::init();
        idt::init();
    }
    cpu::enable_hardening()
}
