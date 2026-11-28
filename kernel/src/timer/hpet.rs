//! Timer events from an HPET comparator in one-shot mode, by the first
//! route that works (optimal_time::hpet::pick):
//!
//! 1. FSB delivery: the comparator writes an MSI straight to the boot
//!    CPU's local APIC, on vector optimal_apic::vectors::HPET_TIMER
//!    (Tn_FSB_INT_DEL_CAP; crate::irq::level::claim_msi).
//! 2. Edge-triggered on an I/O APIC input the routing plan leaves free,
//!    on the same vector (crate::irq::level::claim_edge).
//! 3. Legacy replacement: comparator 0 on ISA IRQ 0, on that IRQ's
//!    vector, with comparator 1 (whose interrupt would go to IRQ 8) held
//!    quiet beside it.
//!
//! The FSB message is in compatibility format (optimal_apic::msi). When
//! interrupt remapping comes on (ROADMAP 1c's IOMMU step; design/
//! threat-model.md D1 puts every HPET in scope and blocks compatibility
//! format), it must become a remappable message with an IRTE for the
//! HPET's source ID from the DMAR or IVRS table, or FSB events stop.
//!
//! Chosen: FSB first. It uses no I/O APIC input, so it takes none from
//! the PCI INTx lines that later phases route, and nothing but the local
//! APIC sees each interrupt. Edge needs nothing after the interrupt either
//! (level would mask the pin, clear the comparator's status bit and
//! unmask it, three I/O APIC and HPET accesses per event). Rejected: the
//! I/O APIC route first, which costs the same per event but holds a pin.
//!
//! Legacy replacement, decided (28 Nov 2026): kept, as the last HPET
//! route. It is what a machine whose comparators have no FSB and reach
//! only inputs the ISA IRQs own (QEMU's pc machine without msi=on: GSI 2
//! only; HPETs on several AMD southbridges, where Linux relies on it too)
//! has besides the PIT, and HPET events beat PIT ones (seconds per arming
//! against 54.9 ms, one MMIO write against three port writes). It takes
//! ISA IRQ 0 from the PIT and IRQ 8 from the RTC (LEG_RT_CNF disconnects
//! both), so it is used only while the PIT is not giving events (the PIT
//! is tried after the HPET, and setting either up refuses an ISA IRQ 0
//! vector that already has a handler) and nothing uses the RTC's
//! interrupt (nothing does yet; an RTC alarm driver must check
//! [`legacy_in_use`] first). Rejected: dropping it, which leaves those
//! machines on PIT events; and using it ahead of the I/O APIC route,
//! which would take IRQ 0 and 8 where nothing needs them taken.
//!
//! Every route is proved before it is trusted ([`probe`]): a comparator
//! is armed 100 us ahead and its interrupt must arrive within 10 ms of
//! the counter passing it, in one of three armings, or the route is given
//! back and the next one tried. A route capability or FSB capability that
//! firmware or a chipset gets wrong (Linux keeps a list of southbridges
//! whose HPET MSIs never arrive) then costs 30 ms at boot, not a kernel
//! that waits forever.
//!
//! Arming writes the comparator `ticks` ahead of a counter read and reads
//! the counter again (optimal_time::oneshot::hpet_program): a comparator
//! matches only when the counter equals it, so if the counter got there
//! first the comparator goes further ahead, never leaving a value the
//! counter has already passed.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use optimal_apic::vectors;
use optimal_time::calibrate::Estimate;
use optimal_time::hpet::{self as regs, Allowed, Route};
use optimal_time::oneshot::{self, Comparator};
use optimal_time::select::{Clock, HpetCheck};

use crate::arch::idt::InterruptFrame;
use crate::clock;
use crate::irq::{self, level};
use crate::time::{self, Counter, Hpet};

