//! Time: the reference clocks, the TSC's frequency, and the choice of
//! clock.
//!
//! 1. Find the reference clocks ACPI describes: the HPET (its table) and
//!    the ACPI PM timer (the FADT). The HPET is started from whatever
//!    firmware left (optimal_time::hpet::start) and must be seen to count.
//! 2. When both exist, measure the PM timer against the HPET. The PM
//!    timer's frequency is fixed by the ACPI specification and the HPET's
//!    comes from a register, so if they disagree the HPET is not trusted.
//! 3. Pick the calibration reference (optimal_time::select::reference):
//!    the HPET, else the PM timer, else the PIT's channel 2.
//! 4. Take the TSC's frequency from CPUID when it gives it exactly (leaf
//!    15h, or a hypervisor's timing leaf), as ROADMAP 1c says, so boot
//!    spends no time on it; otherwise measure the TSC against the
//!    reference (optimal_time::calibrate), with a proven error bound.
//!    Leaf 16h's base frequency is only a last resort, and a warning.
//! 5. Pick the clock (optimal_time::select::clock).
//!
//! Every measurement runs [`measure`]: one tightest-of-eight sample at the
//! start, then a new end sample every 10 ms of the reference until the
//! bound is within [`GOAL_PPB`] or [`BOOT_WINDOW_NS`] has passed. Anything
//! odd is a warning in the boot log, never a guess.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::__cpuid_count;
use core::fmt::Write;

use optimal_acpi::fadt::PmTimer;
use optimal_time::calibrate::{self, Estimate, Refused, Sample};
use optimal_time::counter::{self, Extend};
use optimal_time::cpuid::{self, Claimed, Source as CpuidSource};
use optimal_time::freq::Freq;
use optimal_time::hpet::{self, Capabilities};
use optimal_time::select::{self, Clock, HpetCheck, Reference};
use optimal_time::{pit, pm};

use crate::arch::cpu;
use crate::arch::port::{inb, inl, outb};
use crate::bootinfo::BootInfo;
use crate::mm::paging;
use crate::sync::{IrqSpinLock, Once};

/// A measurement stops once its bound is this tight: 10 ppm, 0.86 s a
/// day. That takes tens of milliseconds against the HPET even under
/// emulation; the LAPIC timer's periodic tick (1c3b) will refine it over
/// a longer window without holding up boot.
pub const GOAL_PPB: u64 = 10_000;
/// At boot a measurement also stops after this long, with whatever bound
/// it reached: boot time matters more than the last few ppm, which the
/// periodic tick (1c3b) will win back without holding anything up.
pub const BOOT_WINDOW_NS: u64 = 100_000_000;
/// A new end sample this often.
const STEP_NS: u64 = 10_000_000;
/// A boot measurement of the TSC looser than this (1000 ppm, a millisecond
/// a second) is reported as a warning: past it, timers armed from the
/// TSC before the refinement would be visibly off. Anything tighter is a
/// good enough starting value, because the 10 s refinement (1c3b), not the
/// boot measurement, is what holds the clock to the ROADMAP's 1 ppm. In
/// nested VMs (GitHub's KVM runners) every HPET read is a slow exit, and
/// 100 ms only reaches about 100 ppm there.
pub const WARN_PPB: u64 = 1_000_000;
/// How far the PM timer may be from its nominal frequency, measured with
/// the HPET, before the HPET is distrusted: 500 ppm. Both normally come
/// from the same 14.318 MHz crystal and agree to within a few ppm; a
/// firmware that misprograms the HPET's period is off by far more.
const HPET_PM_SLACK_PPB: u64 = 500_000;
/// How far a measured TSC may be from an exact CPUID claim (leaf 15h or
/// the hypervisor's) beyond both their bounds: 200 ppm, for the
/// reference's own crystal error.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests use it now; the refinement (1c3b) will")
)]
pub const CPUID_SLACK_PPB: u64 = 200_000;
/// Samples taken back to back at each end of a window; the tightest is
/// used.
const SAMPLE_TRIES: usize = 8;

/// The HPET, mapped and running.
pub struct Hpet {
    regs: u64,
    pub caps: Capabilities,
}

impl Hpet {
    pub fn read(&self, reg: u64) -> u64 {
        // SAFETY: `regs` maps the HPET's 1 KiB register block uncached
        // (init), and `reg` is a register offset in it, 8-byte aligned.
        unsafe { ((self.regs + reg) as *const u64).read_volatile() }
    }

