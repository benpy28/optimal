//! The boot sequence, from the verified config to the jump into the kernel.

use crate::console::{self, Hex, boot_services, runtime_services, system_table};
use crate::efi::{self, ucs2};
use crate::fs::Volume;
use crate::handoff::{self, Block};
use crate::memory::{self, Arena, PAGE, Pages};
use crate::platform::{self, Identity};
use crate::tpm::{self, Tpm};
use crate::{NAME, TRUSTED_KEYS, VERSION, fatal, gop, hw, menu, println};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::ptr;
use optimal_boot_core::acpi::{self, PhysRead};
use optimal_boot_core::memmap::{self, EfiRegion, Event, Override};
use optimal_boot_core::paging::{self, FrameSource, Mapper};
use optimal_boot_core::protocol::{self as lp, Feature, MemmapEntry};
use optimal_boot_core::rng::Drbg;
use optimal_boot_core::{config, elf, kaslr, scan, sig};

const CONFIG_PATH: &str = "/boot/optimal-boot.conf";
const SIG_PATH: &str = "/boot/optimal-boot.conf.sig";
const MAX_KERNEL: u64 = 512 << 20;
const MAX_MODULE: u64 = 1 << 30;
const BELOW_4G: u64 = 0xffff_f000;

macro_rules! fail {
    ($($arg:tt)*) => { return Err(fatal(format_args!($($arg)*))) };
}

struct Loaded {
    path: String,
    string: String,
    pages: Pages,
    size: u64,
}

fn read_var_u8(name: &[u16]) -> Option<u8> {
    let mut data = [0u8; 1];
    let mut size = 1usize;
    let mut attrs = 0u32;
    let s = (runtime_services().get_variable)(
        name.as_ptr(),
        &efi::GLOBAL_VARIABLE_GUID,
        &mut attrs,
        &mut size,
        data.as_mut_ptr() as *mut _,
    );
    (s == efi::SUCCESS && size == 1).then_some(data[0])
}

fn gather_entropy(feat: &hw::Features) -> Drbg {
    let mut d = Drbg::new();
    let mut p: *mut core::ffi::c_void = ptr::null_mut();
    if (boot_services().locate_protocol)(&efi::RNG_GUID, ptr::null(), &mut p) == efi::SUCCESS
        && !p.is_null()
    {
        let rng = p as *mut efi::RngProtocol;
        let mut buf = [0u8; 64];
        // SAFETY: firmware RNG protocol; buf is writable for 64 bytes.
        if unsafe { ((*rng).get_rng)(rng, ptr::null(), buf.len(), buf.as_mut_ptr()) }
            == efi::SUCCESS
        {
            d.mix(b"efi-rng", &buf, 256);
        }
    }
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
    // Timing jitter: never credited, but mixed in.
    for _ in 0..64 {
        let t = hw::rdtsc();
        (boot_services().stall)(1);
        d.mix(b"jitter", &(hw::rdtsc() - t).to_le_bytes(), 0);
    }
    d
}

fn config_table(guid: &efi::Guid) -> Option<u64> {
    let st = system_table();
    // SAFETY: the configuration table array has number_of_table_entries items.
    let tables =
        unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    tables
        .iter()
        .find(|t| t.guid == *guid)
        .map(|t| t.table as u64)
}

/// Gets the current memory map into `buf`. Returns (size, key, desc_size, desc_version).
fn memory_map(buf: &mut Pages) -> Result<(usize, usize, usize, u32), efi::Status> {
    let mut size = buf.len();
    let mut key = 0usize;
    let mut dsize = 0usize;
    let mut dver = 0u32;
    let s = (boot_services().get_memory_map)(
        &mut size,
        buf.phys as *mut efi::MemoryDescriptor,
        &mut key,
        &mut dsize,
        &mut dver,
    );
    if s != efi::SUCCESS || dsize < core::mem::size_of::<efi::MemoryDescriptor>() {
        return Err(s);
    }
    Ok((size, key, dsize, dver))
}

fn regions(buf: &Pages, size: usize, dsize: usize) -> impl Iterator<Item = EfiRegion> + '_ {
    (0..size / dsize).map(move |i| {
        // SAFETY: the firmware wrote `size` bytes of descriptors, `dsize` apart.
        let d = unsafe {
            ptr::read_unaligned((buf.phys as usize + i * dsize) as *const efi::MemoryDescriptor)
        };
        EfiRegion {
            typ: d.typ,
            start: d.physical_start,
            pages: d.number_of_pages,
        }
    })
}

/// Page-table frames from a block of pages we own (identity mapped).
/// Page-table frames, from up to PT_CHUNKS separately allocated blocks
/// below 4 GiB, so the tables never need one large contiguous hole (on a
/// small, KASLR-fragmented machine there may be none).
struct PtArena {
    chunks: [(u64, u64); PT_CHUNKS],
    count: usize,
    used: u64,
}

const PT_CHUNKS: usize = 64;
/// Frames per chunk: at least 1 MiB, so a typical handoff is one or two
/// blocks (each block is one more memory map entry).
const PT_CHUNK_MIN: u64 = 256;

impl PtArena {
    /// Allocates `frames` frames below 4 GiB in at most PT_CHUNKS blocks.
    fn new(frames: u64) -> Option<PtArena> {
        let per = frames.div_ceil(PT_CHUNKS as u64).max(PT_CHUNK_MIN);
        let mut a = PtArena {
            chunks: [(0, 0); PT_CHUNKS],
            count: 0,
            used: 0,
        };
        let mut left = frames;
        while left > 0 {
            let n = left.min(per);
            let Some(base) = memory::alloc_pages(n, efi::LOADER_DATA, Some(BELOW_4G)) else {
                a.free();
                return None;
            };
            a.chunks[a.count] = (base, n);
            a.count += 1;
            left -= n;
        }
        Some(a)
    }

