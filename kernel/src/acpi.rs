//! ACPI: find the firmware's tables, copy every one into the kernel heap,
//! parse what the kernel needs now (MADT, HPET, FADT), report it on one
//! line, then give ACPI reclaimable memory to the frame allocator.
//!
//! The parsing is all in `optimal-acpi` (safe Rust, tested on the host).
//! This module is the only code that reads firmware memory, and it reads it
//! only through [`FirmwareMem`], which refuses any byte that the memory map
//! doesn't say holds firmware data (see `optimal_acpi::reach`). A corrupt
//! table address can therefore never make the kernel read its own RAM, a
//! device register, or an unmapped page.
//!
//! After [`init`], nothing points into firmware memory: every table lives in
//! a heap copy, so reclaiming the memory is safe. The one structure the OS
//! must leave in place, the FACS (it holds the firmware waking vector), is
//! never reclaimed, even if firmware wrongly puts it in reclaimable memory.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use optimal_acpi::fadt::{Fadt, PmTimer};
use optimal_acpi::hpet::Hpet;
use optimal_acpi::madt::Madt;
use optimal_acpi::reach::{PAGE, Reach, Readable, Span};
use optimal_acpi::tables::{PhysRead, Root, TableRef, Tables};
use optimal_acpi::{Error, checksum};
use optimal_mm::paging::{Cache, Flags};

use crate::bootinfo::{BootInfo, Kind};
use crate::mm::{frames, paging};
use crate::sync::{IrqSpinLock, Once};

/// The legacy BIOS area, where the ACPI specification (5.2.5.1) says BIOS
/// firmware may put the RSDP. Readable where the memory map has nothing.
const BIOS_AREA: Span = Span {
    start: 0xe_0000,
    end: 0x10_0000,
};

/// The PC's fixed interrupt-controller and HPET window (I/O APICs at
/// 0xfec00000, HPET at 0xfed00000, local APICs at 0xfee00000). Device
/// registers, never table memory, so never read even if a table points
/// there and the memory map calls it reserved: a read there can have side
/// effects.
const DEVICE_WINDOW: Span = Span {
    start: 0xfec0_0000,
    end: 0xff00_0000,
};

/// Most bytes of tables copied into the heap, all tables together. Real
/// machines need well under 1 MiB (a big DSDT is a few hundred KiB); a
/// corrupt length can't exhaust memory past this. Also capped at a quarter
/// of free memory, for tiny machines.
const COPY_BUDGET: u64 = 64 << 20;

/// One table, copied out of firmware memory.
pub struct Table {
    pub signature: [u8; 4],
    /// Where it was (for reports; the memory may be reused now).
    #[cfg_attr(not(feature = "ktest"), expect(dead_code, reason = "tests read it"))]
    pub phys: u64,
    pub bytes: Vec<u8>,
    #[cfg_attr(not(feature = "ktest"), expect(dead_code, reason = "tests read it"))]
    pub checksum_ok: bool,
}

/// What firmware got wrong, all counted rather than fatal.
#[derive(Default)]
pub struct Warnings {
    pub lines: Vec<String>,
}

impl Warnings {
    fn add(&mut self, args: core::fmt::Arguments) {
        let mut s = String::new();
        let _ = s.write_fmt(args);
        self.lines.push(s);
    }
}

macro_rules! warn {
    ($w:expr, $($arg:tt)*) => { $w.add(format_args!($($arg)*)) };
}

pub struct Acpi {
    pub revision: u8,
    pub root: Root,
    pub tables: Vec<Table>,
    pub madt: Option<Madt>,
    pub hpet: Option<Hpet>,
    pub fadt: Option<Fadt>,
    pub warnings: Warnings,
    /// Bytes of ACPI reclaimable memory given to the frame allocator.
    pub reclaimed: u64,
    /// Free memory just before and just after the reclaim (tests).
    pub free_around_reclaim: (u64, u64),
    /// The FACS's physical range, kept out of the allocator.
    pub facs: Option<(u64, u64)>,
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is 1c2 (AML later)")
)]
impl Acpi {
    pub fn table(&self, sig: &[u8; 4]) -> Option<&Table> {
        self.tables.iter().find(|t| &t.signature == sig)
    }
}