    pub fn write(&self, reg: u64, value: u64) {
        // SAFETY: as in read, with a value optimal_time::hpet produced.
        unsafe { ((self.regs + reg) as *mut u64).write_volatile(value) }
    }

    pub fn counter(&self) -> u64 {
        self.read(hpet::MAIN_COUNTER)
    }
}

#[derive(Clone, Copy, Debug)]
enum PmAccess {
    Io(u16),
    /// Virtual address of the 32-bit register.
    Mem(u64),
}

/// The ACPI PM timer.
pub struct Pm {
    access: PmAccess,
    pub bits: u32,
    pub io: bool,
    pub address: u64,
}

impl Pm {
    fn raw(&self) -> u64 {
        let v = match self.access {
            // SAFETY: the FADT names this port as the PM timer, whose
            // reads have no side effects.
            PmAccess::Io(p) => unsafe { inl(p) },
            // SAFETY: mapped uncached by init; a 4-byte register.
            PmAccess::Mem(va) => unsafe { (va as *const u32).read_volatile() },
        };
        u64::from(v) & counter::mask(self.bits)
    }

    /// A reading that two neighbours confirm (optimal_time::counter::
    /// in_order), so a glitched read never reaches a calibration. Three
    /// back-to-back reads are far under 1 ms apart. `None` after 100
    /// failed tries.
    pub fn read(&self) -> Option<u64> {
        let max_step = pm::FREQ.ns_to_ticks_ceil(1_000_000);
        (0..100).find_map(|_| {
            let (a, b, c) = (self.raw(), self.raw(), self.raw());
            counter::in_order(a, b, c, self.bits, max_step).then_some(b)
        })
    }
}

/// Serialises use of PIT channel 2 and port B.
static PIT_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

/// Channel 2 counting from 0xFFFF in mode 0 with the speaker off. Returns
/// port B as it was, for [`pit_stop`].
pub fn pit_start() -> u8 {
    let _g = PIT_LOCK.lock();
    // SAFETY: port B and the PIT's command and channel 2 ports; this only
    // changes channel 2 (whose output only reaches the speaker, kept off).
    unsafe {
        let b = inb(pit::PORT_B);
        outb(pit::PORT_B, pit::gate_open(b));
        for (port, v) in pit::START {
            outb(port, v);
        }
        b
    }
}

pub fn pit_stop(port_b: u8) {
    let _g = PIT_LOCK.lock();
    // SAFETY: restores port B's writable bits as they were.
    unsafe { outb(pit::PORT_B, pit::restore(port_b)) };
}

/// Channel 2's count as an up-counter (16 bits).
pub fn pit_read() -> u64 {
    let _g = PIT_LOCK.lock();
    // SAFETY: the latch command then the two bytes of channel 2's count.
    unsafe {
        outb(pit::COMMAND, pit::LATCH);
        let lo = inb(pit::CHANNEL2);
        let hi = inb(pit::CHANNEL2);
        pit::up(lo, hi)
    }
}

/// A counter a measurement can read.
#[derive(Clone, Copy)]
pub enum Counter<'a> {
    Hpet(&'a Hpet),
    Pm(&'a Pm),
    /// Channel 2, already started with [`pit_start`].
    Pit,
    Tsc,
}

impl Counter<'_> {
    fn bits(self) -> u32 {
        match self {
            Counter::Hpet(h) => h.caps.counter_bits(),
            Counter::Pm(p) => p.bits,
            Counter::Pit => 16,
            Counter::Tsc => 64,
        }
    }

    fn raw(self) -> Option<u64> {
        match self {
            Counter::Hpet(h) => Some(h.counter()),
            Counter::Pm(p) => p.read(),
            Counter::Pit => Some(pit_read()),
            Counter::Tsc => Some(cpu::rdtsc_ordered()),
        }
    }

    /// The nominal frequency of a reference.
    pub fn freq(self) -> Option<Freq> {
        match self {
            Counter::Hpet(h) => Some(h.caps.freq()),
            Counter::Pm(_) => Some(pm::FREQ),
            Counter::Pit => Some(pit::FREQ),
            Counter::Tsc => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Counter::Hpet(_) => "the HPET",
            Counter::Pm(_) => "the PM timer",
            Counter::Pit => "the PIT",
            Counter::Tsc => "the TSC",
        }
    }
}

