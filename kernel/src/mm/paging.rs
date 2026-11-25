//! The kernel's own page tables.
//!
//! Limine's tables map everything read-write-execute through a direct map at
//! a fixed address. The kernel replaces them early with its own:
//!
//! - its image section by section: code read+execute, read-only data and the
//!   RELRO region read-only and no-execute, data and bss read-write and
//!   no-execute. All global, so they survive address-space switches.
//! - a direct map of all RAM (never MMIO holes), read-write and no-execute,
//!   with the biggest pages that fit (1 GiB where the CPU has them), at a
//!   random 1 GiB-aligned address (KASLR), so a leaked kernel pointer
//!   doesn't reveal where physical memory is.
//! - the framebuffer, write-combining, inside the direct map region.
//! - kernel stacks in their own random region, each with an unmapped guard
//!   page below it, so a stack overflow faults instead of corrupting memory.
//!
//! Virtual layout of the kernel half (PML4 slots, 512 GiB each):
//!   256..448  direct map region (96 TiB), the map placed at random in it
//!   448..480  kernel stacks region (16 TiB), placed at random in it
//!   480       firmware read window: one page at a random spot, mapped only
//!             while the ACPI reader copies a table out (1c)
//!   481       device registers (MMIO), uncached, from a random 1 GiB-aligned
//!             spot in the slot, one page after another, never unmapped (1c2)
//!   482..510  reserved for other kernel mappings
//!   510       never mapped
//!   511       kernel image (Limine places it at random in the top 2 GiB)

use optimal_mm::PAGE_SIZE;
use optimal_mm::paging::{AddressSpace, Cache, Flags, PageSize, TableMemory, for_each_page};

use crate::arch::cpu;
use crate::bootinfo::BootInfo;
use crate::mm::frames;
use crate::sync::{IrqSpinLock, Once};

const GIB: u64 = 1 << 30;
const SLOT: u64 = 512 * GIB;
const KERNEL_HALF: u64 = 0xffff_8000_0000_0000;
pub const DIRECT_REGION: (u64, u64) = (KERNEL_HALF, KERNEL_HALF + 192 * SLOT);
pub const STACK_REGION: (u64, u64) = (KERNEL_HALF + 192 * SLOT, KERNEL_HALF + 224 * SLOT);
pub const WINDOW_REGION: (u64, u64) = (KERNEL_HALF + 224 * SLOT, KERNEL_HALF + 225 * SLOT);
pub const MMIO_REGION: (u64, u64) = (KERNEL_HALF + 225 * SLOT, KERNEL_HALF + 226 * SLOT);

/// An address in PML4 slot 510, which is never mapped: a hole between
/// everything else and the kernel image. Crash tests use it.
pub const NEVER_MAPPED: u64 = 0xffff_ff00_dead_0000;

/// Boot stack: 64 KiB, with one unmapped guard page below it.
pub const BOOT_STACK_PAGES: u64 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entropy {
    /// RDRAND: hardware randomness.
    Rdrand,
    /// No RDRAND: timestamp-counter jitter only, which an attacker who can
    /// measure boot timing may be able to guess. Reported as weak.
    TimestampOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// Virtual address of physical address 0 in the kernel's direct map.
    pub direct_map: u64,
    /// Lowest address of the boot stack's guard page.
    pub boot_stack_guard: u64,
    /// One past the top of the boot stack.
    #[cfg_attr(
        not(feature = "ktest"),
        expect(dead_code, reason = "only test builds print it")
    )]
    pub boot_stack_top: u64,
    #[cfg_attr(
        not(feature = "ktest"),
        expect(dead_code, reason = "only test builds print it")
    )]
    pub kernel_virt: u64,
    pub entropy: Entropy,
    pub huge_1g: bool,
    pub nx: bool,
    /// Physical address of the top-level table (CR3).
    pub root: u64,
}

static LAYOUT: Once<Layout> = Once::new();
static KERNEL_SPACE: IrqSpinLock<Option<AddressSpace>> = IrqSpinLock::new(None);

/// The layout chosen at boot. Panics before `build_and_switch`.
pub fn layout() -> &'static Layout {
    LAYOUT.get().expect("kernel page tables not built yet")
}

/// Page-table memory, reached through whatever direct map is current.
pub struct KernelTables;

impl TableMemory for KernelTables {
    fn read(&self, table: u64, index: usize) -> u64 {
        assert!(index < 512);
        // SAFETY: `table` is a page-table frame this kernel allocated, and
        // the direct map covers all RAM.
        unsafe {
            (frames::phys_to_virt(table) as *const u64)
                .add(index)
                .read_volatile()
        }
    }

    fn write(&mut self, table: u64, index: usize, value: u64) {
        assert!(index < 512);
        // SAFETY: as in read; the frame belongs to the page tables.
        unsafe {
            (frames::phys_to_virt(table) as *mut u64)
                .add(index)
                .write_volatile(value)
        }
    }

