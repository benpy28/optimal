//! Timer events from an HPET comparator: one-shot, edge-triggered, on an
//! I/O APIC input the routing plan leaves free, on vector
//! optimal_apic::vectors::HPET_TIMER.
//!
//! Chosen: edge on a plan-free GSI. Edge needs nothing after the
//! interrupt (level would mask the pin, clear the comparator's status
//! bit and unmask it again, three I/O APIC and HPET accesses per event).
//! Rejected for this step: legacy replacement (it takes IRQ 0 and IRQ 8
//! from the PIT and the RTC) and FSB delivery (an MSI straight from the
//! comparator, Tn_FSB_INT_DEL_CAP), both left for a later step; a machine
//! whose comparators reach only planned inputs (QEMU's pc machine: GSI 2
//! only, which ISA IRQ 0 owns) gets PIT events meanwhile, with a warning.
//!
//! Arming writes the comparator `ticks` ahead of a counter read and reads
//! the counter again (optimal_time::oneshot::hpet_program): a comparator
//! matches only when the counter equals it, so if the counter got there
//! first the comparator goes further ahead, never leaving a value the
//! counter has already passed.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use optimal_apic::vectors;
use optimal_time::calibrate::Estimate;
use optimal_time::hpet as regs;
use optimal_time::oneshot::{self, Comparator};
use optimal_time::select::{Clock, HpetCheck};

use crate::arch::idt::InterruptFrame;
use crate::clock;
use crate::irq::level::{self, Edge};
use crate::time::{self, Counter, Hpet};

/// Comparator writes the counter got past first, and armings that gave
/// up (optimal_time::oneshot::Missed), since boot.
pub static RETRIES: AtomicU64 = AtomicU64::new(0);
pub static MISSED: AtomicU64 = AtomicU64::new(0);

/// The first retry moves the comparator 2 us further ahead, then 4, 8 and
/// so on: more than an HPET read takes even as a VM exit (about 1 us),
/// so one retry is the usual worst case.
const STEP_NS: u64 = 2_000;

/// A comparator set up for events.
pub struct Ev {
    pub n: u8,
    pub edge: Edge,
    /// Its configuration with the interrupt on (optimal_time::hpet::
    /// edge_one_shot), and off.
    pub on: u64,
    off: u64,
    /// [`STEP_NS`] in HPET ticks.
    step: u64,
    enabled: bool,
    /// The comparator value last written (optimal_time::oneshot::
    /// early_possible).
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
    /// False when an interrupt may come before then through no fault of
    /// the kernel's (optimal_time::oneshot::early_possible), or the
    /// arming gave up.
    pub fn arm(&mut self, ticks: u64) -> bool {
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
                let sure = !oneshot::early_possible(self.last, p, self.on);
                self.last = Some(p.value);
                sure
            }
            Err(m) => {
                MISSED.fetch_add(1, Ordering::Relaxed);
                self.last = Some(m.value);
                false
            }
        }
    }

    pub fn disarm(&mut self) {
        if self.enabled {
            self.h.write(regs::timer_config(self.n), self.off);
            self.enabled = false;
        }
    }

    /// Give the comparator and its GSI back, quiet.
    #[cfg(feature = "ktest")]
    pub fn release(mut self) {
        self.disarm();
        self.h
            .write(regs::timer_config(self.n), regs::quiet(self.off));
        self.edge.release();
        self.h.unclaim(self.n);
    }
}

/// Take a comparator for events and measure the HPET's rate against the
/// clock. Or say why not.
pub fn setup() -> Result<(Ev, Estimate), String> {
    let c = crate::irq::controllers().ok_or("interrupts are off")?;
    let t = time::get().ok_or("no time setup")?;
    let h = t.hpet.as_ref().ok_or("no HPET")?;
    if let HpetCheck::Disagrees(off) = t.hpet_check {
        return Err(format!(
            "the HPET disagrees with the PM timer by {} ppm",
            time::Ppb(off.unsigned_abs())
        ));
    }
    let against = clock::counter().ok_or("no clock to measure the HPET against")?;
    // Comparators already taken are left out (a zero configuration has no
    // routes).
    let taken = h.claimed();
    let configs: Vec<u64> = (0..h.caps.comparators)
        .map(|n| {
            if taken & (1 << n) != 0 {
                0
            } else {
                h.read(regs::timer_config(n))
            }
        })
        .collect();
    let (n, gsi) = regs::pick_level(&configs, |g| level::free(c, g).is_ok()).ok_or_else(|| {
        format!(
            "no comparator reaches a free I/O APIC input (routes {})",
            routes(&configs)
        )
    })?;
    if !h.claim(n) {
        return Err(format!("comparator {n} was taken meanwhile"));
    }
    let config = configs[usize::from(n)];
    let on = regs::edge_one_shot(config, gsi, true).expect("picked from the route cap");
    let off = regs::edge_one_shot(config, gsi, false).expect("picked from the route cap");
    h.write(regs::timer_config(n), off);
    let edge = match level::claim_edge(gsi, vectors::HPET_TIMER, interrupt) {
        Ok(e) => e,
        Err(e) => {
            h.unclaim(n);
            return Err(format!("comparator {n} on GSI {gsi}: {e:?}"));
        }
    };
    // When the HPET is the clock its ticks are the clock's: the rate is
    // its own, exactly. Otherwise it is measured like the LAPIC timer.
    let rate = if clock::kind() == Some(Clock::Hpet) {
        Estimate {
            freq: h.caps.freq(),
            ppb: 0,
            window: 0,
        }
    } else {
        match time::measure_to(
            against,
            Counter::Hpet(h),
            super::LAPIC_GOAL_PPB,
            super::LAPIC_WINDOW_NS,
        ) {
            Ok(r) => r,
            Err(e) => {
                edge.release();
                h.unclaim(n);
                return Err(format!("could not measure the HPET: {e}"));
            }
        }
    };
    edge.unmask();
    Ok((
        Ev {
            n,
            edge,
            on,
            off,
            step: h.caps.freq().ns_to_ticks_ceil(STEP_NS).max(1),
            enabled: false,
            last: None,
            h,
        },
        rate,
    ))
}

/// Every comparator's route capability, for the refusal.
fn routes(configs: &[u64]) -> String {
    let caps: Vec<String> = configs
        .iter()
        .map(|&c| format!("{:#010x}", regs::route_cap(c)))
        .collect();
    caps.join(" ")
}

fn interrupt(_frame: &mut InterruptFrame) {
    super::interrupt();
}
