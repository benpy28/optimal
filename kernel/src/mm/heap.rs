//! The kernel heap (`GlobalAlloc`), so kernel code can use `Box`, `Vec` and
//! friends.
//!
//! Requests of up to 2048 bytes (and alignment) come from slabs: 4 KiB pages
//! cut into objects of one size class, with a checked header and bitmap at
//! the start of each page (`optimal_mm::slab`). Nothing is ever written into
//! freed memory, and every free is checked: a double free, a pointer that
//! isn't an object start, or a pointer into a page that isn't a slab of this
//! kernel stops the kernel with a clear report instead of corrupting memory.
//! Bigger requests take whole buddy blocks from the frame allocator, whose
//! frees are checked the same way.
//!
//! All heap memory lives in the direct map (read-write, no-execute). The
//! heap only works once the kernel runs on its own page tables.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

use optimal_mm::slab::{CLASSES, SLAB_BYTES, SlabHeader, class_for, large_order};

use crate::mm::frames;
use crate::sync::IrqSpinLock;

struct State {
    /// Per class: the first slab page (virtual address) with free room, or
    /// 0. A doubly linked list through the slab headers.
    partial: [u64; CLASSES.len()],
    /// Boot-time secret mixed into every slab header's magic.
    secret: u64,
    ready: bool,
    slab_pages: u64,
}

static STATE: IrqSpinLock<State> = IrqSpinLock::new(State {
    partial: [0; CLASSES.len()],
    secret: 0,
    ready: false,
    slab_pages: 0,
});

pub struct KernelHeap;

#[global_allocator]
static HEAP: KernelHeap = KernelHeap;

/// Turn the heap on. Call once, on the kernel's own page tables.
pub fn init(secret: u64) {
    let mut s = STATE.lock();
    assert!(!s.ready, "heap initialised twice");
    s.secret = secret;
    s.ready = true;
}

/// Slab pages in use right now (tests).
#[cfg(feature = "ktest")]
pub fn slab_pages() -> u64 {
    STATE.lock().slab_pages
}

/// # Safety
/// `page` must be a live slab page of this heap.
unsafe fn header<'a>(page: u64) -> &'a mut SlabHeader {
    // SAFETY: forwarded; the header is the first 64 bytes of the page.
    unsafe { &mut *(page as *mut SlabHeader) }
}

impl State {
    /// # Safety
    /// `page` must be a slab page whose header is valid and not on a list.
    unsafe fn push(&mut self, page: u64) {
        let c;
        // SAFETY: forwarded from the caller; list neighbours are live slabs.
        unsafe {
            let h = header(page);
            c = h.class();
            h.prev = 0;
            h.next = self.partial[c];
            h.on_list = true;
            if h.next != 0 {
                header(h.next).prev = page;
            }
        }
        self.partial[c] = page;
    }

    /// # Safety
    /// `page` must be a slab page on its class's list.
    unsafe fn unlink(&mut self, page: u64) {
        // SAFETY: forwarded from the caller; list neighbours are live slabs.
        unsafe {
            let h = header(page);
            let (next, prev, c) = (h.next, h.prev, h.class());
            if prev == 0 {
                self.partial[c] = next;
            } else {
                header(prev).next = next;
            }
            if next != 0 {
                header(next).prev = prev;
            }
            h.next = 0;
            h.prev = 0;
            h.on_list = false;
        }
    }
}

fn bad_free(ptr: *mut u8, layout: Layout, why: &str) -> ! {
    panic!(
        "heap: bad free of {ptr:p} (size {}, align {}): {why}",
        layout.size(),
        layout.align()
    );
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(c) = class_for(layout.size(), layout.align()) else {
            let order = large_order(layout.size(), layout.align());
            assert!(STATE.lock().ready, "heap used before it was initialised");
            return match frames::alloc(order) {
                Some(phys) => frames::phys_to_virt(phys) as *mut u8,
                None => null_mut(),
            };
        };
        let mut s = STATE.lock();
        assert!(s.ready, "heap used before it was initialised");
        if s.partial[c] == 0 {
            let Some(phys) = frames::alloc(0) else {
                return null_mut();
            };
            let page = frames::phys_to_virt(phys);
            // SAFETY: a fresh page we own; the header goes at its start.
            unsafe {
                (page as *mut SlabHeader).write(SlabHeader::new(page, c, s.secret));
                s.push(page);
            }
            s.slab_pages += 1;
        }
        let page = s.partial[c];
        // SAFETY: pages on the list are live slabs of class c.
        unsafe {
            let h = header(page);
            let off = h.alloc().expect("slab on the free list is full");
            if h.is_full() {
                s.unlink(page);
            }
            (page + off as u64) as *mut u8
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let addr = ptr as u64;
        let Some(c) = class_for(layout.size(), layout.align()) else {
            let order = large_order(layout.size(), layout.align());
            if let Err(e) = frames::free(frames::virt_to_phys(addr), order) {
                bad_free(
                    ptr,
                    layout,
                    match e {
                        optimal_mm::frames::FreeError::NotAllocated => {
                            "not an allocated block (double free?)"
                        }
                        optimal_mm::frames::FreeError::WrongOrder { .. } => {
                            "block has a different size"
                        }
                        _ => "not heap memory",
                    },
                );
            }
            return;
        };
        let page = addr & !(SLAB_BYTES as u64 - 1);
        let mut s = STATE.lock();
        // Only read a header from a page the frame allocator says is an
        // allocated single frame; a pointer anywhere else can't be ours.
        if !frames::is_allocated_frame(page) {
            drop(s);
            bad_free(ptr, layout, "not heap memory");
        }
        let secret = s.secret;
        // SAFETY: the page is mapped RAM; the header is only trusted after
        // its magic (secret ^ page) checks out inside `free`.
        let h = unsafe { header(page) };
        if !h.valid(page, secret) {
            drop(s);
            bad_free(ptr, layout, "not a heap slab (bad header magic)");
        }
        if h.class() != c {
            drop(s);
            bad_free(
                ptr,
                layout,
                "freed with a different size than it was allocated with",
            );
        }
        if let Err(e) = h.free(page, (addr - page) as usize, secret) {
            drop(s);
            bad_free(
                ptr,
                layout,
                match e {
                    optimal_mm::slab::SlabError::NotAllocated => "double free",
                    optimal_mm::slab::SlabError::Misaligned => "not the start of an object",
                    optimal_mm::slab::SlabError::BadMagic => "not a heap slab (bad header magic)",
                },
            );
        }
        // SAFETY: page is a live slab of class c (header checked above).
        unsafe {
            if !h.on_list {
                s.push(page);
            } else if h.is_empty() && (h.next != 0 || h.prev != 0) {
                // Empty, and not the only slab with room in its class: give
                // the page back. One empty slab per class stays cached.
                s.unlink(page);
                s.slab_pages -= 1;
                frames::free(frames::virt_to_phys(page), 0).expect("slab page not allocated");
            }
        }
    }
}