    fn alloc_table(&mut self) -> Option<u64> {
        let f = frames::alloc(0)?;
        // SAFETY: a fresh frame we own, mapped by the direct map.
        unsafe {
            core::ptr::write_bytes(frames::phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize)
        };
        Some(f)
    }
}

/// A source of random numbers for address-space layout.
struct Random {
    entropy: Entropy,
    state: u64,
}

impl Random {
    fn new() -> Random {
        if cpu::rdrand().is_some() {
            Random {
                entropy: Entropy::Rdrand,
                state: 0,
            }
        } else {
            // Mix many timestamp reads: their low bits jitter with cache
            // and bus timing. Weak, and reported as weak.
            let mut s = 0u64;
            for _ in 0..64 {
                s = mix(s ^ cpu::rdtsc());
            }
            Random {
                entropy: Entropy::TimestampOnly,
                state: s,
            }
        }
    }

    fn next(&mut self) -> u64 {
        match self.entropy {
            Entropy::Rdrand => cpu::rdrand().expect("RDRAND stopped working during boot"),
            Entropy::TimestampOnly => {
                self.state = mix(self.state ^ cpu::rdtsc());
                self.state
            }
        }
    }

    /// Uniform-enough value in 0..n (n is tiny next to 2^64, so the modulo
    /// bias is below 2^-40).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random number for secrets (heap header magic and the like): RDRAND
/// when the CPU has it, else timestamp jitter (weak, as reported at boot).
pub fn random_u64() -> u64 {
    Random::new().next()
}

/// SplitMix64 finaliser.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

unsafe extern "C" {
    // Linker-defined section boundaries: only their addresses matter.
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __relro_start: u8;
    static __relro_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

/// Address of a linker symbol, read with `lea` so the compiler can't
/// assume two different symbols have different addresses (they can be
/// equal: a section's end is the next one's start).
macro_rules! symbol_address {
    ($sym:ident) => {{
        let a: u64;
        // SAFETY: only computes the symbol's address; reads nothing.
        unsafe {
            core::arch::asm!("lea {}, [rip + {}]", out(reg) a, sym $sym, options(nomem, nostack, preserves_flags))
        };
        a
    }};
}

/// The kernel image's sections: (start, end, flags, name), page aligned.
pub fn kernel_sections() -> [(u64, u64, Flags, &'static str); 4] {
    let g = |f: Flags| Flags { global: true, ..f };
    let relro_start = symbol_address!(__relro_start);
    [
        (
            symbol_address!(__kernel_start),
            symbol_address!(__text_end),
            g(Flags::READ_EXECUTE),
            "text",
        ),
        (
            symbol_address!(__rodata_start),
            relro_start,
            g(Flags::READ),
            "rodata",
        ),
        (
            relro_start,
            symbol_address!(__relro_end),
            g(Flags::READ),
            "relro",
        ),
        (
            symbol_address!(__data_start),
            symbol_address!(__kernel_end),
            g(Flags::READ_WRITE),
            "data",
        ),
    ]
}

fn map_or_panic(
    space: &AddressSpace,
    mem: &mut KernelTables,
    virt: u64,
    phys: u64,
    size: PageSize,
    flags: Flags,
    what: &str,
) {
    if let Err(e) = space.map(mem, virt, phys, size, flags) {
        panic!("can't map {what} at {virt:#x} -> {phys:#x}: {e:?}");
    }
}

/// The framebuffer's physical pages, [start, end).
fn framebuffer_span(info: &BootInfo) -> Option<(u64, u64)> {
    info.framebuffer.map(|fb| {
        let start = fb.phys & !(PAGE_SIZE - 1);
        let end = (fb.phys + fb.pitch * fb.height).next_multiple_of(PAGE_SIZE);
        (start, end)
    })
}

/// Every physical span the direct map covers, as [start, end), each to be
/// rounded out to whole pages: the RAM entries of the memory map and the
/// framebuffer. `build_and_switch` maps exactly these; the ACPI reader uses
/// the same list to know which firmware pages it can read in place.
pub fn for_each_direct_span(info: &BootInfo, mut f: impl FnMut(u64, u64)) {
    for e in info.memory_map().iter().filter(|e| e.kind.is_ram()) {
        f(e.base, e.base + e.len);
    }
    if let Some((s, e)) = framebuffer_span(info) {
        f(s, e);
    }
}

/// Build the kernel's page tables, switch to them and to a guarded boot
/// stack, and continue in `next` (which never returns). Limine's HHDM, its
/// page tables and its stack are not used again after this.
///
/// # Safety
/// Call once, on the boot CPU, after `frames::init`, with nothing on the
/// current stack that `next` needs.
pub unsafe fn build_and_switch(info: &BootInfo, next: extern "C" fn() -> !) -> ! {
    let f = cpu::features();
    let nx = cpu::nx_enabled();
    let mut rng = Random::new();
    let mut mem = KernelTables;
    let space = if nx {
        AddressSpace::new(&mut mem)
    } else {
        AddressSpace::new_without_nx(&mut mem)
    }
    .expect("out of memory for the top-level page table");

    // The cache types the entries use (optimal_mm::paging::PAT_VALUE).
    if f.pat {
        // SAFETY: PAT is supported, and this layout is the one Limine's own
        // tables already assume, so nothing mapped now changes meaning.
        unsafe { cpu::write_msr(cpu::MSR_PAT, optimal_mm::paging::PAT_VALUE) };
    }

    // Kernel image.
    let sections = kernel_sections();
    assert_eq!(
        sections[0].0, info.kernel_virt,
        "kernel loaded at an unexpected address"
    );
    for (start, end, flags, name) in sections {
        let mut v = start;
        while v < end {
            let p = info.kernel_phys + (v - info.kernel_virt);
            map_or_panic(&space, &mut mem, v, p, PageSize::Size4K, flags, name);
            v += PAGE_SIZE;
        }
    }

    // Direct map of RAM, at a random 1 GiB-aligned base.
    let mut top = 0;
    for_each_direct_span(info, |_, end| top = top.max(end));
    let fb = framebuffer_span(info);
    let span = top.next_multiple_of(GIB);
    let slots = (DIRECT_REGION.1 - DIRECT_REGION.0 - span) / GIB + 1;
    assert!(
        span < DIRECT_REGION.1 - DIRECT_REGION.0,
        "more physical address space than the direct map region"
    );
    let direct = DIRECT_REGION.0 + rng.below(slots) * GIB;
    let dm = Flags {
        global: true,
        ..Flags::READ_WRITE
    };
    // RAM entries rounded out to whole pages and merged, so neighbours that
    // share a page don't collide and big pages can span entries.
    let mut run: Option<(u64, u64)> = None;
    let map_run = |r: (u64, u64), mem: &mut KernelTables| {
        for_each_page(direct + r.0, r.0, r.1 - r.0, f.huge_1g, |v, p, s| {
            map_or_panic(&space, mem, v, p, s, dm, "direct map");
        });
    };
    for e in info.memory_map().iter().filter(|e| e.kind.is_ram()) {
        let s = e.base & !(PAGE_SIZE - 1);
        let t = (e.base + e.len).next_multiple_of(PAGE_SIZE);
        run = match run {
            Some((rs, re)) if s <= re => Some((rs, re.max(t))),
            Some(prev) => {
                map_run(prev, &mut mem);
                Some((s, t))
            }
            None => Some((s, t)),
        };
    }
    if let Some(prev) = run {
        map_run(prev, &mut mem);
    }
    if let Some((s, t)) = fb {
        let wc = Flags {
            cache: if f.pat {
                Cache::WriteCombining
            } else {
                Cache::Uncached
            },
            ..dm
        };
        for_each_page(direct + s, s, t - s, f.huge_1g, |v, p, size| {
            map_or_panic(&space, &mut mem, v, p, size, wc, "framebuffer");
        });
    }

    // Boot stack with a guard page, at a random 1 GiB-aligned spot.
    let stack_slots = (STACK_REGION.1 - STACK_REGION.0) / GIB;
    let guard = STACK_REGION.0 + rng.below(stack_slots) * GIB;
    let stack_phys = frames::alloc(BOOT_STACK_PAGES.trailing_zeros() as usize)
        .expect("out of memory for the boot stack");
    for i in 0..BOOT_STACK_PAGES {
        let v = guard + PAGE_SIZE * (1 + i);
        map_or_panic(
            &space,
            &mut mem,
            v,
            stack_phys + i * PAGE_SIZE,
            PageSize::Size4K,
            dm,
            "boot stack",
        );
    }
    let stack_top = guard + PAGE_SIZE * (1 + BOOT_STACK_PAGES);

    LAYOUT.set(Layout {
        direct_map: direct,
        boot_stack_guard: guard,
        boot_stack_top: stack_top,
        kernel_virt: info.kernel_virt,
        entropy: rng.entropy,
        huge_1g: f.huge_1g,
        nx,
        root: space.root,
    });
    *KERNEL_SPACE.lock() = Some(space);

    // SAFETY: the new tables map this code (kernel image, same addresses),
    // the new stack and `next`; the old stack is abandoned in the same
    // instruction sequence, so nothing uses it once CR3 changes. Global
    // entries left from Limine's tables are flushed by `next` via
    // `finish_switch`.
    unsafe {
        core::arch::asm!(
            "mov cr3, {root}",
            "mov rsp, {top}",
            "xor ebp, ebp",
            "call {next}",
            "ud2",
            root = in(reg) space.root,
            top = in(reg) stack_top,
            next = in(reg) next,
            options(noreturn),
        )
    }
}

/// First thing on the new tables: flush stale global entries, then move
/// every pointer that used Limine's HHDM to the new direct map.
pub fn finish_switch() {
    let l = layout();
    // SAFETY: reloading the same CR3 we are running on, plus a global flush.
    unsafe { cpu::load_page_tables(l.root) };
    frames::move_direct_map(l.direct_map);
}

/// Map one 4 KiB page in the kernel's tables (later kernel mappings and
/// tests). The caller flushes nothing: a new mapping has no stale entry.
pub fn map_kernel_page(
    virt: u64,
    phys: u64,
    flags: Flags,
) -> Result<(), optimal_mm::paging::MapError> {
    let guard = KERNEL_SPACE.lock();
    let space = guard.as_ref().expect("kernel page tables not built yet");
    space.map(
        &mut KernelTables,
        virt,
        phys,
        PageSize::Size4K,
        Flags {
            global: true,
            ..flags
        },
    )
}

/// Unmap one 4 KiB kernel page and flush it from this CPU's TLB.
pub fn unmap_kernel_page(virt: u64) -> Result<u64, optimal_mm::paging::MapError> {
    let guard = KERNEL_SPACE.lock();
    let space = guard.as_ref().expect("kernel page tables not built yet");
    let r = space.unmap(&mut KernelTables, virt, PageSize::Size4K);
    cpu::flush_page(virt);
    r
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioError {
    /// The range overlaps memory the memory map does not call reserved
    /// (RAM, ACPI tables, the framebuffer...). Mapping it uncached as well
    /// would give the same physical memory two cache types, which x86 does
    /// not allow, and device registers are never there on a sane machine.
    NotReserved {
        phys: u64,
        kind: crate::bootinfo::Kind,
    },
    /// Not page aligned, empty, or past the physical address space.
    BadRange,
    /// The MMIO region is used up (it holds 512 GiB).
    Exhausted,
    Map(optimal_mm::paging::MapError),
}

/// Next free MMIO virtual address (0 until the first mapping picks a
/// random start).
static MMIO_NEXT: IrqSpinLock<u64> = IrqSpinLock::new(0);

/// Map `pages` pages of device registers starting at page-aligned `phys`,
/// uncached, read-write, no-execute and global, and return their virtual
/// address. Mappings are never removed: device registers stay mapped for
/// the kernel's lifetime.
pub fn map_mmio(info: &BootInfo, phys: u64, pages: u64) -> Result<u64, MmioError> {
    let len = pages.checked_mul(PAGE_SIZE).ok_or(MmioError::BadRange)?;
    let end = phys.checked_add(len).ok_or(MmioError::BadRange)?;
    if pages == 0 || !phys.is_multiple_of(PAGE_SIZE) || end > 1 << 52 {
        return Err(MmioError::BadRange);
    }
    use crate::bootinfo::Kind;
    if let Some(e) = info.memory_map().iter().find(|e| {
        !matches!(e.kind, Kind::Reserved | Kind::Unknown(_))
            && e.base < end
            && phys < e.base + e.len
    }) {
        return Err(MmioError::NotReserved { phys, kind: e.kind });
    }
    let mut next = MMIO_NEXT.lock();
    if *next == 0 {
        // Leave room for at least 1 GiB of mappings after the start.
        let slots = (MMIO_REGION.1 - MMIO_REGION.0) / GIB - 1;
        *next = MMIO_REGION.0 + (random_u64() % slots) * GIB;
    }
    let virt = *next;
    if MMIO_REGION.1 - virt < len {
        return Err(MmioError::Exhausted);
    }
    // Taken even if a page below fails to map, so a half-mapped range is
    // never handed out again.
    *next = virt + len;
    drop(next);
    let flags = Flags {
        cache: Cache::Uncached,
        ..Flags::READ_WRITE
    };
    for i in 0..pages {
        map_kernel_page(virt + i * PAGE_SIZE, phys + i * PAGE_SIZE, flags)
            .map_err(MmioError::Map)?;
    }
    Ok(virt)
}

/// Look up a kernel virtual address in the kernel's tables.
#[cfg(feature = "ktest")]
pub fn translate(virt: u64) -> Option<optimal_mm::paging::Translation> {
    let guard = KERNEL_SPACE.lock();
    guard.as_ref()?.translate(&KernelTables, virt)
}

/// Is `addr` inside the boot stack's guard page? (For the fault report.)
pub fn in_stack_guard(addr: u64) -> bool {
    LAYOUT
        .get()
        .is_some_and(|l| l.boot_stack_guard <= addr && addr < l.boot_stack_guard + PAGE_SIZE)
}
