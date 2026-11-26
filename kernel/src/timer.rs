//! Timer events: the boot CPU's local APIC timer on vector 0xF0.
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
use optimal_time::select::{self, Clock, Events};
use optimal_time::watchdog as wd;

use crate::arch::apic::Lapic;
use crate::arch::cpu;
use crate::clock;
use crate::sync::{IrqSpinLock, Once};
use crate::time::watchdog::Poll;
use crate::time::{self, Counter, Time};

/// The LAPIC timer's measurement at boot stops once its bound is this
/// tight: 100 ppm, which makes a timer at most 0.01% late. The count rate
/// only decides how late a one-shot timer can be (never early), so it
/// needs far less than the clock.
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
    lapic_regs: Option<Lapic>,
    pub warnings: Vec<String>,
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
}

static ARMED: IrqSpinLock<Armed> = IrqSpinLock::new(Armed {
    deadline: None,
    housekeeping: None,
    refine: None,
    watchdog: None,
    hw: None,
    periodic: false,
    rates: Rates {
        lapic: None,
        longest_ns: 0,
        tsc: None,
    },
});

/// The expiry hook, a `fn(u64)` as usize; 0 for none.
static HOOK: AtomicUsize = AtomicUsize::new(0);
/// How events are programmed now: 0 none, 1 TSC-deadline, 2 one-shot.
/// Tests can switch it ([`force_events`]).
static EVENTS: AtomicU8 = AtomicU8::new(0);

/// Tests: the longest piece one arming may cover, to exercise deadlines
/// reached in several armings without waiting minutes.
static CHUNK_CAP: AtomicU64 = AtomicU64::new(u64::MAX);

static FIRES: AtomicU64 = AtomicU64::new(0);
static EARLY: AtomicU64 = AtomicU64::new(0);
static PERIODIC_TICKS: AtomicU64 = AtomicU64::new(0);

fn encode(e: Option<Events>) -> u8 {
    match e {
        None => 0,
        Some(Events::TscDeadline) => 1,
        Some(Events::LapicOneShot) => 2,
    }
}

fn events_now() -> Option<Events> {
    match EVENTS.load(Ordering::SeqCst) {
        1 => Some(Events::TscDeadline),
        2 => Some(Events::LapicOneShot),
        _ => None,
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
        None => l.write(
            regs::LVT_TIMER,
            lt::lvt(vectors::LAPIC_TIMER, Mode::OneShot, true),
        ),
    }
}

/// Once, on the boot CPU, after crate::clock::init (interrupts on).
pub fn init(t: &'static Time) -> &'static Timer {
    let mut warnings = Vec::new();
    let leaf6 = (__cpuid_count(0, 0).eax >= 6).then(|| __cpuid_count(6, 0).eax);
    let mut timer = Timer {
        lapic: None,
        against: None,
        events: None,
        deadline_mode: t.tsc_features.deadline,
        arat: leaf6.is_some_and(cpuid::arat),
        longest_ns: 0,
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
    if let Some(against) = clock::counter() {
        timer.against = Some(counter_name(against));
        match calibrate(l, against) {
            Ok(e) => timer.lapic = Some(e),
            Err(e) => warnings.push(format!("could not measure the LAPIC timer: {e}")),
        }
    }
    let tsc_clock = clock::kind() == Some(Clock::Tsc);
    timer.events = select::events(
        timer.deadline_mode,
        t.tsc_estimate().is_some() && tsc_clock,
        t.tsc_features.invariant,
        timer.lapic.is_some(),
    );
    if timer.events == Some(Events::TscDeadline) {
        tsc = match clock::counter() {
            Some(Counter::TscAt(f)) => Some(f),
            _ => unreachable!("TSC-deadline events need the TSC as the clock"),
        };
    }
    if let Some(e) = timer.lapic {
        timer.longest_ns = lt::longest_ns(e.freq, e.ppb);
        if timer.events == Some(Events::LapicOneShot) && e.ppb > time::WARN_PPB {
            warnings.push(format!(
                "the LAPIC timer could only be measured to within {} ppm, so timers can be that late",
                time::Ppb(e.ppb)
            ));
        }
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
    };
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
        match events_now() {
            // SAFETY: as in set_mode; 0 disarms.
            Some(Events::TscDeadline) => unsafe { cpu::write_msr(lt::TSC_DEADLINE_MSR, 0) },
            _ => l.write(regs::TIMER_INITIAL, 0),
        }
        return;
    };
    let now = clock::now_ns();
    let wait = due.saturating_sub(now).saturating_add(CONVERSION_PAD_NS);
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
            let ns = wait
                .min(a.rates.longest_ns)
                .min(CHUNK_CAP.load(Ordering::Relaxed));
            let arm = lt::arm(e.freq, e.ppb, ns).expect("longest_ns always arms");
            // Covers the whole wait, or this is one of several armings.
            a.hw = Some(if ns == wait {
                due
            } else {
                now.saturating_add(ns - CONVERSION_PAD_NS.min(ns))
            });
            l.write(regs::TIMER_DIVIDE, arm.dcr());
            l.write(regs::TIMER_INITIAL, arm.count);
        }
        None => a.hw = None,
    }
}

