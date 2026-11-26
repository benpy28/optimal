//! Optimal kernel.
//!
//! Boot order: serial (so we can report anything), CPU tables and exception
//! handling, hardware protections, then whatever the command line asks for:
//! the normal boot, the in-kernel test suite, or a deliberate crash test.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod arch;
mod backtrace;
mod boot;
mod bootinfo;
mod clock;
mod crashtest;
mod irq;
#[cfg(feature = "ktest")]
mod ktest;
mod mm;
mod serial;
mod sync;
#[cfg(feature = "ktest")]
mod tests;
mod time;
mod timer;

use core::panic::PanicInfo;

/// The line CI looks for to decide the kernel booted.
pub const BOOT_OK: &str = "OPTIMAL: kernel booted";

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    crate::println!();
    crate::println!("Optimal kernel {}", env!("CARGO_PKG_VERSION"));

    // SAFETY: first thing on the boot CPU, interrupts are still disabled.
    let hardening = unsafe { arch::init_cpu() };
    crate::println!(
        "cpu: tables loaded; NX {} WP {} SMEP {} SMAP {} UMIP {}",
        on(hardening.nx),
        on(hardening.wp),
        on(hardening.smep),
        on(hardening.smap),
        on(hardening.umip)
    );

    if !boot::BASE_REVISION.is_supported() {
        panic!("bootloader does not support the Limine base revision we need");
    }
    if let Some(info) = boot::BOOTLOADER_INFO.get_response() {
        crate::println!("bootloader: {} {}", info.name(), info.version());
    }
    let info = bootinfo::capture();
    report_memory(info);
    // SAFETY: once, on the boot CPU, after the frame allocator is up; kmain
    // keeps nothing on this stack that kmain_on_own_tables needs.
    unsafe { mm::paging::build_and_switch(info, kmain_on_own_tables) }
}

/// The rest of boot, on the kernel's own page tables and boot stack.
extern "C" fn kmain_on_own_tables() -> ! {
    mm::paging::finish_switch();
    let info = bootinfo::get();
    let reclaimed = mm::frames::reclaim_bootloader_memory(info);
    mm::heap::init(mm::paging::random_u64());
    let l = mm::paging::layout();
    crate::println!(
        "paging: own tables on; kernel W^X by section, direct map {}, {} pages; {} KiB of bootloader memory reclaimed",
        if l.nx {
            "no-execute"
        } else {
            "without NX (CPU has none)"
        },
        if l.huge_1g { "1 GiB" } else { "2 MiB" },
        reclaimed / 1024
    );
    match l.entropy {
        mm::paging::Entropy::Rdrand => crate::println!("kaslr: on (RDRAND)"),
        mm::paging::Entropy::TimestampOnly => {
            crate::println!("kaslr: on, WEAK entropy (no RDRAND; timestamp jitter only)")
        }
    }
    // Addresses are only printed in test builds, so normal boot logs don't
    // give KASLR away.
    #[cfg(feature = "ktest")]
    crate::println!(
        "kaslr: kernel {:#x} direct map {:#x} boot stack {:#x}",
        l.kernel_virt,
        l.direct_map,
        l.boot_stack_top
    );
    acpi::report(acpi::init(info));
    #[cfg(feature = "ktest")]
    if let Some(words) = boot::arg(info.cmdline(), "irqdirt") {
        irq::dirt::apply(info, words);
    }
    irq::report(irq::init(info));
    let t = time::init(info);
    time::report(t);
    clock::init(t);
    timer::report(timer::init(t));
    // With a faked CPUID claim the clock runs wrong until the refinement
    // replaces it, so the tests start after it (crate::time::fake).
    #[cfg(feature = "ktest")]
    if time::fake::current().claim_ppm.is_some() && !time::refine::wait(time::refine::WAIT_NS) {
        panic!("the refinement did not finish");
    }
    // With a faked TSC fault the tests run after the watchdog has dealt
    // with it, so the whole suite runs on the clock it fell back to (or,
    // for a fault under its limit, on the TSC it left alone).
    #[cfg(feature = "ktest")]
    if time::fake::current().fault.is_some() {
        time::watchdog::tests::wait_for_the_fault();
    }
    paint_screen(info);

    let cmdline = info.cmdline();
    if let Some(name) = boot::arg(cmdline, "crashtest") {
        crashtest::run(name);
    }

    #[cfg(feature = "ktest")]
    ktest::run_all();

    crate::println!("{BOOT_OK}");
    idle();
}

/// Nothing to run yet: wait for interrupts (the timer's events, which
/// run the clock's 10 s refinement and then its watchdog), and report
/// each of those as it has news. With no interrupt controller in use,
/// just halt.
fn idle() -> ! {
    if irq::controllers().is_none() {
        arch::cpu::halt_forever();
    }
    loop {
        // Checked with interrupts off, then enabled and halted in one
        // step, so news that arrives in between still wakes us.
        arch::cpu::interrupts_disable();
        let refined = time::refine::finished();
        if refined && !time::refine::reported() || time::watchdog::pending() {
            arch::cpu::interrupts_enable();
            if refined {
                time::refine::report();
            }
            time::watchdog::report();
            continue;
        }
        arch::cpu::enable_and_halt();
    }
}

fn on(b: bool) -> &'static str {
    if b { "on" } else { "unsupported" }
}

fn report_memory(info: &bootinfo::BootInfo) {
    let usable: u64 = info
        .memory_map()
        .iter()
        .filter(|e| e.kind == bootinfo::Kind::Usable)
        .map(|e| e.len)
        .sum();
    crate::println!(
        "memory: {} MiB usable in {} map entries",
        usable / (1024 * 1024),
        info.memory_map().len()
    );
    let s = mm::frames::init(info);
    crate::println!(
        "frames: {} MiB free in {} zone(s), {} KiB of allocator metadata",
        s.free_bytes / (1024 * 1024),
        s.zones,
        s.metadata_bytes / 1024
    );
}

/// Fill the screen with a dark blue and draw a lighter band across the
/// middle, so a real PC visibly shows the kernel is in control.
fn paint_screen(info: &bootinfo::BootInfo) {
    let Some(fb) = info.framebuffer else {
        crate::println!("screen: none (headless)");
        return;
    };
    crate::println!("screen: {}x{} @ {} bpp", fb.width, fb.height, fb.bpp);
    if fb.bpp != 32 {
        return;
    }
    let (w, h, pitch) = (fb.width, fb.height, fb.pitch);
    let base = mm::frames::phys_to_virt(fb.phys) as *mut u8;
    for y in 0..h {
        let colour: u32 = if y > h * 9 / 20 && y < h * 11 / 20 {
            0x3d7eff
        } else {
            0x0b1626
        };
        for x in 0..w {
            let offset = (y * pitch + x * 4) as usize;
            // SAFETY: (x, y) is inside the framebuffer, which the kernel maps
            // write-combining in its direct map; each pixel is 4 bytes at
            // `pitch` bytes per row.
            unsafe { base.add(offset).cast::<u32>().write_volatile(colour) };
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(feature = "ktest")]
    if let Some(test) = ktest::current_name() {
        crate::println!("KTEST FAIL {test}");
    }
    crate::println!("OPTIMAL: KERNEL PANIC: {info}");
    backtrace::print_here();
    arch::cpu::halt_forever();
}
