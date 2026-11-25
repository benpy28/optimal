//! The kernel clock: nanoseconds since the clock started, from the counter
//! crate::time chose (optimal_time::select::clock).
//!
//! - The TSC and a 64-bit HPET are read and converted with no lock: an
//!   optimal_time::timeline::Timeline fixed at start-up.
//! - A narrow counter (the 24- or 32-bit PM timer, a 32-bit HPET) is
//!   extended to 64 bits as it is read (optimal_time::counter::Extend),
//!   under a lock, so every read on every CPU sees one ordered count.
//!   Extending needs a read at least once per wrap (4.7 s for a 24-bit
//!   PM timer), so crate::timer arms a housekeeping event every
//!   [`housekeeping_ns`] while the clock is narrow.
//!
//! Monotonic: the Timeline is (optimal_time::timeline), the extended
//! count is as long as it is read once per wrap, and the TSC is read with
//! `rdtsc_ordered` so a read never moves before an earlier instruction.
//! With one CPU that is the whole story. When the other CPUs start (SMP
//! bring-up), their TSCs must be checked to be in step with this one
//! before the TSC may stay the clock; that check belongs with SMP.

use optimal_time::counter::Extend;
use optimal_time::freq::Freq;
use optimal_time::select::Clock;
use optimal_time::timeline::Timeline;

use crate::arch::cpu;
use crate::sync::{IrqSpinLock, Once};
use crate::time::{Counter, Time};

struct State {
    kind: Clock,
    counter: Counter<'static>,
    freq: Freq,
    timeline: Timeline,
    /// Some for a narrow counter: its extension.
    narrow: Option<IrqSpinLock<Extend>>,
    bits: u32,
}

static STATE: Once<State> = Once::new();

/// Start the clock at 0 from now. Once, after crate::time::init. `None`
/// when there is no clock (crate::time warned already).
pub fn init(t: &'static Time) -> Option<Clock> {
    let kind = t.clock?;
    let (counter, freq) = match kind {
        Clock::Tsc | Clock::TscNotInvariant => {
            let f = t.tsc_estimate()?.freq;
            (Counter::TscAt(f), f)
        }
        Clock::Hpet => {
            let h = t.hpet.as_ref()?;
            (Counter::Hpet(h), h.caps.freq())
        }
        Clock::PmTimer => (Counter::Pm(t.pm.as_ref()?), optimal_time::pm::FREQ),
    };
    let bits = counter.bits();
    let first = counter.raw()?;
    let (timeline, narrow) = if bits == 64 {
        (Timeline::new(freq, first), None)
    } else {
        (
            Timeline::new(freq, 0),
            Some(IrqSpinLock::new(Extend::new(bits, first))),
        )
    };
    STATE.set(State {
        kind,
        counter,
        freq,
        timeline,
        narrow,
        bits,
    });
    Some(kind)
}

/// Nanoseconds since [`init`]; 0 before it, or with no clock.
pub fn now_ns() -> u64 {
    let Some(s) = STATE.get() else {
        return 0;
    };
    match &s.narrow {
        None => s.timeline.ns(raw(s)),
        Some(ext) => {
            let mut e = ext.lock();
            // A PM timer read that two neighbours never confirm (100
            // tries) leaves the count where it was: time stands still for
            // that read rather than jumping.
            let total = match s.counter.raw() {
                Some(r) => e.update(r),
                None => e.total(),
            };
            s.timeline.ns(total)
        }
    }
}

fn raw(s: &State) -> u64 {
    match s.counter {
        // The common case, without the match in Counter::raw.
        Counter::TscAt(_) => cpu::rdtsc_ordered(),
        c => c.raw().unwrap_or(0),
    }
}

/// While the clock is a narrow counter: the longest gap allowed between
/// reads, a quarter of its wrap period, so interrupts held off for a
/// while still leave plenty of margin. `None` for a 64-bit counter.
pub fn housekeeping_ns() -> Option<u64> {
    let s = STATE.get()?;
    s.narrow.as_ref()?;
    Some(s.freq.ticks_to_ns(1u64 << s.bits) / 4)
}

/// Which clock is running.
pub fn kind() -> Option<Clock> {
    STATE.get().map(|s| s.kind)
}

/// The clock's counter, with its frequency, for calibrating other
/// counters against the clock itself.
pub fn counter() -> Option<Counter<'static>> {
    STATE.get().map(|s| s.counter)
}

#[cfg(feature = "ktest")]
mod tests;