/// Comparator writes the counter got past first, and armings that gave
/// up (optimal_time::oneshot::Missed), since boot.
pub static RETRIES: AtomicU64 = AtomicU64::new(0);
pub static MISSED: AtomicU64 = AtomicU64::new(0);
/// Routes that failed their probe since boot.
pub static PROBE_FAILURES: AtomicU64 = AtomicU64::new(0);

/// The first retry moves the comparator 2 us further ahead, then 4, 8 and
/// so on: more than an HPET read takes even as a VM exit (about 1 us),
/// so one retry is the usual worst case.
const STEP_NS: u64 = 2_000;

/// A probe arms this far ahead...
const PROBE_AHEAD_NS: u64 = 100_000;
/// ... and waits this long past the comparator value for the interrupt.
/// An MSI or an I/O APIC message takes well under a microsecond; 10 ms is
/// a vCPU descheduled for a whole host time slice and more.
const PROBE_WAIT_NS: u64 = 10_000_000;
/// Armings per probe. Under QEMU 8.2 with multi-threaded TCG about one
/// one-shot arming in 100 never matches (crate::irq::level's tests), so
/// three in a row missing is about one in a million; a route that does
/// not deliver misses every time.
const PROBE_TRIES: u32 = 3;

/// While a probe runs, the handler counts here instead of running the
/// timer's interrupt.
static PROBING: AtomicBool = AtomicBool::new(false);
static PROBED: AtomicU64 = AtomicU64::new(0);

/// Tests: only routes whose bit is set here are tried (1 FSB, 2 I/O
/// APIC, 4 legacy), on top of what the caller allows; 0 lifts it.
#[cfg(feature = "ktest")]
pub static ONLY: AtomicU8 = AtomicU8::new(0);
#[cfg(not(feature = "ktest"))]
static ONLY: AtomicU8 = AtomicU8::new(0);

pub const ONLY_FSB: u8 = 1;
pub const ONLY_IOAPIC: u8 = 2;
pub const ONLY_LEGACY: u8 = 4;

/// The [`ONLY`] bit for a route.
#[cfg(feature = "ktest")]
pub fn only_bit(r: Route) -> u8 {
    match r {
        Route::Fsb => ONLY_FSB,
        Route::IoApic(_) => ONLY_IOAPIC,
        Route::Legacy => ONLY_LEGACY,
    }
}

/// How a comparator's interrupts reach the CPU, and what was claimed for
/// it.
enum Via {
    Fsb(level::Msi),
    IoApic(level::Edge),
    /// ISA IRQ 0's vector and GSI (the plan's route for it).
    Legacy {
        vector: u8,
        gsi: u32,
    },
}

/// A route as the `timer:` line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Named {
    pub route: Route,
    /// FSB: the destination APIC ID. Legacy: ISA IRQ 0's GSI.
    pub to: u32,
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.route {
            Route::Fsb => write!(f, "by FSB to APIC {}", self.to),
            Route::IoApic(gsi) => write!(f, "on GSI {gsi}"),
            Route::Legacy => write!(f, "by legacy replacement on ISA IRQ 0 (GSI {})", self.to),
        }
    }
}

/// A comparator set up for events.
pub struct Ev {
    pub n: u8,
    via: Via,
    /// Its configuration with the interrupt on, and off.
    pub on: u64,
    off: u64,
    /// [`STEP_NS`] in HPET ticks.
    step: u64,
    enabled: bool,
    /// The comparator value last written.
    last: Option<u64>,
    h: &'static Hpet,
}

struct Cmp {
    h: &'static Hpet,
    n: u8,
    on: u64,
}

impl Comparator for Cmp {
    fn counter(&mut self) -> u64 {
        self.h.counter()
    }

    fn set(&mut self, value: u64) {
        self.h.write(regs::timer_comparator(self.n), value);
    }

    fn enable(&mut self) {
        self.h.write(regs::timer_config(self.n), self.on);
    }
}

