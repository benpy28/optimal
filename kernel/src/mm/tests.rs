//! Kernel tests for physical memory: the frame allocator on the real
//! (virtual) machine's memory map.

use alloc::vec;
use alloc::vec::Vec;

use optimal_mm::PAGE_SIZE;
use optimal_mm::frames::FreeError;

use crate::bootinfo::{self, Kind};
use crate::ktest;
use crate::mm::LOW_MEMORY_END;
use crate::mm::frames::{self, phys_to_virt, virt_to_phys};

const MAGIC: u64 = 0x0b71_3a1f_5eed_f00d;

/// Memory the allocator may hand out: usable, the bootloader's (reclaimed
/// once the kernel runs on its own page tables), or ACPI reclaimable memory
/// (reclaimed once the tables are copied), except the FACS's pages.
fn usable(phys: u64) -> bool {
    let in_facs = match crate::acpi::get() {
        Ok(a) => a.facs.is_some_and(|(s, e)| {
            (s & !(PAGE_SIZE - 1)) <= phys && phys < e.next_multiple_of(PAGE_SIZE)
        }),
        Err(_) => false,
    };
    bootinfo::get().memory_map().iter().any(|e| {
        let allocatable = match e.kind {
            Kind::Usable | Kind::BootloaderReclaimable => true,
            Kind::AcpiReclaimable => !in_facs,
            _ => false,
        };
        allocatable && e.base <= phys && phys - e.base < e.len
    })
}

fn in_metadata(phys: u64) -> bool {
    let (zones, n) = frames::zones();
    zones[..n]
        .iter()
        .any(|z| z.meta.start <= phys && phys < z.meta.end)
}

/// Frames below this physical address are all written by the test below;
/// above it, only every `TOUCH_STRIDE`th frame is. Writing to every frame
/// of a 32 GiB guest makes the host back all of it, which is more memory
/// than a CI runner has (the tough-conditions run backs that guest lazily).
/// The bitmap still checks every single frame for duplicates, so only the
/// "is this frame really ours to write" part is sampled above the limit.
const TOUCH_ALL_BELOW: u64 = 1 << 30;
const TOUCH_STRIDE: u64 = 64;

fn touched(phys: u64) -> bool {
    phys < TOUCH_ALL_BELOW || (phys / PAGE_SIZE).is_multiple_of(TOUCH_STRIDE)
}

ktest!(touch_policy_is_as_documented, {
    // Hand-worked: everything below 1 GiB, then frame indices that are
    // multiples of 64 (0x4000_0000 is frame 0x40000, a multiple of 64).
    assert!(touched(LOW_MEMORY_END));
    assert!(touched(TOUCH_ALL_BELOW - PAGE_SIZE));
    assert!(touched(TOUCH_ALL_BELOW));
    assert!(!touched(TOUCH_ALL_BELOW + PAGE_SIZE));
    assert!(!touched(TOUCH_ALL_BELOW + 63 * PAGE_SIZE));
    assert!(touched(TOUCH_ALL_BELOW + 64 * PAGE_SIZE));
    assert!(touched(32 << 30));
    assert!(!touched((32 << 30) + 4 * PAGE_SIZE));
});

ktest!(every_free_frame_is_handed_out_exactly_once, {
    // Take every frame, one at a time. A bitmap (one bit per frame, on the
    // heap, made before draining) records each frame handed out, so a
    // frame handed out twice is caught whatever the machine's size. Each
    // touched frame (see `touched`) also gets word 1 = own address ^ MAGIC
    // and its last word = own address; after draining, every touched frame
    // must still hold them: if two allocations overlapped, or a frame were
    // also used for metadata or the bitmap, a later write would show.
    let top = bootinfo::get()
        .memory_map()
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                Kind::Usable | Kind::BootloaderReclaimable | Kind::AcpiReclaimable
            )
        })
        .map(|e| e.base + e.len)
        .max()
        .unwrap();
    let nframes = top.div_ceil(PAGE_SIZE) as usize;
    let mut bits: Vec<u64> = vec![0; nframes.div_ceil(64)];
    let bits_lo = virt_to_phys(bits.as_ptr() as u64);
    let bits_hi = bits_lo + (bits.len() * 8) as u64;

    let before = frames::free_bytes();
    assert!(before > 0);
    let mut count = 0u64;
    let mut written = 0u64;
    while let Some(f) = frames::alloc(0) {
        assert_eq!(f % PAGE_SIZE, 0, "frame {f:#x} not page aligned");
        assert!(f >= LOW_MEMORY_END, "frame {f:#x} below 1 MiB");
        assert!(usable(f), "frame {f:#x} is not usable memory");
        assert!(!in_metadata(f), "frame {f:#x} is allocator metadata");
        assert!(
            f + PAGE_SIZE <= bits_lo || f >= bits_hi,
            "frame {f:#x} holds the test's own bitmap"
        );
        let i = (f / PAGE_SIZE) as usize;
        assert!(
            bits[i / 64] & (1 << (i % 64)) == 0,
            "frame {f:#x} handed out twice"
        );
        bits[i / 64] |= 1 << (i % 64);
        if touched(f) {
            let p = phys_to_virt(f) as *mut u64;
            // SAFETY: we own frame f now; it is mapped by the direct map.
            unsafe {
                p.add(1).write_volatile(f ^ MAGIC);
                p.add(511).write_volatile(f);
            }
            written += 1;
        }
        count += 1;
    }
    assert_eq!(count * PAGE_SIZE, before, "allocated less than was free");
    assert_eq!(frames::free_bytes(), 0);
    assert_eq!(frames::alloc(0), None);
    // Check and free every frame the bitmap recorded.
    let mut seen = 0u64;
    let mut checked = 0u64;
    for (w, word) in bits.iter().enumerate() {
        let mut m = *word;
        while m != 0 {
            let f = ((w * 64 + m.trailing_zeros() as usize) as u64) * PAGE_SIZE;
            m &= m - 1;
            if touched(f) {
                let p = phys_to_virt(f) as *const u64;
                // SAFETY: still ours until freed below.
                let (tag, last) = unsafe { (p.add(1).read_volatile(), p.add(511).read_volatile()) };
                assert_eq!(tag, f ^ MAGIC, "frame {f:#x} was overwritten");
                assert_eq!(last, f, "frame {f:#x} was overwritten at its end");
                checked += 1;
            }
            frames::free(f, 0).unwrap();
            seen += 1;
        }
    }
    assert_eq!(seen, count);
    assert_eq!(checked, written);
    // Small machines (every test VM up to 1 GiB) are checked in full.
    if top <= TOUCH_ALL_BELOW {
        assert_eq!(written, count);
    }
    assert_eq!(frames::free_bytes(), before);
    drop(bits);
    frames::check().unwrap();
});

ktest!(blocks_of_every_order_are_aligned, {
    let before = frames::free_bytes();
    let largest = frames::largest_free_order();
    for order in 0..=optimal_mm::frames::MAX_ORDER {
        let size = PAGE_SIZE << order;
        // Exactly the orders up to the largest free block must succeed (a
        // big enough block can always be split).
        match frames::alloc(order) {
            Some(b) => {
                assert!(
                    Some(order) <= largest,
                    "order {order} allocated above the largest free block"
                );
                assert_eq!(b % size, 0, "order {order} block {b:#x} misaligned");
                assert!(usable(b) && usable(b + size - 1));
                frames::free(b, order).unwrap();
            }
            None => assert!(
                Some(order) > largest,
                "no order-{order} block though one is free"
            ),
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
