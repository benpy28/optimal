//! Physical memory for the BIOS stage: the E820 map, a page allocator over
//! the usable RAM above 1 MiB, and a small heap for `alloc`.
//!
//! Every allocation is recorded with what it holds, so the handoff can
//! tell the kernel which pages are loader-owned (reclaimable) and which
//! hold the kernel and modules. Memory below 1 MiB is never allocated: it
//! holds the IVT, the BIOS data area, stage 1 and 2, the bounce buffer and
//! the EBDA.

use crate::bios::{self, Regs};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use optimal_boot_core::e820::{self, Converted, E820Entry};
use optimal_boot_core::memmap::{self, EfiRegion, Event, Override};
use optimal_boot_core::paging::{Flags, FrameSource, Mapper};
use optimal_boot_core::protocol::{MEMMAP_BAD_MEMORY, MEMMAP_USABLE, MemmapEntry};

pub const PAGE: u64 = 4096;
pub const MAX_E820: usize = 256;
const MAX_USABLE: usize = 2 * MAX_E820 + 1;
const MAX_ALLOCS: usize = 512;
const LOW_LIMIT: u64 = 0x10_0000;
const HEAP_BYTES: u64 = 4 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Loader data, reclaimable once the kernel runs.
    Loader,
    /// The kernel image and modules.
    Kernel,
}

// What each allocation holds is what the handoff (the BIOS stage's next
// part) reports in the kernel's memory map; until then it is only recorded.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Alloc {
    pub start: u64,
    pub end: u64,
    pub kind: Kind,
}

pub struct State {
    pub e820: [E820Entry; MAX_E820],
    pub e820_count: usize,
    usable: [(u64, u64); MAX_USABLE],
    usable_count: usize,
    allocs: [Alloc; MAX_ALLOCS],
    alloc_count: usize,
    heap: (u64, u64, u64), // start, next, end
}

struct Global(UnsafeCell<State>);
// SAFETY: the BIOS stage is single-threaded with interrupts off.
unsafe impl Sync for Global {}

static STATE: Global = Global(UnsafeCell::new(State {
    e820: [E820Entry {
        base: 0,
        len: 0,
        typ: 0,
        attr: 0,
    }; MAX_E820],
    e820_count: 0,
    usable: [(0, 0); MAX_USABLE],
    usable_count: 0,
    allocs: [Alloc {
        start: 0,
        end: 0,
        kind: Kind::Loader,
    }; MAX_ALLOCS],
    alloc_count: 0,
    heap: (0, 0, 0),
}));

pub fn state() -> &'static mut State {
    // SAFETY: single-threaded; callers never hold two of these across a
    // call that takes another (each function below borrows briefly).
    unsafe { &mut *STATE.0.get() }
}

#[derive(Debug)]
pub enum MemError {
    NoE820,
    TooManyEntries,
    NoMemory,
    BadMap,
}

impl MemError {
    pub fn describe(&self) -> &'static str {
        match self {
            MemError::NoE820 => "the BIOS gives no E820 memory map",
            MemError::TooManyEntries => "the E820 memory map has more than 256 entries",
            MemError::NoMemory => "not enough usable memory above 1 MiB",
            MemError::BadMap => "the E820 memory map is inconsistent",
        }
    }
}