impl Ev {
    /// Arm `ticks` ahead (at most optimal_time::oneshot::hpet_max_ticks).
    pub fn arm(&mut self, ticks: u64) {
        let mut c = Cmp {
            h: self.h,
            n: self.n,
            on: self.on,
        };
        let enable = !self.enabled;
        self.enabled = true;
        match oneshot::hpet_program(&mut c, ticks, self.step, self.on, enable) {
            Ok(p) => {
                RETRIES.fetch_add(u64::from(p.retries), Ordering::Relaxed);
                self.last = Some(p.value);
            }
            Err(m) => {
                MISSED.fetch_add(1, Ordering::Relaxed);
                self.last = Some(m.value);
            }
        }
    }

    /// Had the counter, read whole as `count`, reached the value last
    /// written? An interrupt that comes when it had not is not that
    /// value's match (see crate::timer::interrupt).
    pub fn reached(&self, count: u64) -> bool {
        self.last.is_some_and(|v| regs::passed(count, v, self.on))
    }

    pub fn disarm(&mut self) {
        if self.enabled {
            self.h.write(regs::timer_config(self.n), self.off);
            self.enabled = false;
        }
    }

    pub fn route(&self) -> Route {
        match self.via {
            Via::Fsb(_) => Route::Fsb,
            Via::IoApic(e) => Route::IoApic(e.gsi),
            Via::Legacy { .. } => Route::Legacy,
        }
    }

    pub fn named(&self) -> Named {
        Named {
            route: self.route(),
            to: match self.via {
                Via::Fsb(m) => m.apic_id,
                Via::IoApic(e) => e.gsi,
                Via::Legacy { gsi, .. } => gsi,
            },
        }
    }

    /// Tests: the registers say what the route is. FSB: FSB_EN set and
    /// the route register holding the message; I/O APIC: FSB_EN clear and
    /// the route field on the GSI; legacy: LEG_RT_CNF set and comparator
    /// 1 quiet. Every route: edge, one-shot, the interrupt enabled exactly
    /// while armed.
    #[cfg(feature = "ktest")]
    pub fn check(&self) {
        let h = self.h;
        let c = h.read(regs::timer_config(self.n));
        assert_eq!(c & (regs::T_LEVEL | regs::T_PERIODIC), 0, "{c:#x}");
        assert_eq!(c & regs::T_INT_ENABLE != 0, self.enabled, "{c:#x}");
        let general = h.read(regs::CONFIG);
        assert_ne!(general & regs::ENABLE, 0);
        match self.via {
            Via::Fsb(m) => {
                assert_ne!(c & regs::T_FSB_ENABLE, 0, "{c:#x}");
                assert_eq!(
                    h.read(regs::timer_fsb_route(self.n)),
                    regs::fsb_route(m.message.address, m.message.data)
                );
                assert_eq!(general & regs::LEGACY_ROUTE, 0);
            }
            Via::IoApic(e) => {
                assert_eq!(c & regs::T_FSB_ENABLE, 0, "{c:#x}");
                assert_eq!((c & regs::T_ROUTE) >> regs::T_ROUTE_SHIFT, u64::from(e.gsi));
                assert_eq!(general & regs::LEGACY_ROUTE, 0);
            }
            Via::Legacy { .. } => {
                assert_eq!(self.n, 0);
                assert_eq!(c & regs::T_FSB_ENABLE, 0, "{c:#x}");
                assert_ne!(general & regs::LEGACY_ROUTE, 0);
                assert!(legacy_in_use());
                if h.caps.comparators > 1 {
                    assert_eq!(h.read(regs::timer_config(1)) & regs::T_INT_ENABLE, 0);
                    assert_ne!(h.claimed() & 0b10, 0);
                }
            }
        }
        assert_eq!(legacy_in_use(), matches!(self.via, Via::Legacy { .. }));
    }

