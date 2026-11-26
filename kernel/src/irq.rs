//! Interrupt controllers: the boot CPU's local APIC, the I/O APICs and the
//! legacy 8259 PICs, set up from the MADT, plus hardware interrupt
//! dispatch.
//!
//! Setup order (interrupts stay off until the end):
//! 1. Record what firmware left behind, for the boot log (tests check it).
//! 2. Put the local APIC in the chosen mode (optimal_apic::base): x2APIC
//!    when the CPU has it, unless firmware opted out.
//! 3. Set the local APIC up (optimal_apic::lapic::setup): every LVT
//!    masked except error and the LINT pins the MADT marks NMI, then
//!    retire anything firmware left in service.
//! 4. Remap the 8259s onto 0x20 to 0x2F and mask them (LINT0 is already
//!    masked, so nothing they do can arrive), if there are any.
//! 5. Program every pin of every usable I/O APIC from the routing plan:
//!    ISA IRQs masked on their vectors, NMI sources live, the rest masked.
//! 6. Turn interrupts on. Only NMIs, local APIC errors and what firmware
//!    left pending can arrive until a driver unmasks its line.
//!
//! Anything wrong is reported as a warning and worked around, never
//! guessed: see optimal_apic::plan for the routing rules.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::__cpuid_count;
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use optimal_acpi::dmar::Dmar;
use optimal_acpi::madt::Madt;
use optimal_apic::base::{self, Mode, Reason};
use optimal_apic::lapic::{self, Delivery, Lvt};
use optimal_apic::plan::{Inputs, IoApicStatus, NotRouted, Plan, Warning};
use optimal_apic::{ioapic, pic, vectors};
use optimal_mm::PAGE_SIZE;

use crate::arch::apic::{self, IoApicRegs, Lapic};
use crate::arch::cpu;
use crate::arch::idt::InterruptFrame;
use crate::bootinfo::BootInfo;
use crate::mm::paging::{self, MmioError};
use crate::sync::{IrqSpinLock, Once};

/// What firmware left, as found before the kernel touched anything.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub mode: Option<Mode>,
    /// Software enable bit in the SVR (if the APIC was readable).
    pub software_enabled: Option<bool>,
    pub tpr: Option<u32>,
    pub lvts_unmasked: Option<u32>,
    pub in_service: Option<u32>,
    pub pic_masks: Option<(u8, u8)>,
    /// (unmasked pins, pins) over every reachable I/O APIC.
    pub ioapic_unmasked: (u32, u32),
}

pub struct Controllers {
    pub lapic: Lapic,
    pub mode: Mode,
    pub reason: Reason,
    pub lapic_id: u32,
    pub version: lapic::Version,
    pub plan: Plan,
    /// Register blocks, by MADT index (None where not mapped).
    #[cfg_attr(
        not(feature = "ktest"),
        expect(dead_code, reason = "read by isa_mask/isa_unmask, first used in 1c3")
    )]
    pub ioapics: Vec<Option<IoApicRegs>>,
    pub ioapic_versions: Vec<Option<ioapic::Version>>,
    pub pic: bool,
    pub warnings: Vec<String>,
}

pub enum Setup {
    On(alloc::boxed::Box<Controllers>),
    /// Interrupts stay off; the reason, and what was still made safe.
    Off {
        why: String,
        warnings: Vec<String>,
    },
}

pub struct Report {
    pub found: Found,
    pub setup: Setup,
}

static STATE: Once<Report> = Once::new();
/// Serialises IOREGSEL/IOWIN pairs on every I/O APIC.
static IOAPIC_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