    fn free(&mut self) {
        for &(base, n) in &self.chunks[..self.count] {
            memory::free_pages(base, n);
        }
        self.count = 0;
    }
}

impl FrameSource for PtArena {
    fn alloc(&mut self) -> Option<u64> {
        let mut i = self.used;
        let mut p = None;
        for &(base, n) in &self.chunks[..self.count] {
            if i < n {
                p = Some(base + i * PAGE);
                break;
            }
            i -= n;
        }
        let p = p?;
        self.used += 1;
        // SAFETY: inside the arena we allocated.
        unsafe { ptr::write_bytes(p as *mut u8, 0, PAGE as usize) };
        Some(p)
    }
    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: only called with frames from this arena (or tables reached
        // through them), which are identity mapped and page aligned.
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }
}

fn alloc_aligned(bytes: u64, align: u64) -> Option<u64> {
    let pages = memory::pages_for(bytes);
    if align <= PAGE {
        return memory::alloc_pages(pages, efi::LOADER_DATA, None);
    }
    let extra = align / PAGE - 1;
    let raw = memory::alloc_pages(pages + extra, efi::LOADER_DATA, None)?;
    let aligned = (raw + align - 1) & !(align - 1);
    let head = (aligned - raw) / PAGE;
    if head > 0 {
        memory::free_pages(raw, head);
    }
    let tail = extra - head;
    if tail > 0 {
        memory::free_pages(aligned + pages * PAGE, tail);
    }
    Some(aligned)
}

struct Built {
    entries: usize,
    root: u64,
}

