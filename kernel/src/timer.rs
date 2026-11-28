//! Timer events: the boot CPU's local APIC timer on vector 0xF0, else an
//! HPET comparator or the PIT (see 5. below).
//!
//! 1. Measure the LAPIC timer's count rate against the kernel clock's own
//!    counter (crate::clock::counter), so its error bound is relative to
//!    the clock that timer deadlines are written in.
//! 2. Pick how events are programmed (optimal_time::select::events):
//!    TSC-deadline mode when the CPU has it and the TSC is the clock,
//!    else one-shot at the measured rate.
//! 3. Keep the hardware armed for the earliest of: the expiry hook's
//!    deadline ([`arm_at`]), while the clock is a narrow counter the next
//!    housekeeping read (crate::clock::housekeeping_ns), and the clock's
//!    own upkeep: the 10 s refinement (crate::time::refine), then the
//!    clocksource watchdog's checks (crate::time::watchdog). With none of
//!    them, the timer is left disarmed: no tick runs when nothing is
//!    waiting.
//! 4. When the watchdog finds the TSC unstable, fall back ([`fall_back`]):
//!    the clock moves to the HPET or PM timer, TSC-deadline events become
//!    LAPIC one-shot ones, and the LAPIC timer's rate, measured against
//!    the TSC, is carried across with the TSC's bound against the new
//!    clock added. (Chosen: carry the rate across. Rejected: measure it
//!    again against the new clock, which takes up to 50 ms and cannot be
//!    done in interrupt context, where the watchdog fires; the carried
//!    bound is exact, and only makes one-shot events a little later at
//!    worst, never early.)
//!
//! 5. When neither LAPIC mode can be used (its rate could not be measured,
//!    or the `notimer=lapic` boot option, a quirk switch and the test
//!    that the fallbacks work), events come from an HPET comparator in
//!    one-shot edge mode on a plan-free I/O APIC input ([`hpet`]), else
//!    PIT channel 0 in mode 0 on ISA IRQ 0 ([`pit`]), each with its rate
//!    measured against the clock like the LAPIC timer's
//!    (optimal_time::select::events, optimal_time::oneshot).
//!
//! Never early: arming rounds up at the fastest rate the bound allows
//! (optimal_time::lapic), and on every interrupt the handler reads the
//! clock and only runs the hook once its deadline has passed, re-arming
//! otherwise. A deadline longer than one LAPIC arming can cover is reached
//! in several armings. Hardware interrupts that arrive before what the
//! hardware was armed for are counted ([`early_fires`]); the tests require
//! there are none, which checks the arming and the measured bound.
//!
//! Periodic mode exists for the tests and for measurement; nothing in the
//! kernel needs a periodic tick.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::__cpuid_count;
use core::fmt::Write;
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use optimal_apic::lapic as regs;
use optimal_apic::vectors;
use optimal_time::calibrate::Estimate;
use optimal_time::cpuid;
use optimal_time::freq::Freq;
use optimal_time::lapic::{self as lt, Mode};
use optimal_time::oneshot;
use optimal_time::refine as rf;
use optimal_time::select::{self, Clock, Events, Sources};
use optimal_time::watchdog as wd;

use crate::arch::apic::Lapic;
use crate::arch::cpu;
use crate::clock;
use crate::sync::{IrqSpinLock, Once};
use crate::time::watchdog::Poll;
use crate::time::{self, Counter, Time};

/// The LAPIC timer's measurement at boot (and the HPET's or the PIT's,
/// when they drive events) stops once its bound is this tight: 100 ppm,
/// which makes a timer at most 0.01% late. The count rate only decides
/// how late a one-shot timer can be (never early), so it needs far less
/// than the clock.
const LAPIC_GOAL_PPB: u64 = 100_000;
/// And after this long at most.
const LAPIC_WINDOW_NS: u64 = 50_000_000;
/// Added to every arming: converting between nanoseconds and counter
/// ticks is exact to within one nanosecond each way (optimal_time::freq::
/// Scale), so the clock read at the interrupt could otherwise be 2 ns
/// short of a deadline the hardware met exactly.
const CONVERSION_PAD_NS: u64 = 2;

pub struct Timer {
    /// The LAPIC timer's undivided count rate against the clock, as
    /// measured at boot. (After the refinement changes the clock's
    /// frequency the rate in use is [`rates`]'s.)
    pub lapic: Option<Estimate>,
    /// What it was measured against.
    pub against: Option<&'static str>,
    pub events: Option<Events>,
    /// CPUID 01h ECX bit 24.
    pub deadline_mode: bool,
    /// CPUID 06h EAX bit 2.
    pub arat: bool,
    /// The longest one-shot arming (optimal_time::lapic::longest_ns), at
    /// boot.
    pub longest_ns: u64,
    /// Sources the `notimer=` boot option ruled out.
    pub skip: Skip,
    /// The HPET comparator and route events use, when they come from the
    /// HPET, and its rate against the clock at boot.
    pub hpet: Option<(u8, hpet::Named, Estimate)>,
    /// The PIT's rate against the clock at boot, when events come from it.
    pub pit: Option<Estimate>,
    lapic_regs: Option<Lapic>,
    pub warnings: Vec<String>,
}

/// What `notimer=` (words `lapic`, `hpet`, `pit`, `hpet-fsb`,
/// `hpet-ioapic`, `hpet-legacy`, comma-separated) rules out. `lapic`
/// covers both LAPIC modes; the `hpet-` words rule out one way an HPET
/// comparator's interrupt can be delivered (crate::timer::hpet), for a
/// machine where that one is broken, and for the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Skip {
    pub lapic: bool,
    pub hpet: bool,
    pub pit: bool,
    pub hpet_fsb: bool,
    pub hpet_ioapic: bool,
    pub hpet_legacy: bool,
}

