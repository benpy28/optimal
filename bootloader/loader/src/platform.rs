//! Putting interrupt controllers and IOMMUs into the state the Limine
//! protocol promises at entry, and measuring the TSC frequency.
//! Runs after ExitBootServices, with the firmware's identity mapping.

use crate::hw::{self, mmio_read32, mmio_read64, mmio_write32, mmio_write64};
use optimal_boot_core::acpi::{self, Madt, PhysRead};

/// Physical memory reader over the firmware's identity mapping.
pub struct Identity;

impl PhysRead for Identity {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
        if phys == 0
            || phys
                .checked_add(buf.len() as u64)
                .is_none_or(|e| e > 1 << 52)
        {
            return false;
        }
        // SAFETY: firmware identity-maps all memory it reports, which is where
        // ACPI tables live. Reads are byte-wise and unaligned-safe.
        unsafe { core::ptr::copy_nonoverlapping(phys as *const u8, buf.as_mut_ptr(), buf.len()) };
        true
    }
}

/// Hardware the handoff needs to touch, found while boot services run.
#[derive(Default)]
pub struct Platform {
    pub madt: Option<Madt>,
    pub iommus_vtd: [u64; acpi::MAX_IOMMUS],
    pub vtd_count: usize,
    pub iommus_amd: [u64; acpi::MAX_IOMMUS],
    pub amd_count: usize,
    pub pm_timer: Option<(u16, bool)>,
}

pub fn discover(rsdp: Option<u64>) -> Platform {
    let mut p = Platform::default();
    let Some(rsdp) = rsdp else { return p };
    let mem = Identity;
    let Some(a) = acpi::Acpi::new(&mem, rsdp) else {
        return p;
    };
    if let Some((addr, len)) = a.find(b"APIC") {
        p.madt = Madt::parse(&mem, addr, len);
    }
    if let Some((addr, len)) = a.find(b"DMAR") {
        p.vtd_count = acpi::dmar_units(&mem, addr, len, &mut p.iommus_vtd);
    }
    if let Some((addr, len)) = a.find(b"IVRS") {
        p.amd_count = acpi::ivrs_units(&mem, addr, len, &mut p.iommus_amd);
    }
    if let Some((addr, len)) = a.find(b"FACP") {
        // PM_TMR_BLK (offset 76, 32-bit port), flags bit 8 = 32-bit timer,
        // X_PM_TMR_BLK GAS at offset 208 (space id 1 = system I/O).
        let flags = mem.u32(addr + 112).unwrap_or(0);
        let wide = flags & (1 << 8) != 0;
        let mut port = mem.u32(addr + 76).unwrap_or(0) as u64;
        if len >= 220
            && mem.u8(addr + 208) == Some(1)
            && let Some(x) = mem.u64(addr + 212).filter(|&x| x != 0)
        {
            port = x;
        }
        if port != 0 && port <= 0xffff {
            p.pm_timer = Some((port as u16, wide));
        }
    }
    p
}

/// Masks every IRQ on the legacy PICs.
pub fn mask_pics(p: &Platform) {
    if p.madt.as_ref().is_none_or(|m| m.pcat_compat) {
        hw::outb(0x21, 0xff);
        hw::outb(0xa1, 0xff);
    }
}

/// Masks I/O APIC redirection entries: Fixed and Lowest Priority always,
/// and from base revision 5 also NMI and ExtINT. Everything else about each
/// entry is left as the firmware set it.
pub fn mask_ioapics(p: &Platform, revision: u64) {
    let Some(m) = &p.madt else { return };
    for &base in m.ioapics() {
        // SAFETY: the MADT says an I/O APIC lives at `base`; the firmware
        // identity-maps it. IOREGSEL at +0, IOWIN at +0x10.
        unsafe {
            mmio_write32(base, 1);
            let max = (mmio_read32(base + 0x10) >> 16) & 0xff;
            for i in 0..=max {
                mmio_write32(base, 0x10 + 2 * i);
                let lo = mmio_read32(base + 0x10);
                let mode = (lo >> 8) & 7;
                let mask = matches!(mode, 0 | 1) || (revision >= 5 && matches!(mode, 4 | 7));
                if mask {
                    mmio_write32(base + 0x10, lo | 1 << 16);
                }
            }
        }
    }
}

