//! Conformance kernel: boots through any Limine-protocol loader and checks,
//! as strictly as the specification allows, everything the loader promised.
//! It runs under Limine itself (to prove the checks are right) and under
//! Optimal Boot (to prove the loader is right).
//!
//! Output on COM1: one "ok <check>" or "FAIL <check>: why" line per check,
//! then "OPTIMAL-CONFORMANCE: PASS (<n> checks)" or "OPTIMAL-CONFORMANCE: FAIL".
//!
//! Build-time knobs (environment): OB_REVISION (base revision, default 3),
//! OB_PAGING=5 (ask for 5-level paging), OB_ENTRY_REQ=1 (use the Entry Point
//! request), OB_PIE=1 (relocatable, linked in the lower half).

#![no_std]
#![no_main]

mod checks;
mod phys;
mod req;
mod serial;

use core::arch::global_asm;
use core::panic::PanicInfo;

/// Everything the CPU looked like at entry, captured before any Rust runs.
#[repr(C)]
pub struct EntryState {
    pub gprs: [u64; 16], // rax rbx rcx rdx rsi rdi rbp rsp r8..r15
    pub rflags: u64,
    pub ret_addr: u64,
    pub cs: u64,
    pub ds: u64,
    pub es: u64,
    pub ss: u64,
    pub fs: u64,
    pub gs: u64,
    pub via: u64, // 1 = normal entry, 2 = Entry Point request entry
}

#[unsafe(no_mangle)]
pub static mut ENTRY_STATE: EntryState = EntryState {
    gprs: [0; 16],
    rflags: 0,
    ret_addr: 0,
    cs: 0,
    ds: 0,
    es: 0,
    ss: 0,
    fs: 0,
    gs: 0,
    via: 0,
};

macro_rules! entry_stub {
    ($name:literal, $via:literal) => {
        global_asm!(concat!(
            ".global ",
            $name,
            "\n",
            $name,
            ":\n",
            "    push rax\n",
            "    pushfq\n",
            "    pop rax\n",
            "    mov [rip + ENTRY_STATE + 128], rax\n",
            "    pop rax\n",
            "    mov [rip + ENTRY_STATE + 0], rax\n",
            "    mov [rip + ENTRY_STATE + 8], rbx\n",
            "    mov [rip + ENTRY_STATE + 16], rcx\n",
            "    mov [rip + ENTRY_STATE + 24], rdx\n",
            "    mov [rip + ENTRY_STATE + 32], rsi\n",
            "    mov [rip + ENTRY_STATE + 40], rdi\n",
            "    mov [rip + ENTRY_STATE + 48], rbp\n",
            "    mov [rip + ENTRY_STATE + 56], rsp\n",
            "    mov [rip + ENTRY_STATE + 64], r8\n",
            "    mov [rip + ENTRY_STATE + 72], r9\n",
            "    mov [rip + ENTRY_STATE + 80], r10\n",
            "    mov [rip + ENTRY_STATE + 88], r11\n",
            "    mov [rip + ENTRY_STATE + 96], r12\n",
            "    mov [rip + ENTRY_STATE + 104], r13\n",
            "    mov [rip + ENTRY_STATE + 112], r14\n",
            "    mov [rip + ENTRY_STATE + 120], r15\n",
            "    mov rax, [rsp]\n",
            "    mov [rip + ENTRY_STATE + 136], rax\n",
            "    mov ax, cs\n",
            "    mov [rip + ENTRY_STATE + 144], rax\n",
            "    mov ax, ds\n",
            "    mov [rip + ENTRY_STATE + 152], rax\n",
            "    mov ax, es\n",
            "    mov [rip + ENTRY_STATE + 160], rax\n",
            "    mov ax, ss\n",
            "    mov [rip + ENTRY_STATE + 168], rax\n",
            "    mov ax, fs\n",
            "    mov [rip + ENTRY_STATE + 176], rax\n",
            "    mov ax, gs\n",
            "    mov [rip + ENTRY_STATE + 184], rax\n",
            "    mov qword ptr [rip + ENTRY_STATE + 192], ",
            $via,
            "\n",
            "    and rsp, -16\n",
            "    call conformance_main\n",
            "2:  cli\n",
            "    hlt\n",
            "    jmp 2b\n",
        ));
    };
}
entry_stub!("_start", "1");
entry_stub!("alt_start", "2");

unsafe extern "C" {
    pub fn alt_start();
}

#[unsafe(no_mangle)]
extern "C" fn conformance_main() -> ! {
    serial::init();
    checks::run();
    halt()
}

pub fn halt() -> ! {
    loop {
        // SAFETY: stops the CPU with interrupts off.
        unsafe { core::arch::asm!("cli; hlt") };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("FAIL panic: {info}");
    println!("OPTIMAL-CONFORMANCE: FAIL");
    halt()
}