/// The result of [`init`]. Panics before it.
pub fn controllers() -> Option<&'static Controllers> {
    match &STATE.get()?.setup {
        Setup::On(c) => Some(c),
        Setup::Off { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Dispatch.

/// Handlers by vector: a `fn(&mut InterruptFrame)` as usize, 0 for none.
static HANDLERS: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
/// Interrupts seen, by vector.
static COUNTS: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
/// Last local APIC error status seen by the error handler.
pub static LAST_ESR: AtomicU32 = AtomicU32::new(0);

pub type Handler = fn(&mut InterruptFrame);

/// Route `vector` to `handler`. The vector must be one the kernel's map
/// gives to devices or tests (not exceptions, PIC, spurious or error).
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the timer (1c3)")
)]
pub fn set_handler(vector: u8, handler: Handler) {
    assert!(
        (vectors::ISA_BASE..vectors::UNROUTED).contains(&vector),
        "vector {vector:#x} is not a device vector"
    );
    HANDLERS[usize::from(vector)].store(handler as usize, Ordering::SeqCst);
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the timer (1c3)")
)]
pub fn clear_handler(vector: u8) {
    HANDLERS[usize::from(vector)].store(0, Ordering::SeqCst);
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn count(vector: u8) -> u64 {
    COUNTS[usize::from(vector)].load(Ordering::SeqCst)
}

/// Every hardware or software interrupt on vectors 32 to 255.
pub fn dispatch(frame: &mut InterruptFrame) {
    let v = frame.vector as u8;
    COUNTS[usize::from(v)].fetch_add(1, Ordering::SeqCst);
    let lapic = controllers().map(|c| c.lapic);
    match v {
        // A spurious interrupt is never in service: no EOI (SDM 11.9).
        vectors::SPURIOUS => {}
        // Only the local APIC timer raises it (fixed delivery), so it is
        // in service.
        vectors::LAPIC_TIMER => {
            crate::timer::interrupt();
            if let Some(l) = lapic {
                l.eoi();
            }
        }
        vectors::LAPIC_ERROR => {
            if let Some(l) = lapic {
                LAST_ESR.store(l.esr(), Ordering::SeqCst);
                l.eoi();
            }
        }
        _ if (vectors::PIC_MASTER..vectors::PIC_SLAVE + 8).contains(&v) => {
            // Every 8259 line is masked, so this is a spurious IRQ 7 or
            // 15 or the 8259 misbehaving. A real (in-service) one needs
            // the 8259's own EOI; a spurious one must not get it, except
            // that a spurious IRQ 15 still needs the master's, for the
            // cascade line (8259A datasheet).
            let irq = v - vectors::PIC_MASTER;
            let (m, s) = apic::pic_in_service();
            if irq < 8 {
                if m & (1 << irq) != 0 {
                    apic::pic_eoi(false);
                }
            } else if s & (1 << (irq - 8)) != 0 {
                apic::pic_eoi(true);
            } else {
                apic::pic_eoi(false);
            }
            eoi_if_in_service(lapic, v);
        }
        _ => {
            let h = HANDLERS[usize::from(v)].load(Ordering::SeqCst);
            if h != 0 {
                // SAFETY: only set_handler stores here, from a Handler.
                let f: Handler = unsafe { core::mem::transmute::<usize, Handler>(h) };
                f(frame);
                // A registered vector only ever comes from the local APIC
                // (fixed delivery), so it is in service.
                if let Some(l) = lapic {
                    l.eoi();
                }
            } else {
                eoi_if_in_service(lapic, v);
            }
        }
    }
}

/// EOI only if `vector` is really in service. A software `int n` or an
/// ExtINT delivery is not, and an EOI then would retire some other
/// interrupt that is.
fn eoi_if_in_service(lapic: Option<Lapic>, vector: u8) {
    if let Some(l) = lapic {
        let (i, b) = optimal_apic::vector_bit(vector);
        if l.read(lapic::ISR + 0x10 * i as u16) & (1 << b) != 0 {
            l.eoi();
        }
    }
}

// ---------------------------------------------------------------------------
// ISA lines.

/// Unmask ISA IRQ `irq` on its routed pin. Err if the plan did not route it.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the timer (1c3)")
)]
pub fn isa_unmask(irq: u8) -> Result<(), NotRouted> {
    set_isa_mask(irq, false)
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the timer (1c3)")
)]
pub fn isa_mask(irq: u8) -> Result<(), NotRouted> {
    set_isa_mask(irq, true)
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "only isa_mask and isa_unmask call it")
)]
fn set_isa_mask(irq: u8, masked: bool) -> Result<(), NotRouted> {
    let Some(c) = controllers() else {
        return Err(NotRouted::NoIoApic {
            gsi: u32::from(irq),
        });
    };
    let r = c
        .plan
        .isa
        .get(usize::from(irq))
        .copied()
        .unwrap_or(Err(NotRouted::Cascade))?;
    let regs = c.ioapics[r.ioapic].expect("a used I/O APIC is mapped");
    let entry = ioapic::Redirection { masked, ..r.entry };
    let _g = IOAPIC_LOCK.lock();
    for (reg, v) in entry.writes(r.pin).expect("plan pins are below MAX_PINS") {
        regs.write(reg, v);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Setup.

struct CpuApic {
    present: bool,
    x2apic: bool,
    maxphyaddr: u8,
}

fn cpu_apic() -> CpuApic {
    let l1 = __cpuid_count(1, 0);
    let max_ext = __cpuid_count(0x8000_0000, 0).eax;
    CpuApic {
        present: l1.edx & (1 << 9) != 0,
        x2apic: l1.ecx & (1 << 21) != 0,
        // Without leaf 0x80000008, 36 bits (SDM 4.1.4).
        maxphyaddr: if max_ext >= 0x8000_0008 {
            __cpuid_count(0x8000_0008, 0).eax as u8
        } else {
            36
        },
    }
}

/// Maps one page of device registers, or says why not.
fn map_regs(info: &BootInfo, phys: u64) -> Result<u64, String> {
    let page = phys & !(PAGE_SIZE - 1);
    paging::map_mmio(info, page, 1)
        .map(|v| v + (phys - page))
        .map_err(|e| match e {
            MmioError::NotReserved { phys, kind } => {
                format!("{phys:#x} is {kind:?} memory, not device registers; refused")
            }
            e => format!("{phys:#x} could not be mapped: {e:?}"),
        })
}

fn lvts_unmasked(l: Lapic, version: lapic::Version) -> u32 {
    [
        lapic::LVT_TIMER,
        lapic::LVT_THERMAL,
        lapic::LVT_PERF,
        lapic::LVT_LINT0,
        lapic::LVT_LINT1,
        lapic::LVT_ERROR,
        lapic::LVT_CMCI,
    ]
    .into_iter()
    .filter(|&r| version.has_lvt(r) && !Lvt::decode(l.read(r)).masked)
    .count() as u32
}

fn in_service(l: Lapic) -> u32 {
    l.read_array(lapic::ISR)
        .iter()
        .map(|r| r.count_ones())
        .sum()
}

/// Set up every interrupt controller and turn interrupts on. Once, on the
/// boot CPU, after ACPI and the MMIO mapper are up.
pub fn init(info: &BootInfo) -> &'static Report {
    let r = setup(info);
    let on = matches!(r.setup, Setup::On(_));
    let r = STATE.set(r);
    if on {
        cpu::interrupts_enable();
    }
    r
}

fn setup(info: &BootInfo) -> Report {
    let mut found = Found::default();
    let mut warnings = Vec::new();
    let madt: Option<&Madt> = match crate::acpi::get() {
        Ok(a) => a.madt.as_ref(),
        Err(_) => None,
    };
    let opt_out = match crate::acpi::get() {
        Ok(a) => match a.table(b"DMAR").map(|t| Dmar::parse(&t.bytes)) {
            Some(Ok(d)) => d.x2apic_opt_out,
            Some(Err(e)) => {
                warnings.push(format!("DMAR table unusable: {e:?}"));
                false
            }
            None => false,
        },
        Err(_) => false,
    };
    let c = cpu_apic();

    // 1. What firmware left. IA32_APIC_BASE is read even when CPUID says
    // there is no APIC: CPUID.1:EDX.APIC mirrors the MSR's EN bit (SDM
    // 11.4.3), so firmware that globally disabled the local APIC hides it
    // from CPUID. Every x86_64 CPU has the MSR (it is architectural from
    // the P6 family on, and every AMD64 CPU has it).
    found.pic_masks = Some(apic::pic_masks());
    let raw = cpu::read_msr(base::MSR);
    let mode = Mode::of(raw);
    found.mode = Some(mode);
    let old = match mode {
        Mode::X2Apic => Some(Lapic::X2),
        Mode::XApic => map_regs(info, base::address(raw, c.maxphyaddr))
            .ok()
            .map(|regs| Lapic::X { regs }),
        _ => None,
    };
    if let Some(l) = old {
        let v = lapic::Version::decode(l.read(lapic::VERSION));
        found.software_enabled = Some(lapic::svr_enabled(l.read(lapic::SVR)));
        found.tpr = Some(l.read(lapic::TPR) & 0xFF);
        found.lvts_unmasked = Some(lvts_unmasked(l, v));
        found.in_service = Some(in_service(l));
    }
    // The I/O APICs, mapped once, read now and programmed below.
    let mut ioapics: Vec<Option<IoApicRegs>> = Vec::new();
    let mut versions: Vec<Option<ioapic::Version>> = Vec::new();
    for (i, io) in madt.map_or(&[][..], |m| &m.ioapics[..]).iter().enumerate() {
        let regs = if ioapic::address_ok(u64::from(io.address)) {
            match map_regs(info, u64::from(io.address)) {
                Ok(v) => Some(IoApicRegs { regs: v }),
                Err(e) => {
                    warnings.push(format!("I/O APIC {i}: {e}"));
                    None
                }
            }
        } else {
            None
        };
        let ver = regs.map(|r| ioapic::Version::decode(r.read(ioapic::REG_VERSION)));
        if let (Some(r), Some(v)) = (regs, ver) {
            if !ioapic::id_matches(r.read(ioapic::REG_ID), io.id) {
                warnings.push(format!(
                    "I/O APIC {i}: ID register says {}, MADT says {}",
                    (r.read(ioapic::REG_ID) >> 24) & 0xF,
                    io.id
                ));
            }
            let pins = v.pins().min(ioapic::MAX_PINS);
            for pin in 0..pins as u8 {
                let (lo, hi) = r.entry(pin);
                if !ioapic::Redirection::decode(lo, hi).entry.masked {
                    found.ioapic_unmasked.0 += 1;
                }
            }
            found.ioapic_unmasked.1 += u32::from(pins);
        }
        ioapics.push(regs);
        versions.push(ver);
    }

    // A globally disabled local APIC is switched back on (to xAPIC, its
    // reset state) before anything else, so CPUID tells the truth again.
    let mut raw = raw;
    let mut c = c;
    if matches!(mode, Mode::Disabled | Mode::Invalid) {
        let t = base::transition(raw, Mode::XApic).expect("XApic is a valid target");
        for &w in t.writes() {
            // SAFETY: legal IA32_APIC_BASE transitions (optimal_apic::base)
            // that keep every other bit as read.
            unsafe { cpu::write_msr(base::MSR, w) };
        }
        raw = cpu::read_msr(base::MSR);
        c = cpu_apic();
        if Mode::of(raw) == Mode::XApic {
            warnings.push(format!(
                "firmware left the local APIC {}; switched it back on",
                mode.name()
            ));
        }
    }
    if !c.present || Mode::of(raw) != Mode::XApic && Mode::of(raw) != Mode::X2Apic {
        // No local APIC: nothing can deliver an I/O APIC interrupt, so
        // everything is masked and interrupts stay off.
        mask_everything(&ioapics, &versions);
        return Report {
            found,
            setup: Setup::Off {
                why: if c.present {
                    format!("the local APIC is in no usable mode (IA32_APIC_BASE {raw:#x})")
                } else if base::address(raw, c.maxphyaddr) == 0 {
                    format!("the CPU has no local APIC (IA32_APIC_BASE {raw:#x})")
                } else if Mode::of(raw) == Mode::Disabled {
                    // Some CPUs (and QEMU's emulated APIC) only re-enable
                    // a globally disabled local APIC on reset.
                    format!(
                        "the local APIC is disabled and would not switch back on (IA32_APIC_BASE {raw:#x})"
                    )
                } else {
                    format!("the CPU reports no local APIC (IA32_APIC_BASE {raw:#x})")
                },
                warnings,
            },
        };
    }

    // 2. Mode.
    let (mode, reason) = base::choose(Mode::of(raw), c.x2apic, opt_out);
    let t = base::transition(raw, mode).expect("choose never picks Invalid");
    for &w in t.writes() {
        // SAFETY: each write is one legal IA32_APIC_BASE transition
        // (optimal_apic::base), keeping every other bit as read.
        unsafe { cpu::write_msr(base::MSR, w) };
    }
    let now = cpu::read_msr(base::MSR);
    if Mode::of(now) != mode {
        mask_everything(&ioapics, &versions);
        return Report {
            found,
            setup: Setup::Off {
                why: format!(
                    "the local APIC did not switch to {} (IA32_APIC_BASE {now:#x})",
                    mode.name()
                ),
                warnings,
            },
        };
    }
    let lapic = match mode {
        Mode::X2Apic => Lapic::X2,
        _ => {
            let phys = base::address(now, c.maxphyaddr);
            if let Some(m) = madt
                && m.lapic_address != phys
            {
                warnings.push(format!(
                    "MADT says the local APIC is at {:#x}, but it is at {phys:#x}; using {phys:#x}",
                    m.lapic_address
                ));
            }
            match map_regs(info, phys) {
                Ok(regs) => Lapic::X { regs },
                Err(e) => {
                    mask_everything(&ioapics, &versions);
                    return Report {
                        found,
                        setup: Setup::Off {
                            why: format!("local APIC registers at {e}"),
                            warnings,
                        },
                    };
                }
            }
        }
    };
    let lapic_id = lapic.id();
    let version = lapic::Version::decode(lapic.read(lapic::VERSION));

    // The plan.
    let none = empty_madt();
    let plan_madt = madt.unwrap_or(&none);
    let max_entries: Vec<Option<u8>> = versions.iter().map(|v| v.map(|v| v.max_entry)).collect();
    let plan = Plan::build(Inputs {
        madt: plan_madt,
        bsp_apic_id: lapic_id,
        ioapic_max_entry: &max_entries,
    });
    if madt.is_none() {
        warnings.push(String::from(
            "no MADT: no I/O APIC routes, LINT pins masked",
        ));
    }

    // 3. Local APIC.
    for (reg, v) in lapic::setup(version, plan.lint[0], plan.lint[1]) {
        lapic.write(reg, v);
    }
    let mut stale = 0;
    while stale < 256 && lapic.read_array(lapic::ISR).iter().any(|&r| r != 0) {
        lapic.eoi();
        stale += 1;
    }
    if stale > 0 {
        warnings.push(format!(
            "firmware left {stale} interrupts in service; retired"
        ));
    }

    // 4. PICs.
    let has_pic = apic::pic_probe();
    if has_pic {
        let seq = pic::remap_and_mask(vectors::PIC_MASTER, vectors::PIC_SLAVE)
            .expect("the vector map's PIC bases are valid");
        apic::pic_write(&seq);
        if apic::pic_masks() != (pic::ALL_MASKED, pic::ALL_MASKED) {
            warnings.push(format!(
                "8259 masks read back {:#04x}/{:#04x} after masking",
                apic::pic_masks().0,
                apic::pic_masks().1
            ));
        }
    }
    if let Some(m) = madt
        && m.pcat_compat != has_pic
    {
        warnings.push(format!(
            "MADT PCAT_COMPAT is {}, but 8259s {}",
            if m.pcat_compat { "set" } else { "clear" },
            if has_pic {
                "answered"
            } else {
                "did not answer"
            }
        ));
    }

    // 5. I/O APICs.
    for (i, regs) in ioapics.iter().enumerate() {
        let Some(regs) = *regs else { continue };
        let table = plan.table(i);
        let _g = IOAPIC_LOCK.lock();
        if table.is_empty() {
            // Reachable but not used (overlapping, too many pins): masked.
            let pins = versions[i].map_or(0, |v| v.pins().min(ioapic::MAX_PINS));
            for pin in 0..pins as u8 {
                mask_pin(regs, pin);
            }
            continue;
        }
        for (pin, e) in table.iter().enumerate() {
            for (reg, v) in e.writes(pin as u8).expect("plan pins are below MAX_PINS") {
                regs.write(reg, v);
            }
            let (lo, hi) = regs.entry(pin as u8);
            if ioapic::Redirection::decode(lo, hi).entry != *e {
                warnings.push(format!(
                    "I/O APIC {i} pin {pin} reads back {lo:#x}/{hi:#x} after programming"
                ));
            }
        }
    }

    let esr = lapic.esr();
    if esr != 0 {
        let mut s = format!("local APIC error status {esr:#x} after setup:");
        for e in lapic::esr_errors(esr) {
            let _ = write!(s, " {e};");
        }
        warnings.push(s);
    }
    // Without a MADT every plan warning (each ISA IRQ unserved, the boot
    // CPU not listed) says the same thing as the one warning above.
    if madt.is_some() {
        for w in &plan.warnings {
            warnings.push(describe(w, &plan));
        }
    }

    Report {
        found,
        setup: Setup::On(alloc::boxed::Box::new(Controllers {
            lapic,
            mode,
            reason,
            lapic_id,
            version,
            plan,
            ioapics,
            ioapic_versions: versions,
            pic: has_pic,
            warnings,
        })),
    }
}

fn mask_pin(regs: IoApicRegs, pin: u8) {
    let (lo_reg, _) = ioapic::redirection_regs(pin).expect("pin below MAX_PINS");
    let lo = regs.read(lo_reg);
    regs.write(lo_reg, lo | ioapic::MASKED);
}

/// Without a usable local APIC: mask the 8259s and every I/O APIC pin.
fn mask_everything(ioapics: &[Option<IoApicRegs>], versions: &[Option<ioapic::Version>]) {
    if apic::pic_probe() {
        let seq = pic::remap_and_mask(vectors::PIC_MASTER, vectors::PIC_SLAVE)
            .expect("the vector map's PIC bases are valid");
        apic::pic_write(&seq);
    }
    let _g = IOAPIC_LOCK.lock();
    for (regs, v) in ioapics.iter().zip(versions) {
        if let (Some(r), Some(v)) = (regs, v) {
            for pin in 0..v.pins().min(ioapic::MAX_PINS) as u8 {
                mask_pin(*r, pin);
            }
        }
    }
}

fn empty_madt() -> Madt {
    Madt {
        checksum_ok: true,
        lapic_address: 0,
        pcat_compat: false,
        cpus: Vec::new(),
        ioapics: Vec::new(),
        overrides: Vec::new(),
        lapic_nmis: Vec::new(),
        nmi_sources: Vec::new(),
        unknown_entries: 0,
        bad_entries: 0,
        truncated: false,
    }
}

fn status_name(s: IoApicStatus) -> String {
    match s {
        IoApicStatus::Used => String::from("used"),
        IoApicStatus::BadAddress => String::from("its address cannot hold I/O APIC registers"),
        IoApicStatus::Unreachable => String::from("not reachable"),
        IoApicStatus::RangeOverflow => String::from("its GSI range runs past 2^32"),
        IoApicStatus::TooManyPins => String::from("its version register claims more than 120 pins"),
        IoApicStatus::Overlaps(o) => format!("its GSIs overlap I/O APIC {o}'s"),
    }
}

fn describe(w: &Warning, plan: &Plan) -> String {
    match *w {
        Warning::IoApic { index, status } => {
            let io = &plan.ioapics[index];
            format!(
                "I/O APIC {index} (id {} at {:#x}, GSI base {}) not used: {}",
                io.id,
                io.address,
                io.gsi_base,
                status_name(status)
            )
        }
        Warning::Isa { irq, why } => format!("ISA IRQ {irq} not routed: {}", not_routed(why)),
        Warning::OverrideNotIsa { source } => {
            format!(
                "interrupt source override for source {source}, which is not an ISA IRQ; ignored"
            )
        }
        Warning::NmiSourceNoIoApic { gsi } => {
            format!("NMI source GSI {gsi}: no I/O APIC serves it")
        }
        Warning::NmiSourceReservedFlags { gsi } => {
            format!("NMI source GSI {gsi}: reserved polarity; left masked")
        }
        Warning::NmiSourceDuplicate { gsi } => format!("NMI source GSI {gsi} listed twice"),
        Warning::NmiSourceDestinationTooWide { gsi } => {
            format!("NMI source GSI {gsi}: boot CPU APIC ID too wide for an I/O APIC; left masked")
        }
        Warning::LintReservedFlags { lint } => {
            format!("LINT{lint} NMI entry has the reserved polarity; LINT{lint} left masked")
        }
        Warning::LintConflict { lint } => {
            format!("LINT{lint} NMI entries disagree on polarity; using the first")
        }
        Warning::BspNotInMadt { apic_id } => {
            format!("boot CPU (APIC ID {apic_id}) is not in the MADT")
        }
        Warning::BspNotEnabled { apic_id } => {
            format!("MADT lists the boot CPU (APIC ID {apic_id}) as disabled")
        }
    }
}

fn not_routed(n: NotRouted) -> String {
    match n {
        NotRouted::Cascade => String::from("8259 cascade"),
        NotRouted::Shadowed { gsi, by } => format!("GSI {gsi} belongs to IRQ {by}"),
        NotRouted::NoIoApic { gsi } => format!("no I/O APIC serves GSI {gsi}"),
        NotRouted::ReservedFlags { gsi } => format!("override to GSI {gsi} has reserved flags"),
        NotRouted::GsiTaken { gsi, by } => format!("GSI {gsi} already taken by IRQ {by}"),
        NotRouted::NmiSource { gsi } => format!("GSI {gsi} is an NMI source"),
        NotRouted::DestinationTooWide { apic_id } => {
            format!("boot CPU APIC ID {apic_id} is too wide for an I/O APIC")
        }
    }
}

fn lint_name(l: Lvt) -> &'static str {
    match (l.masked, l.delivery, l.active_low) {
        (true, _, _) => "masked",
        (false, Delivery::Nmi, false) => "NMI",
        (false, Delivery::Nmi, true) => "NMI active low",
        _ => "other",
    }
}