const IA32_APIC_BASE: u32 = 0x1b;

struct Lapic {
    x2: bool,
    base: u64,
}

impl Lapic {
    fn read(&self, reg: u32) -> u32 {
        if self.x2 {
            // SAFETY: x2APIC mode is enabled, so its MSRs exist.
            unsafe { hw::rdmsr(0x800 + (reg >> 4)) as u32 }
        } else {
            // SAFETY: the xAPIC page is identity mapped by the firmware.
            unsafe { mmio_read32(self.base + reg as u64) }
        }
    }
    fn write(&self, reg: u32, v: u32) {
        if self.x2 {
            // SAFETY: as read().
            unsafe { hw::wrmsr(0x800 + (reg >> 4), v as u64) }
        } else {
            // SAFETY: as read().
            unsafe { mmio_write32(self.base + reg as u64, v) }
        }
    }
}

/// Base revision 5 local APIC state: enabled, SVR = 0x1FF, TPR = 0, LVT
/// entries masked, and LINT pins wired as NMIs where the MADT says so.
pub fn init_lapic(p: &Platform) {
    // CPUID.1:EDX.APIC mirrors IA32_APIC_BASE.EN, so firmware that left the
    // APIC globally disabled makes CPUID say there is none. A MADT means
    // the hardware is there (and IA32_APIC_BASE with it), so trust either.
    if !hw::features().apic && p.madt.is_none() {
        return;
    }
    // SAFETY: IA32_APIC_BASE exists on every CPU with an APIC.
    let mut apic_base = unsafe { hw::rdmsr(IA32_APIC_BASE) };
    if apic_base & (1 << 11) == 0 {
        apic_base |= 1 << 11;
        // SAFETY: setting the global enable bit is always valid.
        unsafe { hw::wrmsr(IA32_APIC_BASE, apic_base) };
        // Some CPUs (P6-era, and QEMU's emulated APIC) cannot re-enable a
        // globally disabled APIC before a reset. Then it is not available,
        // and its registers must not be touched.
        // SAFETY: as above.
        apic_base = unsafe { hw::rdmsr(IA32_APIC_BASE) };
        if apic_base & (1 << 11) == 0 {
            return;
        }
    }
    let lapic = Lapic {
        x2: apic_base & (1 << 10) != 0,
        base: apic_base & 0x000f_ffff_ffff_f000,
    };
    lapic.write(0xf0, 0x1ff);
    lapic.write(0x80, 0);
    let max_lvt = (lapic.read(0x30) >> 16) & 0xff;
    let maskable = |v: u32| matches!((v >> 8) & 7, 0 | 1 | 4 | 7);
    let mut lvts: [(u32, bool); 7] = [
        (0x320, true),         // timer
        (0x330, max_lvt >= 5), // thermal
        (0x340, max_lvt >= 4), // performance counter
        (0x350, true),         // LINT0
        (0x360, true),         // LINT1
        (0x370, true),         // error
        (0x2f0, max_lvt >= 6), // CMCI
    ];
    for (reg, present) in lvts.iter_mut() {
        if *present {
            let v = lapic.read(*reg);
            if maskable(v) {
                lapic.write(*reg, v | 1 << 16);
            }
        }
    }
    // LINT NMI wiring from the MADT, for this processor.
    let Some(m) = &p.madt else { return };
    let apic_id = if lapic.x2 {
        lapic.read(0x20)
    } else {
        lapic.read(0x20) >> 24
    };
    let mem = Identity;
    let uid = m.uid_for_apic(&mem, apic_id);
    for nmi in m.nmis() {
        if nmi.uid != u32::MAX && Some(nmi.uid) != uid {
            continue;
        }
        let reg = match nmi.lint {
            0 => 0x350,
            1 => 0x360,
            _ => continue,
        };
        let v = lapic.read(reg);
        if !maskable(v) {
            continue;
        }
        // MPS INTI flags: polarity bits 0-1 (3 = active low), trigger bits
        // 2-3 (3 = level). "Conforms" (0) means the bus default: high, edge.
        let low = nmi.flags & 3 == 3;
        let level = (nmi.flags >> 2) & 3 == 3;
        let mut nv = (v & !(0x7 << 8 | 1 << 13 | 1 << 15)) | 4 << 8 | 1 << 16;
        if low {
            nv |= 1 << 13;
        }
        if level {
            nv |= 1 << 15;
        }
        lapic.write(reg, nv);
    }
}