impl Skip {
    /// Parse the option's value. Unknown words are returned, to be
    /// reported; the known ones still apply.
    pub fn parse(words: &str) -> (Skip, Vec<&str>) {
        let mut s = Skip::default();
        let mut unknown = Vec::new();
        for w in words.split(',').filter(|w| !w.is_empty()) {
            match w {
                "lapic" => s.lapic = true,
                "hpet" => s.hpet = true,
                "pit" => s.pit = true,
                "hpet-fsb" => s.hpet_fsb = true,
                "hpet-ioapic" => s.hpet_ioapic = true,
                "hpet-legacy" => s.hpet_legacy = true,
                w => unknown.push(w),
            }
        }
        (s, unknown)
    }

    fn words(self) -> Vec<&'static str> {
        [
            (self.lapic, "lapic"),
            (self.hpet, "hpet"),
            (self.pit, "pit"),
            (self.hpet_fsb, "hpet-fsb"),
            (self.hpet_ioapic, "hpet-ioapic"),
            (self.hpet_legacy, "hpet-legacy"),
        ]
        .into_iter()
        .filter_map(|(on, w)| on.then_some(w))
        .collect()
    }

    /// The HPET routes left, with legacy replacement only if `legacy`
    /// (the PIT is not giving events).
    fn hpet_routes(self, legacy: bool) -> optimal_time::hpet::Allowed {
        optimal_time::hpet::Allowed {
            fsb: !self.hpet_fsb,
            ioapic: !self.hpet_ioapic,
            legacy: legacy && !self.hpet_legacy,
        }
    }
}

static STATE: Once<Timer> = Once::new();

/// The rates arming works from. They change when the refinement gives
/// the clock a new frequency (crate::time::refine), always under the
/// [`ARMED`] lock together with the clock, so an arming never mixes a
/// clock reading with a rate from before or after.
#[derive(Clone, Copy, Debug)]
pub struct Rates {
    /// The LAPIC timer's undivided count rate against the clock.
    pub lapic: Option<Estimate>,
    /// optimal_time::lapic::longest_ns for it.
    pub longest_ns: u64,
    /// The TSC's frequency, in TSC-deadline mode (the clock's).
    pub tsc: Option<Freq>,
    /// The HPET's rate against the clock, while a comparator is set up
    /// for events, and that comparator's configuration (its width decides
    /// the longest arming).
    pub hpet: Option<Estimate>,
    pub hpet_config: u64,
    /// optimal_time::oneshot::hpet_longest_ns for it.
    pub hpet_longest_ns: u64,
    /// The PIT's rate against the clock, while channel 0 is set up.
    pub pit: Option<Estimate>,
    /// optimal_time::oneshot::pit_longest_ns for it.
    pub pit_longest_ns: u64,
}

impl Rates {
    const NONE: Rates = Rates {
        lapic: None,
        longest_ns: 0,
        tsc: None,
        hpet: None,
        hpet_config: 0,
        hpet_longest_ns: 0,
        pit: None,
        pit_longest_ns: 0,
    };

    /// Work out each longest arming again from the rates.
    fn limits(mut self) -> Rates {
        if let Some(l) = self.lapic {
            self.longest_ns = lt::longest_ns(l.freq, l.ppb);
        }
        if let Some(h) = self.hpet {
            self.hpet_longest_ns = oneshot::hpet_longest_ns(h.freq, h.ppb, self.hpet_config);
        }
        if let Some(p) = self.pit {
            self.pit_longest_ns = oneshot::pit_longest_ns(p.freq, p.ppb);
        }
        self
    }

    /// Every counter's rate restated for a clock whose frequency moves
    /// from `from` to `to` (crate::time::refine): each moves by the same
    /// factor (optimal_time::refine::rescale). The TSC's own frequency is
    /// the refinement's to set. None if any rate does not fit.
    pub fn rescaled(self, from: Freq, to: Freq) -> Option<Rates> {
        let r = |e: Option<Estimate>| match e {
            Some(e) => rf::rescale(e, from, to).map(Some),
            None => Some(None),
        };
        Some(
            Rates {
                lapic: r(self.lapic)?,
                hpet: r(self.hpet)?,
                pit: r(self.pit)?,
                ..self
            }
            .limits(),
        )
    }

    /// Every counter's rate, measured against a clock that turned out to
    /// be within `bound_ppb` of its fallback, restated against the
    /// fallback (optimal_time::watchdog::carry).
    fn carried(self, bound_ppb: u64) -> Rates {
        let c = |e: Option<Estimate>| e.map(|e| wd::carry(e, bound_ppb));
        Rates {
            lapic: c(self.lapic),
            hpet: c(self.hpet),
            pit: c(self.pit),
            ..self
        }
        .limits()
    }
}

struct Armed {
    /// When the expiry hook is due (clock ns).
    deadline: Option<u64>,
    /// When the next housekeeping read is due (narrow clocks).
    housekeeping: Option<u64>,
    /// When the refinement next needs to run (crate::time::refine).
    refine: Option<u64>,
    /// When the watchdog next checks the clock (crate::time::watchdog).
    watchdog: Option<u64>,
    /// What the hardware is armed for (clock ns).
    hw: Option<u64>,
    periodic: bool,
    rates: Rates,
    /// The HPET comparator set up for events, if any.
    hpet: Option<hpet::Ev>,
    /// PIT channel 0 and ISA IRQ 0 are set up for events.
    pit: bool,
}