/// A counter extended to 64 bits as it is read.
struct Reader<'a> {
    c: Counter<'a>,
    ext: Extend,
}

impl<'a> Reader<'a> {
    fn new(c: Counter<'a>) -> Result<Reader<'a>, String> {
        let first = c.raw().ok_or_else(|| unreadable(c))?;
        Ok(Reader {
            c,
            ext: Extend::new(c.bits(), first),
        })
    }

    fn read(&mut self) -> Result<u64, String> {
        let raw = self.c.raw().ok_or_else(|| unreadable(self.c))?;
        Ok(self.ext.update(raw))
    }
}

fn unreadable(c: Counter) -> String {
    format!("{} gave no consistent reading in 100 tries", c.name())
}

/// The tightest of [`SAMPLE_TRIES`] samples, with interrupts off.
fn sample(r: &mut Reader, t: &mut Reader) -> Result<Sample, String> {
    let were = cpu::interrupts_disable();
    let mut s = [Sample {
        before: 0,
        target: 0,
        after: 0,
    }; SAMPLE_TRIES];
    let mut res = Ok(());
    for x in s.iter_mut() {
        match (r.read(), t.read(), r.read()) {
            (Ok(before), Ok(target), Ok(after)) => {
                *x = Sample {
                    before,
                    target,
                    after,
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                res = Err(e);
                break;
            }
        }
    }
    if were {
        cpu::interrupts_enable();
    }
    res?;
    calibrate::tightest(&s).ok_or_else(|| format!("{} went backwards", r.c.name()))
}

/// About 2^34 TSC cycles (several seconds on any CPU this kernel runs on)
/// without the reference reaching the next step means it has stopped.
const STUCK_TSC: u64 = 1 << 34;

/// Measure `target` against `reference` (see the module documentation),
/// for at most about `max_ns` of the reference.
pub fn measure(reference: Counter, target: Counter, max_ns: u64) -> Result<Estimate, String> {
    let rf = reference.freq().expect("a reference has a known frequency");
    let step = rf.ns_to_ticks_ceil(STEP_NS);
    let max = rf.ns_to_ticks_ceil(max_ns);
    let mut r = Reader::new(reference)?;
    let mut t = Reader::new(target)?;
    let s0 = sample(&mut r, &mut t)?;
    let mut next = s0.after + step;
    loop {
        // Wait, reading both counters so narrow ones never wrap unseen.
        let since = cpu::rdtsc();
        loop {
            let now = r.read()?;
            t.read()?;
            if now >= next {
                break;
            }
            if cpu::rdtsc().wrapping_sub(since) > STUCK_TSC {
                return Err(format!("{} stopped counting", reference.name()));
            }
            core::hint::spin_loop();
        }
        let s1 = sample(&mut r, &mut t)?;
        let done = s1.after - s0.before >= max;
        match calibrate::estimate(rf, s0, s1) {
            Ok(e) if e.ppb <= GOAL_PPB || done => return Ok(e),
            Ok(_) => {}
            // Too early to tell: keep going, unless out of time.
            Err(Refused::WindowTooShort | Refused::TargetStopped) if !done => {}
            Err(e) => {
                return Err(format!(
                    "{} against {}: {e:?}",
                    target.name(),
                    reference.name()
                ));
            }
        }
        next = s1.after + step;
    }
}

/// The TSC's frequency and where it came from.
#[derive(Clone, Copy, Debug)]
pub enum TscFreq {
    Measured(Estimate),
    /// CPUID's claim, when nothing could measure it.
    Claimed(Claimed),
}

/// What [`init`] found. When the PIT is the reference it is stopped again
/// after use: [`pit_start`] it before reading it.
pub struct Time {
    pub hpet: Option<Hpet>,
    #[cfg_attr(
        not(feature = "ktest"),
        expect(dead_code, reason = "tests read it; the scorecard will")
    )]
    pub hpet_check: HpetCheck,
    pub pm: Option<Pm>,
    pub reference: Option<Reference>,
    pub tsc_features: cpuid::Tsc,
    pub tsc: Option<TscFreq>,
    pub clock: Option<Clock>,
    pub warnings: Vec<String>,
}

static STATE: Once<Time> = Once::new();

#[cfg_attr(
    not(feature = "ktest"),
    expect(
        dead_code,
        reason = "first user outside tests is the LAPIC timer (1c3b)"
    )
)]
pub fn get() -> Option<&'static Time> {
    STATE.get()
}