/// The TSC is unstable (crate::time::watchdog): move the clock to its
/// fallback, starting at `floor_ns` or later, events to LAPIC one-shot,
/// and the LAPIC timer's rate across with `bound_ppb` added. Everything
/// that can fail is the switch itself, done first: when it fails nothing
/// else changes. With the timer's lock held.
fn fall_back(a: &mut Armed, now: u64, floor_ns: u64, bound_ppb: u64) {
    let lapic = a.rates.lapic.map(|r| wd::carry(r, bound_ppb));
    let to = clock::switch(floor_ns);
    if to.is_some() {
        a.rates.lapic = lapic;
        if let Some(l) = lapic {
            a.rates.longest_ns = lt::longest_ns(l.freq, l.ppb);
        }
        // TSC-deadline events are timed by the TSC, which is no longer
        // trusted. Without a LAPIC rate they stay: every event is still
        // checked against the clock, so none is early, only late by
        // whatever the TSC does.
        if events_now() == Some(Events::TscDeadline)
            && lapic.is_some()
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

/// The LAPIC timer interrupt (crate::irq::dispatch, vector 0xF0).
pub fn interrupt() {
    FIRES.fetch_add(1, Ordering::Relaxed);
    // Reading the clock is also what keeps a narrow counter extended.
    let now = clock::now_ns();
    let mut a = ARMED.lock();
    if a.periodic {
        PERIODIC_TICKS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let early = a.hw.is_some_and(|hw| now < hw);
    if early {
        EARLY.fetch_add(1, Ordering::Relaxed);
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
/// TSC-deadline mode, say). Returns the previous way. Refused (None) when
/// the new way lacks what it needs.
#[cfg(feature = "ktest")]
pub fn force_events(e: Events) -> Option<Events> {
    let t = STATE.get()?;
    let l = t.lapic_regs?;
    let mut a = ARMED.lock();
    match e {
        Events::TscDeadline if a.rates.tsc.is_none() => return None,
        Events::LapicOneShot if a.rates.lapic.is_none() => return None,
        _ => {}
    }
    let was = events_now();
    EVENTS.store(encode(Some(e)), Ordering::SeqCst);
    set_mode(l, Some(e));
    program(&mut a);
    was
}

pub fn report(t: &Timer) {
    let mut line = String::from("timer: lapic ");
    match (t.lapic, t.against) {
        (Some(e), Some(against)) => {
            let ms = clock::counter()
                .and_then(|c| c.freq())
                .map_or(0, |f| f.ticks_to_ns(e.window) / 1_000_000);
            let _ = write!(
                line,
                "{} +/-{} ppm against the {against} over {ms} ms",
                time::Mhz(e.freq),
                time::Ppb(e.ppb)
            );
        }
        _ => line.push_str("unknown"),
    }
    let _ = write!(
        line,
        "; events {}; {}; {} warnings",
        match t.events {
            Some(Events::TscDeadline) => "tsc-deadline",
            Some(Events::LapicOneShot) => "lapic one-shot",
            None => "none",
        },
        if t.arat { "arat" } else { "no arat" },
        t.warnings.len()
    );
    crate::println!("{line}");
    for w in &t.warnings {
        crate::println!("timer: warning: {w}");
    }
}

#[cfg(feature = "ktest")]
mod tests;