static ARMED: IrqSpinLock<Armed> = IrqSpinLock::new(Armed {
    deadline: None,
    housekeeping: None,
    refine: None,
    watchdog: None,
    hw: None,
    periodic: false,
    rates: Rates::NONE,
    hpet: None,
    pit: false,
});

/// The expiry hook, a `fn(u64)` as usize; 0 for none.
static HOOK: AtomicUsize = AtomicUsize::new(0);
/// How events are programmed now: 0 none, 1 TSC-deadline, 2 LAPIC
/// one-shot, 3 HPET one-shot, 4 PIT one-shot. Tests can switch it
/// ([`force_events`]).
static EVENTS: AtomicU8 = AtomicU8::new(0);

/// Tests: the longest piece one arming may cover, to exercise deadlines
/// reached in several armings without waiting minutes.
static CHUNK_CAP: AtomicU64 = AtomicU64::new(u64::MAX);

static FIRES: AtomicU64 = AtomicU64::new(0);
static EARLY: AtomicU64 = AtomicU64::new(0);
/// Interrupts before what the hardware was armed for that its own state
/// shows are not the armed match (see [`interrupt`]).
static STRAYS: AtomicU64 = AtomicU64::new(0);
/// How early (ns) the last early interrupt came, for failure reports.
static LAST_EARLY: AtomicU64 = AtomicU64::new(0);
/// One-shot armings that covered only a piece of their wait.
static PIECES: AtomicU64 = AtomicU64::new(0);
static PERIODIC_TICKS: AtomicU64 = AtomicU64::new(0);

fn encode(e: Option<Events>) -> u8 {
    match e {
        None => 0,
        Some(Events::TscDeadline) => 1,
        Some(Events::LapicOneShot) => 2,
        Some(Events::HpetOneShot) => 3,
        Some(Events::PitOneShot) => 4,
    }
}

fn events_now() -> Option<Events> {
    match EVENTS.load(Ordering::SeqCst) {
        1 => Some(Events::TscDeadline),
        2 => Some(Events::LapicOneShot),
        3 => Some(Events::HpetOneShot),
        4 => Some(Events::PitOneShot),
        _ => None,
    }
}

pub fn events_name(e: Option<Events>) -> &'static str {
    match e {
        Some(Events::TscDeadline) => "tsc-deadline",
        Some(Events::LapicOneShot) => "lapic one-shot",
        Some(Events::HpetOneShot) => "hpet one-shot",
        Some(Events::PitOneShot) => "pit one-shot",
        None => "none",
    }
}

/// Measure the LAPIC timer against `against`: one-shot from u32::MAX at
/// divide by 1, masked, so it counts without interrupting. At 1 GHz (QEMU
/// and KVM) that lasts 4.3 s, far past the window; a timer that runs out
/// first is refused, never misread (Counter::Lapic).
fn calibrate(l: Lapic, against: Counter) -> Result<Estimate, String> {
    l.write(
        regs::LVT_TIMER,
        lt::lvt(vectors::LAPIC_TIMER, Mode::OneShot, true),
    );
    l.write(regs::TIMER_DIVIDE, lt::dcr(0));
    l.write(regs::TIMER_INITIAL, u32::MAX);
    let r = time::measure_to(against, Counter::Lapic(l), LAPIC_GOAL_PPB, LAPIC_WINDOW_NS);
    l.write(regs::TIMER_INITIAL, 0);
    r
}

fn counter_name(c: Counter) -> &'static str {
    match c {
        Counter::Hpet(_) => "hpet",
        Counter::Pm(_) => "pm-timer",
        Counter::Pit => "pit",
        Counter::Tsc | Counter::TscAt(_) => "tsc",
        Counter::Lapic(_) => "lapic",
    }
}

/// Put the LVT in the mode `e` programs with, unmasked but disarmed.
fn set_mode(l: Lapic, e: Option<Events>) {
    // Disarm both ways first, so no stale arming fires in the new mode.
    l.write(regs::TIMER_INITIAL, 0);
    match e {
        Some(Events::TscDeadline) => {
            l.write(
                regs::LVT_TIMER,
                lt::lvt(vectors::LAPIC_TIMER, Mode::TscDeadline, false),
            );
            // The LVT write must take effect before the MSR write that
            // arms it (SDM Vol. 3A 11.5.4.1: in xAPIC mode the MMIO write
            // and the WRMSR are not ordered otherwise).
            cpu::mfence();
            // SAFETY: IA32_TSC_DEADLINE exists (CPUID 01h ECX bit 24, which
            // select::events required); 0 disarms it.
            unsafe { cpu::write_msr(lt::TSC_DEADLINE_MSR, 0) };
        }
        Some(Events::LapicOneShot) => l.write(
            regs::LVT_TIMER,
            lt::lvt(vectors::LAPIC_TIMER, Mode::OneShot, false),
        ),
        // The LAPIC timer is not used: masked.
        None | Some(Events::HpetOneShot | Events::PitOneShot) => l.write(
            regs::LVT_TIMER,
            lt::lvt(vectors::LAPIC_TIMER, Mode::OneShot, true),
        ),
    }
}

