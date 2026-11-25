//! Firmware-quirk injector: a test-only EFI application.
//!
//! OVMF is a tidy firmware: at ExitBootServices it leaves the legacy PICs
//! and I/O APIC pins masked, the IOMMUs off and the local APIC in its reset
//! state. Real firmware often does not. That tidiness hid three bugs-in-
//! waiting from the conformance runs: deleting Optimal Boot's PIC masking,
//! I/O APIC masking or IOMMU shutdown still passed, because the firmware had
//! already done the work.
//!
//! This app makes the machine look like sloppy firmware left it. It reads
//! `\EFI\BOOT\QUIRKS.TXT` (words, see `Quirk`), hooks ExitBootServices,
//! and chain-loads `\EFI\BOOT\NEXT.EFI` (Optimal Boot or Limine). When the
//! loaded loader calls ExitBootServices, the real one runs first (so no
//! firmware callback can tidy up after us), then the hook dirties the
//! hardware, reads every change back, and reports on COM1:
//!
//!   OPTIMAL-QUIRK: applied: pic ioapic lapic vtd
//!
//! The case script fails unless every requested quirk is in that line, so a
//! quirk that silently did not take cannot make a test vacuous. Anything
//! that cannot be set up before the handoff (an unknown word, a requested
//! IOMMU the machine does not have) stops the boot with
//! `OPTIMAL-QUIRK: FATAL`, rather than running a weaker test than asked.
//!
//! Never shipped: it exists only to test loaders.

#![no_std]
#![no_main]

#[path = "../../loader/src/efi.rs"]
mod efi;

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::ptr;
use optimal_boot_core::acpi::{self, Madt, PhysRead};

/// One kind of firmware sloppiness. The words are what QUIRKS.TXT holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Quirk {
    /// Both 8259 PICs fully unmasked.
    Pic,
    /// Every I/O APIC pin unmasked: pin 0 ExtINT (virtual wire), pin 13 NMI,
    /// the rest Fixed with vector 0x40 + pin.
    Ioapic,
    /// Local APIC: TPR 0x20, SVR 0x1EF, every LVT unmasked (timer, thermal,
    /// error and CMCI Fixed; perf, LINT1 NMI; LINT0 ExtINT).
    Lapic,
    /// Local APIC switched to x2APIC mode (applied before `lapic`).
    X2apic,
    /// Local APIC globally disabled in IA32_APIC_BASE.
    ApicOff,
    /// Every VT-d unit with DMA translation on (and interrupt remapping
    /// where supported), over empty tables that block all DMA.
    Vtd,
    /// Every AMD-Vi IOMMU enabled over an empty device table.
    Amdvi,
}

const ALL: [(Quirk, &str); 7] = [
    (Quirk::Pic, "pic"),
    (Quirk::Ioapic, "ioapic"),
    (Quirk::Lapic, "lapic"),
    (Quirk::X2apic, "x2apic"),
    (Quirk::ApicOff, "apic-off"),
    (Quirk::Vtd, "vtd"),
    (Quirk::Amdvi, "amdvi"),
];

fn bit(q: Quirk) -> u32 {
    1 << q as u8
}

fn name(q: Quirk) -> &'static str {
    ALL.iter().find(|a| a.0 == q).map_or("?", |a| a.1)
}

/// Parses QUIRKS.TXT: whitespace-separated words, `#` starts a comment.
/// Unknown words and contradictory sets are refused, never skipped.
fn parse(text: &[u8]) -> Result<u32, &[u8]> {
    let mut set = 0;
    for line in text.split(|&b| b == b'\n') {
        let line = line.split(|&b| b == b'#').next().unwrap_or(&[]);
        for word in line
            .split(|b| b.is_ascii_whitespace())
            .filter(|w| !w.is_empty())
        {
            let q = ALL.iter().find(|a| a.1.as_bytes() == word).ok_or(word)?.0;
            set |= bit(q);
        }
    }
    if set & bit(Quirk::ApicOff) != 0 && set & (bit(Quirk::Lapic) | bit(Quirk::X2apic)) != 0 {
        return Err(b"apic-off together with lapic or x2apic");
    }
    if set == 0 {
        return Err(b"(no quirks listed)");
    }
    Ok(set)
}