// ---------------------------------------------------------------------------
// Report.

pub fn report(r: &Report) {
    let f = &r.found;
    let mut line = String::from("apic: firmware left: local APIC ");
    match f.mode {
        None => line.push_str("none"),
        Some(m) => {
            line.push_str(m.name());
            if let (Some(en), Some(tpr), Some(lvts), Some(isr)) =
                (f.software_enabled, f.tpr, f.lvts_unmasked, f.in_service)
            {
                let _ = write!(
                    line,
                    ", software {}, TPR {tpr:#x}, {lvts} LVTs unmasked, {isr} in service",
                    if en { "enabled" } else { "disabled" }
                );
            }
        }
    }
    if let Some((m, s)) = f.pic_masks {
        let _ = write!(line, "; PIC masks {m:#04x}/{s:#04x}");
    }
    let _ = write!(
        line,
        "; I/O APIC pins unmasked {}/{}",
        f.ioapic_unmasked.0, f.ioapic_unmasked.1
    );
    crate::println!("{line}");

    let (warnings, on) = match &r.setup {
        Setup::Off { why, warnings } => {
            for w in warnings {
                crate::println!("apic: warning: {w}");
            }
            crate::println!("interrupts: off ({why}); {} warnings", warnings.len());
            return;
        }
        Setup::On(c) => (&c.warnings, c),
    };
    let c = on;
    crate::println!(
        "apic: {} ({}), id {}, version {:#x}, {} LVTs; LINT0 {}, LINT1 {}; spurious {:#x}, error {:#x}",
        c.mode.name(),
        c.reason.name(),
        c.lapic_id,
        c.version.version,
        u32::from(c.version.max_lvt) + 1,
        lint_name(c.plan.lint[0]),
        lint_name(c.plan.lint[1]),
        vectors::SPURIOUS,
        vectors::LAPIC_ERROR
    );
    for (i, io) in c.plan.ioapics.iter().enumerate() {
        if io.status == IoApicStatus::Used {
            crate::println!(
                "ioapic: {i}: id {} at {:#x}, GSIs {}-{}, version {:#x}",
                io.id,
                io.address,
                io.gsi_base,
                u64::from(io.gsi_base) + u64::from(io.pins) - 1,
                c.ioapic_versions[i].map_or(0, |v| v.version)
            );
        }
    }
    let routed: Vec<_> = (0..16u8)
        .filter_map(|i| c.plan.isa_route(i).map(|r| (i, r)))
        .collect();
    let mut moved = String::new();
    let mut level = String::new();
    let mut low = String::new();
    for (irq, r) in &routed {
        if r.gsi != u32::from(*irq) {
            let _ = write!(moved, " {irq}>{}", r.gsi);
        }
        if r.entry.level {
            let _ = write!(level, " {irq}");
        }
        if r.entry.active_low {
            let _ = write!(low, " {irq}");
        }
    }
    let or_none = |s: &str| {
        if s.is_empty() {
            String::from(" none")
        } else {
            String::from(s)
        }
    };
    crate::println!(
        "ioapic: ISA IRQs: {} routed; moved{}; level{}; active low{}; {} NMI pins",
        routed.len(),
        or_none(&moved),
        or_none(&level),
        or_none(&low),
        c.plan.nmi_pins.len()
    );
    if c.pic {
        crate::println!(
            "pic: 8259s remapped to {:#x}/{:#x} and masked",
            vectors::PIC_MASTER,
            vectors::PIC_SLAVE
        );
    } else {
        crate::println!("pic: none found");
    }
    for w in warnings {
        crate::println!("apic: warning: {w}");
    }
    crate::println!("interrupts: on; {} warnings", warnings.len());
}

#[cfg(feature = "ktest")]
pub mod dirt;
#[cfg(feature = "ktest")]
mod tests;