/// Why there is no ACPI.
#[derive(Clone, Copy, Debug)]
pub enum Absent {
    /// The bootloader found no RSDP (a machine without ACPI).
    NoRsdp,
    /// The RSDP is somewhere the memory map doesn't call firmware memory.
    RsdpRefused(u64),
    /// The RSDP or its root table is unusable.
    Broken(Error),
}

static ACPI: Once<Result<Acpi, Absent>> = Once::new();

/// The result of [`init`]. Panics before it.
#[cfg_attr(
    not(feature = "ktest"),
    expect(
        dead_code,
        reason = "first user outside tests is 1c2 (APIC setup from the MADT)"
    )
)]
pub fn get() -> &'static Result<Acpi, Absent> {
    ACPI.get().expect("ACPI read before init")
}

/// Reads firmware memory, only where the memory map allows it.
pub struct FirmwareMem {
    readable: Readable,
    /// The page of kernel virtual address space reads through the window
    /// use; mapped only during one read.
    window: u64,
}

/// Set once ACPI reclaimable memory belongs to the frame allocator: from
/// then on no reader may include it.
static RECLAIMED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// One firmware page mapped at a time.
static WINDOW_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

impl FirmwareMem {
    /// The reader for this machine. With `include_reclaimable` false, ACPI
    /// reclaimable memory is refused too (it belongs to the allocator once
    /// reclaimed).
    pub fn new(info: &BootInfo, include_reclaimable: bool, window: u64) -> FirmwareMem {
        assert!(
            !(include_reclaimable && RECLAIMED.load(core::sync::atomic::Ordering::SeqCst)),
            "ACPI reclaimable memory read after it was reclaimed"
        );
        let mut allowed = Vec::new();
        let mut forbidden = Vec::new();
        let mut mapped = Vec::new();
        for e in info.memory_map() {
            let span = Span {
                start: e.base,
                end: e.base.saturating_add(e.len),
            };
            match e.kind {
                Kind::AcpiReclaimable if include_reclaimable => allowed.push(span),
                Kind::AcpiNvs | Kind::Reserved => allowed.push(span),
                _ => forbidden.push(span),
            }
            mapped.push(span);
        }
        // Holes in the BIOS area: allowed. Parts the map covers follow the
        // map (allowed above if reserved, forbidden if anything else).
        let mut sorted = mapped;
        sorted.sort_unstable_by_key(|s| s.start);
        let mut at = BIOS_AREA.start;
        for s in sorted
            .iter()
            .filter(|s| s.end > BIOS_AREA.start && s.start < BIOS_AREA.end)
        {
            if s.start > at {
                allowed.push(Span {
                    start: at,
                    end: s.start,
                });
            }
            at = at.max(s.end);
        }
        if at < BIOS_AREA.end {
            allowed.push(Span {
                start: at,
                end: BIOS_AREA.end,
            });
        }
        forbidden.push(DEVICE_WINDOW);
        let mut direct = Vec::new();
        paging::for_each_direct_span(info, |start, end| direct.push(Span { start, end }));
        FirmwareMem {
            readable: Readable::new(&allowed, &forbidden, &direct),
            window,
        }
    }

    pub fn allows(&self, phys: u64, len: u64) -> bool {
        self.readable.allows(phys, len)
    }

    /// Is the window page mapped right now? (Tests: it must never stay so.)
    #[cfg(feature = "ktest")]
    pub fn window(&self) -> u64 {
        self.window
    }