    /// Give the comparator and its route back, quiet: the comparator
    /// first, so nothing it sends finds the route gone.
    pub fn release(mut self) {
        self.disarm();
        let h = self.h;
        h.write(regs::timer_config(self.n), regs::quiet(self.off));
        match self.via {
            Via::Fsb(m) => {
                h.write(regs::timer_fsb_route(self.n), 0);
                m.release();
            }
            Via::IoApic(e) => e.release(),
            Via::Legacy { vector, .. } => {
                let _ = irq::isa_mask(0);
                h.write(regs::CONFIG, regs::running(h.read(regs::CONFIG)));
                irq::clear_handler(vector);
                h.unclaim(1);
                LEGACY.store(false, Ordering::SeqCst);
            }
        }
        h.unclaim(self.n);
    }
}

/// Legacy replacement is on: ISA IRQ 0 and IRQ 8 are the HPET's.
static LEGACY: AtomicBool = AtomicBool::new(false);

/// Is legacy replacement on? While it is, the PIT's and the RTC's
/// interrupts do not reach IRQ 0 and IRQ 8.
pub fn legacy_in_use() -> bool {
    LEGACY.load(Ordering::SeqCst)
}

/// A route before it is set up, for a refusal.
fn label(r: Route) -> String {
    match r {
        Route::Fsb => String::from("by FSB"),
        Route::IoApic(gsi) => format!("on GSI {gsi}"),
        Route::Legacy => String::from("by legacy replacement"),
    }
}

/// Why setup took no comparator: for each route, what a try of it said,
/// or why it was never tried.
fn refusal(
    tried: &[String],
    picked: Allowed,
    asked: Allowed,
    legacy_why: Option<&str>,
    configs: &[u64],
) -> String {
    let mut parts: Vec<String> = tried.to_vec();
    if !picked.fsb {
        parts.push(String::from(if asked.fsb {
            "no free comparator has FSB delivery"
        } else {
            "FSB ruled out"
        }));
    }
    if !picked.ioapic {
        parts.push(String::from(if asked.ioapic {
            "no free comparator reaches a free I/O APIC input"
        } else {
            "I/O APIC inputs ruled out"
        }));
    }
    if !picked.legacy {
        parts.push(String::from(
            legacy_why.unwrap_or("comparator 0 or 1 is taken"),
        ));
    }
    format!("{} (routes {})", parts.join(", "), routes(configs))
}