pub fn run(image: efi::Handle) -> Result<Infallible, efi::Status> {
    let bs = boot_services();
    (bs.set_watchdog_timer)(0, 0, 0, ptr::null());
    let t_init = hw::rdtsc();
    println!("{NAME} {VERSION}");
    let feat = hw::features();
    // The trampoline programs the PAT unconditionally; every x86-64 CPU has
    // one, so a CPU claiming otherwise is refused rather than half-handled.
    if !feat.pat {
        fail!("this CPU reports no PAT, which every x86-64 CPU has");
    }
    let mut rng = gather_entropy(&feat);

    let secure_boot = read_var_u8(&ucs2::<11>("SecureBoot")) == Some(1);
    let setup_mode = read_var_u8(&ucs2::<10>("SetupMode")) == Some(1);
    println!(
        "Secure Boot: {}",
        if secure_boot {
            "on"
        } else if setup_mode {
            "off (setup mode)"
        } else {
            "off"
        }
    );

    // ---- The signed config ----
    let vol = match Volume::open(image) {
        Ok(v) => v,
        Err(e) => fail!("{}", e.describe()),
    };
    let (mut cfg_pages, cfg_len) = match vol.read(CONFIG_PATH, config::MAX_CONFIG_SIZE as u64) {
        Ok(f) => f,
        Err(e) => fail!("{CONFIG_PATH}: {}", e.describe()),
    };
    let (mut sig_pages, sig_len) = match vol.read(SIG_PATH, 4096) {
        Ok(f) => f,
        Err(e) => fail!("{SIG_PATH}: {} (the config must be signed)", e.describe()),
    };
    // SAFETY-free: Pages::bytes gives the file contents; keep them alive.
    let cfg_bytes: &'static [u8] = {
        let b = cfg_pages.bytes();
        // SAFETY: the pages live until the kernel runs; nothing frees them.
        unsafe { core::slice::from_raw_parts(b.as_ptr(), cfg_len as usize) }
    };
    let sig_bytes = &sig_pages.bytes()[..sig_len as usize];
    let key_index = match sig::verify(cfg_bytes, sig_bytes, &TRUSTED_KEYS) {
        Ok(i) => i,
        Err(e) => fail!("{CONFIG_PATH}: {}", e.describe()),
    };
    println!(
        "Config: signature valid (key {})",
        Hex(&sig::key_id(&TRUSTED_KEYS[key_index])[..8])
    );
    let cfg = match config::parse(cfg_bytes) {
        Ok(c) => c,
        Err(e) => fail!("{CONFIG_PATH} line {}: {}", e.line, e.kind.describe()),
    };

    let tpm = Tpm::find();
    let mut measured_ok = true;
    if let Some(t) = &tpm {
        measured_ok &= t.measure(
            tpm::PCR_CONFIG,
            cfg_bytes,
            &format!("optimal-boot config {CONFIG_PATH}"),
        );
    }

    let entry = cfg.entry(menu::choose(&cfg));
    println!("Booting: {}", entry.title);
    if let Some(t) = &tpm {
        let desc = format!("optimal-boot entry {} cmdline {}", entry.id, entry.cmdline);
        measured_ok &= t.measure(tpm::PCR_CONFIG, desc.as_bytes(), &desc);
    }

    // ---- Every file, verified against the config's hashes ----
    let mut load =
        |path: &str, want: &[u8; 64], max: u64, what: &str| -> Result<(Pages, u64), efi::Status> {
            let (mut pages, size) = match vol.read(path, max) {
                Ok(f) => f,
                Err(e) => fail!("{path}: {}", e.describe()),
            };
            let data = &pages.bytes()[..size as usize];
            if !sig::ct_eq(&sig::sha512(data), want) {
                fail!("{path}: SHA-512 does not match the signed config (file changed or damaged)");
            }
            if let Some(t) = &tpm {
                measured_ok &= t.measure(
                    tpm::PCR_IMAGES,
                    data,
                    &format!("optimal-boot {what} {path}"),
                );
            }
            Ok((pages, size))
        };
    let kref = entry.kernel();
    let (mut kfile, ksize) = load(kref.path, &kref.sha512, MAX_KERNEL, "kernel")?;
    let mut modules: Vec<Loaded> = Vec::new();
    for m in entry.modules() {
        let (pages, size) = load(m.file.path, &m.file.sha512, MAX_MODULE, "module")?;
        modules.push(Loaded {
            path: m.file.path.into(),
            string: m.string.into(),
            pages,
            size,
        });
    }
    drop(load);
    match (&tpm, measured_ok) {
        (Some(t), true) if t.sha256_active => println!("TPM: measured into PCR 8 and 9"),
        (Some(_), true) => {
            println!("TPM: measured into PCR 8 and 9 (WARNING: the SHA-256 bank is not active)")
        }
        (Some(_), false) => {
            println!("TPM: WARNING: a measurement failed; sealed secrets will not unlock")
        }
        (None, _) => println!("TPM: none found, boot is not measured"),
    }

    // ---- The kernel image ----
    let kbytes: &[u8] = {
        let b = kfile.bytes();
        // SAFETY: the file pages stay allocated for the whole boot.
        unsafe { core::slice::from_raw_parts(b.as_ptr(), ksize as usize) }
    };
    let layout = match elf::parse(kbytes) {
        Ok(l) => l,
        Err(e) => fail!("{}: {}", kref.path, e.describe()),
    };
    let (slide_min, slide_count) = match elf::slide_range(&layout) {
        Ok(r) => r,
        Err(e) => fail!("{}: {}", kref.path, e.describe()),
    };
    let slide = if layout.pie && cfg.kaslr {
        slide_min + rng.below(slide_count) * layout.align
    } else {
        slide_min
    };

    let probe = match Pages::new(64 * 1024, efi::LOADER_DATA, None) {
        Some(p) => p,
        None => fail!("out of memory"),
    };
    let phys_align = layout.align.clamp(PAGE, 2 << 20);
    let mut kphys = None;
    if cfg.kaslr {
        let mut sz = probe.len();
        let mut key = 0;
        let mut dsize = 0;
        let mut dver = 0;
        let s = (bs.get_memory_map)(
            &mut sz,
            probe.phys as *mut _,
            &mut key,
            &mut dsize,
            &mut dver,
        );
        if s == efi::SUCCESS && dsize >= 40 {
            let free: Vec<(u64, u64)> = regions(&probe, sz, dsize)
                .filter(|r| r.typ == memmap::EFI_CONVENTIONAL)
                .map(|r| (r.start, r.start + r.pages * PAGE))
                .collect();
            for _ in 0..4 {
                if let Some(a) =
                    kaslr::pick_physical(&free, layout.span, phys_align, 16 << 20, &mut rng)
                    && memory::alloc_pages_at(a, layout.span / PAGE, efi::LOADER_DATA)
                {
                    kphys = Some(a);
                    break;
                }
            }
        }
    }
    probe.free();
    let kphys = match kphys.or_else(|| alloc_aligned(layout.span, phys_align)) {
        Some(p) => p,
        None => fail!("out of memory for the kernel image"),
    };
    // SAFETY: we own `span` bytes at kphys (identity mapped).
    let img = unsafe { core::slice::from_raw_parts_mut(kphys as *mut u8, layout.span as usize) };
    elf::load(kbytes, &layout, img);
    if let Err(e) = elf::relocate(&layout, img, slide) {
        fail!("{}: {}", kref.path, e.describe());
    }

    // ---- What the kernel asks for ----
    let reqs = match scan::scan(img) {
        Ok(s) => s,
        Err(e) => fail!("{}: bad Limine requests: {e:?}", kref.path),
    };
    let rev = reqs.requested_revision;
    if !(lp::MIN_BASE_REVISION..=lp::MAX_BASE_REVISION).contains(&rev) {
        fail!("kernel asks for Limine base revision {rev}; {NAME} supports 3 to 6");
    }
    let field = |f: Feature, i: usize| -> Option<u64> {
        let r = reqs.get(f)?;
        let o = r.offset + 48 + 8 * i;
        Some(u64::from_le_bytes(img[o..o + 8].try_into().unwrap()))
    };
    let wants = |f: Feature| reqs.get(f).is_some();

    // Paging mode.
    let (pref, max, min) = match reqs.get(Feature::PagingMode) {
        Some(r) => {
            let m = field(Feature::PagingMode, 0).unwrap();
            if r.revision >= 1 {
                (
                    m,
                    field(Feature::PagingMode, 1).unwrap(),
                    field(Feature::PagingMode, 2).unwrap(),
                )
            } else {
                (m, m, 0)
            }
        }
        None => (0, 0, 0),
    };
    let avail = |m: u64| m == 0 || (m == 1 && feat.la57);
    let mode = if avail(pref) && pref >= min && pref <= max {
        pref
    } else {
        match (min..=max.min(1)).rev().find(|&m| avail(m)) {
            Some(m) => m,
            None => fail!("kernel needs a paging mode this CPU does not have"),
        }
    };
    let levels = if mode == 1 { 5 } else { 4 };

    let mut entry_point = layout.entry.wrapping_add(slide);
    if let Some(e) = field(Feature::EntryPoint, 0) {
        let link = e.wrapping_sub(slide);
        let ok = layout
            .segments()
            .iter()
            .any(|s| s.flags & elf::PF_X != 0 && link >= s.vaddr && link < s.vaddr + s.memsz);
        if !ok {
            fail!("kernel's entry point request is not inside its code");
        }
        entry_point = e;
    }
    let stack_size = field(Feature::StackSize, 0)
        .unwrap_or(0)
        .max(lp::DEFAULT_STACK_SIZE);
    if stack_size > lp::MAX_STACK_SIZE {
        fail!(
            "kernel asks for a {stack_size}-byte stack (limit {})",
            lp::MAX_STACK_SIZE
        );
    }
    let stack_size = stack_size.div_ceil(PAGE) * PAGE;
    let entropy_req = field(Feature::Entropy, 0);

    // ---- Firmware information ----
    let display = gop::setup(cfg.resolution);
    let rsdp =
        config_table(&efi::ACPI_20_TABLE_GUID).or_else(|| config_table(&efi::ACPI_10_TABLE_GUID));
    let smbios32 = config_table(&efi::SMBIOS_TABLE_GUID);
    let smbios64 = config_table(&efi::SMBIOS3_TABLE_GUID);
    let plat = platform::discover(rsdp);
    let tsc_hz = if wants(Feature::TscFrequency) || wants(Feature::BootloaderPerformance) {
        platform::tsc_frequency(&plat)
    } else {
        None
    };
    let date = {
        let mut t: efi::Time = unsafe { core::mem::zeroed() };
        if (runtime_services().get_time)(&mut t, ptr::null_mut()) == efi::SUCCESS {
            optimal_boot_core::time::unix_time(
                t.year,
                t.month,
                t.day,
                t.hour,
                t.minute,
                t.second,
                t.time_zone,
            )
        } else {
            None
        }
    };
    let event_log = match (&tpm, wants(Feature::TpmEventLog)) {
        (Some(t), true) => t.event_log(),
        _ => None,
    };
    if cfg.report_pcrs
        && let Some(pcrs) = tpm.as_ref().and_then(|t| t.read_pcrs(&[8, 9]))
    {
        println!("OPTIMAL-BOOT: PCR8 sha256 {}", Hex(&pcrs[0]));
        println!("OPTIMAL-BOOT: PCR9 sha256 {}", Hex(&pcrs[1]));
    }
    let keep_kernel_file = wants(Feature::ExecutableFile);
    if !keep_kernel_file {
        // Not needed any more; give the memory back.
    }

    // ---- Ranges the memory map must know about ----
    let mut overrides: Vec<Override> = Vec::new();
    let exec = lp::MEMMAP_EXECUTABLE_AND_MODULES;
    overrides.push(Override {
        start: kphys,
        end: kphys + layout.span,
        typ: exec,
    });
    if keep_kernel_file {
        overrides.push(Override {
            start: kfile.phys,
            end: kfile.phys + kfile.len() as u64,
            typ: exec,
        });
    }
    for m in &modules {
        overrides.push(Override {
            start: m.pages.phys,
            end: m.pages.phys + m.pages.len() as u64,
            typ: exec,
        });
    }
    if let Some(d) = &display {
        overrides.push(Override {
            start: d.base,
            end: d.base + d.size,
            typ: lp::MEMMAP_FRAMEBUFFER,
        });
    }
    let mapped = lp::MEMMAP_RESERVED_MAPPED;
    if rev >= 4
        && let Some(a) = rsdp.and_then(|r| acpi::Acpi::new(&Identity, r))
    {
        a.for_each_range(|r| {
            overrides.push(Override {
                start: r.start,
                end: r.start + r.len,
                typ: mapped,
            })
        });
    }
    if rev >= 5 {
        let mem = Identity;
        if let Some(e) = smbios32 {
            let len = mem.u8(e + 5).unwrap_or(31).max(31) as u64;
            overrides.push(Override {
                start: e,
                end: e + len,
                typ: mapped,
            });
            if let (Some(t), Some(l)) = (mem.u32(e + 0x18), mem.u16(e + 0x16)) {
                overrides.push(Override {
                    start: t as u64,
                    end: t as u64 + l as u64,
                    typ: mapped,
                });
            }
        }
        if let Some(e) = smbios64 {
            let len = mem.u8(e + 6).unwrap_or(24).max(24) as u64;
            overrides.push(Override {
                start: e,
                end: e + len,
                typ: mapped,
            });
            if let (Some(t), Some(l)) = (mem.u64(e + 0x10), mem.u32(e + 0x0c)) {
                overrides.push(Override {
                    start: t,
                    end: t + l as u64,
                    typ: mapped,
                });
            }
        }
        let st = system_table() as *const _ as u64;
        let sys = system_table();
        overrides.push(Override {
            start: st,
            end: st + sys.hdr.header_size as u64,
            typ: mapped,
        });
        let rt = sys.runtime_services as u64;
        overrides.push(Override {
            start: rt,
            end: rt + runtime_services().hdr.header_size as u64,
            typ: mapped,
        });
        let ct = sys.configuration_table as u64;
        overrides.push(Override {
            start: ct,
            end: ct + (sys.number_of_table_entries * 24) as u64,
            typ: mapped,
        });
        let fv = sys.firmware_vendor as u64;
        if fv != 0 {
            let mut n = 0u64;
            while n < 512 && mem.u16(fv + 2 * n).is_some_and(|c| c != 0) {
                n += 1;
            }
            overrides.push(Override {
                start: fv,
                end: fv + 2 * (n + 1),
                typ: mapped,
            });
        }
    }

    // ---- Allocations for the handoff (all before the final memory map) ----
    let stack = Pages::new(stack_size, efi::LOADER_DATA, None);
    let code_page = memory::alloc_pages(1, efi::LOADER_CODE, Some(BELOW_4G));
    let block_page = memory::alloc_pages(1, efi::LOADER_DATA, Some(BELOW_4G));
    let (Some(stack), Some(code_page), Some(block_page)) = (stack, code_page, block_page) else {
        fail!("out of memory for the handoff (kernel stack or trampoline pages)");
    };
    // The handoff page's alias: just below the kernel's lowest page (see handoff.rs).
    let code_alias = layout.base.wrapping_add(slide) - PAGE;
    let (tramp, stub64_off) = handoff::trampoline();
    assert!(tramp.len() < PAGE as usize);

    let mut map_size = 0usize;
    let mut key = 0usize;
    let mut dsize = 0usize;
    let mut dver = 0u32;
    (bs.get_memory_map)(
        &mut map_size,
        ptr::null_mut(),
        &mut key,
        &mut dsize,
        &mut dver,
    );
    if dsize < 40 {
        dsize = 48;
    }
    let desc_cap = map_size / dsize + 256;
    let entry_cap = 2 * desc_cap + 2 * overrides.len() + 16;
    let mut scratch = vec![Event::default(); memmap::scratch_needed(desc_cap, overrides.len())];
    let phys_end = {
        // A generous bound: firmware-described memory plus our overrides.
        let mut end = 4u64 << 30;
        let mut tmp = match Pages::new((desc_cap * dsize) as u64, efi::LOADER_DATA, None) {
            Some(p) => p,
            None => fail!("out of memory"),
        };
        if let Ok((sz, _, ds, _)) = memory_map(&mut tmp) {
            for r in regions(&tmp, sz, ds) {
                if lp::MEMMAP_RESERVED != memmap::efi_to_limine(r.typ, rev)
                    || r.typ == memmap::EFI_RUNTIME_SERVICES_CODE
                {
                    end = end.max(r.start.saturating_add(r.pages * PAGE));
                }
            }
        }
        tmp.free();
        for o in &overrides {
            end = end.max(o.end);
        }
        end
    };
    let hhdm = match kaslr::hhdm_offset(
        levels,
        phys_end,
        if cfg.kaslr { Some(&mut rng) } else { None },
    ) {
        Some(h) => h,
        None => fail!("too much physical memory for the chosen paging mode"),
    };
    // A proven upper bound (paging::table_frames_bound, fuzzed against the
    // real Mapper): at most entry_cap HHDM ranges, the kernel span, and the
    // three single pages mapped below (trampoline, handoff alias, block).
    let pt_frames = paging::table_frames_bound(&paging::TableBudget {
        levels,
        huge_1g: feat.huge_1g,
        hhdm_ranges: entry_cap as u64,
        phys_end,
        kernel_span: layout.span,
        single_pages: 3,
    });
    let Some(mut pt_arena) = PtArena::new(pt_frames) else {
        fail!("out of memory for the handoff (page tables: {pt_frames} pages below 4 GiB)");
    };
    let modes = display.as_ref().map_or(0, |d| d.modes.len().min(256));
    let arena_bytes = 64 * 1024
        + entry_cap as u64 * 32
        + modes as u64 * 48
        + display.as_ref().map_or(0, |d| d.edid.len() as u64)
        + event_log.as_ref().map_or(0, |l| l.len() as u64)
        + entropy_req.unwrap_or(0).min(lp::MAX_ENTROPY_VALUES) * 8
        + modules
            .iter()
            .map(|m| 256 + m.path.len() as u64 + m.string.len() as u64)
            .sum::<u64>()
        + entry.cmdline.len() as u64;
    let arena = Pages::new(arena_bytes, efi::LOADER_DATA, None);
    let mut mapbuf = Pages::new((desc_cap * dsize) as u64, efi::LOADER_DATA, None);
    let (Some(arena), Some(mapbuf_ref)) = (arena, mapbuf.as_mut()) else {
        fail!("out of memory for the handoff (memory map and response arena)");
    };
    let mut arena = Arena::new(arena.phys, arena.len() as u64);
    let entries_phys = arena.alloc(entry_cap as u64 * 24, 8).unwrap();
    // SAFETY: entry_cap entries fit in the arena block we just took.
    let entries =
        unsafe { core::slice::from_raw_parts_mut(entries_phys as *mut MemmapEntry, entry_cap) };

    let build = |pt_arena: &mut PtArena,
                 entries: &mut [MemmapEntry],
                 scratch: &mut [Event],
                 map: &Pages,
                 size: usize,
                 ds: usize|
     -> Result<Built, &'static str> {
        let n = memmap::build(regions(map, size, ds), &overrides, rev, scratch, entries).map_err(
            |e| match e {
                memmap::MemmapError::OutOfSpace => "memory map too large",
                memmap::MemmapError::FramebufferOverlap => {
                    "framebuffer shares a page with firmware tables"
                }
                memmap::MemmapError::BadRange => "firmware memory map is corrupt",
            },
        )?;
        pt_arena.used = 0;
        let mut m = Mapper::new(pt_arena, levels, feat.nx, feat.huge_1g)
            .map_err(|_| "page tables: out of frames")?;
        let mut err = None;
        memmap::hhdm_ranges(&entries[..n], rev, |r| {
            // The HHDM is data: never executable (W^X), on every CPU with NX.
            let flags = paging::Flags {
                write: true,
                exec: false,
                write_combining: r.write_combining,
            };
            if err.is_none()
                && let Err(e) = m.map(pt_arena, hhdm + r.start, r.start, r.end - r.start, flags)
            {
                err = Some(e);
            }
        });
        if err.is_some() {
            return Err("page tables: HHDM mapping failed");
        }
        let mut off = 0;
        while off < layout.span {
            if let Some(p) = elf::page_perms(&layout, layout.base + off) {
                let flags = paging::Flags {
                    write: p.write,
                    exec: p.exec,
                    write_combining: false,
                };
                m.map(
                    pt_arena,
                    layout.base.wrapping_add(slide).wrapping_add(off),
                    kphys + off,
                    PAGE,
                    flags,
                )
                .map_err(|_| "page tables: kernel mapping failed")?;
            }
            off += PAGE;
        }
        m.map(
            pt_arena,
            code_page,
            code_page,
            PAGE,
            paging::Flags {
                write: false,
                exec: true,
                write_combining: false,
            },
        )
        .map_err(|_| "page tables: trampoline mapping failed")?;
        m.map(
            pt_arena,
            code_alias,
            code_page,
            PAGE,
            paging::Flags {
                write: false,
                exec: true,
                write_combining: false,
            },
        )
        .map_err(|_| "page tables: handoff page mapping failed")?;
        m.map(
            pt_arena,
            block_page,
            block_page,
            PAGE,
            paging::Flags {
                write: true,
                exec: false,
                write_combining: false,
            },
        )
        .map_err(|_| "page tables: trampoline mapping failed")?;
        // Check the result the way the CPU will see it.
        let mut t = |v: u64| paging::translate(pt_arena, m.root, levels, feat.nx, v);
        let ok_entry = t(entry_point).is_some_and(|x| x.exec);
        let ok_stack =
            t(hhdm + stack.phys + stack_size - 8).is_some_and(|x| x.write && (!feat.nx || !x.exec));
        let ok_tramp = t(code_alias).is_some_and(|x| x.exec && !x.write && x.phys == code_page)
            && t(hhdm + code_page).is_some_and(|x| x.write && (!feat.nx || !x.exec))
            && t(hhdm + block_page).is_some_and(|x| x.write);
        if !(ok_entry && ok_stack && ok_tramp) {
            return Err("page tables failed the self-check");
        }
        Ok(Built {
            entries: n,
            root: m.root,
        })
    };

    // Dry run with boot services still up, so any problem can be reported.
    match memory_map(mapbuf_ref) {
        Ok((size, _, ds, _)) => {
            if let Err(e) = build(&mut pt_arena, entries, &mut scratch, mapbuf_ref, size, ds) {
                fail!("{e}");
            }
        }
        Err(s) => fail!("cannot read the memory map (status {s:#x})"),
    }
    println!(
        "Page tables: {} of {pt_frames} reserved frames used, in {} block(s)",
        pt_arena.used, pt_arena.count
    );
    println!(
        "Kernel: {} bytes at {:#x}, base revision {rev}, {}-level paging, KASLR {}",
        ksize,
        layout.base.wrapping_add(slide),
        levels,
        if cfg.kaslr { "on" } else { "off" }
    );
    println!("OPTIMAL-BOOT: handing over to the kernel");

    // ---- Exit boot services: no allocations or prints past this point ----
    let mut final_map = None;
    for _ in 0..16 {
        let Ok((size, key, ds, dv)) = memory_map(mapbuf_ref) else {
            continue;
        };
        if (bs.exit_boot_services)(image, key) == efi::SUCCESS {
            final_map = Some((size, ds, dv));
            break;
        }
    }
    let Some((map_size, dsize, dver)) = final_map else {
        fail!("the firmware refused ExitBootServices");
    };
    console::boot_services_exited();
    hw::cli();

    let built = match build(
        &mut pt_arena,
        entries,
        &mut scratch,
        mapbuf_ref,
        map_size,
        dsize,
    ) {
        Ok(b) => b,
        Err(e) => {
            println!("OPTIMAL-BOOT: FATAL: after leaving the firmware: {e}");
            (runtime_services().reset_system)(efi::RESET_COLD, efi::LOAD_ERROR, 0, ptr::null());
        }
    };

    // ---- Machine state ----
    platform::mask_pics(&plat);
    platform::mask_ioapics(&plat, rev);
    let keep_iommu = wants(Feature::KeepIommu);
    if rev >= 5 {
        platform::init_lapic(&plat);
        if !keep_iommu {
            platform::disable_iommus(&plat);
        }
    }

    // ---- Responses ----
    let hh = |p: u64| p + hhdm;
    let put_str = |arena: &mut Arena, s: &str| -> u64 {
        let p = arena.alloc(s.len() as u64 + 1, 8).unwrap();
        // SAFETY: p has s.len() + 1 zeroed bytes in the arena.
        unsafe { ptr::copy_nonoverlapping(s.as_ptr(), p as *mut u8, s.len()) };
        p
    };
    fn put<T>(arena: &mut Arena, v: T) -> u64 {
        let p = arena.alloc(core::mem::size_of::<T>() as u64, 8).unwrap();
        // SAFETY: p is 8-aligned with room for a T in the arena.
        unsafe { ptr::write(p as *mut T, v) };
        p
    }
    fn put_words(arena: &mut Arena, words: &[u64]) -> u64 {
        let p = arena.alloc(words.len() as u64 * 8, 8).unwrap();
        for (i, w) in words.iter().enumerate() {
            // SAFETY: inside the block just allocated.
            unsafe { ptr::write((p + 8 * i as u64) as *mut u64, *w) };
        }
        p
    }
    let mut respond = |f: Feature, resp_phys: u64| {
        if let Some(r) = reqs.get(f) {
            let o = r.offset + 40;
            img[o..o + 8].copy_from_slice(&hh(resp_phys).to_le_bytes());
        }
    };

    let cmdline = put_str(&mut arena, entry.cmdline);
    let vi = vol.info;
    let file = |address: u64, size: u64, path: u64, string: u64| lp::File {
        revision: 0,
        address: hh(address),
        size,
        path: hh(path),
        string: hh(string),
        media_type: if vi.optical {
            lp::MEDIA_TYPE_OPTICAL
        } else {
            lp::MEDIA_TYPE_GENERIC
        },
        unused: 0,
        tftp_ipv4: [0; 4],
        tftp_port: 0,
        partition_index: vi.partition_index,
        mbr_disk_id: vi.mbr_disk_id,
        // SAFETY: Uuid is 16 plain bytes in EFI GUID order, as the spec uses.
        gpt_disk_uuid: unsafe { core::mem::transmute::<[u8; 16], lp::Uuid>(vi.gpt_disk_uuid) },
        gpt_part_uuid: unsafe { core::mem::transmute::<[u8; 16], lp::Uuid>(vi.gpt_part_uuid) },
        part_uuid: lp::Uuid::default(),
    };

    if wants(Feature::BootloaderInfo) {
        let name = put_str(&mut arena, NAME);
        let ver = put_str(&mut arena, VERSION);
        let p = put_words(&mut arena, &[0, hh(name), hh(ver)]);
        respond(Feature::BootloaderInfo, p);
    }
    if wants(Feature::ExecutableCmdline) {
        let p = put_words(&mut arena, &[0, hh(cmdline)]);
        respond(Feature::ExecutableCmdline, p);
    }
    if wants(Feature::FirmwareType) {
        let p = put_words(&mut arena, &[0, lp::FIRMWARE_TYPE_EFI64]);
        respond(Feature::FirmwareType, p);
    }
    if wants(Feature::StackSize) {
        let p = put_words(&mut arena, &[0]);
        respond(Feature::StackSize, p);
    }
    if wants(Feature::Hhdm) {
        let p = put_words(&mut arena, &[0, hhdm]);
        respond(Feature::Hhdm, p);
    }
    if wants(Feature::PagingMode) {
        let p = put_words(&mut arena, &[0, mode]);
        respond(Feature::PagingMode, p);
    }
    if wants(Feature::EntryPoint) {
        let p = put_words(&mut arena, &[0]);
        respond(Feature::EntryPoint, p);
    }
    if wants(Feature::KeepIommu) {
        let p = put_words(&mut arena, &[0]);
        respond(Feature::KeepIommu, p);
    }
    if let (true, Some(d)) = (wants(Feature::Framebuffer), &display) {
        let fmt = |m: &gop::Mode| (m.format.bpp, m.format.red, m.format.green, m.format.blue);
        // No allocations after ExitBootServices: pointer arrays live in the arena.
        let mode_count = d.modes.len().min(256);
        let modes_arr = arena.alloc(mode_count as u64 * 8, 8).unwrap();
        for (i, m) in d.modes.iter().take(mode_count).enumerate() {
            let (bpp, r, g, b) = fmt(m);
            let vm = hh(put(
                &mut arena,
                lp::VideoMode {
                    pitch: m.pitch,
                    width: m.width as u64,
                    height: m.height as u64,
                    bpp,
                    memory_model: lp::FRAMEBUFFER_RGB,
                    red_mask_size: r.0,
                    red_mask_shift: r.1,
                    green_mask_size: g.0,
                    green_mask_shift: g.1,
                    blue_mask_size: b.0,
                    blue_mask_shift: b.1,
                },
            ));
            // SAFETY: inside the pointer array just allocated.
            unsafe { ptr::write((modes_arr + 8 * i as u64) as *mut u64, vm) };
        }
        let edid = if d.edid.is_empty() {
            0
        } else {
            let p = arena.alloc(d.edid.len() as u64, 8).unwrap();
            // SAFETY: room for the EDID in the arena.
            unsafe { ptr::copy_nonoverlapping(d.edid.as_ptr(), p as *mut u8, d.edid.len()) };
            hh(p)
        };
        let (bpp, r, g, b) = fmt(&d.current);
        let fb = put(
            &mut arena,
            lp::Framebuffer {
                address: hh(d.base),
                width: d.current.width as u64,
                height: d.current.height as u64,
                pitch: d.current.pitch,
                bpp,
                memory_model: lp::FRAMEBUFFER_RGB,
                red_mask_size: r.0,
                red_mask_shift: r.1,
                green_mask_size: g.0,
                green_mask_shift: g.1,
                blue_mask_size: b.0,
                blue_mask_shift: b.1,
                unused: [0; 7],
                edid_size: d.edid.len() as u64,
                edid,
                mode_count: mode_count as u64,
                modes: hh(modes_arr),
                upright_rotation: 0,
            },
        );
        let arr = put_words(&mut arena, &[hh(fb)]);
        let p = put_words(&mut arena, &[2, 1, hh(arr)]);
        respond(Feature::Framebuffer, p);
    }
    if wants(Feature::MemoryMap) {
        let n = built.entries;
        let ptrs = arena.alloc(n as u64 * 8, 8).unwrap();
        for i in 0..n {
            // SAFETY: n pointers fit in the block just allocated.
            unsafe {
                ptr::write(
                    (ptrs + 8 * i as u64) as *mut u64,
                    hh(entries_phys + 24 * i as u64),
                )
            };
        }
        let p = put_words(&mut arena, &[0, n as u64, hh(ptrs)]);
        respond(Feature::MemoryMap, p);
    }
    if wants(Feature::ExecutableFile) {
        let path = put_str(&mut arena, kref.path);
        let f = put(&mut arena, file(kfile.phys, ksize, path, cmdline));
        let p = put_words(&mut arena, &[0, hh(f)]);
        respond(Feature::ExecutableFile, p);
    }
    if wants(Feature::Module) && !modules.is_empty() {
        let arr = arena.alloc(modules.len() as u64 * 8, 8).unwrap();
        for (i, m) in modules.iter().enumerate() {
            let path = put_str(&mut arena, &m.path);
            let st = put_str(&mut arena, &m.string);
            let f = hh(put(&mut arena, file(m.pages.phys, m.size, path, st)));
            // SAFETY: inside the pointer array just allocated.
            unsafe { ptr::write((arr + 8 * i as u64) as *mut u64, f) };
        }
        let p = put_words(&mut arena, &[0, modules.len() as u64, hh(arr)]);
        respond(Feature::Module, p);
    }
    let phys_or_virt = |p: u64, virt: bool| if virt { hh(p) } else { p };
    if let (true, Some(r)) = (wants(Feature::Rsdp), rsdp) {
        let p = put_words(&mut arena, &[0, phys_or_virt(r, rev != 3)]);
        respond(Feature::Rsdp, p);
    }
    if wants(Feature::Smbios) && (smbios32.is_some() || smbios64.is_some()) {
        let v = rev >= 5;
        let p = put_words(
            &mut arena,
            &[
                0,
                smbios32.map_or(0, |a| phys_or_virt(a, v)),
                smbios64.map_or(0, |a| phys_or_virt(a, v)),
            ],
        );
        respond(Feature::Smbios, p);
    }
    if wants(Feature::EfiSystemTable) {
        let st = system_table() as *const _ as u64;
        let p = put_words(&mut arena, &[0, phys_or_virt(st, rev >= 5)]);
        respond(Feature::EfiSystemTable, p);
    }
    if let (true, Some(log)) = (wants(Feature::TpmEventLog), &event_log) {
        let a = arena.alloc(log.len() as u64, 8).unwrap();
        // SAFETY: room for the log in the arena.
        unsafe { ptr::copy_nonoverlapping(log.as_ptr(), a as *mut u8, log.len()) };
        let p = put_words(
            &mut arena,
            &[0, lp::TPM_EVENT_LOG_FORMAT_TCG_2, log.len() as u64, hh(a)],
        );
        respond(Feature::TpmEventLog, p);
    }
    if wants(Feature::EfiMemoryMap) {
        let p = put_words(
            &mut arena,
            &[
                0,
                hh(mapbuf_ref.phys),
                map_size as u64,
                dsize as u64,
                dver as u64,
            ],
        );
        respond(Feature::EfiMemoryMap, p);
    }
    if let (true, Some(t)) = (wants(Feature::DateAtBoot), date) {
        let p = put_words(&mut arena, &[0, t as u64]);
        respond(Feature::DateAtBoot, p);
    }
    if wants(Feature::ExecutableAddress) {
        let p = put_words(&mut arena, &[0, kphys, layout.base.wrapping_add(slide)]);
        respond(Feature::ExecutableAddress, p);
    }
    if let (true, Some(hz)) = (wants(Feature::TscFrequency), tsc_hz) {
        let p = put_words(&mut arena, &[0, hz]);
        respond(Feature::TscFrequency, p);
    }
    if let Some(n) = entropy_req {
        let n = n.min(lp::MAX_ENTROPY_VALUES);
        let a = arena.alloc(n * 8, 8).unwrap();
        for i in 0..n {
            // SAFETY: n values fit in the block just allocated.
            unsafe { ptr::write((a + 8 * i) as *mut u64, rng.next_u64()) };
        }
        let p = put_words(&mut arena, &[0, n, if n == 0 { 0 } else { hh(a) }]);
        respond(Feature::Entropy, p);
    }
    if let (true, Some(hz)) = (wants(Feature::BootloaderPerformance), tsc_hz) {
        let us = |t: u64| (t as u128 * 1_000_000 / hz as u128) as u64;
        let p = put_words(&mut arena, &[0, 0, us(t_init), us(hw::rdtsc())]);
        respond(Feature::BootloaderPerformance, p);
    }

    // The base revision tag: supported, and loaded with exactly this revision.
    if let Some(t) = reqs.base_revision_tag {
        img[t + 8..t + 16].copy_from_slice(&rev.to_le_bytes());
        img[t + 16..t + 24].copy_from_slice(&0u64.to_le_bytes());
    }

    // ---- Jump ----
    // SAFETY: block_page is ours, below 4 GiB, identity mapped.
    let block = unsafe { &mut *(block_page as *mut Block) };
    block.cr3 = built.root;
    block.cr4 = 1 << 5 | if levels == 5 { 1 << 12 } else { 0 };
    block.efer = 1 << 8 | if feat.nx { 1 << 11 } else { 0 };
    block.cr0 = 1 | 1 << 4 | 1 << 16 | 1 << 31;
    block.pat = paging::PAT_VALUE;
    block.gdt = handoff::GDT;
    let gdt_phys = block_page + core::mem::offset_of!(Block, gdt) as u64;
    handoff::set_gdtr(&mut block.gdtr_low, gdt_phys);
    handoff::set_gdtr(&mut block.gdtr_high, hh(gdt_phys));
    block.idtr_zero = [0; 8];
    block.stub64 = code_page + stub64_off as u64;
    block.code_delta = code_alias.wrapping_sub(code_page);
    block.hhdm = hhdm;
    block.low_entry = hh(built.root);
    block.stack_top = hh(stack.phys + stack_size);
    block.entry = entry_point;
    // SAFETY: interrupts are off; clearing CR0.WP lets us write the code page
    // even if the firmware mapped it read-only; WP is restored right after
    // (and the trampoline sets CR0 completely anyway).
    unsafe {
        let cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        core::arch::asm!("mov cr0, {}", in(reg) cr0 & !(1 << 16));
        ptr::copy_nonoverlapping(tramp.as_ptr(), code_page as *mut u8, tramp.len());
        core::arch::asm!("mov cr0, {}", in(reg) cr0);
        let enter: extern "sysv64" fn(u64) -> ! = core::mem::transmute(code_page as usize);
        enter(block_page)
    }
}
