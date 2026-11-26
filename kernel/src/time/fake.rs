//! Test builds only: `tscfake=` tells the kernel things about the TSC that
//! QEMU cannot be made to say, so the refinement's paths run in CI and
//! under TCG. Comma-separated words:
//!
//! - `invariant`: take the TSC as invariant. Under TCG the guest's TSC is
//!   the host's, which is invariant on every CI host, but QEMU's TCG never
//!   sets the CPUID bit; with this the TSC becomes the clock there, so the
//!   refinement (and a PM timer reference, with `-machine hpet=off`) runs
//!   without KVM.
//! - `claim+N` or `claim-N`: after measuring the TSC at boot, pretend
//!   CPUID's hypervisor leaf claimed that measurement off by N ppm, as a
//!   lying hypervisor would. The refinement must find the claim wrong and
//!   replace it.
//! - `skew+N` or `skew-N`: from [`FAULT_AFTER_NS`] after the clocksource
//!   watchdog starts, the clock's TSC readings run N ppm fast (or slow):
//!   each reading r past the fault's start s reads as r + (r - s) N /
//!   10^6. A TSC whose rate wanders that far is unstable from 500 ppm on
//!   (crate::time::watchdog); under that it must be left alone.
//! - `back`: from the same moment, the clock's TSC readings are 3 s
//!   behind, as a TSC reset by firmware would be. The watchdog must catch
//!   it going backwards at its next check.
//!
//! Only the clock's readings (crate::clock) are faked: the refinement and
//! TSC-deadline arming read the real TSC, which is still steady, so the
//! fault is the clock's alone, as for a TSC that really went wrong.
//!
//! The kernel prints what it applied, so a scenario can check the fake was
//! in force.

use optimal_time::calibrate::Estimate;
use optimal_time::cpuid::{Claimed, Source};
use optimal_time::freq::Freq;
use optimal_time::refine;

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use super::Time;
use crate::bootinfo::BootInfo;
use crate::sync::Once;

/// A faked fault in the clock's TSC readings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Readings run this many ppm fast (negative: slow).
    Skew(i64),
    /// Readings go back by [`BACK_NS`].
    Back,
}

/// How long after the watchdog starts the fault begins: 3 s, so the
/// watchdog has made 3 good checks first.
pub const FAULT_AFTER_NS: u64 = 3_000_000_000;
/// How far `back` steps the TSC back: 3 s, more than a check interval, so
/// the next check reads the clock below the last one.
pub const BACK_NS: u64 = 3_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fake {
    pub invariant: bool,
    pub claim_ppm: Option<i64>,
    pub fault: Option<Fault>,
}

/// The fault's start, a real TSC reading; 0 while not armed.
static START: AtomicU64 = AtomicU64::new(0);
/// `back`'s step in TSC ticks; 0 for a skew.
static BACK: AtomicU64 = AtomicU64::new(0);
static SKEW_PPM: AtomicI64 = AtomicI64::new(0);

static FAKE: Once<Fake> = Once::new();

/// Parse `tscfake=` once and say what was applied. Unknown words panic:
/// a scenario that asks for something that does not exist must not pass.
pub fn get(info: &BootInfo) -> Fake {
    let mut f = Fake::default();
    let Some(words) = crate::boot::arg(info.cmdline(), "tscfake") else {
        return *FAKE.set(f);
    };
    for w in words.split(',') {
        if w == "invariant" {
            f.invariant = true;
        } else if let Some(n) = w.strip_prefix("claim") {
            let ppm: i64 = n
                .strip_prefix('+')
                .unwrap_or(n)
                .parse()
                .unwrap_or_else(|_| panic!("tscfake: bad claim {w:?}"));
            assert!(ppm.abs() < 1_000_000, "tscfake: claim off by 100% or more");
            f.claim_ppm = Some(ppm);
        } else if let Some(n) = w.strip_prefix("skew") {
            let ppm: i64 = n
                .strip_prefix('+')
                .unwrap_or(n)
                .parse()
                .unwrap_or_else(|_| panic!("tscfake: bad skew {w:?}"));
            assert!(
                ppm != 0 && ppm.abs() < 1_000_000,
                "tscfake: skew of 0, or 100% or more"
            );
            assert!(f.fault.is_none(), "tscfake: one fault at a time");
            f.fault = Some(Fault::Skew(ppm));
        } else if w == "back" {
            assert!(f.fault.is_none(), "tscfake: one fault at a time");
            f.fault = Some(Fault::Back);
        } else {
            panic!("tscfake: unknown word {w:?}");
        }
    }
    crate::println!(
        "tscfake: applied:{}{}{}",
        if f.invariant { " invariant" } else { "" },
        f.claim_ppm
            .map(|p| alloc::format!(" claim{p:+}"))
            .unwrap_or_default(),
        match f.fault {
            Some(Fault::Skew(p)) => alloc::format!(" skew{p:+}"),
            Some(Fault::Back) => alloc::string::String::from(" back"),
            None => alloc::string::String::new(),
        }
    );
    *FAKE.set(f)
}

/// What is in force (none before crate::time::init).
pub fn current() -> Fake {
    FAKE.get().copied().unwrap_or_default()
}

/// The boot measurement off by `ppm`, as a hypervisor leaf's exact claim.
pub fn claim(t: &Time, ppm: i64) -> Option<Claimed> {
    let e = t.tsc_estimate()?;
    let million = 1_000_000u64;
    let to = million.checked_add_signed(ppm)?;
    let r = refine::rescale(Estimate { ppb: 0, ..e }, Freq::hz(million)?, Freq::hz(to)?)?;
    Some(Claimed {
        freq: r.freq,
        source: Source::Hypervisor,
        resolution_ppb: 0,
    })
}

/// Start the faked fault [`FAULT_AFTER_NS`] after the real TSC reading
/// `now`, the TSC running at `freq` (crate::time::watchdog::start). Does
/// nothing without a fault.
pub fn arm(now: u64, freq: Freq) {
    let Some(fault) = current().fault else { return };
    let start = now
        .saturating_add(freq.ns_to_ticks_ceil(FAULT_AFTER_NS))
        .max(1);
    match fault {
        Fault::Skew(ppm) => SKEW_PPM.store(ppm, Ordering::SeqCst),
        Fault::Back => BACK.store(freq.ns_to_ticks_ceil(BACK_NS), Ordering::SeqCst),
    }
    START.store(start, Ordering::SeqCst);
}

/// Has the faked fault begun (the real TSC is past its start)?
pub fn begun() -> bool {
    let s = START.load(Ordering::SeqCst);
    s != 0 && crate::arch::cpu::rdtsc_ordered() >= s
}

/// The real TSC reading `t` as the clock reads it under the faked fault.
pub fn tsc(t: u64) -> u64 {
    let s = START.load(Ordering::Relaxed);
    if s == 0 || t < s {
        return t;
    }
    let back = BACK.load(Ordering::Relaxed);
    if back != 0 {
        return t - back;
    }
    let ppm = i128::from(SKEW_PPM.load(Ordering::Relaxed));
    let off = i128::from(t - s) * ppm / 1_000_000;
    (i128::from(t) + off) as u64
}