/// Reads the E820 map (INT 15h EAX=E820h), entry by entry.
fn read_e820(s: &mut State) -> Result<(), MemError> {
    let buf = bios::low_buf();
    let (seg, off) = bios::seg_off(buf);
    let mut ebx = 0u32;
    s.e820_count = 0;
    for first in [true].into_iter().chain(core::iter::repeat(false)) {
        // Preset the extended attributes to "enabled", for firmware that
        // writes only 20 bytes (ACPI 6.5, 15.1).
        // SAFETY: the low buffer is ours and real-mode addressable.
        unsafe {
            core::ptr::write_bytes(buf as *mut u8, 0, 24);
            core::ptr::write_unaligned((buf + 20) as *mut u32, e820::ATTR_ENABLED);
        }
        let r = bios::int(
            0x15,
            Regs {
                eax: 0xe820,
                ebx,
                ecx: 24,
                edx: 0x534d_4150, // "SMAP"
                edi: off as u32,
                es: seg,
                ..Default::default()
            },
        );
        if r.carry() || r.eax != 0x534d_4150 {
            // Carry on the first call: no E820. Later: the end of the list.
            if first {
                return Err(MemError::NoE820);
            }
            break;
        }
        let len = (r.ecx as usize).clamp(20, 24);
        if r.ecx >= 20 {
            // SAFETY: the BIOS wrote `len` bytes into the low buffer.
            let raw = unsafe { core::slice::from_raw_parts(buf as *const u8, len) };
            if let Some(e) = e820::parse(raw) {
                if s.e820_count == MAX_E820 {
                    return Err(MemError::TooManyEntries);
                }
                s.e820[s.e820_count] = e;
                s.e820_count += 1;
            }
        }
        ebx = r.ebx;
        if ebx == 0 {
            break;
        }
    }
    if s.e820_count == 0 {
        return Err(MemError::NoE820);
    }
    Ok(())
}

/// Builds the Limine-typed map of the firmware's memory (no loader
/// allocations) into `out`, and returns how many entries it wrote.
pub fn firmware_map(
    extra: &[Override],
    revision: u64,
    out: &mut [MemmapEntry],
) -> Result<usize, MemError> {
    let s = state();
    let mut regions = [EfiRegion {
        typ: 0,
        start: 0,
        pages: 0,
    }; MAX_E820];
    let mut overrides = [Override {
        start: 0,
        end: 0,
        typ: 0,
    }; MAX_E820 + 64];
    let (mut nr, mut no) = (0, 0);
    for e in &s.e820[..s.e820_count] {
        match e820::convert(e) {
            Some(Converted::Region(r)) => {
                regions[nr] = r;
                nr += 1;
            }
            Some(Converted::Bad { start, end }) => {
                overrides[no] = Override {
                    start,
                    end,
                    typ: MEMMAP_BAD_MEMORY,
                };
                no += 1;
            }
            None => {}
        }
    }
    for o in extra {
        if no == overrides.len() {
            return Err(MemError::BadMap);
        }
        overrides[no] = *o;
        no += 1;
    }
    let mut scratch = [Event::default(); memmap::scratch_needed(MAX_E820, MAX_E820 + 64)];
    memmap::build(
        regions[..nr].iter().copied(),
        &overrides[..no],
        revision,
        &mut scratch,
        out,
    )
    .map_err(|_| MemError::BadMap)
}

pub fn init() -> Result<(), MemError> {
    read_e820(state())?;
    let mut map = [MemmapEntry {
        base: 0,
        length: 0,
        typ: 0,
    }; MAX_USABLE];
    let n = firmware_map(&[], 3, &mut map)?;
    let s = state();
    s.usable_count = 0;
    for m in &map[..n] {
        if m.typ != MEMMAP_USABLE {
            continue;
        }
        let start = m.base.max(LOW_LIMIT);
        let end = m.base + m.length;
        if end > start {
            s.usable[s.usable_count] = (start, end);
            s.usable_count += 1;
        }
    }
    identity_map(&map[..n])?;
    let heap =
        alloc_pages(HEAP_BYTES / PAGE, Some(1 << 32), Kind::Loader).ok_or(MemError::NoMemory)?;
    state().heap = (heap, heap, heap + HEAP_BYTES);
    let s = state();
    let usable: u64 = s.usable[..s.usable_count].iter().map(|&(a, b)| b - a).sum();
    crate::println!(
        "Memory: {} MiB usable above 1 MiB, from {} E820 entries",
        usable >> 20,
        s.e820_count
    );
    if usable < 16 << 20 {
        return Err(MemError::NoMemory);
    }
    Ok(())
}

/// Page-table frames for stage 2's own tables, from RAM below 4 GiB (the
/// thunk reloads CR3 from 32-bit code).
struct LowFrames;