/// Once, on the boot CPU, after crate::clock::init (interrupts on).
pub fn init(t: &'static Time) -> &'static Timer {
    let mut warnings = Vec::new();
    let leaf6 = (__cpuid_count(0, 0).eax >= 6).then(|| __cpuid_count(6, 0).eax);
    let skip = match crate::boot::arg(crate::bootinfo::get().cmdline(), "notimer") {
        Some(words) => {
            let (skip, unknown) = Skip::parse(words);
            for w in unknown {
                warnings.push(format!("notimer: unknown word {w:?}, ignored"));
            }
            skip
        }
        None => Skip::default(),
    };
    let mut timer = Timer {
        lapic: None,
        against: None,
        events: None,
        deadline_mode: t.tsc_features.deadline,
        arat: leaf6.is_some_and(cpuid::arat),
        longest_ns: 0,
        skip,
        hpet: None,
        pit: None,
        lapic_regs: None,
        warnings: Vec::new(),
    };
    let mut tsc = None;
    let Some(c) = crate::irq::controllers() else {
        warnings.push(String::from(
            "the local APIC is not in use, so there are no timer events",
        ));
        timer.warnings = warnings;
        // Records why the refinement and the watchdog cannot run.
        let _ = time::refine::start(t, clock::now_ns(), false);
        let _ = time::watchdog::start(clock::now_ns(), false);
        return STATE.set(timer);
    };
    let l = c.lapic;
    timer.lapic_regs = Some(l);
    // Masked and disarmed until the choice is made.
    set_mode(l, None);
    let against = clock::counter();
    timer.against = against.map(counter_name);
    if !skip.lapic
        && let Some(against) = against
    {
        match calibrate(l, against) {
            Ok(e) => timer.lapic = Some(e),
            Err(e) => warnings.push(format!("could not measure the LAPIC timer: {e}")),
        }
    }
    let tsc_clock = clock::kind() == Some(Clock::Tsc);
    let mut sources = Sources {
        deadline_mode: timer.deadline_mode && !skip.lapic,
        tsc_calibrated: t.tsc_estimate().is_some() && tsc_clock,
        tsc_invariant: t.tsc_features.invariant,
        lapic_calibrated: timer.lapic.is_some(),
        hpet: false,
        pit: false,
    };
    timer.events = select::events(sources);
    // The fallbacks are set up (and their rates measured, up to 50 ms
    // each) only when they are needed.
    let mut hpet_ev = None;
    if timer.events.is_none() && !skip.hpet && t.hpet.is_some() {
        // The PIT is tried after the HPET, so legacy replacement takes ISA
        // IRQ 0 from nothing.
        match hpet::setup(skip.hpet_routes(true), None) {
            Ok((ev, rate)) => {
                timer.hpet = Some((ev.n, ev.named(), rate));
                sources.hpet = true;
                hpet_ev = Some((ev, rate));
            }
            Err(e) => warnings.push(format!("no HPET timer events: {e}")),
        }
        timer.events = select::events(sources);
    }
    if timer.events.is_none() && !skip.pit {
        match pit::setup(None) {
            Ok(rate) => {
                timer.pit = Some(rate);
                sources.pit = true;
            }
            Err(e) => warnings.push(format!("no PIT timer events: {e}")),
        }
        timer.events = select::events(sources);
    }
    if timer.events == Some(Events::TscDeadline) {
        tsc = match clock::counter() {
            Some(Counter::TscAt(f)) => Some(f),
            _ => unreachable!("TSC-deadline events need the TSC as the clock"),
        };
    }
    if let Some(e) = timer.lapic {
        timer.longest_ns = lt::longest_ns(e.freq, e.ppb);
    }
    // The rate that times one-shot events, if it is loose.
    let rate = match timer.events {
        Some(Events::LapicOneShot) => timer.lapic.map(|e| ("LAPIC timer", e)),
        Some(Events::HpetOneShot) => timer.hpet.map(|(_, _, e)| ("HPET", e)),
        Some(Events::PitOneShot) => timer.pit.map(|e| ("PIT", e)),
        _ => None,
    };
    if let Some((what, e)) = rate
        && e.ppb > time::WARN_PPB
    {
        warnings.push(format!(
            "the {what} could only be measured to within {} ppm, so timers can be that late",
            time::Ppb(e.ppb)
        ));
    }
    match (timer.events, clock::housekeeping_ns()) {
        (None, Some(hk)) => warnings.push(format!(
            "no timer events, and the clock is a narrow counter that nothing keeps extended: it is wrong after {} s",
            hk * 4 / 1_000_000_000
        )),
        (None, None) if clock::kind().is_some() => {
            warnings.push(String::from("no timer events"))
        }
        _ => {}
    }
    timer.warnings = warnings;
    let timer = STATE.set(timer);
    EVENTS.store(encode(timer.events), Ordering::SeqCst);
    set_mode(l, timer.events);
    let mut a = ARMED.lock();
    a.rates = Rates {
        lapic: timer.lapic,
        longest_ns: timer.longest_ns,
        tsc,
        ..Rates::NONE
    };
    if let Some((ev, rate)) = hpet_ev {
        a.rates.hpet = Some(rate);
        a.rates.hpet_config = ev.on;
        a.hpet = Some(ev);
    }
    if let Some(rate) = timer.pit {
        a.rates.pit = Some(rate);
        a.pit = true;
    }
    a.rates = a.rates.limits();
    // The 10 s refinement starts now, driven from interrupt() (or says
    // why not).
    a.refine = time::refine::start(t, clock::now_ns(), timer.events.is_some());
    // The watchdog starts once the refinement is done, at once if it
    // never runs.
    if a.refine.is_none() {
        a.watchdog = time::watchdog::start(clock::now_ns(), timer.events.is_some());
    }
    if timer.events.is_some() {
        if let Some(hk) = clock::housekeeping_ns() {
            a.housekeeping = Some(clock::now_ns().saturating_add(hk));
        }
        program(&mut a);
    }
    drop(a);
    timer
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn get() -> Option<&'static Timer> {
    STATE.get()
}

/// The rates arming uses now.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn rates() -> Rates {
    ARMED.lock().rates
}