/// Take a comparator for events by the first route in `allowed` that
/// works, and measure the HPET's rate against the clock. Or say why not.
pub fn setup(allowed: Allowed) -> Result<(Ev, Estimate), String> {
    let c = irq::controllers().ok_or("interrupts are off")?;
    let t = time::get().ok_or("no time setup")?;
    let h = t.hpet.as_ref().ok_or("no HPET")?;
    if let HpetCheck::Disagrees(off) = t.hpet_check {
        return Err(format!(
            "the HPET disagrees with the PM timer by {} ppm",
            time::Ppb(off.unsigned_abs())
        ));
    }
    let against = clock::counter().ok_or("no clock to measure the HPET against")?;
    let only = ONLY.load(Ordering::SeqCst);
    let mut allowed = if only == 0 {
        allowed
    } else {
        Allowed {
            fsb: allowed.fsb && only & ONLY_FSB != 0,
            ioapic: allowed.ioapic && only & ONLY_IOAPIC != 0,
            legacy: allowed.legacy && only & ONLY_LEGACY != 0,
        }
    };
    // Legacy replacement needs ISA IRQ 0 routed, edge-triggered (the
    // comparator sends edges) and free.
    let isa0 = c.plan.isa_route(0).filter(|r| !r.entry.level).copied();
    let legacy_why = if !allowed.legacy {
        Some("legacy replacement ruled out")
    } else if isa0.is_none() {
        Some("ISA IRQ 0 is not routed edge-triggered, for legacy replacement")
    } else if !h.caps.legacy_replacement {
        Some("no legacy replacement (LEG_RT_CAP clear)")
    } else {
        None
    };
    allowed.legacy &= legacy_why.is_none();
    let asked = allowed;
    let mut picked = Allowed {
        fsb: false,
        ioapic: false,
        legacy: false,
    };
    let configs: Vec<u64> = (0..h.caps.comparators)
        .map(|n| h.read(regs::timer_config(n)))
        .collect();
    // The rate is measured once, for the first route that is set up.
    let mut rate: Option<Estimate> = None;
    let mut tried = Vec::new();
    loop {
        let Some((n, route)) = regs::pick(
            &configs,
            h.claimed(),
            h.caps.legacy_replacement,
            allowed,
            |g| level::free(c, g).is_ok(),
        ) else {
            return Err(refusal(&tried, picked, asked, legacy_why, &configs));
        };
        // The next pick must not come back to this one.
        match route {
            Route::Fsb => (allowed.fsb, picked.fsb) = (false, true),
            Route::IoApic(_) => (allowed.ioapic, picked.ioapic) = (false, true),
            Route::Legacy => (allowed.legacy, picked.legacy) = (false, true),
        }
        let mut ev = match claim(h, &configs, n, route, isa0) {
            Ok(ev) => ev,
            Err(e) => {
                tried.push(format!("comparator {n} {}: {e}", label(route)));
                continue;
            }
        };
        let r = match rate {
            Some(r) => r,
            None => match measure(h, against) {
                Ok(r) => {
                    rate = Some(r);
                    r
                }
                Err(e) => {
                    ev.release();
                    return Err(e);
                }
            },
        };
        if probe(&mut ev) {
            return Ok((ev, r));
        }
        PROBE_FAILURES.fetch_add(1, Ordering::Relaxed);
        tried.push(format!(
            "comparator {n} {}: no interrupt within {} ms of its match in {PROBE_TRIES} armings",
            ev.named(),
            PROBE_WAIT_NS / 1_000_000
        ));
        ev.release();
    }
}

/// When the HPET is the clock its ticks are the clock's: the rate is its
/// own, exactly. Otherwise it is measured like the LAPIC timer.
fn measure(h: &'static Hpet, against: Counter<'static>) -> Result<Estimate, String> {
    if clock::kind() == Some(Clock::Hpet) {
        return Ok(Estimate {
            freq: h.caps.freq(),
            ppb: 0,
            window: 0,
        });
    }
    time::measure_to(
        against,
        Counter::Hpet(h),
        super::LAPIC_GOAL_PPB,
        super::LAPIC_WINDOW_NS,
    )
    .map_err(|e| format!("could not measure the HPET: {e}"))
}