// ---- Output -------------------------------------------------------------

fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: port reads have no memory effects; only standard PC ports
    // (COM1, PIC) are read.
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)) };
    v
}

fn outb(port: u16, v: u8) {
    // SAFETY: as inb; writes go only to COM1 and the PIC mask registers.
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack)) };
}

/// COM1, used after ExitBootServices (the firmware console is gone then).
struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            for _ in 0..100_000 {
                if inb(0x3f8 + 5) & 0x20 != 0 {
                    break;
                }
            }
            outb(0x3f8, b);
        }
        Ok(())
    }
}

/// The firmware console, used before ExitBootServices.
struct ConOut(*mut efi::SimpleTextOutput);

impl Write for ConOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.0.is_null() {
            return Ok(());
        }
        let mut buf = [0u16; 130];
        let mut n = 0;
        for b in s.bytes() {
            if b == b'\n' {
                buf[n] = b'\r' as u16;
                n += 1;
            }
            buf[n] = b as u16;
            n += 1;
            if n >= 126 {
                buf[n] = 0;
                // SAFETY: con_out is the firmware's console protocol; buf is
                // NUL-terminated UCS-2.
                unsafe { ((*self.0).output_string)(self.0, buf.as_ptr()) };
                n = 0;
            }
        }
        buf[n] = 0;
        // SAFETY: as above.
        unsafe { ((*self.0).output_string)(self.0, buf.as_ptr()) };
        Ok(())
    }
}

// ---- Hardware -----------------------------------------------------------

/// Physical memory through the firmware's identity mapping.
struct Identity;

impl PhysRead for Identity {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
        if phys == 0
            || phys
                .checked_add(buf.len() as u64)
                .is_none_or(|e| e > 1 << 52)
        {
            return false;
        }
        // SAFETY: UEFI identity-maps all memory it reports, including the
        // ACPI tables; the copy is byte-wise.
        unsafe { ptr::copy_nonoverlapping(phys as *const u8, buf.as_mut_ptr(), buf.len()) };
        true
    }
}

/// # Safety
/// `a` must be a mapped device register.
unsafe fn r32(a: u64) -> u32 {
    // SAFETY: caller's promise.
    unsafe { ptr::read_volatile(a as *const u32) }
}
/// # Safety
/// `a` must be a mapped device register and `v` valid for it.
unsafe fn w32(a: u64, v: u32) {
    // SAFETY: caller's promise.
    unsafe { ptr::write_volatile(a as *mut u32, v) }
}
/// # Safety
/// As `r32`.
unsafe fn r64(a: u64) -> u64 {
    // SAFETY: caller's promise.
    unsafe { ptr::read_volatile(a as *const u64) }
}
/// # Safety
/// As `w32`.
unsafe fn w64(a: u64, v: u64) {
    // SAFETY: caller's promise.
    unsafe { ptr::write_volatile(a as *mut u64, v) }
}

/// # Safety
/// The MSR must exist.
unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: caller's promise.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}
/// # Safety
/// The MSR must exist and accept `v`.
unsafe fn wrmsr(msr: u32, v: u64) {
    // SAFETY: caller's promise.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nomem, nostack))
    };
}

fn cpuid1() -> (u32, u32) {
    let r = core::arch::x86_64::__cpuid(1);
    (r.ecx, r.edx)
}

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_EN: u64 = 1 << 11;
const APIC_X2: u64 = 1 << 10;

struct Lapic {
    x2: bool,
    base: u64,
}