impl FrameSource for LowFrames {
    fn alloc(&mut self) -> Option<u64> {
        alloc_pages(1, Some(1 << 32), Kind::Loader)
    }
    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: frames come from alloc() above: ours, below 4 GiB, and
        // identity mapped by the current tables.
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }
}

unsafe extern "sysv64" {
    static mut ob_cr3: u64;
}

/// The boot tables from stage2.s map only the first 4 GiB. Replace them
/// with tables that also map every range the firmware map describes above
/// 4 GiB, so files can be loaded anywhere in RAM.
fn identity_map(map: &[MemmapEntry]) -> Result<(), MemError> {
    let f = crate::hw::features();
    let mut src = LowFrames;
    let rwx = Flags {
        write: true,
        exec: true,
        write_combining: false,
    };
    let mut m = Mapper::new(&mut src, 4, false, f.huge_1g).map_err(|_| MemError::NoMemory)?;
    m.map(&mut src, 0, 0, 1 << 32, rwx)
        .map_err(|_| MemError::NoMemory)?;
    for e in map {
        let start = e.base.max(1 << 32) & !(PAGE - 1);
        let end = (e.base + e.length).div_ceil(PAGE) * PAGE;
        if end > start {
            m.map(&mut src, start, start, end - start, rwx)
                .map_err(|_| MemError::BadMap)?;
        }
    }
    // SAFETY: the new tables map everything the old ones did (the first
    // 4 GiB, identity, writable and executable), so execution continues;
    // ob_cr3 is what the real-mode thunk reloads on the way back.
    unsafe {
        core::ptr::addr_of_mut!(ob_cr3).write(m.root);
        core::arch::asm!("mov cr3, {}", in(reg) m.root, options(nostack));
    }
    Ok(())
}

fn overlaps_alloc(s: &State, a: u64, b: u64) -> Option<u64> {
    s.allocs[..s.alloc_count]
        .iter()
        .filter(|x| x.start < b && a < x.end)
        .map(|x| x.end)
        .max()
}

fn record(s: &mut State, start: u64, end: u64, kind: Kind) -> bool {
    if s.alloc_count == MAX_ALLOCS {
        return false;
    }
    s.allocs[s.alloc_count] = Alloc { start, end, kind };
    s.alloc_count += 1;
    true
}

/// `pages` zero-filled pages at the lowest free address that ends at or
/// below `below` (exclusive limit), aligned to `align` (a power of two, at
/// least a page).
pub fn alloc_aligned(pages: u64, align: u64, below: Option<u64>, kind: Kind) -> Option<u64> {
    let s = state();
    let len = pages.checked_mul(PAGE)?;
    if len == 0 {
        return None;
    }
    let limit = below.unwrap_or(u64::MAX);
    for i in 0..s.usable_count {
        let (us, ue) = s.usable[i];
        let mut a = us.checked_add(align - 1)? & !(align - 1);
        while let Some(b) = a.checked_add(len) {
            if b > ue || b > limit {
                break;
            }
            match overlaps_alloc(s, a, b) {
                Some(next) => a = next.checked_add(align - 1)? & !(align - 1),
                None => {
                    if !record(s, a, b, kind) {
                        return None;
                    }
                    // SAFETY: [a, b) is usable RAM nobody else owns, and is
                    // identity mapped.
                    unsafe { core::ptr::write_bytes(a as *mut u8, 0, len as usize) };
                    return Some(a);
                }
            }
        }
    }
    None
}

pub fn alloc_pages(pages: u64, below: Option<u64>, kind: Kind) -> Option<u64> {
    alloc_aligned(pages, PAGE, below, kind)
}

