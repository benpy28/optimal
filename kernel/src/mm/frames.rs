//! The physical frame allocator: one buddy allocator (optimal-mm) per zone
//! of physical memory, behind an interrupt-safe lock.
//!
//! Zones and where their metadata lives are planned by
//! `optimal_mm::zones::plan` from the usable entries of the memory map;
//! the metadata arrays are reached through the current direct map.

use core::sync::atomic::{AtomicU64, Ordering};

use optimal_mm::PAGE_SIZE;
use optimal_mm::frames::{Buddy, FrameMeta, FreeError};
use optimal_mm::zones::{self, Range, Zone};

use crate::bootinfo::{BootInfo, Kind, MAX_MAP_ENTRIES};
use crate::sync::IrqSpinLock;

/// Most zones supported. QEMU and ordinary PCs need one to three.
pub const MAX_ZONES: usize = 64;

/// Virtual address = physical address + this. Limine's HHDM offset until
/// the kernel runs on its own page tables.
static DIRECT_MAP: AtomicU64 = AtomicU64::new(0);

pub fn phys_to_virt(phys: u64) -> u64 {
    phys + DIRECT_MAP.load(Ordering::Relaxed)
}

struct Allocator {
    zones: [Zone; MAX_ZONES],
    buddies: [Option<Buddy<'static>>; MAX_ZONES],
    count: usize,
}

static FRAMES: IrqSpinLock<Allocator> = IrqSpinLock::new(Allocator {
    zones: [Zone::EMPTY; MAX_ZONES],
    buddies: [const { None }; MAX_ZONES],
    count: 0,
});

pub struct Summary {
    pub zones: usize,
    pub free_bytes: u64,
    pub metadata_bytes: u64,
}

/// Bring up the allocator from the memory map. Call once, after the boot
/// info is captured, while Limine's HHDM is still the direct map.
pub fn init(info: &BootInfo) -> Summary {
    DIRECT_MAP.store(info.hhdm, Ordering::Relaxed);
    let mut usable = [Range::EMPTY; MAX_MAP_ENTRIES];
    let mut n = 0;
    for e in info.memory_map().iter().filter(|e| e.kind == Kind::Usable) {
        usable[n] = Range {
            start: e.base,
            end: e.base.saturating_add(e.len),
        };
        n += 1;
    }
    let usable = &usable[..n];

    let mut a = FRAMES.lock();
    assert!(a.count == 0, "frame allocator initialised twice");
    let count = match zones::plan(usable, super::LOW_MEMORY_END, &mut a.zones) {
        Ok(c) => c,
        Err(e) => panic!("can't plan physical memory zones: {e:?}"),
    };
    a.count = count;
    let mut metadata_bytes = 0;
    for i in 0..count {
        let z = a.zones[i];
        let ptr = phys_to_virt(z.meta.start) as *mut FrameMeta;
        // SAFETY: the planner carved [meta.start, meta.end) out of usable
        // RAM that nothing else owns, it is mapped by the direct map, it is
        // big enough for `frames` entries, and FrameMeta's alignment (4)
        // divides the page-aligned start. Each entry is written before the
        // slice is formed, so no invalid value is ever read.
        let meta = unsafe {
            for k in 0..z.frames as usize {
                ptr.add(k).write(FrameMeta::RESERVED);
            }
            core::slice::from_raw_parts_mut(ptr, z.frames as usize)
        };
        a.buddies[i] = Some(Buddy::new(meta, z.base_frame));
        metadata_bytes += z.meta.len();
    }
    let zones_copy = a.zones;
    zones::free_ranges(usable, super::LOW_MEMORY_END, &zones_copy[..count], |r| {
        let i =
            zones::zone_of(&zones_copy[..count], r.start).expect("free range outside every zone");
        let b = a.buddies[i].as_mut().unwrap();
        let frames = (r.end - r.start) / PAGE_SIZE;
        let added = b.add_range(r.start / PAGE_SIZE, frames);
        assert_eq!(added, frames, "usable memory added twice");
    });
    let free_bytes = a.free_frames() * PAGE_SIZE;
    Summary {
        zones: count,
        free_bytes,
        metadata_bytes,
    }
}

impl Allocator {
    fn free_frames(&self) -> u64 {
        self.buddies[..self.count]
            .iter()
            .map(|b| b.as_ref().unwrap().free_frames())
            .sum()
    }
}

/// Allocate 2^order contiguous frames, aligned to their size. Returns the
/// physical address of the first. Lowest zone first.
pub fn alloc(order: usize) -> Option<u64> {
    let mut a = FRAMES.lock();
    let count = a.count;
    a.buddies[..count]
        .iter_mut()
        .find_map(|b| b.as_mut().unwrap().alloc(order))
        .map(|f| f * PAGE_SIZE)
}

/// Free a block from `alloc(order)`. A bad free (double free, wrong order,
/// not a block start, unknown memory) is refused and changes nothing.
pub fn free(phys: u64, order: usize) -> Result<(), FreeError> {
    if !phys.is_multiple_of(PAGE_SIZE) {
        return Err(FreeError::NotAllocated);
    }
    let mut a = FRAMES.lock();
    let count = a.count;
    let i = zones::zone_of(&a.zones[..count], phys).ok_or(FreeError::OutOfZone)?;
    a.buddies[i].as_mut().unwrap().free(phys / PAGE_SIZE, order)
}

pub fn free_bytes() -> u64 {
    FRAMES.lock().free_frames() * PAGE_SIZE
}

/// Check every zone's invariants (tests and debugging).
pub fn check() -> Result<(), &'static str> {
    let a = FRAMES.lock();
    for b in &a.buddies[..a.count] {
        b.as_ref().unwrap().check()?;
    }
    Ok(())
}

/// The planned zones (tests and diagnostics).
pub fn zones() -> ([Zone; MAX_ZONES], usize) {
    let a = FRAMES.lock();
    (a.zones, a.count)
}
