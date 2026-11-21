//! In-kernel test runner.
//!
//! Tests run inside the real kernel under QEMU, because allocators, page
//! tables and interrupt handling can't be trusted from host-side tests alone.
//! Tests are registered with `ktest!` and collected by the linker into the
//! `.ktests` section, so adding a test never means editing a central list.
//!
//! Built only with `--features ktest`. CI boots that build and requires the
//! final "OPTIMAL: ktest" summary line with zero failures.

use crate::println;

pub struct Test {
    pub name: &'static str,
    pub func: fn(),
}

#[macro_export]
macro_rules! ktest {
    ($name:ident, $body:block) => {
        const _: () = {
            fn $name() $body
            #[used]
            #[unsafe(link_section = ".ktests")]
            static TEST: $crate::ktest::Test = $crate::ktest::Test {
                name: concat!(module_path!(), "::", stringify!($name)),
                func: $name,
            };
        };
    };
}

unsafe extern "C" {
    // Linker-defined boundary symbols: only their addresses are meaningful.
    static __ktests_start: u8;
    static __ktests_end: u8;
}

fn all() -> &'static [Test] {
    // SAFETY: the linker script places every `Test` static contiguously
    // between these two symbols, and they are never mutated.
    unsafe {
        let start = (&raw const __ktests_start).cast::<Test>();
        let end = (&raw const __ktests_end).cast::<Test>();
        let len = (end as usize - start as usize) / core::mem::size_of::<Test>();
        core::slice::from_raw_parts(start, len)
    }
}

/// Run every registered test. A failing test panics, which the panic handler
/// reports as "KTEST FAIL", so reaching the summary means all passed.
pub fn run_all() {
    let tests = all();
    println!("OPTIMAL: running {} kernel tests", tests.len());
    for t in tests {
        crate::ktest::CURRENT.store(
            t as *const Test as *mut Test,
            core::sync::atomic::Ordering::SeqCst,
        );
        (t.func)();
        println!("KTEST PASS {}", t.name);
    }
    crate::ktest::CURRENT.store(core::ptr::null_mut(), core::sync::atomic::Ordering::SeqCst);
    println!("OPTIMAL: ktest {} passed, 0 failed", tests.len());
}

/// The test currently running, so the panic handler can name it.
pub static CURRENT: core::sync::atomic::AtomicPtr<Test> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

pub fn current_name() -> Option<&'static str> {
    let p = CURRENT.load(core::sync::atomic::Ordering::SeqCst);
    // SAFETY: CURRENT only ever holds null or a pointer into the static test table.
    unsafe { p.as_ref() }.map(|t| t.name)
}
