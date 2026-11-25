//! Everything the kernel needs from the bootloader, copied into kernel-owned
//! memory first thing.
//!
//! Limine's responses live in bootloader-reclaimable memory, which the
//! kernel will hand to its allocator once it runs on its own page tables.
//! So nothing may keep pointing into them: this module copies the memory
//! map, the command line and the framebuffer description into statics, and
//! afterwards only these copies are used.

use limine::memory_map::EntryType;

use crate::boot;
use crate::sync::Once;

/// Most memory map entries accepted. Real PCs report tens; a map longer
/// than this is refused at boot with a clear message rather than truncated.
pub const MAX_MAP_ENTRIES: usize = 512;
/// Longest command line kept; a longer one is refused at boot.
pub const MAX_CMDLINE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Usable,
    Reserved,
    AcpiReclaimable,
    AcpiNvs,
    Bad,
    BootloaderReclaimable,
    KernelAndModules,
    Framebuffer,
    /// A type this kernel doesn't know: treated as reserved.
    Unknown(u64),
}

impl Kind {
    fn from_limine(t: EntryType) -> Kind {
        match t {
            EntryType::USABLE => Kind::Usable,
            EntryType::RESERVED => Kind::Reserved,
            EntryType::ACPI_RECLAIMABLE => Kind::AcpiReclaimable,
            EntryType::ACPI_NVS => Kind::AcpiNvs,
            EntryType::BAD_MEMORY => Kind::Bad,
            EntryType::BOOTLOADER_RECLAIMABLE => Kind::BootloaderReclaimable,
            EntryType::EXECUTABLE_AND_MODULES => Kind::KernelAndModules,
            EntryType::FRAMEBUFFER => Kind::Framebuffer,
            // SAFETY: EntryType is a transparent wrapper around the raw u64.
            other => Kind::Unknown(unsafe { core::mem::transmute::<EntryType, u64>(other) }),
        }
    }

    /// Is this RAM the kernel may map in its direct map (as opposed to MMIO
    /// or holes)?
    pub fn is_ram(self) -> bool {
        matches!(
            self,
            Kind::Usable
                | Kind::AcpiReclaimable
                | Kind::AcpiNvs
                | Kind::BootloaderReclaimable
                | Kind::KernelAndModules
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MapEntry {
    pub base: u64,
    pub len: u64,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    /// Physical address (from the HHDM address Limine gave).
    pub phys: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
}

pub struct BootInfo {
    map: [MapEntry; MAX_MAP_ENTRIES],
    map_len: usize,
    /// Offset of Limine's higher-half direct map. Only valid until the
    /// kernel switches to its own page tables.
    pub hhdm: u64,
    pub kernel_phys: u64,
    pub kernel_virt: u64,
    cmdline: [u8; MAX_CMDLINE],
    cmdline_len: usize,
    pub framebuffer: Option<Framebuffer>,
}

impl BootInfo {
    pub fn memory_map(&self) -> &[MapEntry] {
        &self.map[..self.map_len]
    }

    pub fn cmdline(&self) -> &str {
        // Checked to be UTF-8 when captured.
        core::str::from_utf8(&self.cmdline[..self.cmdline_len]).unwrap_or("")
    }
}

static INFO: Once<BootInfo> = Once::new();

/// The captured boot information. Panics if called before `capture`.
pub fn get() -> &'static BootInfo {
    INFO.get().expect("boot info read before it was captured")
}

/// Copy everything out of Limine's responses. Call once, first thing after
/// the serial port is up. Panics with a clear reason if a required response
/// is missing or something doesn't fit.
pub fn capture() -> &'static BootInfo {
    let map = boot::MEMORY_MAP
        .get_response()
        .expect("bootloader gave no memory map");
    let hhdm = boot::HHDM
        .get_response()
        .expect("bootloader gave no direct-map offset")
        .offset();
    let exe = boot::EXECUTABLE_ADDRESS
        .get_response()
        .expect("bootloader gave no kernel address");
    if let Some(mode) = boot::PAGING_MODE.get_response() {
        assert!(
            mode.mode() == limine::paging::Mode::FOUR_LEVEL,
            "bootloader enabled a paging mode other than 4-level"
        );
    }

    let entries = map.entries();
    assert!(
        entries.len() <= MAX_MAP_ENTRIES,
        "memory map has {} entries, more than the {} supported",
        entries.len(),
        MAX_MAP_ENTRIES
    );
    let mut info = BootInfo {
        map: [MapEntry {
            base: 0,
            len: 0,
            kind: Kind::Reserved,
        }; MAX_MAP_ENTRIES],
        map_len: entries.len(),
        hhdm,
        kernel_phys: exe.physical_base(),
        kernel_virt: exe.virtual_base(),
        cmdline: [0; MAX_CMDLINE],
        cmdline_len: 0,
        framebuffer: None,
    };
    for (slot, e) in info.map.iter_mut().zip(entries) {
        *slot = MapEntry {
            base: e.base,
            len: e.length,
            kind: Kind::from_limine(e.entry_type),
        };
    }

    let cmdline = boot::cmdline().as_bytes();
    assert!(
        cmdline.len() <= MAX_CMDLINE,
        "kernel command line is {} bytes, more than the {} supported",
        cmdline.len(),
        MAX_CMDLINE
    );
    info.cmdline[..cmdline.len()].copy_from_slice(cmdline);
    info.cmdline_len = cmdline.len();

    if let Some(fb) = boot::FRAMEBUFFER
        .get_response()
        .and_then(|r| r.framebuffers().next())
    {
        info.framebuffer = Some(Framebuffer {
            phys: (fb.addr() as u64).wrapping_sub(hhdm),
            width: fb.width(),
            height: fb.height(),
            pitch: fb.pitch(),
            bpp: fb.bpp(),
        });
    }
    INFO.set(info)
}
