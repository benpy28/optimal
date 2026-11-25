//! Kernel tests for the heap and for running out of memory.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;

use optimal_mm::PAGE_SIZE;
use optimal_mm::paging::{Flags, MapError, PageSize};
use optimal_mm::slab::CLASSES;

use crate::ktest;
use crate::mm::frames::{self, fault};
use crate::mm::heap;
use crate::mm::paging;

ktest!(box_and_vec_work, {
    let b = Box::new(0x5eed_u64);
    assert_eq!(*b, 0x5eed);
    let mut v: Vec<u32> = Vec::new();
    for i in 0..10_000 {
        v.push(i * 3);
    }
    assert!(v.iter().enumerate().all(|(i, x)| *x == i as u32 * 3));
    // Heap memory is in the direct map, writable and not executable.
    let t = paging::translate(v.as_ptr() as u64).unwrap();
    assert!(t.flags.writable && (!t.flags.executable || !paging::layout().nx));
});

ktest!(every_size_class_hands_out_distinct_aligned_objects, {
    // For each class: allocate three slabs' worth, fill each object with a
    // pattern of its own, then check none was overwritten by another.
    let base_pages = heap::slab_pages();
    let free_before = frames::free_bytes();
    for &size in &CLASSES {
        let layout = Layout::from_size_align(size, 8).unwrap();
        let n = 3 * PAGE_SIZE as usize / size;
        let mut ptrs = Vec::with_capacity(n);
        for i in 0..n {
            // SAFETY: a valid, non-zero-size layout.
            let p = unsafe { alloc::alloc::alloc(layout) };
            assert!(!p.is_null());
            assert_eq!(p as usize % size, 0, "{size}-byte object misaligned");
            // SAFETY: we own `size` bytes at p.
            unsafe { core::ptr::write_bytes(p, (i % 251) as u8, size) };
            ptrs.push(p);
        }
        for (i, &p) in ptrs.iter().enumerate() {
            // SAFETY: still ours.
            let s = unsafe { core::slice::from_raw_parts(p, size) };
            assert!(
                s.iter().all(|&b| b == (i % 251) as u8),
                "{size}-byte object {i} overwritten"
            );
        }
        for &p in &ptrs {
            // SAFETY: allocated above with this layout.
            unsafe { alloc::alloc::dealloc(p, layout) };
        }
    }
    // Empty slabs go back to the frame allocator (one cached per class).
    assert!(heap::slab_pages() <= base_pages + CLASSES.len() as u64);
    assert!(frames::free_bytes() + (CLASSES.len() as u64) * PAGE_SIZE >= free_before);
    frames::check().unwrap();
});

ktest!(large_and_over_aligned_allocations, {
    for (size, align) in [
        (4096, 8),
        (5000, 8),
        (1 << 20, 8),
        (64, 4096),
        (100, 1 << 16),
        (3 << 20, 1 << 21),
    ] {
        let layout = Layout::from_size_align(size, align).unwrap();
        // SAFETY: valid layout.
        let p = unsafe { alloc::alloc::alloc(layout) };
        assert!(!p.is_null(), "{size} bytes aligned {align} failed");
        assert_eq!(p as usize % align, 0);
        // SAFETY: we own `size` bytes at p.
        unsafe {
            core::ptr::write_bytes(p, 0xa5, size);
            assert_eq!(*p.add(size - 1), 0xa5);
            alloc::alloc::dealloc(p, layout);
        }
    }
    frames::check().unwrap();
});

ktest!(running_out_of_memory_is_reported_and_harmless, {
    let free_before = frames::free_bytes();
    // Every frame allocation fails: the heap reports failure, nothing breaks.
    fault::fail_every(1);
    let mut v: Vec<u8> = Vec::new();
    assert!(v.try_reserve(1 << 20).is_err());
    assert!(v.try_reserve(8 << 10).is_err());
    fault::fail_every(0);
    // Every third fails: a run of try_reserve calls either succeeds fully
    // or leaves the vector as it was.
    fault::fail_every(3);
    let mut vs: Vec<Vec<u8>> = Vec::new();
    let mut ok = 0;
    for i in 0..60usize {
        let mut v: Vec<u8> = Vec::new();
        match v.try_reserve(5000 + i * 4096) {
            Ok(()) => {
                v.resize(5000 + i * 4096, i as u8);
                ok += 1;
                vs.push(v);
            }
            Err(_) => assert_eq!(v.capacity(), 0),
        }
    }
    fault::fail_every(0);
    assert!(
        ok > 0 && ok < 60,
        "injection didn't hit some but not all ({ok})"
    );
    for (k, v) in vs.iter().enumerate() {
        let first = v[0];
        assert!(v.iter().all(|&b| b == first), "vector {k} corrupted");
    }
    drop(vs);
    assert_eq!(frames::free_bytes(), free_before);
    frames::check().unwrap();
});

ktest!(page_table_map_out_of_memory_changes_nothing, {
    // A fresh mapping far from anything else needs three new tables; with
    // every allocation failing it must report OutOfMemory and leave no
    // translation behind, and the kernel's existing mappings untouched.
    let virt = paging::NEVER_MAPPED - (1u64 << 39) + (5 << 30); // slot 509, unused
    let phys = frames::alloc(0).unwrap();
    let text = paging::kernel_sections()[0].0;
    let before = paging::translate(text);
    for n in [1, 2, 3] {
        fault::fail_every(n);
        let r = paging::map_kernel_page(virt, phys, Flags::READ);
        fault::fail_every(0);
        if r.is_ok() {
            // With n = 3 the third table fails only if three were needed.
            assert!(n > 1);
            assert_eq!(paging::unmap_kernel_page(virt), Ok(phys));
            continue;
        }
        assert_eq!(r, Err(MapError::OutOfMemory));
        assert!(
            paging::translate(virt).is_none(),
            "failed map left a translation"
        );
    }
    assert_eq!(paging::translate(text), before);
    // And with memory it works, then unmaps cleanly.
    paging::map_kernel_page(virt, phys, Flags::READ).unwrap();
    let t = paging::translate(virt + 0x10).unwrap();
    assert_eq!((t.phys, t.size), (phys + 0x10, PageSize::Size4K));
    assert_eq!(paging::unmap_kernel_page(virt), Ok(phys));
    assert!(paging::translate(virt).is_none());
    frames::free(phys, 0).unwrap();
});