impl Lapic {
    fn now() -> Lapic {
        // SAFETY: only built when CPUID reports an APIC, so the MSR exists.
        let ab = unsafe { rdmsr(IA32_APIC_BASE) };
        Lapic {
            x2: ab & APIC_X2 != 0,
            base: ab & 0x000f_ffff_ffff_f000,
        }
    }
    fn rd(&self, reg: u32) -> u32 {
        if self.x2 {
            // SAFETY: x2APIC mode is on, so its MSRs exist.
            unsafe { rdmsr(0x800 + (reg >> 4)) as u32 }
        } else {
            // SAFETY: the xAPIC page is identity mapped by the firmware.
            unsafe { r32(self.base + reg as u64) }
        }
    }
    fn wr(&self, reg: u32, v: u32) {
        if self.x2 {
            // SAFETY: as rd().
            unsafe { wrmsr(0x800 + (reg >> 4), v as u64) }
        } else {
            // SAFETY: as rd().
            unsafe { w32(self.base + reg as u64, v) }
        }
    }
}

// ---- State shared with the ExitBootServices hook -------------------------

struct State {
    quirks: u32,
    real_ebs: Option<extern "efiapi" fn(efi::Handle, usize) -> Status>,
    ioapics: [u64; acpi::MAX_IOAPICS],
    ioapic_count: usize,
    vtd: [u64; acpi::MAX_IOMMUS],
    vtd_count: usize,
    amd: [u64; acpi::MAX_IOMMUS],
    amd_count: usize,
    /// Three zeroed pages: VT-d root table, VT-d interrupt remapping table,
    /// AMD-Vi device table. All-zero entries mean "not present": every DMA
    /// and remapped interrupt is blocked (VT-d) or passed through (AMD-Vi).
    tables: u64,
    applied: bool,
}

type Status = efi::Status;

struct Shared(UnsafeCell<State>);
// SAFETY: UEFI boot is single-threaded: only the BSP runs, and the hook runs
// strictly after main() has finished writing the state.
unsafe impl Sync for Shared {}

static STATE: Shared = Shared(UnsafeCell::new(State {
    quirks: 0,
    real_ebs: None,
    ioapics: [0; acpi::MAX_IOAPICS],
    ioapic_count: 0,
    vtd: [0; acpi::MAX_IOMMUS],
    vtd_count: 0,
    amd: [0; acpi::MAX_IOMMUS],
    amd_count: 0,
    tables: 0,
    applied: false,
}));

fn state() -> &'static mut State {
    // SAFETY: single-threaded (see Shared); no two references are live at
    // once because each caller finishes with it before the next begins.
    unsafe { &mut *STATE.0.get() }
}

extern "efiapi" fn hooked_exit_boot_services(image: efi::Handle, key: usize) -> Status {
    let st = state();
    let Some(real) = st.real_ebs else {
        return efi::LOAD_ERROR;
    };
    let s = real(image, key);
    if s == efi::SUCCESS && !st.applied {
        st.applied = true;
        apply(st);
    }
    s
}