/// Arm the hardware for the earliest thing due, or disarm it.
fn program(a: &mut Armed) {
    let Some(t) = STATE.get() else { return };
    let Some(l) = t.lapic_regs else { return };
    if a.periodic {
        return;
    }
    let due = [a.deadline, a.housekeeping, a.refine, a.watchdog]
        .into_iter()
        .flatten()
        .min();
    let Some(due) = due else {
        a.hw = None;
        disarm(a, l, events_now());
        return;
    };
    let now = clock::now_ns();
    let wait = due.saturating_sub(now).saturating_add(CONVERSION_PAD_NS);
    let cap = CHUNK_CAP.load(Ordering::Relaxed);
    // Covers the whole wait, or this is one of several armings.
    let reached = |a: &mut Armed, ns: u64| {
        a.hw = Some(if ns == wait {
            due
        } else {
            PIECES.fetch_add(1, Ordering::Relaxed);
            now.saturating_add(ns - CONVERSION_PAD_NS.min(ns))
        });
    };
    match events_now() {
        Some(Events::TscDeadline) => {
            let f = a
                .rates
                .tsc
                .expect("TSC-deadline mode has the TSC's frequency");
            // The clock is the TSC itself, so the conversion is exact at
            // the clock's own rate: no error bound to allow for.
            let d = lt::tsc_deadline(cpu::rdtsc_ordered(), f, 0, wait);
            a.hw = Some(due);
            // SAFETY: as in set_mode.
            unsafe { cpu::write_msr(lt::TSC_DEADLINE_MSR, d) };
        }
        Some(Events::LapicOneShot) => {
            let e = a.rates.lapic.expect("one-shot mode has the LAPIC's rate");
            let ns = wait.min(a.rates.longest_ns).min(cap);
            let arm = lt::arm(e.freq, e.ppb, ns).expect("longest_ns always arms");
            reached(a, ns);
            l.write(regs::TIMER_DIVIDE, arm.dcr());
            l.write(regs::TIMER_INITIAL, arm.count);
        }
        Some(Events::HpetOneShot) => {
            let e = a.rates.hpet.expect("HPET events have the HPET's rate");
            let ns = wait.min(a.rates.hpet_longest_ns).min(cap);
            let ev = a.hpet.as_mut().expect("HPET events have a comparator");
            let ticks =
                oneshot::hpet_ticks(e.freq, e.ppb, ns, ev.on).expect("hpet_longest_ns always arms");
            ev.arm(ticks);
            reached(a, ns);
        }
        Some(Events::PitOneShot) => {
            let e = a.rates.pit.expect("PIT events have the PIT's rate");
            let ns = wait.min(a.rates.pit_longest_ns).min(cap);
            let count = oneshot::pit_count(e.freq, e.ppb, ns).expect("pit_longest_ns always arms");
            reached(a, ns);
            time::pit0_arm(count);
        }
        None => a.hw = None,
    }
}

/// Stop the hardware `e` programs from firing.
fn disarm(a: &mut Armed, l: Lapic, e: Option<Events>) {
    match e {
        // SAFETY: as in set_mode; 0 disarms.
        Some(Events::TscDeadline) => unsafe { cpu::write_msr(lt::TSC_DEADLINE_MSR, 0) },
        Some(Events::LapicOneShot) | None => l.write(regs::TIMER_INITIAL, 0),
        Some(Events::HpetOneShot) => {
            if let Some(ev) = a.hpet.as_mut() {
                ev.disarm();
            }
        }
        Some(Events::PitOneShot) => time::pit0_disarm(),
    }
}

/// The TSC is unstable (crate::time::watchdog): move the clock to its
/// fallback, starting at `floor_ns` or later, events to LAPIC one-shot,
/// and the LAPIC timer's rate across with `bound_ppb` added. Everything
/// that can fail is the switch itself, done first: when it fails nothing
/// else changes. With the timer's lock held.
fn fall_back(a: &mut Armed, now: u64, floor_ns: u64, bound_ppb: u64) {
    let carried = a.rates.carried(bound_ppb);
    let to = clock::switch(floor_ns);
    if to.is_some() {
        a.rates = carried;
        // TSC-deadline events are timed by the TSC, which is no longer
        // trusted. Without a LAPIC rate they stay: every event is still
        // checked against the clock, so none is early, only late by
        // whatever the TSC does. (HPET and PIT events, whose rates are
        // carried across too, only ever run when there is no LAPIC rate,
        // and TSC-deadline events never run alongside them.)
        if events_now() == Some(Events::TscDeadline)
            && a.rates.lapic.is_some()
            && let Some(l) = STATE.get().and_then(|t| t.lapic_regs)
        {
            EVENTS.store(encode(Some(Events::LapicOneShot)), Ordering::SeqCst);
            set_mode(l, Some(Events::LapicOneShot));
            a.rates.tsc = None;
        }
        if let Some(hk) = clock::housekeeping_ns() {
            a.housekeeping = Some(clock::now_ns().saturating_add(hk));
        }
    }
    let kind = clock::kind();
    time::watchdog::fired(now, kind.zip(to), events_now());
}

/// How events are programmed now: the boot choice ([`Timer::events`]),
/// unless the watchdog moved them off TSC-deadline mode.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn events() -> Option<Events> {
    events_now()
}

/// The state of an HPET or PIT event source, read at an interrupt before
/// the clock is.
enum Seen {
    /// The HPET's main counter, read whole.
    Hpet(u64),
    /// PIT channel 0's OUT pin.
    Pit(bool),
}

