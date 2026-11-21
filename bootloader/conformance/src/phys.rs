//! Reading physical memory the HHDM does not cover (ACPI tables in base
//! revision 3, MMIO registers): pages are mapped on demand, uncached, into a
//! free window of the live page tables, using page-table frames from a pool
//! in our own .bss. The kernel owns the loader's tables, so this is allowed.

use core::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use optimal_boot_core::acpi::PhysRead;
use optimal_boot_core::paging::{self, PAGE};

use crate::checks::Ctx;

#[repr(C, align(4096))]
struct Pool([[u64; 512]; 16]);
static mut POOL: Pool = Pool([[0; 512]; 16]);
static POOL_USED: AtomicUsize = AtomicUsize::new(0);

const SLOTS: usize = 256;
static mut CACHE: [(u64, u64); SLOTS] = [(u64::MAX, 0); SLOTS];
static NEXT: AtomicUsize = AtomicUsize::new(0);
static mut WINDOW: u64 = 0;

pub struct Phys<'a>(pub &'a Ctx);

impl Phys<'_> {
    fn table(&self, phys: u64) -> &'static mut [u64; 512] {
        // SAFETY: page tables are reachable through the HHDM (checked earlier).
        unsafe { &mut *((phys + self.0.hhdm) as *mut [u64; 512]) }
    }

    fn pool_frame(&self) -> Option<u64> {
        let i = POOL_USED.fetch_add(1, SeqCst);
        if i >= 16 {
            return None;
        }
        // SAFETY: each pool frame is handed out once.
        let v = unsafe { (&raw mut POOL.0[i]) as u64 };
        self.0.phys(v)
    }

    /// A free, 2 MiB aligned virtual window of SLOTS pages, found once.
    fn window(&self) -> Option<u64> {
        // SAFETY: single-threaded.
        let w = unsafe { WINDOW };
        if w != 0 {
            return Some(w);
        }
        let top = if self.0.levels == 5 {
            0xff00_0000_0000_0000u64
        } else {
            0xffff_8000_0000_0000
        };
        // Try spots spread through the higher half until one is wholly free.
        for k in 1..64u64 {
            let cand = top
                + k * 0x0000_0123_4560_0000 % (1u64 << (if self.0.levels == 5 { 55 } else { 46 }));
            let cand = cand & !(paging::SIZE_2M - 1);
            if !paging::canonical(cand, self.0.levels) {
                continue;
            }
            if (0..SLOTS as u64).all(|i| self.0.tr(cand + i * PAGE).is_none()) {
                // SAFETY: single-threaded.
                unsafe { WINDOW = cand };
                return Some(cand);
            }
        }
        None
    }

    fn map_at(&self, virt: u64, phys: u64) -> bool {
        let c = self.0;
        let mut table = c.root;
        for level in (2..=c.levels).rev() {
            let idx = ((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize;
            let e = self.table(table)[idx];
            if e & paging::P == 0 {
                let Some(f) = self.pool_frame() else {
                    return false;
                };
                self.table(table)[idx] = f | paging::P | paging::RW;
                table = f;
            } else if e & paging::PS != 0 {
                return false;
            } else {
                table = e & paging::ADDR_MASK;
            }
        }
        let idx = ((virt >> 12) & 0x1ff) as usize;
        let nx = if c.nx { paging::NX } else { 0 };
        self.table(table)[idx] = phys | paging::P | paging::RW | paging::PCD | paging::PWT | nx;
        // SAFETY: flushes our own new mapping.
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt) };
        true
    }

    /// A virtual address for physical `phys` (any alignment): the HHDM when
    /// it covers the page, else an uncached window slot.
    pub fn virt(&self, phys: u64) -> Option<u64> {
        let page = phys & !(PAGE - 1);
        let off = phys - page;
        if self
            .0
            .tr(page + self.0.hhdm)
            .is_some_and(|t| t.phys == page)
        {
            return Some(page + self.0.hhdm + off);
        }
        // SAFETY: single-threaded access to the cache.
        #[allow(clippy::deref_addrof)]
        let cache = unsafe { &mut *(&raw mut CACHE) };
        if let Some(&(_, v)) = cache.iter().find(|(p, _)| *p == page) {
            return Some(v + off);
        }
        let w = self.window()?;
        let slot = NEXT.fetch_add(1, SeqCst) % SLOTS;
        let v = w + slot as u64 * PAGE;
        if !self.map_at(v, page) {
            return None;
        }
        cache[slot] = (page, v);
        Some(v + off)
    }

    pub fn r32(&self, phys: u64) -> Option<u32> {
        // SAFETY: an uncached mapping of the register.
        self.virt(phys)
            .map(|v| unsafe { core::ptr::read_volatile(v as *const u32) })
    }
    pub fn w32(&self, phys: u64, val: u32) -> bool {
        // SAFETY: as above; only index registers are written.
        self.virt(phys)
            .map(|v| unsafe { core::ptr::write_volatile(v as *mut u32, val) })
            .is_some()
    }
    pub fn r64(&self, phys: u64) -> Option<u64> {
        // SAFETY: as above.
        self.virt(phys)
            .map(|v| unsafe { core::ptr::read_volatile(v as *const u64) })
    }
}

impl PhysRead for Phys<'_> {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
        let mut done = 0usize;
        while done < buf.len() {
            let p = phys + done as u64;
            let n = ((PAGE - (p & (PAGE - 1))) as usize).min(buf.len() - done);
            let Some(v) = self.virt(p) else { return false };
            for (i, b) in buf[done..done + n].iter_mut().enumerate() {
                // SAFETY: inside the page just mapped.
                *b = unsafe { core::ptr::read_volatile((v + i as u64) as *const u8) };
            }
            done += n;
        }
        true
    }
}