/// Dirties the hardware. Runs after the real ExitBootServices, with
/// interrupts off (EDK2's ExitBootServices disables them; we make sure,
/// so nothing we unmask can be delivered before the loader takes over).
fn apply(st: &State) {
    // SAFETY: ring 0; disabling interrupts is always allowed.
    unsafe { asm!("cli", options(nomem, nostack)) };
    let mut out = Serial;
    let mut ok = [false; 7];
    let mut detail = Detail::default();
    let has = |q: Quirk| st.quirks & bit(q) != 0;
    let (_, d1) = cpuid1();
    let apic = d1 & (1 << 9) != 0;

    if has(Quirk::ApicOff) && apic {
        // SAFETY: the APIC exists (CPUID); clearing EN (and EXTD with it, as
        // the SDM requires) is a valid transition.
        unsafe {
            let ab = rdmsr(IA32_APIC_BASE);
            wrmsr(IA32_APIC_BASE, ab & !(APIC_EN | APIC_X2));
            ok[Quirk::ApicOff as usize] = rdmsr(IA32_APIC_BASE) & APIC_EN == 0;
        }
    }
    if has(Quirk::X2apic) && apic {
        // SAFETY: main() checked CPUID reports x2APIC; xAPIC -> x2APIC with
        // EN set is a valid transition.
        unsafe {
            let ab = rdmsr(IA32_APIC_BASE);
            if ab & APIC_X2 == 0 {
                wrmsr(IA32_APIC_BASE, ab | APIC_EN | APIC_X2);
            }
            ok[Quirk::X2apic as usize] =
                rdmsr(IA32_APIC_BASE) & (APIC_EN | APIC_X2) == APIC_EN | APIC_X2;
        }
    }
    if has(Quirk::Lapic) && apic {
        let l = Lapic::now();
        l.wr(0x80, 0x20); // TPR
        l.wr(0xf0, 0x1ef); // SVR: enabled, spurious vector 0xEF
        let max_lvt = l.rd(0x30) >> 16 & 0xff;
        let lvts: [(u32, u32, bool); 7] = [
            (0x320, 0x50, true),           // timer, one-shot, count 0 (never fires)
            (0x330, 0x52, max_lvt >= 5),   // thermal
            (0x340, 4 << 8, max_lvt >= 4), // perf counters: NMI
            (0x350, 7 << 8, true),         // LINT0: ExtINT (virtual wire)
            (0x360, 4 << 8, true),         // LINT1: NMI
            (0x370, 0x51, true),           // error
            (0x2f0, 0x54, max_lvt >= 6),   // CMCI
        ];
        l.wr(0x380, 0); // timer initial count
        let mut all = true;
        for (reg, v, present) in lvts {
            if present {
                l.wr(reg, v);
                let back = l.rd(reg);
                all &= back & (1 << 16) == 0;
                detail.lvts += (back & (1 << 16) == 0) as u32;
            }
        }
        detail.svr = l.rd(0xf0);
        detail.tpr = l.rd(0x80);
        ok[Quirk::Lapic as usize] = all && detail.svr & 0x3ff == 0x1ef && detail.tpr == 0x20;
    }
    if has(Quirk::Pic) {
        outb(0x21, 0);
        outb(0xa1, 0);
        ok[Quirk::Pic as usize] = inb(0x21) == 0 && inb(0xa1) == 0;
    }
    if has(Quirk::Ioapic) {
        let mut all = st.ioapic_count > 0;
        for &base in &st.ioapics[..st.ioapic_count] {
            // SAFETY: the MADT lists an I/O APIC at `base` (IOREGSEL +0,
            // IOWIN +0x10), identity mapped by the firmware.
            unsafe {
                w32(base, 1);
                let max = r32(base + 0x10) >> 16 & 0xff;
                for pin in 0..=max {
                    let sel = 0x10 + 2 * pin;
                    w32(base, sel);
                    let old = r32(base + 0x10);
                    // Keep polarity and trigger mode as the firmware set
                    // them; physical destination; unmasked.
                    let mode_vec = match pin {
                        0 => 7 << 8,
                        13 => 4 << 8,
                        _ => 0x40 + pin,
                    };
                    let new = (old & (1 << 13 | 1 << 15)) | mode_vec;
                    w32(base + 0x10, new);
                    w32(base, sel);
                    let back = r32(base + 0x10);
                    all &= back & (1 << 16) == 0;
                    detail.pins += (back & (1 << 16) == 0) as u32;
                }
            }
        }
        ok[Quirk::Ioapic as usize] = all;
    }
    if has(Quirk::Vtd) {
        ok[Quirk::Vtd as usize] = st.vtd_count > 0;
        for &base in &st.vtd[..st.vtd_count] {
            let (te, ire) = enable_vtd(base, st.tables, st.tables + 4096);
            ok[Quirk::Vtd as usize] &= te;
            detail.vtd_te += te as u32;
            detail.vtd_ire += ire as u32;
        }
    }
    if has(Quirk::Amdvi) {
        ok[Quirk::Amdvi as usize] = st.amd_count > 0;
        for &base in &st.amd[..st.amd_count] {
            // SAFETY: the IVRS lists an IOMMU at `base`: device table base
            // at +0 (size field 0 = one 4 KiB page), control at +0x18.
            let on = unsafe {
                w64(base, st.tables + 8192);
                w64(base + 0x18, r64(base + 0x18) | 1);
                r64(base + 0x18) & 1 != 0
            };
            ok[Quirk::Amdvi as usize] &= on;
            detail.amd += on as u32;
        }
    }

    let _ = write!(out, "\r\nOPTIMAL-QUIRK: applied:");
    for (q, n) in ALL {
        if has(q) && ok[q as usize] {
            let _ = write!(out, " {n}");
        }
    }
    let _ = write!(out, "\r\nOPTIMAL-QUIRK: not applied:");
    for (q, n) in ALL {
        if has(q) && !ok[q as usize] {
            let _ = write!(out, " {n}");
        }
    }
    let _ = write!(
        out,
        "\r\nOPTIMAL-QUIRK: detail: {} I/O APIC pins unmasked, {} LVTs unmasked, SVR {:#x}, TPR {:#x}, VT-d {} TE {} IRE, AMD-Vi {} on\r\n",
        detail.pins, detail.lvts, detail.svr, detail.tpr, detail.vtd_te, detail.vtd_ire, detail.amd
    );
}