fn seen() -> Option<Seen> {
    match events_now() {
        Some(Events::HpetOneShot) => time::get()
            .and_then(|t| t.hpet.as_ref())
            .map(|h| Seen::Hpet(h.counter_whole())),
        Some(Events::PitOneShot) => Some(Seen::Pit(time::pit0_out())),
        _ => None,
    }
}

/// The timer interrupt (crate::irq::dispatch): the LAPIC timer's vector
/// 0xF0, the HPET comparator's and ISA IRQ 0's.
///
/// An interrupt before what the hardware was armed for counts as early,
/// unless the HPET or PIT shows it is not the armed match: an HPET counter
/// short of the comparator value, or a PIT OUT pin still low. Those are
/// edges raised before the arming took over (a comparator value the
/// counter passed, which QEMU fires at once; a replaced value or count
/// that ran out just before it was replaced, whose edge waits in the
/// local APIC behind the one being handled), or a 32-bit comparator's
/// interrupt at the counter's wrap. The source is read before the clock:
/// if the armed value had been reached at the source's read, the clock,
/// read after, must be at or past what it was armed for, or the arming
/// was early. This checks the rate bound the arming used, which is what
/// the early count is for; the expiry hook never runs early either way,
/// because it is run by the clock.
pub fn interrupt() {
    FIRES.fetch_add(1, Ordering::Relaxed);
    let seen = seen();
    // Reading the clock is also what keeps a narrow counter extended.
    let now = clock::now_ns();
    let mut a = ARMED.lock();
    if a.periodic {
        PERIODIC_TICKS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let matched = match seen {
        Some(Seen::Hpet(count)) => a.hpet.as_ref().map(|ev| ev.reached(count)),
        Some(Seen::Pit(out)) => a.pit.then_some(out),
        None => None,
    };
    let before = a.hw.is_some_and(|hw| now < hw);
    let early = before && matched != Some(false);
    if early {
        EARLY.fetch_add(1, Ordering::Relaxed);
        LAST_EARLY.store(a.hw.unwrap_or(0).saturating_sub(now), Ordering::Relaxed);
    } else if before {
        STRAYS.fetch_add(1, Ordering::Relaxed);
    }
    a.hw = None;
    if a.housekeeping.is_some_and(|h| h <= now) {
        a.housekeeping = clock::housekeeping_ns().map(|hk| now.saturating_add(hk));
    }
    if a.refine.is_some_and(|r| r <= now) {
        // May give the clock a new frequency, and these rates with it.
        a.refine = time::refine::poll(now, &mut a.rates);
        if a.refine.is_none() {
            a.watchdog = time::watchdog::start(now, events_now().is_some());
        }
    }
    // An event early by the clock means the clock fell behind the
    // timer's own count (the LAPIC's crystal, or in TSC-deadline mode the
    // TSC itself where the clock's reading of it went wrong): check at
    // once, rather than when the clock reaches the next check, which a
    // clock that went back would put off.
    if a.watchdog.is_some_and(|w| w <= now || early) {
        a.watchdog = match time::watchdog::poll(now) {
            Poll::Next(t) => Some(t),
            Poll::Stop => None,
            Poll::Fall {
                floor_ns,
                bound_ppb,
                ..
            } => {
                fall_back(&mut a, now, floor_ns, bound_ppb);
                None
            }
        };
    }
    let fire = a.deadline.is_some_and(|d| d <= now);
    if fire {
        a.deadline = None;
    }
    program(&mut a);
    drop(a);
    let h = HOOK.load(Ordering::SeqCst);
    if fire && h != 0 {
        // SAFETY: only set_hook stores here, from a fn(u64).
        let f: fn(u64) = unsafe { core::mem::transmute::<usize, fn(u64)>(h) };
        f(now);
    }
}

/// The function run (in interrupt context, interrupts off) when the
/// deadline given to [`arm_at`] has passed, with the clock's time then.
/// The scheduler's timer queue will be its user; tests are for now.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the scheduler")
)]
pub fn set_hook(f: Option<fn(u64)>) {
    HOOK.store(f.map_or(0, |f| f as usize), Ordering::SeqCst);
}

/// Run the hook once the clock reaches `deadline_ns` (never before),
/// replacing any earlier deadline. A deadline already past fires at once.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the scheduler")
)]
pub fn arm_at(deadline_ns: u64) {
    let mut a = ARMED.lock();
    a.deadline = Some(deadline_ns);
    program(&mut a);
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "first user outside tests is the scheduler")
)]
pub fn cancel() {
    let mut a = ARMED.lock();
    a.deadline = None;
    program(&mut a);
}

/// Hardware interrupts so far, and how many came before what the
/// hardware was armed for.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read them; the scorecard will")
)]
pub fn fires() -> (u64, u64) {
    (FIRES.load(Ordering::SeqCst), EARLY.load(Ordering::SeqCst))
}

/// One-shot armings so far that covered only a piece of their wait.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the scorecard will")
)]
pub fn pieces() -> u64 {
    PIECES.load(Ordering::SeqCst)
}