impl Time {
    /// The reference counter in use.
    pub fn reference_counter(&self) -> Option<Counter<'_>> {
        match self.reference? {
            Reference::Hpet => self.hpet.as_ref().map(Counter::Hpet),
            Reference::PmTimer => self.pm.as_ref().map(Counter::Pm),
            Reference::Pit => Some(Counter::Pit),
        }
    }
}

fn tsc_features() -> (cpuid::Tsc, Option<Claimed>) {
    let max = __cpuid_count(0, 0).eax;
    let l1 = __cpuid_count(1, 0);
    let max_ext = __cpuid_count(0x8000_0000, 0).eax;
    let ext7 = (max_ext >= 0x8000_0007).then(|| __cpuid_count(0x8000_0007, 0).edx);
    let features = cpuid::Tsc::decode(l1.ecx, l1.edx, ext7);
    let crystal = (max >= 0x15)
        .then(|| __cpuid_count(0x15, 0))
        .and_then(|l| cpuid::crystal(l.eax, l.ebx, l.ecx));
    let base = (max >= 0x16)
        .then(|| __cpuid_count(0x16, 0))
        .and_then(|l| cpuid::base_frequency(l.eax));
    // The hypervisor leaves exist only when CPUID says a hypervisor is
    // present (leaf 1 ECX bit 31).
    let hv = (l1.ecx & (1 << 31) != 0)
        .then(|| __cpuid_count(0x4000_0000, 0).eax)
        .filter(|&m| (0x4000_0010..0x5000_0000).contains(&m))
        .and_then(|_| cpuid::hypervisor(__cpuid_count(0x4000_0010, 0).eax));
    (features, cpuid::best(crystal, hv, base))
}

fn find_hpet(info: &BootInfo, warnings: &mut Vec<String>) -> Option<Hpet> {
    let table = crate::acpi::get().as_ref().ok()?.hpet?;
    let page = table.address & !(optimal_mm::PAGE_SIZE - 1);
    let va = match paging::map_mmio(info, page, 1) {
        Ok(va) => va + (table.address - page),
        Err(e) => {
            warnings.push(format!(
                "HPET at {:#x} could not be mapped ({e:?}); not used",
                table.address
            ));
            return None;
        }
    };
    // SAFETY: `va` maps the register block named by the HPET table
    // uncached; the capabilities register is its first 8 bytes and
    // reading it has no side effects.
    let raw = unsafe { ((va + hpet::CAPABILITIES) as *const u64).read_volatile() };
    let caps = match Capabilities::decode(raw) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!(
                "HPET at {:#x} is unusable ({e:?}); not used",
                table.address
            ));
            return None;
        }
    };
    let h = Hpet { regs: va, caps };
    let check = hpet::check_table(caps, table.comparators, table.counter_64bit);
    if let Some((t, hw)) = check.comparators {
        warnings.push(format!(
            "the ACPI HPET table says {t} comparators, the HPET has {hw}; using {hw}"
        ));
    }
    if let Some((t, hw)) = check.counter_bits {
        warnings.push(format!(
            "the ACPI HPET table says a {t}-bit counter, the HPET has {hw} bits; using {hw}"
        ));
    }
    let timers: Vec<u64> = (0..caps.comparators)
        .map(|n| h.read(hpet::timer_config(n)))
        .collect();
    let config = h.read(hpet::CONFIG);
    for (reg, v) in hpet::start(config, &timers) {
        h.write(reg, v);
    }
    // It must count: a few thousand reads take microseconds, which is
    // hundreds of ticks at 10 MHz or more.
    let a = h.counter();
    if !(0..1_000_000).any(|_| h.counter() != a) {
        warnings.push(format!(
            "the HPET at {:#x} does not count; not used",
            table.address
        ));
        return None;
    }
    Some(h)
}

