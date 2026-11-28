//! The BIOS boot sequence, from the boot volume to a verified, loaded
//! kernel. It mirrors the UEFI loader's (loader/src/boot.rs) step for step;
//! the handoff itself (page tables, responses, the jump) is the next part
//! of the BIOS stage, shared with the UEFI loader rather than copied.

use crate::console::Hex;
use crate::disk::BiosDisk;
use crate::mem::{self, Kind, PAGE, Pages};
use crate::{NAME, TRUSTED_KEYS, VERSION, fatal, hw, menu, println};
use alloc::vec::Vec;
use optimal_boot_core::crc32::Crc32;
use optimal_boot_core::disk::{BlockDevice, Window};
use optimal_boot_core::fat::{Fat, FatError, FileRef};
use optimal_boot_core::part::{self, Mbr};
use optimal_boot_core::protocol as lp;
use optimal_boot_core::rng::Drbg;
use optimal_boot_core::{config, elf, kaslr, scan, sig};

const CONFIG_PATH: &str = "/boot/optimal-boot.conf";
const SIG_PATH: &str = "/boot/optimal-boot.conf.sig";
const MAX_KERNEL: u64 = 512 << 20;
const MAX_MODULE: u64 = 1 << 30;

/// Installer-written header at the start of stage 2 (stage2.s).
#[repr(C)]
struct Header {
    magic: [u8; 8],
    size: u32,
    crc: u32,
    volume_kind: u8,
    mbr_index: u8,
    _r0: u16,
    _r1: u32,
    volume_lba: u64,
    gpt_guid: [u8; 16],
    version: u32,
}

const VOLUME_GPT: u8 = 1;
const VOLUME_MBR: u8 = 2;
const HEADER_LEN: usize = 0x40;

unsafe extern "C" {
    static stage2_header: Header;
    static __stage2_file_end: u8;
}

fn header() -> &'static Header {
    // SAFETY: the header is the first thing in stage 2, written by the
    // installer and never changed at run time.
    unsafe { &*core::ptr::addr_of!(stage2_header) }
}

/// Checks stage 2 against the CRC the installer recorded: a damaged or
/// half-written install stops here with a clear message, instead of
/// running whatever the disk returned. (On BIOS nothing verifies stage 2
/// cryptographically; that is what boot tier B4 means.)
pub fn check_self() -> Result<(), &'static str> {
    let h = header();
    let start = h as *const Header as u64;
    let file_end = core::ptr::addr_of!(__stage2_file_end) as u64;
    let len = h.size as u64;
    if &h.magic != b"OBSTAGE2" || h.version != 1 {
        return Err("stage 2 header is not valid (reinstall)");
    }
    if len != file_end - start {
        return Err("stage 2 size in its header is wrong (reinstall)");
    }
    // SAFETY: stage 1 loaded `len` bytes from `start`.
    let body = unsafe {
        core::slice::from_raw_parts(
            (start + HEADER_LEN as u64) as *const u8,
            len as usize - HEADER_LEN,
        )
    };
    let mut c = Crc32::new();
    c.update(body);
    if c.finish() != h.crc {
        return Err("stage 2 is damaged: its CRC does not match (reinstall)");
    }
    Ok(())
}

fn gather_entropy(feat: &hw::Features) -> Drbg {
    let mut d = Drbg::new();
    for (present, label, f) in [
        (
            feat.rdseed,
            &b"rdseed"[..],
            hw::rdseed as fn() -> Option<u64>,
        ),
        (feat.rdrand, b"rdrand", hw::rdrand),
    ] {
        if present {
            for _ in 0..8 {
                if let Some(v) = f() {
                    d.mix(label, &v.to_le_bytes(), 32);
                }
            }
        }
    }
    // Timing jitter around BIOS waits: never credited, but mixed in.
    for _ in 0..64 {
        let t = hw::rdtsc();
        crate::bios::wait_us(1);
        d.mix(b"jitter", &(hw::rdtsc() - t).to_le_bytes(), 0);
    }
    d
}

struct Volume {
    disk: BiosDisk,
    start: u64,
    sectors: u64,
}

impl Volume {
    fn fat(&mut self) -> Result<Fat<'_, BiosDisk>, FatError> {
        let w = Window::new(&mut self.disk, self.start, self.sectors).ok_or(FatError::BadBpb)?;
        Fat::mount(w)
    }

    /// Reads a whole file of at most `max` bytes into fresh pages.
    fn read(&mut self, path: &str, max: u64, kind: Kind) -> Result<(Pages, u64), FatError> {
        let mut fat = self.fat()?;
        let f: FileRef = fat.open(path)?;
        if f.size as u64 > max {
            return Err(FatError::TooLarge);
        }
        let mut pages = Pages::new(f.size as u64, kind).ok_or(FatError::TooLarge)?;
        fat.read(&f, &mut pages.bytes()[..f.size as usize])?;
        Ok((pages, f.size as u64))
    }
}