#[derive(Default)]
struct Detail {
    pins: u32,
    lvts: u32,
    svr: u32,
    tpr: u32,
    vtd_te: u32,
    vtd_ire: u32,
    amd: u32,
}

/// Turns on DMA translation over the zeroed root table at `root` and, if the
/// unit supports it, interrupt remapping over the zeroed table at `irt`.
/// Each step waits (bounded) for the unit to confirm. Returns (TE, IRE).
fn enable_vtd(base: u64, root: u64, irt: u64) -> (bool, bool) {
    const ECAP: u64 = 0x10;
    const GCMD: u64 = 0x18;
    const GSTS: u64 = 0x1c;
    const RTADDR: u64 = 0x20;
    const IRTA: u64 = 0xb8;
    const TE: u32 = 1 << 31;
    const SRTP: u32 = 1 << 30;
    const IRE: u32 = 1 << 25;
    const SIRTP: u32 = 1 << 24;
    // VT-d spec 11.4.4: write GCMD as GSTS with the one-shot bits cleared,
    // plus the one command bit being changed.
    const KEEP: u32 = 0x96ff_ffff;
    // SAFETY: the DMAR lists a remapping unit at `base`; its register page
    // is identity mapped by the firmware.
    let cmd = |bit: u32| unsafe {
        let sts = r32(base + GSTS) & KEEP;
        w32(base + GCMD, sts | bit);
        for _ in 0..1_000_000 {
            if r32(base + GSTS) & bit != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    };
    // SAFETY: as above.
    let ir = unsafe {
        w64(base + RTADDR, root);
        r64(base + ECAP) & (1 << 3) != 0
    };
    let te = cmd(SRTP) && cmd(TE);
    let ire = ir && {
        // 256 entries (S = 7); x2APIC destinations allowed (EIME).
        // SAFETY: as above.
        unsafe { w64(base + IRTA, irt | 1 << 11 | 7) };
        cmd(SIRTP) && cmd(IRE)
    };
    (te, ire)
}

// ---- Setup and chain-loading ---------------------------------------------

macro_rules! fatal {
    ($con:expr, $($a:tt)*) => {{
        let _ = write!($con, "\nOPTIMAL-QUIRK: FATAL: ");
        let _ = write!($con, $($a)*);
        let _ = write!($con, "\n");
        return efi::LOAD_ERROR;
    }};
}

fn protocol<T>(bs: &efi::BootServices, h: efi::Handle, guid: &efi::Guid) -> Option<*mut T> {
    let mut p: *mut c_void = ptr::null_mut();
    ((bs.handle_protocol)(h, guid, &mut p) == efi::SUCCESS && !p.is_null()).then_some(p as *mut T)
}

/// Reads up to `buf.len()` bytes of a file on the volume `dev`.
fn read_file(
    bs: &efi::BootServices,
    dev: efi::Handle,
    path: &[u16],
    buf: &mut [u8],
) -> Option<usize> {
    let sfs: *mut efi::SimpleFileSystem = protocol(bs, dev, &efi::SIMPLE_FILE_SYSTEM_GUID)?;
    let mut root = ptr::null_mut();
    let mut file = ptr::null_mut();
    let mut n = buf.len();
    // SAFETY: firmware protocol pointers; path is NUL-terminated UCS-2 and
    // buf has n writable bytes.
    unsafe {
        if ((*sfs).open_volume)(sfs, &mut root) != efi::SUCCESS {
            return None;
        }
        let s = ((*root).open)(root, &mut file, path.as_ptr(), efi::FILE_MODE_READ, 0);
        ((*root).close)(root);
        if s != efi::SUCCESS {
            return None;
        }
        let s = ((*file).read)(file, &mut n, buf.as_mut_ptr());
        ((*file).close)(file);
        (s == efi::SUCCESS).then_some(n)
    }
}

const QUIRKS_PATH: [u16; 22] = efi::ucs2("\\EFI\\BOOT\\QUIRKS.TXT");
const NEXT_PATH: [u16; 20] = efi::ucs2("\\EFI\\BOOT\\NEXT.EFI");

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: efi::Handle, st: *mut efi::SystemTable) -> Status {
    // SAFETY: the firmware passes a valid system table that outlives us.
    let st = unsafe { &mut *st };
    let mut con = ConOut(st.con_out);
    // SAFETY: valid while boot services run, which is all of main().
    let bs = unsafe { &mut *st.boot_services };
    let _ = writeln!(con, "OPTIMAL-QUIRK: firmware-quirk injector (test only)");

    let Some(li) = protocol::<efi::LoadedImage>(bs, image, &efi::LOADED_IMAGE_GUID) else {
        fatal!(con, "no loaded image protocol");
    };
    // SAFETY: the firmware's loaded image protocol for this image.
    let dev = unsafe { (*li).device_handle };

    let mut text = [0u8; 512];
    let Some(n) = read_file(bs, dev, &QUIRKS_PATH, &mut text) else {
        fatal!(con, "cannot read \\EFI\\BOOT\\QUIRKS.TXT");
    };
    if n == text.len() {
        fatal!(con, "QUIRKS.TXT is too long");
    }
    let quirks = match parse(&text[..n]) {
        Ok(q) => q,
        Err(w) => fatal!(
            con,
            "bad QUIRKS.TXT word {:?}",
            core::str::from_utf8(w).unwrap_or("?")
        ),
    };
    let s = state();
    s.quirks = quirks;

    // ACPI: the RSDP from the configuration table (2.0 preferred).
    // SAFETY: the firmware's configuration table array, of the stated length.
    let tables =
        unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    let rsdp = [efi::ACPI_20_TABLE_GUID, efi::ACPI_10_TABLE_GUID]
        .iter()
        .find_map(|g| tables.iter().find(|t| t.guid == *g))
        .map(|t| t.table as u64);
    let mem = Identity;
    if let Some(a) = rsdp.and_then(|r| acpi::Acpi::new(&mem, r)) {
        if let Some(m) = a.find(b"APIC").and_then(|(p, l)| Madt::parse(&mem, p, l)) {
            s.ioapic_count = m.ioapics().len();
            s.ioapics[..s.ioapic_count].copy_from_slice(m.ioapics());
        }
        if let Some((p, l)) = a.find(b"DMAR") {
            s.vtd_count = acpi::dmar_units(&mem, p, l, &mut s.vtd);
        }
        if let Some((p, l)) = a.find(b"IVRS") {
            s.amd_count = acpi::ivrs_units(&mem, p, l, &mut s.amd);
        }
    }
    let (c1, d1) = cpuid1();
    let has = |q: Quirk| quirks & bit(q) != 0;
    for (q, missing) in [
        (Quirk::Ioapic, s.ioapic_count == 0),
        (Quirk::Vtd, s.vtd_count == 0),
        (Quirk::Amdvi, s.amd_count == 0),
        (Quirk::Lapic, d1 & (1 << 9) == 0),
        (Quirk::ApicOff, d1 & (1 << 9) == 0),
        (Quirk::X2apic, c1 & (1 << 21) == 0),
    ] {
        if has(q) && missing {
            fatal!(
                con,
                "quirk {} asked for, but this machine has no such hardware",
                name(q)
            );
        }
    }

    let mut pages = 0u64;
    if (bs.allocate_pages)(efi::ALLOCATE_ANY_PAGES, efi::LOADER_DATA, 3, &mut pages) != efi::SUCCESS
    {
        fatal!(con, "cannot allocate the IOMMU tables");
    }
    // SAFETY: three pages just allocated for us, identity mapped.
    unsafe { ptr::write_bytes(pages as *mut u8, 0, 3 * 4096) };
    s.tables = pages;

    // The next loader's device path: our volume's path + \EFI\BOOT\NEXT.EFI.
    let Some(dp) = protocol::<efi::DevicePath>(bs, dev, &efi::DEVICE_PATH_GUID) else {
        fatal!(con, "the boot volume has no device path");
    };
    let mut path = [0u8; 1024];
    let mut len = 0usize;
    // SAFETY: a firmware device path: nodes with 16-bit lengths, ending in
    // an End node (type 0x7f, subtype 0xff). Copied node by node, bounded.
    unsafe {
        let mut p = dp as *const u8;
        loop {
            let (t, sub) = (*p, *p.add(1));
            let l = u16::from_le_bytes([*p.add(2), *p.add(3)]) as usize;
            if t == 0x7f && sub == 0xff {
                break;
            }
            if l < 4 || len + l > path.len() - 64 {
                fatal!(
                    con,
                    "the boot volume's device path is malformed or too long"
                );
            }
            ptr::copy_nonoverlapping(p, path.as_mut_ptr().add(len), l);
            len += l;
            p = p.add(l);
        }
    }
    // Media / File Path node, then End.
    let node_len = 4 + 2 * NEXT_PATH.len();
    path[len..len + 4].copy_from_slice(&[4, 4, node_len as u8, (node_len >> 8) as u8]);
    for (i, c) in NEXT_PATH.iter().enumerate() {
        path[len + 4 + 2 * i..len + 6 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    len += node_len;
    path[len..len + 4].copy_from_slice(&[0x7f, 0xff, 4, 0]);

    let mut next: efi::Handle = ptr::null_mut();
    let s_load = (bs.load_image)(
        false,
        image,
        path.as_ptr() as *const efi::DevicePath,
        ptr::null(),
        0,
        &mut next,
    );
    if s_load != efi::SUCCESS {
        fatal!(
            con,
            "cannot load \\EFI\\BOOT\\NEXT.EFI (status {s_load:#x})"
        );
    }

    // Hook ExitBootServices and fix the table's CRC, as the spec requires of
    // anyone who changes it.
    s.real_ebs = Some(bs.exit_boot_services);
    bs.exit_boot_services = hooked_exit_boot_services;
    bs.hdr.crc32 = 0;
    let mut crc = 0u32;
    (bs.calculate_crc32)(
        bs as *const efi::BootServices as *const c_void,
        bs.hdr.header_size as usize,
        &mut crc,
    );
    bs.hdr.crc32 = crc;

    let _ = write!(con, "OPTIMAL-QUIRK: will apply:");
    for (q, n) in ALL {
        if has(q) {
            let _ = write!(con, " {n}");
        }
    }
    let _ = writeln!(con, "\nOPTIMAL-QUIRK: starting \\EFI\\BOOT\\NEXT.EFI");
    let s_start = (bs.start_image)(next, ptr::null_mut(), ptr::null_mut());

    // The next loader returned to us: it failed before handing over. Undo the
    // hook so the firmware's own boot manager is not left with it.
    bs.exit_boot_services = s.real_ebs.unwrap_or(bs.exit_boot_services);
    bs.hdr.crc32 = 0;
    let mut crc = 0u32;
    (bs.calculate_crc32)(
        bs as *const efi::BootServices as *const c_void,
        bs.hdr.header_size as usize,
        &mut crc,
    );
    bs.hdr.crc32 = crc;
    let _ = writeln!(con, "OPTIMAL-QUIRK: the next loader returned {s_start:#x}");
    s_start
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = write!(Serial, "\r\nOPTIMAL-QUIRK: FATAL: panic: {info}\r\n");
    loop {
        // SAFETY: halting is always allowed in ring 0.
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}
