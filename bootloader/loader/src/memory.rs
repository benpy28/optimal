//! Memory from the firmware: a global allocator on top of AllocatePool for
//! the loader's own bookkeeping, and page allocations for everything that
//! is handed to the kernel.

use crate::console::{boot_services, boot_services_active};
use crate::efi;
use core::alloc::{GlobalAlloc, Layout};

struct PoolAllocator;

// SAFETY: AllocatePool returns 8-byte aligned memory; bigger alignments are
// handled by over-allocating and storing the original pointer just below the
// returned block. After ExitBootServices every allocation fails cleanly.
unsafe impl GlobalAlloc for PoolAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !boot_services_active() {
            return core::ptr::null_mut();
        }
        let align = layout.align().max(8);
        let size = layout.size() + align;
        let mut p: *mut u8 = core::ptr::null_mut();
        if (boot_services().allocate_pool)(efi::LOADER_DATA, size, &mut p) != efi::SUCCESS {
            return core::ptr::null_mut();
        }
        let aligned = ((p as usize + 8 + align - 1) & !(align - 1)) as *mut u8;
        // SAFETY: aligned - 8 >= p because aligned >= p + 8.
        unsafe { (aligned as *mut *mut u8).sub(1).write(p) };
        aligned
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if !boot_services_active() {
            return; // memory is the kernel's to reclaim now
        }
        // SAFETY: alloc() stored the pool pointer just below the block.
        let orig = unsafe { (ptr as *mut *mut u8).sub(1).read() };
        (boot_services().free_pool)(orig);
    }
}

#[global_allocator]
static ALLOCATOR: PoolAllocator = PoolAllocator;

pub const PAGE: u64 = 4096;

pub fn pages_for(bytes: u64) -> u64 {
    bytes.div_ceil(PAGE)
}

/// Allocates zeroed pages. `max_address` keeps them below a limit.
pub fn alloc_pages(pages: u64, typ: u32, max_address: Option<u64>) -> Option<u64> {
    let mut addr = max_address.unwrap_or(0);
    let kind = if max_address.is_some() {
        efi::ALLOCATE_MAX_ADDRESS
    } else {
        efi::ALLOCATE_ANY_PAGES
    };
    if (boot_services().allocate_pages)(kind, typ, pages as usize, &mut addr) != efi::SUCCESS {
        return None;
    }
    // SAFETY: the firmware just gave us these pages; they are identity mapped.
    unsafe { core::ptr::write_bytes(addr as *mut u8, 0, (pages * PAGE) as usize) };
    Some(addr)
}

/// Allocates pages at exactly `addr`.
pub fn alloc_pages_at(addr: u64, pages: u64, typ: u32) -> bool {
    let mut a = addr;
    (boot_services().allocate_pages)(efi::ALLOCATE_ADDRESS, typ, pages as usize, &mut a)
        == efi::SUCCESS
}

pub fn free_pages(addr: u64, pages: u64) {
    (boot_services().free_pages)(addr, pages as usize);
}

/// A run of pages owned by the loader, viewed as bytes.
pub struct Pages {
    pub phys: u64,
    pub pages: u64,
}

impl Pages {
    pub fn new(bytes: u64, typ: u32, max_address: Option<u64>) -> Option<Pages> {
        let pages = pages_for(bytes.max(1));
        Some(Pages {
            phys: alloc_pages(pages, typ, max_address)?,
            pages,
        })
    }
    pub fn len(&self) -> usize {
        (self.pages * PAGE) as usize
    }
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: we own these identity-mapped pages for the whole boot.
        unsafe { core::slice::from_raw_parts_mut(self.phys as *mut u8, self.len()) }
    }
    pub fn free(self) {
        free_pages(self.phys, self.pages);
    }
}

/// Simple bump allocator over a block of pages, for data handed to the kernel.
pub struct Arena {
    pub base: u64,
    pub size: u64,
    pub used: u64,
}

impl Arena {
    pub fn new(base: u64, size: u64) -> Arena {
        Arena {
            base,
            size,
            used: 0,
        }
    }
    pub fn alloc(&mut self, bytes: u64, align: u64) -> Option<u64> {
        let start = (self.base + self.used).checked_add(align - 1)? & !(align - 1);
        let end = start.checked_add(bytes)?;
        if end > self.base + self.size {
            return None;
        }
        self.used = end - self.base;
        // SAFETY: inside the arena's pages, which we own.
        unsafe { core::ptr::write_bytes(start as *mut u8, 0, bytes as usize) };
        Some(start)
    }
}