/// Turns off DMA translation and interrupt remapping on every VT-d unit and
/// disables every AMD-Vi IOMMU (base revision 5, unless the kernel asked to
/// keep them). Waits for each unit to confirm, with a bounded wait.
pub fn disable_iommus(p: &Platform) {
    const GCMD: u64 = 0x18;
    const GSTS: u64 = 0x1c;
    const TE: u32 = 1 << 31;
    const IRE: u32 = 1 << 25;
    // One-shot command bits that must not be echoed back from GSTS.
    const ONE_SHOT: u32 = 1 << 30 | 1 << 29 | 1 << 27 | 1 << 24;
    for &base in &p.iommus_vtd[..p.vtd_count] {
        for bit in [TE, IRE] {
            // SAFETY: the DMAR lists a remapping unit at `base`; its register
            // page is identity mapped by the firmware.
            unsafe {
                let sts = mmio_read32(base + GSTS);
                if sts & bit == 0 {
                    continue;
                }
                mmio_write32(base + GCMD, (sts & !ONE_SHOT) & !bit);
                for _ in 0..1_000_000 {
                    if mmio_read32(base + GSTS) & bit == 0 {
                        break;
                    }
                    core::hint::spin_loop();
                }
            }
        }
    }
    for &base in &p.iommus_amd[..p.amd_count] {
        // SAFETY: the IVRS lists an IOMMU at `base`; control register at 0x18.
        unsafe {
            let ctrl = mmio_read64(base + 0x18);
            if ctrl & 1 != 0 {
                mmio_write64(base + 0x18, ctrl & !1);
            }
        }
    }
}

/// TSC frequency in Hz: from CPUID leaf 0x15 when the crystal is reported,
/// from the hypervisor timing leaf, or measured against the ACPI PM timer.
/// Runs while boot services are active (falls back to Stall()).
pub fn tsc_frequency(p: &Platform) -> Option<u64> {
    if hw::max_leaf() >= 0x15 {
        let (den, num, crystal, _) = hw::cpuid(0x15, 0);
        if den != 0 && num != 0 && crystal != 0 {
            return Some(crystal as u64 * num as u64 / den as u64);
        }
    }
    let (_, _, c1, _) = hw::cpuid(1, 0);
    if c1 & (1 << 31) != 0 {
        let (max_hv, ..) = hw::cpuid(0x4000_0000, 0);
        if max_hv >= 0x4000_0010 {
            let (khz, ..) = hw::cpuid(0x4000_0010, 0);
            if khz != 0 {
                return Some(khz as u64 * 1000);
            }
        }
    }
    if let Some((port, wide)) = p.pm_timer {
        let mask: u64 = if wide { 0xffff_ffff } else { 0xff_ffff };
        // 3.579545 MHz; measure ~50 ms (179 000 ticks), twice, keep the closer pair.
        let mut best: Option<u64> = None;
        for _ in 0..2 {
            let start_pm = hw::inl(port) as u64 & mask;
            let start_tsc = hw::rdtsc();
            let mut now;
            let mut guard = 0u64;
            loop {
                now = hw::inl(port) as u64 & mask;
                let elapsed = now.wrapping_sub(start_pm) & mask;
                if elapsed >= 179_000 {
                    let tsc = hw::rdtsc() - start_tsc;
                    let f = tsc * 3_579_545 / elapsed;
                    best = Some(match best {
                        Some(b) => b.min(f).max(b.min(f)),
                        None => f,
                    });
                    break;
                }
                guard += 1;
                if guard > 500_000_000 {
                    break;
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    if crate::console::boot_services_active() {
        let bs = crate::console::boot_services();
        let t0 = hw::rdtsc();
        (bs.stall)(50_000);
        let t1 = hw::rdtsc();
        return Some((t1 - t0) * 20);
    }
    None
}