/// `pages` zero-filled pages at the highest free address. Files go here,
/// so on machines with RAM above 4 GiB they land there, and the
/// low memory stays free for the tables that must sit below 4 GiB.
pub fn alloc_high(pages: u64, kind: Kind) -> Option<u64> {
    let s = state();
    let len = pages.checked_mul(PAGE)?;
    if len == 0 {
        return None;
    }
    for i in (0..s.usable_count).rev() {
        let (us, ue) = s.usable[i];
        let mut b = ue & !(PAGE - 1);
        while let Some(a) = b.checked_sub(len) {
            if a < us {
                break;
            }
            // The lowest start of any allocation overlapping [a, b).
            let clash = s.allocs[..s.alloc_count]
                .iter()
                .filter(|x| x.start < b && a < x.end)
                .map(|x| x.start)
                .min();
            match clash {
                Some(start) => b = start & !(PAGE - 1),
                None => {
                    if !record(s, a, b, kind) {
                        return None;
                    }
                    // SAFETY: as in alloc_aligned.
                    unsafe { core::ptr::write_bytes(a as *mut u8, 0, len as usize) };
                    return Some(a);
                }
            }
        }
    }
    None
}

/// Claims exactly `[addr, addr + pages * PAGE)` if all of it is free.
pub fn alloc_at(addr: u64, pages: u64, kind: Kind) -> bool {
    let s = state();
    let Some(end) = pages.checked_mul(PAGE).and_then(|l| addr.checked_add(l)) else {
        return false;
    };
    if !addr.is_multiple_of(PAGE) || pages == 0 {
        return false;
    }
    let inside = s.usable[..s.usable_count]
        .iter()
        .any(|&(us, ue)| us <= addr && end <= ue);
    if !inside || overlaps_alloc(s, addr, end).is_some() || !record(s, addr, end, kind) {
        return false;
    }
    // SAFETY: as in alloc_aligned.
    unsafe { core::ptr::write_bytes(addr as *mut u8, 0, (end - addr) as usize) };
    true
}

/// Free RAM ranges (usable, not allocated), for KASLR's physical choice.
pub fn free_ranges(out: &mut [(u64, u64)]) -> usize {
    let s = state();
    let mut n = 0;
    for &(us, ue) in &s.usable[..s.usable_count] {
        let mut a = us;
        while a < ue && n < out.len() {
            // The next allocation starting at or after `a` inside this range.
            let mut cut = ue;
            let mut skip_to = None;
            for x in &s.allocs[..s.alloc_count] {
                if x.start <= a && x.end > a {
                    skip_to = Some(skip_to.map_or(x.end, |v: u64| v.max(x.end)));
                } else if x.start > a && x.start < cut {
                    cut = x.start;
                }
            }
            if let Some(t) = skip_to {
                a = t;
                continue;
            }
            out[n] = (a, cut);
            n += 1;
            a = cut;
        }
    }
    n
}

#[allow(dead_code)] // used by the handoff, next part
pub fn allocations() -> &'static [Alloc] {
    let s = state();
    &s.allocs[..s.alloc_count]
}

/// Pages holding a file or a table, with their byte length.
pub struct Pages {
    pub phys: u64,
    pub len: u64,
}

impl Pages {
    pub fn new(bytes: u64, kind: Kind) -> Option<Pages> {
        let pages = bytes.div_ceil(PAGE).max(1);
        let phys = alloc_high(pages, kind)?;
        Some(Pages {
            phys,
            len: pages * PAGE,
        })
    }
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: we own these pages; they are identity mapped.
        unsafe { core::slice::from_raw_parts_mut(self.phys as *mut u8, self.len as usize) }
    }
}

/// The heap behind `alloc`: a bump allocator over 4 MiB of loader pages.
/// Freeing the most recent allocation gives its space back; anything else
/// is kept until the kernel reclaims the loader's memory.
struct Heap;

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let s = state();
        let (start, next, end) = s.heap;
        if start == 0 {
            return core::ptr::null_mut();
        }
        let a = (next + layout.align() as u64 - 1) & !(layout.align() as u64 - 1);
        match a.checked_add(layout.size() as u64) {
            Some(b) if b <= end => {
                s.heap.1 = b;
                a as *mut u8
            }
            _ => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let s = state();
        if ptr as u64 + layout.size() as u64 == s.heap.1 {
            s.heap.1 = ptr as u64;
        }
    }
}

#[global_allocator]
static HEAP: Heap = Heap;