    fn read_window(&self, phys: u64, out: &mut [u8]) -> bool {
        let page = phys & !(PAGE - 1);
        let _g = WINDOW_LOCK.lock();
        let flags = Flags {
            cache: Cache::Uncached,
            ..Flags::READ
        };
        if paging::map_kernel_page(self.window, page, flags).is_err() {
            return false;
        }
        let src = (self.window + (phys - page)) as *const u8;
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: the window maps `page` read-only, `phys..phys+len`
            // lies inside it (plan never lets a window piece cross a page),
            // and the planner allowed every byte as firmware memory.
            *b = unsafe { src.add(i).read_volatile() };
        }
        paging::unmap_kernel_page(self.window).expect("read window vanished");
        true
    }
}

impl PhysRead for FirmwareMem {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
        let Some(pieces) = self.readable.plan(phys, buf.len() as u64) else {
            return false;
        };
        for p in pieces {
            let at = (p.phys - phys) as usize;
            let out = &mut buf[at..at + p.len as usize];
            match p.reach {
                Reach::DirectMap => {
                    let src = frames::phys_to_virt(p.phys) as *const u8;
                    // SAFETY: the planner says every page of the piece is in
                    // the kernel's direct map, and every byte is firmware
                    // memory that nothing in the kernel writes.
                    unsafe { core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len()) };
                }
                Reach::Window => {
                    if !self.read_window(p.phys, out) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// A random page in the window region, chosen once.
pub fn window_page() -> u64 {
    static PAGE_VA: Once<u64> = Once::new();
    if let Some(v) = PAGE_VA.get() {
        return *v;
    }
    let pages = (paging::WINDOW_REGION.1 - paging::WINDOW_REGION.0) / PAGE;
    *PAGE_VA.set(paging::WINDOW_REGION.0 + (paging::random_u64() % pages) * PAGE)
}

fn sig_str(sig: &[u8; 4]) -> &str {
    match core::str::from_utf8(sig) {
        Ok(s) if sig.iter().all(|b| b.is_ascii_graphic()) => s,
        _ => "????",
    }
}

/// Copy every table, then parse. Call once, after the heap is up.
pub fn init(info: &BootInfo) -> &'static Result<Acpi, Absent> {
    let r = discover(info).map(|mut acpi| {
        let before = frames::free_bytes();
        RECLAIMED.store(true, core::sync::atomic::Ordering::SeqCst);
        acpi.reclaimed = frames::reclaim_acpi_memory(info, acpi.facs);
        acpi.free_around_reclaim = (before, frames::free_bytes());
        acpi
    });
    ACPI.set(r)
}

fn discover(info: &BootInfo) -> Result<Acpi, Absent> {
    let rsdp = info.rsdp.ok_or(Absent::NoRsdp)?;
    let mem = FirmwareMem::new(info, true, window_page());
    if !mem.allows(rsdp, 20) {
        return Err(Absent::RsdpRefused(rsdp));
    }
    let found = Tables::discover(&mem, rsdp).map_err(Absent::Broken)?;
    let mut w = Warnings::default();
    if found.rsdp.extended_broken {
        warn!(
            w,
            "RSDP extended part unusable (length or checksum); using the RSDT"
        );
    }
    if found.xsdt_fell_back {
        warn!(w, "XSDT unusable; using the RSDT");
    }
    if !found.root_checksum_ok {
        warn!(w, "root table checksum wrong (used anyway)");
    }
    if found.bad_entries > 0 {
        warn!(
            w,
            "{} root table entries unreadable or invalid", found.bad_entries
        );
    }
    if found.ignored_entries > 0 {
        warn!(
            w,
            "{} root table entries ignored (too many, or a partial entry)", found.ignored_entries
        );
    }

    let mut budget = COPY_BUDGET.min(frames::free_bytes() / 4);
    let mut tables: Vec<Table> = Vec::new();
    let mut copy = |t: TableRef, w: &mut Warnings, tables: &mut Vec<Table>| {
        let len = u64::from(t.len);
        if len > budget {
            warn!(
                w,
                "{} at {:#x} is {} bytes, over what is left of the copy budget; skipped",
                sig_str(&t.signature),
                t.phys,
                len
            );
            return;
        }
        match Tables::copy(&mem, t) {
            Some(bytes) => {
                budget -= len;
                let ok = checksum(&bytes) == 0;
                if !ok {
                    warn!(w, "{} checksum wrong (used anyway)", sig_str(&t.signature));
                }
                tables.push(Table {
                    signature: t.signature,
                    phys: t.phys,
                    bytes,
                    checksum_ok: ok,
                });
            }
            None => warn!(
                w,
                "{} at {:#x} (+{}) is outside firmware memory; skipped",
                sig_str(&t.signature),
                t.phys,
                len
            ),
        }
    };
    for &t in &found.list {
        copy(t, &mut w, &mut tables);
    }
    for sig in [b"APIC", b"FACP", b"HPET", b"DSDT", b"FACS"] {
        let n = found.count(sig);
        if n > 1 {
            warn!(w, "{n} {} tables listed; using the first", sig_str(sig));
        }
    }

    let first = |tables: &[Table], sig: &[u8; 4]| tables.iter().position(|t| &t.signature == sig);
    let fadt = first(&tables, b"FACP").and_then(|i| match Fadt::parse(&tables[i].bytes) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!(w, "FADT unusable: {e:?}");
            None
        }
    });
    if fadt.is_none() {
        warn!(w, "no usable FADT");
    }
    let mut facs = None;
    if let Some(f) = &fadt {
        if f.dsdt_mismatch {
            warn!(w, "FADT DSDT and X_DSDT disagree; using X_DSDT");
        }
        if f.facs_mismatch {
            warn!(
                w,
                "FADT FIRMWARE_CTRL and X_FIRMWARE_CTRL disagree; using X_FIRMWARE_CTRL"
            );
        }
        if f.pm_timer_mismatch {
            warn!(w, "FADT PM timer fields disagree; using X_PM_TMR_BLK");
        }
        // The DSDT isn't listed in the root table; the FADT points at it.
        match f.dsdt {
            Some(p) if found.count(b"DSDT") == 0 => {
                let mut h = [0u8; 8];
                if !mem.read(p, &mut h) || &h[..4] != b"DSDT" {
                    warn!(w, "DSDT at {p:#x} unreadable or not a DSDT; skipped");
                } else if let len @ 36..=optimal_acpi::MAX_TABLE_LEN =
                    u32::from_le_bytes([h[4], h[5], h[6], h[7]])
                {
                    copy(
                        TableRef {
                            signature: *b"DSDT",
                            phys: p,
                            len,
                        },
                        &mut w,
                        &mut tables,
                    );
                } else {
                    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
                    warn!(w, "DSDT at {p:#x} has an impossible length {len}; skipped");
                }
            }
            Some(_) => {}
            None if !f.hw_reduced => warn!(w, "FADT gives no DSDT"),
            None => {}
        }
        // The FACS stays where it is (firmware and OS share it); only
        // remember where, so its pages are never reclaimed.
        if let Some(p) = f.facs {
            let mut h = [0u8; 8];
            if mem.read(p, &mut h) && &h[..4] == b"FACS" {
                // At least the 64 bytes the specification fixes; a corrupt
                // length can only keep more pages out of the allocator.
                let len =
                    u64::from(u32::from_le_bytes([h[4], h[5], h[6], h[7]])).clamp(64, 1 << 20);
                let end = p.saturating_add(len);
                facs = Some((p, end));
                if info.memory_map().iter().any(|e| {
                    e.kind == Kind::AcpiReclaimable
                        && e.base < end
                        && p < e.base.saturating_add(e.len)
                }) {
                    warn!(
                        w,
                        "FACS is in ACPI reclaimable memory (should be NVS); its pages are kept"
                    );
                }
            } else {
                warn!(w, "FACS at {p:#x} unreadable or bad signature");
            }
        }
    }

