//! Kernel tests for the kernel's own page tables, run on them.

use optimal_mm::PAGE_SIZE;
use optimal_mm::paging::{ADDR_MASK, Cache, Flags, PageSize};

use crate::arch::cpu;
use crate::bootinfo::{self, Kind};
use crate::ktest;
use crate::mm::paging::{self, DIRECT_REGION, STACK_REGION, kernel_sections, layout, translate};

const GIB: u64 = 1 << 30;

ktest!(the_kernel_runs_on_its_own_tables, {
    assert_eq!(cpu::read_cr3() & ADDR_MASK, layout().root);
});

ktest!(kernel_sections_are_mapped_w_xor_x, {
    let info = bootinfo::get();
    let nx = layout().nx;
    for (start, end, flags, name) in kernel_sections() {
        assert!(start < end, "{name} is empty");
        assert_eq!(start % PAGE_SIZE, 0, "{name} start not page aligned");
        assert_eq!(end % PAGE_SIZE, 0, "{name} end not page aligned");
        let mut v = start;
        while v < end {
            let t = translate(v).unwrap_or_else(|| panic!("{name} page {v:#x} unmapped"));
            assert_eq!(
                t.phys,
                info.kernel_phys + (v - info.kernel_virt),
                "{name} at {v:#x}"
            );
            assert_eq!(t.size, PageSize::Size4K);
            let want = Flags {
                executable: flags.executable || !nx,
                ..flags
            };
            assert_eq!(
                t.flags, want,
                "{name} page {v:#x} has the wrong permissions"
            );
            assert!(t.flags.global);
            v += PAGE_SIZE;
        }
    }
    // The four sections tile the image with no gaps, in this order.
    let s = kernel_sections();
    for w in s.windows(2) {
        assert_eq!(w[0].1, w[1].0, "gap between {} and {}", w[0].3, w[1].3);
    }
    // Nothing is mapped just below or just above the image.
    assert!(translate(s[0].0 - PAGE_SIZE).is_none());
    assert!(translate(s[3].1).is_none());
});

ktest!(direct_map_covers_all_ram_and_nothing_else, {
    let info = bootinfo::get();
    let l = layout();
    let dm = l.direct_map;
    assert_eq!(dm % GIB, 0, "direct map base not 1 GiB aligned");
    assert!(DIRECT_REGION.0 <= dm && dm < DIRECT_REGION.1);
    let map = info.memory_map();
    let ram = |p: u64| {
        map.iter().any(|e| {
            e.kind.is_ram()
                && e.base & !(PAGE_SIZE - 1) <= p
                && p < (e.base + e.len).next_multiple_of(PAGE_SIZE)
        })
    };
    let fb = |p: u64| {
        info.framebuffer.is_some_and(|f| {
            f.phys & !(PAGE_SIZE - 1) <= p
                && p < (f.phys + f.pitch * f.height).next_multiple_of(PAGE_SIZE)
        })
    };
    let mut checked = 0;
    for e in map {
        // Probe the first page, the last page and one in the middle.
        let first = e.base & !(PAGE_SIZE - 1);
        let last = (e.base + e.len - 1) & !(PAGE_SIZE - 1);
        for p in [first, ((first + last) / 2) & !(PAGE_SIZE - 1), last] {
            let t = translate(dm + p);
            if ram(p) {
                let t = t.unwrap_or_else(|| {
                    panic!("RAM page {p:#x} ({:?}) not in the direct map", e.kind)
                });
                assert_eq!(t.phys, p);
                assert!(t.flags.writable && !t.flags.user && t.flags.global);
                assert!(
                    !t.flags.executable || !l.nx,
                    "direct map is executable at {p:#x}"
                );
                assert_eq!(t.flags.cache, Cache::WriteBack);
                checked += 1;
            } else if !fb(p) {
                assert!(t.is_none(), "non-RAM page {p:#x} ({:?}) is mapped", e.kind);
            }
        }
    }
    assert!(checked > 0);
    // A hole that is in no map entry at all (the PCI hole below 4 GiB, here
    // the local APIC page) is not mapped.
    if !map
        .iter()
        .any(|e| e.base <= 0xfee0_0000 && 0xfee0_0000 < e.base + e.len)
    {
        assert!(
            translate(dm + 0xfee0_0000).is_none(),
            "the LAPIC page is in the direct map"
        );
    }
});