fn find_pm(info: &BootInfo, warnings: &mut Vec<String>) -> Option<Pm> {
    let fadt = crate::acpi::get().as_ref().ok()?.fadt?;
    let bits = pm::bits(fadt.pm_timer_32bit);
    let pm = match fadt.pm_timer? {
        PmTimer::Io(port) => Pm {
            access: PmAccess::Io(port),
            bits,
            io: true,
            address: u64::from(port),
        },
        PmTimer::Memory(phys) => {
            let page = phys & !(optimal_mm::PAGE_SIZE - 1);
            if !phys.is_multiple_of(4) {
                warnings.push(format!(
                    "PM timer at {phys:#x} is not 4-byte aligned; not used"
                ));
                return None;
            }
            match paging::map_mmio(info, page, 1) {
                Ok(va) => Pm {
                    access: PmAccess::Mem(va + (phys - page)),
                    bits,
                    io: false,
                    address: phys,
                },
                Err(e) => {
                    warnings.push(format!(
                        "PM timer at {phys:#x} could not be mapped ({e:?}); not used"
                    ));
                    return None;
                }
            }
        }
    };
    match pm.read() {
        Some(a) if (0..1_000_000).any(|_| pm.read().is_some_and(|b| b != a)) => Some(pm),
        Some(_) => {
            warnings.push(format!(
                "the PM timer at {:#x} does not count; not used",
                pm.address
            ));
            None
        }
        None => {
            warnings.push(format!(
                "the PM timer at {:#x} gives no consistent reading; not used",
                pm.address
            ));
            None
        }
    }
}

/// Is PIT channel 2 there and counting? If so it is left running and
/// port B as it was is returned, for [`pit_stop`].
fn pit_works() -> Option<u8> {
    let b = pit_start();
    let a = pit_read();
    if (0..100_000).any(|_| pit_read() != a) {
        Some(b)
    } else {
        pit_stop(b);
        None
    }
}

/// Once, on the boot CPU, after ACPI and interrupts.
pub fn init(info: &BootInfo) -> &'static Time {
    let mut warnings = Vec::new();
    let hpet = find_hpet(info, &mut warnings);
    let pm = find_pm(info, &mut warnings);

    let hpet_check = match (&hpet, &pm) {
        (Some(h), Some(p)) => match measure(Counter::Hpet(h), Counter::Pm(p), BOOT_WINDOW_NS) {
            Ok(e) => match calibrate::agrees(e, pm::FREQ, HPET_PM_SLACK_PPB) {
                Ok(()) => HpetCheck::Agrees,
                Err(off) => {
                    warnings.push(format!(
                        "the PM timer measured with the HPET is off by {} ppm; the HPET's period register is wrong, so the HPET is not used",
                        Ppb(off.unsigned_abs())
                    ));
                    HpetCheck::Disagrees(off)
                }
            },
            Err(e) => {
                warnings.push(format!(
                    "could not check the HPET against the PM timer: {e}"
                ));
                HpetCheck::NotChecked
            }
        },
        _ => HpetCheck::NotChecked,
    };
    let hpet_trusted = hpet.is_some() && !matches!(hpet_check, HpetCheck::Disagrees(_));
    // The PIT is only touched when nothing better exists.
    let pit_port_b = if hpet.is_none() && pm.is_none() {
        pit_works()
    } else {
        None
    };
    let pit = pit_port_b.is_some();
    let reference = select::reference(hpet.is_some(), pm.is_some(), pit, hpet_check);

    let (tsc_features, tsc_claimed) = tsc_features();
    let mut t = Time {
        hpet,
        hpet_check,
        pm,
        reference,
        tsc_features,
        tsc: None,
        clock: None,
        warnings,
    };

    // An exact CPUID claim (leaf 15h, or the hypervisor's leaf) is the
    // TSC's frequency at boot without spending any time on it (ROADMAP
    // 1c); the refinement against the reference over the first 10 s
    // (1c3b) then checks it. Without one, the TSC is measured now.
    let exact = tsc_claimed.filter(|c| c.source != CpuidSource::BaseFrequency);
    if !tsc_features.present {
        // Nothing to do: the clock will be the HPET or the PM timer.
    } else if let Some(c) = exact {
        t.tsc = Some(TscFreq::Claimed(c));
    } else {
        let measured = match t.reference_counter() {
            Some(r) => match measure(r, Counter::Tsc, BOOT_WINDOW_NS) {
                Ok(e) => Some(e),
                Err(e) => {
                    t.warnings.push(format!("could not measure the TSC: {e}"));
                    None
                }
            },
            None => None,
        };
        t.tsc = match (measured, tsc_claimed) {
            (Some(e), _) => {
                if e.ppb > WARN_PPB {
                    t.warnings.push(format!(
                        "the TSC could only be measured to within {} ppm",
                        Ppb(e.ppb)
                    ));
                }
                Some(TscFreq::Measured(e))
            }
            (None, Some(c)) => {
                t.warnings.push(format!(
                    "the TSC frequency is CPUID's {} ({}), unverified",
                    Mhz(c.freq),
                    source_name(c.source)
                ));
                Some(TscFreq::Claimed(c))
            }
            (None, None) => {
                t.warnings
                    .push(String::from("the TSC frequency is unknown"));
                None
            }
        };
    }
    if let Some(b) = pit_port_b {
        pit_stop(b);
    }
    // A TSC counts as calibrated when it was measured or an exact CPUID
    // claim gave its frequency (not leaf 16h's base frequency alone).
    let calibrated = matches!(t.tsc, Some(TscFreq::Measured(_)))
        || matches!(t.tsc, Some(TscFreq::Claimed(c)) if c.source != CpuidSource::BaseFrequency);
    t.clock = select::clock(
        calibrated,
        tsc_features.invariant,
        hpet_trusted,
        t.pm.is_some(),
    );
    match t.clock {
        Some(Clock::TscNotInvariant) => t.warnings.push(String::from(
            "the only clock is a TSC that is not invariant; time may drift with CPU power states",
        )),
        None => t.warnings.push(String::from("no clock at all")),
        _ => {}
    }
    STATE.set(t)
}

