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
//! The TSC's frequency can change once after start-up: the 10 s
//! refinement (crate::time::refine) calls [`rebase`]. The Timeline and
//! frequency are therefore kept in a seqlock ([`Live`]): a reader copies
//! them and retries if a rebase overlapped, so reads stay lock-free.
//!
//! Monotonic: the Timeline is (optimal_time::timeline), the extended
//! count is as long as it is read once per wrap, and the TSC is read with
//! `rdtsc_ordered` so a read never moves before an earlier instruction.
//! With one CPU that is the whole story. When the other CPUs start (SMP
//! bring-up), their TSCs must be checked to be in step with this one
//! before the TSC may stay the clock; that check belongs with SMP.

use core::sync::atomic::{AtomicU64, Ordering, fence};

use optimal_time::counter::Extend;
use optimal_time::freq::{Freq, Scale};
use optimal_time::select::Clock;
use optimal_time::timeline::Timeline;

use crate::arch::cpu;
use crate::sync::{IrqSpinLock, Once};
use crate::time::{Counter, Time};

/// The clock's Timeline and frequency, behind a sequence count: odd while
/// a write is in progress. The one writer ([`rebase`]) runs with
/// interrupts off and holds [`WRITER`]; a reader that saw the count odd,
/// or changed, reads again. Every field is an atomic, so an overlapping
/// read is a stale value to throw away, never undefined behaviour.
struct Live {
    seq: AtomicU64,
    base_ticks: AtomicU64,
    base_ns: AtomicU64,
    mult: AtomicU64,
    shift: AtomicU64,
    num: AtomicU64,
    den: AtomicU64,
}

impl Live {
    fn new(t: Timeline, f: Freq) -> Live {
        Live {
            seq: AtomicU64::new(0),
            base_ticks: AtomicU64::new(t.base_ticks),
            base_ns: AtomicU64::new(t.base_ns),
            mult: AtomicU64::new(t.scale.mult),
            shift: AtomicU64::new(u64::from(t.scale.shift)),
            num: AtomicU64::new(f.num()),
            den: AtomicU64::new(f.den()),
        }
    }

    /// Run `f` on a consistent copy of the fields.
    fn read<R>(&self, f: impl Fn(&Live) -> R) -> R {
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 0 {
                let r = f(self);
                // Orders the field loads above before the second load of
                // the count (the standard seqlock reader).
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == s1 {
                    return r;
                }
            }
            core::hint::spin_loop();
        }
    }

    /// The fields as they are: only inside [`Live::read`].
    fn fields(&self) -> Timeline {
        Timeline {
            base_ticks: self.base_ticks.load(Ordering::Relaxed),
            base_ns: self.base_ns.load(Ordering::Relaxed),
            scale: Scale {
                mult: self.mult.load(Ordering::Relaxed),
                shift: self.shift.load(Ordering::Relaxed) as u32,
            },
        }
    }

    fn timeline(&self) -> Timeline {
        self.read(Live::fields)
    }

    fn freq(&self) -> Freq {
        let (n, d) = self.read(|l| (l.num.load(Ordering::Relaxed), l.den.load(Ordering::Relaxed)));
        Freq::new(n, d).expect("only valid frequencies are stored")
    }

    /// Only with [`WRITER`] held and the count odd ([`rebase`]).
    fn write_fields(&self, t: Timeline, f: Freq) {
        self.base_ticks.store(t.base_ticks, Ordering::Relaxed);
        self.base_ns.store(t.base_ns, Ordering::Relaxed);
        self.mult.store(t.scale.mult, Ordering::Relaxed);
        self.shift
            .store(u64::from(t.scale.shift), Ordering::Relaxed);
        self.num.store(f.num(), Ordering::Relaxed);
        self.den.store(f.den(), Ordering::Relaxed);
    }
}

static WRITER: IrqSpinLock<()> = IrqSpinLock::new(());

struct State {
    kind: Clock,
    counter: Counter<'static>,
    live: Live,
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
        live: Live::new(timeline, freq),
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
        // The counter is read inside the retry: a reading taken after a
        // rebase is never converted with the Timeline from before it
        // (which could give more nanoseconds than the new one does a
        // moment later).
        None => s.live.read(|l| l.fields().ns(raw(s))),
        Some(ext) => {
            let mut e = ext.lock();
            // A PM timer read that two neighbours never confirm (100
            // tries) leaves the count where it was: time stands still for
            // that read rather than jumping.
            let total = match s.counter.raw() {
                Some(r) => e.update(r),
                None => e.total(),
            };
            // Narrow counters are never rebased (the refinement is for
            // the TSC), so the Timeline needs no retry here.
            s.live.timeline().ns(total)
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
    Some(s.live.freq().ticks_to_ns(1u64 << s.bits) / 4)
}

/// The clock's frequency now: the TSC's after the refinement, if it
/// changed it.
pub fn freq() -> Option<Freq> {
    STATE.get().map(|s| s.live.freq())
}

/// Take a new frequency for the TSC from now on (crate::time::refine).
/// Continuous and monotonic (optimal_time::timeline::Timeline::rebase).
/// Refused (false) unless the clock is the TSC.
pub fn rebase(freq: Freq) -> bool {
    let Some(s) = STATE.get() else { return false };
    if !matches!(s.kind, Clock::Tsc | Clock::TscNotInvariant) {
        return false;
    }
    let _w = WRITER.lock();
    let old = s.live.timeline();
    let seq = s.live.seq.load(Ordering::Relaxed);
    s.live.seq.store(seq + 1, Ordering::Relaxed);
    // The rebase point is read after the count turned odd: any reader
    // whose TSC reading came later sees the odd count and retries, so no
    // reading past the rebase point is converted with the old Timeline.
    cpu::mfence();
    let at = cpu::rdtsc_ordered();
    let new = old.rebase(at, freq);
    s.live.write_fields(new, freq);
    s.live.seq.store(seq + 2, Ordering::Release);
    true
}

/// Which clock is running.
pub fn kind() -> Option<Clock> {
    STATE.get().map(|s| s.kind)
}

/// The clock's counter, with its frequency now, for calibrating other
/// counters against the clock itself.
pub fn counter() -> Option<Counter<'static>> {
    STATE.get().map(|s| match s.counter {
        Counter::TscAt(_) => Counter::TscAt(s.live.freq()),
        c => c,
    })
}

#[cfg(feature = "ktest")]
mod tests;