fn open_volume(drive: u8) -> Volume {
    let h = header();
    let mut disk = match BiosDisk::open(drive) {
        Ok(d) => d,
        Err(e) => fatal(format_args!("boot disk {drive:#04x}: {e}")),
    };
    let ss = disk.sector_size();
    let mut s0 = [0u8; 4096];
    if !disk.read(0, &mut s0[..ss]) {
        fatal(format_args!("boot disk {drive:#04x}: cannot read sector 0"));
    }
    let mbr = match Mbr::parse(&s0[..ss.max(512)], disk.sectors()) {
        Ok(m) => m,
        Err(e) => fatal(format_args!("boot disk: {}", e.describe())),
    };
    let (start, sectors, what) = match h.volume_kind {
        VOLUME_GPT => match part::gpt_find(&mut disk, &h.gpt_guid, h.volume_lba) {
            Ok(f) => {
                if f.used_backup {
                    println!(
                        "WARNING: the primary GPT is damaged; using the backup copy (repair the disk)"
                    );
                }
                (
                    f.part.start_lba,
                    f.part.end_lba - f.part.start_lba + 1,
                    f.part.index,
                )
            }
            Err(e) => fatal(format_args!("boot volume: {}", e.describe())),
        },
        VOLUME_MBR => match mbr.volume(h.mbr_index, h.volume_lba) {
            Ok(e) => (e.start_lba as u64, e.sectors as u64, h.mbr_index as u32),
            Err(e) => fatal(format_args!("boot volume: {}", e.describe())),
        },
        _ => fatal(format_args!(
            "stage 2 has no boot volume recorded (reinstall)"
        )),
    };
    println!(
        "Boot volume: {} partition {what}, {} sectors of {ss} bytes at LBA {start}",
        if h.volume_kind == VOLUME_GPT {
            "GPT"
        } else {
            "MBR"
        },
        sectors
    );
    Volume {
        disk,
        start,
        sectors,
    }
}