/// ppb shown as ppm with one decimal, rounded up (it is a bound).
struct Ppb(u64);

impl core::fmt::Display for Ppb {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let tenths = self.0.div_ceil(100);
        write!(f, "{}.{}", tenths / 10, tenths % 10)
    }
}

/// A frequency in MHz to the hertz.
struct Mhz(Freq);

impl core::fmt::Display for Mhz {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let hz = self.0.hz_rounded();
        write!(f, "{}.{:06} MHz", hz / 1_000_000, hz % 1_000_000)
    }
}

fn source_name(s: CpuidSource) -> &'static str {
    match s {
        CpuidSource::Crystal => "leaf 15h",
        CpuidSource::Hypervisor => "hypervisor leaf",
        CpuidSource::BaseFrequency => "leaf 16h base frequency",
    }
}

pub fn report(t: &Time) {
    let mut line = String::from("time: reference ");
    match t.reference {
        Some(Reference::Hpet) => {
            let h = t.hpet.as_ref().expect("the HPET reference exists");
            let _ = write!(
                line,
                "hpet {} ({}-bit, {} comparators)",
                Mhz(h.caps.freq()),
                h.caps.counter_bits(),
                h.caps.comparators
            );
        }
        Some(Reference::PmTimer) => {
            let p = t.pm.as_ref().expect("the PM timer reference exists");
            let _ = write!(
                line,
                "pm-timer ({} {:#x}, {}-bit)",
                if p.io { "io" } else { "mem" },
                p.address,
                p.bits
            );
        }
        Some(Reference::Pit) => line.push_str("pit"),
        None => line.push_str("none"),
    }
    line.push_str("; tsc ");
    match t.tsc {
        Some(TscFreq::Measured(e)) => {
            let _ = write!(
                line,
                "{} +/-{} ppm over {} ms",
                Mhz(e.freq),
                Ppb(e.ppb),
                t.reference_counter()
                    .and_then(|r| r.freq())
                    .map_or(0, |f| f.ticks_to_ns(e.window) / 1_000_000)
            );
        }
        Some(TscFreq::Claimed(c)) => {
            let _ = write!(line, "{} (CPUID {})", Mhz(c.freq), source_name(c.source));
        }
        None if !t.tsc_features.present => line.push_str("none"),
        None => line.push_str("unknown"),
    }
    if t.tsc_features.present {
        let _ = write!(
            line,
            ", {}invariant, {}",
            if t.tsc_features.invariant { "" } else { "not " },
            if t.tsc_features.deadline {
                "TSC-deadline"
            } else {
                "no TSC-deadline"
            }
        );
    }
    let _ = write!(
        line,
        "; clock {}; {} warnings",
        match t.clock {
            Some(Clock::Tsc) => "tsc",
            Some(Clock::Hpet) => "hpet",
            Some(Clock::PmTimer) => "pm-timer",
            Some(Clock::TscNotInvariant) => "tsc (not invariant)",
            None => "none",
        },
        t.warnings.len()
    );
    crate::println!("{line}");
    for w in &t.warnings {
        crate::println!("time: warning: {w}");
    }
}

#[cfg(feature = "ktest")]
mod tests;