ktest!(framebuffer_is_write_combining, {
    let Some(f) = bootinfo::get().framebuffer else {
        return;
    };
    let t = translate(layout().direct_map + f.phys).expect("framebuffer not mapped");
    assert_eq!(t.phys, f.phys);
    let want = if cpu::features().pat {
        Cache::WriteCombining
    } else {
        Cache::Uncached
    };
    assert_eq!(t.flags.cache, want);
    assert!(t.flags.writable && (!t.flags.executable || !layout().nx));
    assert_eq!(cpu::read_msr(cpu::MSR_PAT), optimal_mm::paging::PAT_VALUE);
});

ktest!(boot_stack_has_an_unmapped_guard_page, {
    let l = layout();
    assert_eq!(l.boot_stack_guard % GIB, 0);
    assert!(STACK_REGION.0 <= l.boot_stack_guard && l.boot_stack_top < STACK_REGION.1);
    assert!(translate(l.boot_stack_guard).is_none());
    assert!(paging::in_stack_guard(l.boot_stack_guard + 8));
    assert!(!paging::in_stack_guard(l.boot_stack_guard + PAGE_SIZE));
    let bottom = l.boot_stack_guard + PAGE_SIZE;
    for v in (bottom..l.boot_stack_top).step_by(PAGE_SIZE as usize) {
        let t = translate(v).expect("boot stack page unmapped");
        assert!(t.flags.writable);
        assert!(!t.flags.executable || !l.nx);
    }
    assert!(translate(l.boot_stack_top).is_none());
    // We are running on it.
    let rsp: u64;
    // SAFETY: reading RSP has no side effects.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags))
    };
    assert!(
        bottom < rsp && rsp <= l.boot_stack_top,
        "rsp {rsp:#x} is not on the boot stack"
    );
});

ktest!(limine_direct_map_is_gone, {
    // Limine's HHDM sits at a fixed, well-known offset: once the kernel's
    // own tables are on, nothing may be mapped there (unless the random
    // base happened to land on the same address).
    let info = bootinfo::get();
    if layout().direct_map == info.hhdm {
        return;
    }
    let usable = info
        .memory_map()
        .iter()
        .find(|e| e.kind == Kind::Usable)
        .unwrap();
    let probe = info.hhdm + usable.base;
    let dm = layout().direct_map;
    let top = info
        .memory_map()
        .iter()
        .map(|e| e.base + e.len)
        .max()
        .unwrap();
    // Only meaningful if the probe isn't inside our own direct map.
    if !(dm <= probe && probe < dm + top) {
        assert!(
            translate(probe).is_none(),
            "Limine's direct map is still mapped"
        );
    }
});

ktest!(locks_restore_the_interrupt_state, {
    use crate::sync::IrqSpinLock;
    static L1: IrqSpinLock<u32> = IrqSpinLock::new(0);
    static L2: IrqSpinLock<u32> = IrqSpinLock::new(0);
    let before = cpu::interrupts_enabled();
    {
        let mut a = L1.lock();
        assert!(!cpu::interrupts_enabled());
        {
            let mut b = L2.lock();
            *b += 1;
            assert!(!cpu::interrupts_enabled());
        }
        // Still off: the outer lock is held.
        assert!(!cpu::interrupts_enabled());
        *a += 1;
    }
    assert_eq!(cpu::interrupts_enabled(), before);
    assert_eq!((*L1.lock(), *L2.lock()), (1, 1));
});