/// `self_check` is check_self()'s result, taken before anything wrote to
/// stage 2's data.
pub fn run(drive: u8, self_check: Result<(), &'static str>, t_init: u64) -> ! {
    println!("{NAME} {VERSION} (BIOS)");
    if let Err(e) = self_check {
        fatal(format_args!("{e}"));
    }
    let feat = hw::features();
    if !feat.pat {
        fatal(format_args!(
            "this CPU reports no PAT, which every x86-64 CPU has"
        ));
    }
    // SAFETY: the BIOS data area word at 0x413 is conventional memory in KiB.
    let conv_kib = unsafe { core::ptr::read_volatile(0x413 as *const u16) } as u64;
    if conv_kib * 1024 < crate::bios::BOUNCE + crate::bios::BOUNCE_LEN as u64 {
        fatal(format_args!(
            "only {conv_kib} KiB of conventional memory: the EBDA overlaps the disk buffer"
        ));
    }
    if let Err(e) = mem::init() {
        fatal(format_args!("{}", e.describe()));
    }
    let mut rng = gather_entropy(&feat);
    println!("Secure Boot: not available (BIOS); boot tier B4");

    // ---- The signed config ----
    let mut vol = open_volume(drive);
    let (mut cfg_pages, cfg_len) =
        match vol.read(CONFIG_PATH, config::MAX_CONFIG_SIZE as u64, Kind::Loader) {
            Ok(f) => f,
            Err(e) => fatal(format_args!("{CONFIG_PATH}: {}", e.describe())),
        };
    let (mut sig_pages, sig_len) = match vol.read(SIG_PATH, 4096, Kind::Loader) {
        Ok(f) => f,
        Err(e) => fatal(format_args!(
            "{SIG_PATH}: {} (the config must be signed)",
            e.describe()
        )),
    };
    let cfg_bytes: &'static [u8] = {
        let b = cfg_pages.bytes();
        // SAFETY: the pages are never freed before the kernel runs.
        unsafe { core::slice::from_raw_parts(b.as_ptr(), cfg_len as usize) }
    };
    let sig_bytes = &sig_pages.bytes()[..sig_len as usize];
    let key_index = match sig::verify(cfg_bytes, sig_bytes, &TRUSTED_KEYS) {
        Ok(i) => i,
        Err(e) => fatal(format_args!("{CONFIG_PATH}: {}", e.describe())),
    };
    println!(
        "Config: signature valid (key {})",
        Hex(&sig::key_id(&TRUSTED_KEYS[key_index])[..8])
    );
    let cfg = match config::parse(cfg_bytes) {
        Ok(c) => c,
        Err(e) => fatal(format_args!(
            "{CONFIG_PATH} line {}: {}",
            e.line,
            e.kind.describe()
        )),
    };
    let entry = cfg.entry(menu::choose(&cfg));
    println!("Booting: {}", entry.title);
    println!("TPM: not used by the BIOS stage yet, boot is not measured");

    // ---- Every file, verified against the config's hashes ----
    let mut load = |path: &str, want: &[u8; 64], max: u64| -> (Pages, u64) {
        let (mut pages, size) = match vol.read(path, max, Kind::Kernel) {
            Ok(f) => f,
            Err(e) => fatal(format_args!("{path}: {}", e.describe())),
        };
        if !sig::ct_eq(&sig::sha512(&pages.bytes()[..size as usize]), want) {
            fatal(format_args!(
                "{path}: SHA-512 does not match the signed config (file changed or damaged)"
            ));
        }
        (pages, size)
    };
    let kref = entry.kernel();
    let (mut kfile, ksize) = load(kref.path, &kref.sha512, MAX_KERNEL);
    let mut modules: Vec<(Pages, u64)> = Vec::new();
    for m in entry.modules() {
        modules.push(load(m.file.path, &m.file.sha512, MAX_MODULE));
    }
    println!(
        "Files: kernel and {} module(s) match the signed config (kernel file at {:#x})",
        modules.len(),
        kfile.phys
    );

    // ---- The kernel image ----
    let kbytes: &[u8] = {
        let b = kfile.bytes();
        // SAFETY: the file pages stay allocated for the whole boot.
        unsafe { core::slice::from_raw_parts(b.as_ptr(), ksize as usize) }
    };
    let layout = match elf::parse(kbytes) {
        Ok(l) => l,
        Err(e) => fatal(format_args!("{}: {}", kref.path, e.describe())),
    };
    let (slide_min, slide_count) = match elf::slide_range(&layout) {
        Ok(r) => r,
        Err(e) => fatal(format_args!("{}: {}", kref.path, e.describe())),
    };
    let slide = if layout.pie && cfg.kaslr {
        slide_min + rng.below(slide_count) * layout.align
    } else {
        slide_min
    };
    let phys_align = layout.align.clamp(PAGE, 2 << 20);
    let mut kphys = None;
    if cfg.kaslr {
        let mut free = [(0u64, 0u64); 512];
        let n = mem::free_ranges(&mut free);
        for _ in 0..4 {
            if let Some(a) =
                kaslr::pick_physical(&free[..n], layout.span, phys_align, 16 << 20, &mut rng)
                && mem::alloc_at(a, layout.span / PAGE, Kind::Kernel)
            {
                kphys = Some(a);
                break;
            }
        }
    }
    let kphys = match kphys
        .or_else(|| mem::alloc_aligned(layout.span / PAGE, phys_align, None, Kind::Kernel))
    {
        Some(p) => p,
        None => fatal(format_args!("out of memory for the kernel image")),
    };
    // SAFETY: we own `span` bytes at kphys (identity mapped).
    let img = unsafe { core::slice::from_raw_parts_mut(kphys as *mut u8, layout.span as usize) };
    elf::load(kbytes, &layout, img);
    if let Err(e) = elf::relocate(&layout, img, slide) {
        fatal(format_args!("{}: {}", kref.path, e.describe()));
    }
    let reqs = match scan::scan(img) {
        Ok(s) => s,
        Err(e) => fatal(format_args!("{}: bad Limine requests: {e:?}", kref.path)),
    };
    let rev = reqs.requested_revision;
    if !(lp::MIN_BASE_REVISION..=lp::MAX_BASE_REVISION).contains(&rev) {
        fatal(format_args!(
            "kernel asks for Limine base revision {rev}; {NAME} supports 3 to 6"
        ));
    }
    println!(
        "Kernel: {} bytes at {:#x} (physical {kphys:#x}), base revision {rev}, KASLR {}",
        ksize,
        layout.base.wrapping_add(slide),
        if cfg.kaslr { "on" } else { "off" }
    );
    let tsc = hw::rdtsc() - t_init;
    println!("OPTIMAL-BOOT: BIOS stage: kernel verified and loaded ({tsc} TSC ticks)");
    fatal(format_args!(
        "the BIOS stage cannot hand over to the kernel yet (that is its next part)"
    ))
}
