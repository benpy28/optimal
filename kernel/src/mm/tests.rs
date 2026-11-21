//! Kernel tests for physical memory: the frame allocator on the real
//! (virtual) machine's memory map.

use optimal_mm::PAGE_SIZE;
use optimal_mm::frames::FreeError;

use crate::bootinfo::{self, Kind};
use crate::ktest;
use crate::mm::LOW_MEMORY_END;
use crate::mm::frames::{self, phys_to_virt};

const MAGIC: u64 = 0x0b71_3a1f_5eed_f00d;

fn usable(phys: u64) -> bool {
    bootinfo::get()
        .memory_map()
        .iter()
        .any(|e| e.kind == Kind::Usable && e.base <= phys && phys - e.base < e.len)
}

fn in_metadata(phys: u64) -> bool {
    let (zones, n) = frames::zones();
    zones[..n]
        .iter()
        .any(|z| z.meta.start <= phys && phys < z.meta.end)
}

ktest!(every_free_frame_is_handed_out_exactly_once, {
    // Take every frame, one at a time, threading a list through the frames
    // themselves: word 0 = next frame, word 1 = own address ^ MAGIC, and
    // the last word = own address. Then walk the list and check each frame
    // still holds what was written into it: if two allocations overlapped,
    // or a frame were also used for metadata, a later write would show.
    let before = frames::free_bytes();
    assert!(before > 0);
    let mut head = 0u64;
    let mut count = 0u64;
    while let Some(f) = frames::alloc(0) {
        assert_eq!(f % PAGE_SIZE, 0, "frame {f:#x} not page aligned");
        assert!(f >= LOW_MEMORY_END, "frame {f:#x} below 1 MiB");
        assert!(usable(f), "frame {f:#x} is not usable memory");
        assert!(!in_metadata(f), "frame {f:#x} is allocator metadata");
        let p = phys_to_virt(f) as *mut u64;
        // SAFETY: we own frame f now; it is mapped by the direct map.
        unsafe {
            p.write_volatile(head);
            p.add(1).write_volatile(f ^ MAGIC);
            p.add(511).write_volatile(f);
        }
        head = f;
        count += 1;
    }
    assert_eq!(count * PAGE_SIZE, before, "allocated less than was free");
    assert_eq!(frames::free_bytes(), 0);
    assert_eq!(frames::alloc(0), None);
    // Walk and free.
    let mut seen = 0u64;
    let mut f = head;
    while f != 0 {
        let p = phys_to_virt(f) as *const u64;
        // SAFETY: still ours until freed below.
        let (next, tag, last) = unsafe {
            (
                p.read_volatile(),
                p.add(1).read_volatile(),
                p.add(511).read_volatile(),
            )
        };
        assert_eq!(tag, f ^ MAGIC, "frame {f:#x} was overwritten");
        assert_eq!(last, f, "frame {f:#x} was overwritten at its end");
        frames::free(f, 0).unwrap();
        seen += 1;
        f = next;
    }
    assert_eq!(seen, count);
    assert_eq!(frames::free_bytes(), before);
    frames::check().unwrap();
});

ktest!(blocks_of_every_order_are_aligned, {
    let before = frames::free_bytes();
    for order in 0..=optimal_mm::frames::MAX_ORDER {
        let size = PAGE_SIZE << order;
        if let Some(b) = frames::alloc(order) {
            assert_eq!(b % size, 0, "order {order} block {b:#x} misaligned");
            assert!(usable(b) && usable(b + size - 1));
            frames::free(b, order).unwrap();
        } else {
            // Only acceptable if the machine has no free block that big.
            assert!(
                before < size * 2,
                "no order-{order} block in {before} free bytes"
            );
        }
    }
    assert_eq!(frames::alloc(optimal_mm::frames::MAX_ORDER + 1), None);
    assert_eq!(frames::free_bytes(), before);
    frames::check().unwrap();
});

ktest!(bad_frees_are_refused_and_change_nothing, {
    let before = frames::free_bytes();
    let b = frames::alloc(2).unwrap();
    assert_eq!(
        frames::free(b, 1),
        Err(FreeError::WrongOrder { allocated: 2 })
    );
    assert_eq!(frames::free(b + PAGE_SIZE, 0), Err(FreeError::NotAllocated));
    assert_eq!(frames::free(b + 8, 2), Err(FreeError::NotAllocated));
    assert_eq!(frames::free(1 << 51, 0), Err(FreeError::OutOfZone));
    assert_eq!(frames::free(b, 99), Err(FreeError::BadOrder));
    frames::free(b, 2).unwrap();
    assert_eq!(frames::free(b, 2), Err(FreeError::NotAllocated));
    // Memory below 1 MiB is never the allocator's.
    assert!(matches!(
        frames::free(0x8000, 0),
        Err(FreeError::NotAllocated | FreeError::OutOfZone)
    ));
    assert_eq!(frames::free_bytes(), before);
    frames::check().unwrap();
});

ktest!(boot_info_was_captured, {
    let info = bootinfo::get();
    assert!(!info.memory_map().is_empty());
    assert!(info.hhdm >= 0xffff_8000_0000_0000);
    assert_eq!(info.kernel_phys % PAGE_SIZE, 0);
    assert!(info.kernel_virt >= 0xffff_ffff_8000_0000);
    // The kernel image is in the map as kernel memory, never as usable.
    assert!(
        info.memory_map()
            .iter()
            .any(|e| e.kind == Kind::KernelAndModules
                && e.base <= info.kernel_phys
                && info.kernel_phys - e.base < e.len)
    );
    assert!(!usable(info.kernel_phys));
    // Map entries are sorted and don't overlap.
    for w in info.memory_map().windows(2) {
        assert!(w[0].base + w[0].len <= w[1].base, "memory map overlaps");
    }
});