/// Run the LAPIC timer periodically, every `period_ns` or a little more
/// (never less), counting ticks instead of running the hook. Refused
/// without a measured LAPIC rate, or while the clock is narrow with a
/// period past its housekeeping gap.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests and measurement use it")
)]
pub fn start_periodic(period_ns: u64) -> Result<lt::Arm, &'static str> {
    let t = STATE.get().ok_or("no timer")?;
    let l = t.lapic_regs.ok_or("no local APIC")?;
    let mut a = ARMED.lock();
    let e = a.rates.lapic.ok_or("the LAPIC timer's rate is unknown")?;
    if clock::housekeeping_ns().is_some_and(|hk| period_ns > hk) {
        return Err("the period is longer than the clock's housekeeping gap");
    }
    let arm = lt::arm(e.freq, e.ppb, period_ns).ok_or("the period is too long")?;
    a.periodic = true;
    a.hw = None;
    set_mode(l, None);
    l.write(
        regs::LVT_TIMER,
        lt::lvt(vectors::LAPIC_TIMER, Mode::Periodic, false),
    );
    l.write(regs::TIMER_DIVIDE, arm.dcr());
    l.write(regs::TIMER_INITIAL, arm.count);
    Ok(arm)
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests and measurement use it")
)]
pub fn stop_periodic() {
    let Some(l) = STATE.get().and_then(|t| t.lapic_regs) else {
        return;
    };
    let mut a = ARMED.lock();
    a.periodic = false;
    set_mode(l, events_now());
    // Housekeeping reads happened on every tick; carry on from now.
    if let Some(hk) = clock::housekeeping_ns() {
        a.housekeeping = Some(clock::now_ns().saturating_add(hk));
    }
    program(&mut a);
}

#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests and measurement use it")
)]
pub fn periodic_ticks() -> u64 {
    PERIODIC_TICKS.load(Ordering::SeqCst)
}

/// Tests: run `f` with the timer's lock held (interrupts off), so no
/// timer event, refinement poll or arming happens meanwhile.
#[cfg(feature = "ktest")]
pub fn hold<R>(f: impl FnOnce() -> R) -> R {
    let _a = ARMED.lock();
    f()
}

/// Tests: program events another way (one-shot on a machine with
/// TSC-deadline mode, say, or the HPET or the PIT on a machine whose
/// LAPIC timer works). Returns the previous way. Refused (Err) when the
/// new way lacks what it needs. The HPET and PIT are set up (and their
/// rates measured) the first time they are asked for; [`release_unused`]
/// gives them back. An HPET comparator held on a route that
/// hpet::ONLY no longer allows is given back and set up again.
///
/// ISA IRQ 0 is the PIT's or the HPET's legacy replacement's, never
/// both: forcing the PIT gives back an HPET comparator on legacy
/// replacement, and setting the HPET up gives back the PIT, so the HPET
/// can have every route it had at boot. (If what is given back is the
/// source in use, events stop until the new one is set up.)
#[cfg(feature = "ktest")]
pub fn force_events(e: Events) -> Result<Option<Events>, String> {
    let t = STATE.get().ok_or("no timer")?;
    let l = t.lapic_regs.ok_or("no local APIC")?;
    // An HPET comparator held on a route hpet::ONLY no longer allows is
    // given back (the routes share vector 0xED) and set up again by
    // another route with the rate already measured: the rate is the
    // counter's against the clock, whatever the route, and measuring it
    // again would lose what the refinement and the watchdog did to it.
    {
        let mut a = ARMED.lock();
        let only = hpet::ONLY.load(Ordering::SeqCst);
        if e == Events::PitOneShot
            && a.hpet
                .as_ref()
                .is_some_and(|ev| ev.route() == optimal_time::hpet::Route::Legacy)
        {
            give_back_hpet(&mut a, l, true);
        }
        if e == Events::HpetOneShot
            && only != 0
            && a.hpet
                .as_ref()
                .is_some_and(|ev| only & hpet::only_bit(ev.route()) == 0)
        {
            give_back_hpet(&mut a, l, true);
        }
        if e == Events::HpetOneShot && a.hpet.is_none() && a.pit {
            give_back_pit(&mut a, l, true);
        }
    }
    // Set up outside the lock: measuring takes up to 50 ms, and the
    // HPET's probe needs interrupts.
    let new_hpet = match e {
        Events::HpetOneShot if ARMED.lock().hpet.is_none() => {
            let known = ARMED.lock().rates.hpet;
            Some(hpet::setup(t.skip.hpet_routes(true), known)?)
        }
        _ => None,
    };
    let new_pit = match e {
        Events::PitOneShot if !ARMED.lock().pit => {
            let known = ARMED.lock().rates.pit;
            Some(pit::setup(known)?)
        }
        _ => None,
    };
    let mut a = ARMED.lock();
    if let Some((ev, rate)) = new_hpet {
        a.rates.hpet = Some(rate);
        a.rates.hpet_config = ev.on;
        a.hpet = Some(ev);
    }
    if let Some(rate) = new_pit {
        a.rates.pit = Some(rate);
        a.pit = true;
    }
    a.rates = a.rates.limits();
    match e {
        Events::TscDeadline if a.rates.tsc.is_none() => return Err(String::from("no TSC rate")),
        Events::LapicOneShot if a.rates.lapic.is_none() => {
            return Err(String::from("no LAPIC timer rate"));
        }
        _ => {}
    }
    let was = events_now();
    disarm(&mut a, l, was);
    // QEMU's PIT keeps counting through a mode write (the 8254 datasheet
    // says the write resets it), so a count armed before the switch can
    // still raise IRQ 0 once: masked while the PIT is not the source, the
    // I/O APIC drops that edge. (Never while the HPET's legacy
    // replacement holds IRQ 0: then the PIT is not set up.)
    if was == Some(Events::PitOneShot) && e != Events::PitOneShot {
        let _ = crate::irq::isa_mask(0);
    }
    if e == Events::PitOneShot && was != Some(Events::PitOneShot) {
        let _ = crate::irq::isa_unmask(0);
    }
    a.hw = None;
    EVENTS.store(encode(Some(e)), Ordering::SeqCst);
    set_mode(l, Some(e));
    program(&mut a);
    Ok(was)
}