/// Claim comparator `n` (and comparator 1 with it for legacy
/// replacement) and set its route up, interrupt off but delivery live.
fn claim(
    h: &'static Hpet,
    configs: &[u64],
    n: u8,
    route: Route,
    isa0: Option<optimal_apic::plan::Route>,
) -> Result<Ev, String> {
    if !h.claim(n) {
        return Err(String::from("taken meanwhile"));
    }
    let config = configs[usize::from(n)];
    let made = |on: u64, off: u64, via: Via| Ev {
        n,
        via,
        on,
        off,
        step: h.caps.freq().ns_to_ticks_ceil(STEP_NS).max(1),
        enabled: false,
        last: None,
        h,
    };
    match route {
        Route::Fsb => {
            let on = regs::fsb_one_shot(config, true).expect("picked for its FSB capability");
            let off = regs::fsb_one_shot(config, false).expect("picked for its FSB capability");
            let m = match level::claim_msi(vectors::HPET_TIMER, interrupt) {
                Ok(m) => m,
                Err(e) => {
                    h.unclaim(n);
                    return Err(format!("{e:?}"));
                }
            };
            // The message first, then FSB delivery on: the comparator
            // never sends to an address it has not been given.
            h.write(
                regs::timer_fsb_route(n),
                regs::fsb_route(m.message.address, m.message.data),
            );
            h.write(regs::timer_config(n), off);
            Ok(made(on, off, Via::Fsb(m)))
        }
        Route::IoApic(gsi) => {
            let on = regs::edge_one_shot(config, gsi, true).expect("picked from the route cap");
            let off = regs::edge_one_shot(config, gsi, false).expect("picked from the route cap");
            h.write(regs::timer_config(n), off);
            match level::claim_edge(gsi, vectors::HPET_TIMER, interrupt) {
                Ok(e) => {
                    e.unmask();
                    Ok(made(on, off, Via::IoApic(e)))
                }
                Err(e) => {
                    h.unclaim(n);
                    Err(format!("{e:?}"))
                }
            }
        }
        Route::Legacy => {
            let r = isa0.expect("legacy is allowed only with ISA IRQ 0 routed");
            if configs.len() > 1 && !h.claim(1) {
                h.unclaim(n);
                return Err(String::from("comparator 1 was taken meanwhile"));
            }
            if !irq::try_set_handler(r.entry.vector, interrupt) {
                h.unclaim(1);
                h.unclaim(n);
                return Err(format!(
                    "ISA IRQ 0's vector {:#x} already has a handler (the PIT's)",
                    r.entry.vector
                ));
            }
            let on = regs::legacy_one_shot(config, true);
            let off = regs::legacy_one_shot(config, false);
            h.write(regs::timer_config(n), off);
            if let Some(&c1) = configs.get(1) {
                h.write(regs::timer_config(1), regs::quiet(c1));
            }
            h.write(regs::CONFIG, regs::legacy_running(h.read(regs::CONFIG)));
            LEGACY.store(true, Ordering::SeqCst);
            if let Err(e) = irq::isa_unmask(0) {
                let ev = made(
                    on,
                    off,
                    Via::Legacy {
                        vector: r.entry.vector,
                        gsi: r.gsi,
                    },
                );
                ev.release();
                return Err(format!("ISA IRQ 0: {e:?}"));
            }
            Ok(made(
                on,
                off,
                Via::Legacy {
                    vector: r.entry.vector,
                    gsi: r.gsi,
                },
            ))
        }
    }
}

/// Does an arming's interrupt arrive? Up to [`PROBE_TRIES`] armings,
/// each waited for while reading the HPET's counter (under multi-threaded
/// TCG a wait that only spins can starve the HPET's timer thread). The
/// comparator is left disarmed. Interrupts are on (crate::irq::init).
fn probe(ev: &mut Ev) -> bool {
    let f = ev.h.caps.freq();
    let ahead = f.ns_to_ticks_ceil(PROBE_AHEAD_NS).max(1);
    let wait = f.ns_to_ticks_ceil(PROBE_WAIT_NS);
    PROBING.store(true, Ordering::SeqCst);
    let ok = (0..PROBE_TRIES).any(|_| {
        let before = PROBED.load(Ordering::SeqCst);
        let came = || PROBED.load(Ordering::SeqCst) != before;
        ev.arm(ahead);
        let limit = regs::deadline(ev.last.expect("armed"), wait, ev.on);
        loop {
            if came() {
                break true;
            }
            if regs::passed(ev.h.counter_whole(), limit, ev.on) {
                break came();
            }
            core::hint::spin_loop();
        }
    });
    ev.disarm();
    ev.last = None;
    PROBING.store(false, Ordering::SeqCst);
    ok
}

/// Every comparator's route capability, and whether it has FSB
/// delivery, for the refusal.
fn routes(configs: &[u64]) -> String {
    let caps: Vec<String> = configs
        .iter()
        .map(|&c| {
            format!(
                "{:#010x}{}",
                regs::route_cap(c),
                if regs::fsb_capable(c) { " fsb" } else { "" }
            )
        })
        .collect();
    caps.join(" ")
}

fn interrupt(_frame: &mut InterruptFrame) {
    if PROBING.load(Ordering::SeqCst) {
        PROBED.fetch_add(1, Ordering::SeqCst);
        return;
    }
    super::interrupt();
}