    let madt = first(&tables, b"APIC").and_then(|i| match Madt::parse(&tables[i].bytes) {
        Ok(m) => Some(m),
        Err(e) => {
            warn!(w, "MADT unusable: {e:?}");
            None
        }
    });
    match &madt {
        None => warn!(w, "no usable MADT"),
        Some(m) => {
            if m.bad_entries > 0 {
                warn!(
                    w,
                    "MADT: {} entries skipped (short, duplicate or invalid)", m.bad_entries
                );
            }
            if m.truncated {
                warn!(w, "MADT entry list truncated");
            }
            if !m.cpus.iter().any(|c| c.enabled) {
                warn!(w, "MADT lists no enabled CPU");
            }
        }
    }
    let hpet = first(&tables, b"HPET").and_then(|i| match Hpet::parse(&tables[i].bytes) {
        Ok(h) => Some(h),
        Err(e) => {
            warn!(w, "HPET table unusable: {e:?}");
            None
        }
    });

    Ok(Acpi {
        revision: found.rsdp.revision,
        root: found.root,
        tables,
        madt,
        hpet,
        fadt,
        warnings: w,
        reclaimed: 0,
        free_around_reclaim: (0, 0),
        facs,
    })
}

/// The one-line summary, then one line per warning.
pub fn report(r: &Result<Acpi, Absent>) {
    let a = match r {
        Ok(a) => a,
        Err(Absent::NoRsdp) => {
            crate::println!("acpi: none (no RSDP from the bootloader)");
            return;
        }
        Err(Absent::RsdpRefused(p)) => {
            crate::println!("acpi: none (RSDP at {p:#x} is not in firmware memory; refused)");
            return;
        }
        Err(Absent::Broken(e)) => {
            crate::println!("acpi: none (RSDP or root table unusable: {e:?})");
            return;
        }
    };
    let mut line = String::new();
    let _ = write!(
        line,
        "acpi: {} rev {}, {} tables [",
        if a.root == Root::Xsdt { "XSDT" } else { "RSDT" },
        a.revision,
        a.tables.len()
    );
    for (i, t) in a.tables.iter().enumerate() {
        let _ = write!(
            line,
            "{}{}",
            if i > 0 { " " } else { "" },
            sig_str(&t.signature)
        );
    }
    line.push_str("]; ");
    match &a.madt {
        Some(m) => {
            let on = m.cpus.iter().filter(|c| c.enabled).count();
            let hot = m
                .cpus
                .iter()
                .filter(|c| !c.enabled && c.online_capable)
                .count();
            let x2 = m.cpus.iter().filter(|c| c.x2apic).count();
            let _ = write!(
                line,
                "{on} CPUs ({} off, {hot} hot-add, {x2} x2APIC), {} I/O APIC, {} overrides, {} LAPIC NMI, ",
                m.cpus.len() - on - hot,
                m.ioapics.len(),
                m.overrides.len(),
                m.lapic_nmis.len()
            );
        }
        None => line.push_str("no MADT, "),
    }
    match &a.hpet {
        Some(h) => {
            let _ = write!(line, "HPET {:#x}, ", h.address);
        }
        None => line.push_str("HPET none, "),
    }
    match a
        .fadt
        .as_ref()
        .and_then(|f| f.pm_timer.map(|t| (t, f.pm_timer_32bit)))
    {
        Some((PmTimer::Io(p), wide)) => {
            let _ = write!(
                line,
                "PM timer io {p:#x} {}-bit; ",
                if wide { 32 } else { 24 }
            );
        }
        Some((PmTimer::Memory(p), wide)) => {
            let _ = write!(
                line,
                "PM timer mem {p:#x} {}-bit; ",
                if wide { 32 } else { 24 }
            );
        }
        None => line.push_str("PM timer none; "),
    }
    let _ = write!(
        line,
        "{} KiB reclaimed; {} warnings",
        a.reclaimed / 1024,
        a.warnings.lines.len()
    );
    crate::println!("{line}");
    for wl in &a.warnings.lines {
        crate::println!("acpi: warning: {wl}");
    }
}

#[cfg(feature = "ktest")]
mod tests;