/// Tests: stop events from `e` if they come from it now.
#[cfg(feature = "ktest")]
fn stop_if_current(a: &mut Armed, l: Lapic, e: Events) {
    if events_now() == Some(e) {
        disarm(a, l, Some(e));
        a.hw = None;
        EVENTS.store(encode(None), Ordering::SeqCst);
    }
}

/// Tests: give the HPET comparator back; with `keep_rate`, its rate stays
/// for the next setup.
#[cfg(feature = "ktest")]
fn give_back_hpet(a: &mut Armed, l: Lapic, keep_rate: bool) {
    stop_if_current(a, l, Events::HpetOneShot);
    if let Some(ev) = a.hpet.take() {
        ev.release();
    }
    if !keep_rate {
        a.rates.hpet = None;
    }
}

/// Tests: give PIT channel 0 and ISA IRQ 0 back; with `keep_rate`, the
/// PIT's rate stays for the next setup.
#[cfg(feature = "ktest")]
fn give_back_pit(a: &mut Armed, l: Lapic, keep_rate: bool) {
    stop_if_current(a, l, Events::PitOneShot);
    if a.pit {
        pit::release();
        a.pit = false;
    }
    if !keep_rate {
        a.rates.pit = None;
    }
}

/// Tests: give back the HPET comparator and PIT channel 0 if events no
/// longer come from them.
#[cfg(feature = "ktest")]
pub fn release_unused() {
    let Some(l) = STATE.get().and_then(|t| t.lapic_regs) else {
        return;
    };
    let mut a = ARMED.lock();
    let now = events_now();
    if now != Some(Events::HpetOneShot) {
        give_back_hpet(&mut a, l, false);
    }
    if now != Some(Events::PitOneShot) {
        give_back_pit(&mut a, l, false);
    }
}

/// Tests: the route of the HPET comparator held for events, and a check
/// that its registers say so (hpet::Ev::check).
#[cfg(feature = "ktest")]
pub fn hpet_route() -> Option<optimal_time::hpet::Route> {
    let a = ARMED.lock();
    a.hpet.as_ref().map(|ev| {
        ev.check();
        ev.route()
    })
}

/// Is ISA IRQ 0 the timer's (the PIT's events, or the HPET's legacy
/// replacement)? Tests that drive IRQ 0 themselves must leave it alone
/// then.
#[cfg_attr(not(feature = "ktest"), expect(dead_code, reason = "tests read it"))]
pub fn owns_isa_irq0() -> bool {
    let a = ARMED.lock();
    a.pit || hpet::legacy_in_use()
}

/// HPET comparator retries and misses since boot
/// (optimal_time::oneshot::hpet_program), the interrupts before what an
/// HPET or PIT was armed for that were not its match ([`interrupt`]),
/// and HPET routes that failed their delivery probe.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read them; the scorecard will")
)]
pub fn hpet_retries() -> (u64, u64, u64, u64) {
    (
        hpet::RETRIES.load(Ordering::SeqCst),
        hpet::MISSED.load(Ordering::SeqCst),
        STRAYS.load(Ordering::SeqCst),
        hpet::PROBE_FAILURES.load(Ordering::SeqCst),
    )
}

/// Is the HPET's legacy replacement on? While it is, ISA IRQ 0 and IRQ 8
/// are the HPET's: the PIT's and the RTC's interrupts do not reach them.
#[cfg_attr(
    not(feature = "ktest"),
    expect(dead_code, reason = "tests read it; the RTC alarm driver will")
)]
pub fn hpet_legacy() -> bool {
    hpet::legacy_in_use()
}

pub fn report(t: &Timer) {
    let clock_ms = |window: u64| {
        clock::counter()
            .and_then(|c| c.freq())
            .map_or(0, |f| f.ticks_to_ns(window) / 1_000_000)
    };
    let against = t.against.unwrap_or("clock");
    let mut line = String::from("timer: lapic ");
    match (t.lapic, t.against) {
        (Some(e), Some(against)) => {
            let _ = write!(
                line,
                "{} +/-{} ppm against the {against} over {} ms",
                time::Mhz(e.freq),
                time::Ppb(e.ppb),
                clock_ms(e.window)
            );
        }
        _ if t.skip.lapic => line.push_str("skipped"),
        _ => line.push_str("unknown"),
    }
    if let Some((n, named, e)) = t.hpet {
        let _ = write!(line, "; hpet comparator {n} {named} {}", time::Mhz(e.freq));
        if e.ppb == 0 && e.window == 0 {
            line.push_str(", the clock itself");
        } else {
            let _ = write!(
                line,
                " +/-{} ppm against the {against} over {} ms",
                time::Ppb(e.ppb),
                clock_ms(e.window)
            );
        }
    }
    if let Some(e) = t.pit {
        let _ = write!(
            line,
            "; pit {} +/-{} ppm against the {against} over {} ms",
            time::Mhz(e.freq),
            time::Ppb(e.ppb),
            clock_ms(e.window)
        );
    }
    let words = t.skip.words();
    if !words.is_empty() {
        let _ = write!(line, "; notimer {}", words.join(","));
    }
    let _ = write!(
        line,
        "; events {}; {}; {} warnings",
        events_name(t.events),
        if t.arat { "arat" } else { "no arat" },
        t.warnings.len()
    );
    crate::println!("{line}");
    for w in &t.warnings {
        crate::println!("timer: warning: {w}");
    }
}

mod hpet;
mod pit;

#[cfg(feature = "ktest")]
mod tests;
