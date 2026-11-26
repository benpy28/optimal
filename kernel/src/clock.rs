//! The kernel clock: nanoseconds since the clock started, from the counter
//! crate::time chose (optimal_time::select::clock).
//!
//! - The TSC and a 64-bit HPET are read and converted with no lock,
//!   through an optimal_time::timeline::Timeline.
//! - A narrow counter (the 24- or 32-bit PM timer, a 32-bit HPET) is
//!   extended to 64 bits as it is read (optimal_time::counter::Extend),
//!   under a lock, so every read on every CPU sees one ordered count.
//!   Extending needs a read at least once per wrap (4.7 s for a 24-bit
//!   PM timer), so crate::timer arms a housekeeping event every
//!   [`housekeeping_ns`] while the clock is narrow.
//!
//! The TSC's frequency can change once after start-up: the 10 s
//! refinement (crate::time::refine) calls [`rebase`]. And the TSC can be
//! dropped for good: when the clocksource watchdog (crate::time::watchdog)
//! finds it unstable, [`switch`] moves the clock to its fallback, the
//! reference it was watched against (the HPET, else the PM timer),
//! continuing from where the TSC's timeline was. The Timeline, frequency
//! and which counter is in use are therefore kept in a seqlock ([`Live`]):
//! a reader copies them and retries if a change overlapped, so reads stay
//! lock-free.
//!
//! Monotonic: the Timeline is (optimal_time::timeline), the extended
//! count is as long as it is read once per wrap, the TSC is read with
//! `rdtsc_ordered` so a read never moves before an earlier instruction,
//! and a switch starts the new timeline at or past the old one. (A TSC
//! that goes backwards breaks this until the watchdog's next check, at
//! most a second; reads in between can go back with it. Guarding every
//! read against that would put an atomic read-modify-write on the
//! clock's fastest path, shared by every CPU once there are several, so
//! the guarantee is instead that the new timeline starts past them.)
//! With one CPU that is the whole story. When the other CPUs start (SMP
//! bring-up), their TSCs must be checked to be in step with this one
//! before the TSC may stay the clock; that check belongs with SMP.

use core::sync::atomic::{AtomicU64, Ordering, fence};

use optimal_time::counter::Extend;
use optimal_time::freq::{Freq, Scale};
use optimal_time::select::{Clock, Reference};
use optimal_time::timeline::Timeline;

use crate::arch::cpu;
use crate::sync::{IrqSpinLock, Once};
use crate::time::{Counter, Time};

/// The clock's Timeline and frequency, and which source is in use,
/// behind a sequence count: odd while a write is in progress. The writers
/// ([`rebase`], [`switch`]) run with interrupts off and hold [`WRITER`]; a
/// reader that saw the count odd, or changed, reads again. Every field is
/// an atomic, so an overlapping read is a stale value to throw away, never
/// undefined behaviour.
struct Live {
    seq: AtomicU64,
    /// 0 while the first source runs, 1 once [`switch`] moved to the
    /// fallback (for good).
    which: AtomicU64,
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
            which: AtomicU64::new(0),
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

    /// Only with [`WRITER`] held and the count odd ([`rebase`], [`switch`]).
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

/// A counter the clock can keep time with.
struct Source {
    kind: Clock,
    counter: Counter<'static>,
    freq: Freq,
    /// Some for a narrow counter: its extension.
    narrow: Option<IrqSpinLock<Extend>>,
    bits: u32,
}

impl Source {
    fn new(kind: Clock, counter: Counter<'static>, freq: Freq) -> Source {
        let bits = counter.bits();
        Source {
            kind,
            counter,
            freq,
            narrow: (bits < 64).then(|| IrqSpinLock::new(Extend::new(bits, 0))),
            bits,
        }
    }
}

struct State {
    /// The source at start-up, then the one [`switch`] falls back to.
    sources: [Option<Source>; 2],
    live: Live,
}

impl State {
    /// The source in use; only inside [`Live::read`], or once it can no
    /// longer change.
    fn source(&self, which: u64) -> &Source {
        self.sources[which as usize]
            .as_ref()
            .expect("only an existing source is in use")
    }

    fn current(&self) -> &Source {
        self.source(self.live.read(|l| l.which.load(Ordering::Relaxed)))
    }
}

static STATE: Once<State> = Once::new();

/// The TSC as the clock reads it: in test builds with a faked fault
/// (crate::time::fake), as the fault makes it read.
fn tsc() -> u64 {
    let t = cpu::rdtsc_ordered();
    #[cfg(feature = "ktest")]
    let t = crate::time::fake::tsc(t);
    t
}

/// Start the clock at 0 from now. Once, after crate::time::init. `None`
/// when there is no clock (crate::time warned already).
pub fn init(t: &'static Time) -> Option<Clock> {
    let kind = t.clock?;
    let first = match kind {
        Clock::Tsc | Clock::TscNotInvariant => {
            let f = t.tsc_estimate()?.freq;
            Source::new(kind, Counter::TscAt(f), f)
        }
        Clock::Hpet => {
            let h = t.hpet.as_ref()?;
            Source::new(kind, Counter::Hpet(h), h.caps.freq())
        }
        Clock::PmTimer => Source::new(kind, Counter::Pm(t.pm.as_ref()?), optimal_time::pm::FREQ),
    };
    // The TSC falls back to the reference it is watched against
    // (crate::time::watchdog): the HPET or the PM timer, never the PIT.
    let fallback = match (kind, t.reference) {
        (Clock::Tsc, Some(Reference::Hpet)) => t
            .hpet
            .as_ref()
            .map(|h| Source::new(Clock::Hpet, Counter::Hpet(h), h.caps.freq())),
        (Clock::Tsc, Some(Reference::PmTimer)) => {
            t.pm.as_ref()
                .map(|p| Source::new(Clock::PmTimer, Counter::Pm(p), optimal_time::pm::FREQ))
        }
        _ => None,
    };
    let raw = match first.counter {
        Counter::TscAt(_) => tsc(),
        c => c.raw()?,
    };
    let freq = first.freq;
    let timeline = match &first.narrow {
        None => Timeline::new(freq, raw),
        Some(ext) => {
            *ext.lock() = Extend::new(first.bits, raw);
            Timeline::new(freq, 0)
        }
    };
    STATE.set(State {
        sources: [Some(first), fallback],
        live: Live::new(timeline, freq),
    });
    Some(kind)
}

/// Nanoseconds since [`init`]; 0 before it, or with no clock.
pub fn now_ns() -> u64 {
    let Some(s) = STATE.get() else {
        return 0;
    };
    // A wide source is read and converted inside the retry: a reading
    // taken after a rebase or a switch is never converted with the
    // Timeline from before it (which could give more nanoseconds than the
    // new one does a moment later). A narrow one is only ever in use for
    // good (at start-up, or after the one switch), so its Timeline no
    // longer changes.
    let which = s.live.read(|l| {
        let which = l.which.load(Ordering::Relaxed);
        let src = s.source(which);
        match src.narrow {
            None => Ok(l.fields().ns(raw(src))),
            Some(_) => Err(which),
        }
    });
    let which = match which {
        Ok(ns) => return ns,
        Err(w) => w,
    };
    let src = s.source(which);
    let ext = src.narrow.as_ref().expect("a narrow source");
    let mut e = ext.lock();
    // A PM timer read that two neighbours never confirm (100 tries)
    // leaves the count where it was: time stands still for that read
    // rather than jumping.
    let total = match src.counter.raw() {
        Some(r) => e.update(r),
        None => e.total(),
    };
    s.live.timeline().ns(total)
}

fn raw(s: &Source) -> u64 {
    match s.counter {
        // The common case, without the match in Counter::raw.
        Counter::TscAt(_) => tsc(),
        c => c.raw().unwrap_or(0),
    }
}

/// While the clock is a narrow counter: the longest gap allowed between
/// reads, a quarter of its wrap period, so interrupts held off for a
/// while still leave plenty of margin. `None` for a 64-bit counter.
pub fn housekeeping_ns() -> Option<u64> {
    let s = STATE.get()?;
    let src = s.current();
    src.narrow.as_ref()?;
    Some(src.freq.ticks_to_ns(1u64 << src.bits) / 4)
}

/// The clock's frequency now: the TSC's after the refinement, if it
/// changed it, or the fallback's after a switch.
pub fn freq() -> Option<Freq> {
    STATE.get().map(|s| s.live.freq())
}

/// Take a new frequency for the TSC from now on (crate::time::refine).
/// Continuous and monotonic (optimal_time::timeline::Timeline::rebase).
/// Refused (false) unless the clock is the TSC.
pub fn rebase(freq: Freq) -> bool {
    let Some(s) = STATE.get() else { return false };
    let _w = WRITER.lock();
    if !matches!(s.current().kind, Clock::Tsc | Clock::TscNotInvariant) {
        return false;
    }
    let old = s.live.timeline();
    let seq = s.live.seq.load(Ordering::Relaxed);
    s.live.seq.store(seq + 1, Ordering::Relaxed);
    // The rebase point is read after the count turned odd: any reader
    // whose TSC reading came later sees the odd count and retries, so no
    // reading past the rebase point is converted with the old Timeline.
    cpu::mfence();
    let at = tsc();
    let new = old.rebase(at, freq);
    s.live.write_fields(new, freq);
    s.live.seq.store(seq + 2, Ordering::Release);
    true
}

/// What the clock would fall back to ([`switch`]), while it has not.
pub fn fallback() -> Option<Clock> {
    let s = STATE.get()?;
    if s.live.read(|l| l.which.load(Ordering::Relaxed)) != 0 {
        return None;
    }
    s.sources[1].as_ref().map(|f| f.kind)
}

/// Drop the TSC for good and keep time with the fallback from now on
/// (crate::time::watchdog, with the timer's lock held). The new timeline
/// starts at the old one's time now, or at `floor_ns` if that is later
/// (after the TSC went backwards), so the clock never goes backwards.
/// Returns where the new timeline starts, or `None`, changing nothing,
/// with no fallback, after a switch, or when the fallback gives no
/// reading.
pub fn switch(floor_ns: u64) -> Option<u64> {
    let s = STATE.get()?;
    let _w = WRITER.lock();
    if s.live.which.load(Ordering::Relaxed) != 0 {
        return None;
    }
    let fb = s.sources[1].as_ref()?;
    let old = s.live.timeline();
    let seq = s.live.seq.load(Ordering::Relaxed);
    s.live.seq.store(seq + 1, Ordering::Relaxed);
    // As in rebase: the last TSC reading the old Timeline converts is
    // taken after the count turned odd, so every reader's TSC reading
    // before it gives at most what the new timeline starts at.
    cpu::mfence();
    let at = tsc();
    let Some(r) = fb.counter.raw() else {
        s.live.seq.store(seq + 2, Ordering::Release);
        return None;
    };
    let base_ns = old.ns(at).max(floor_ns);
    let new = match &fb.narrow {
        None => Timeline {
            base_ticks: r,
            base_ns,
            scale: fb.freq.scale(),
        },
        Some(ext) => {
            // No reader uses the extension before `which` says so.
            *ext.lock() = Extend::new(fb.bits, r);
            Timeline {
                base_ticks: 0,
                base_ns,
                scale: fb.freq.scale(),
            }
        }
    };
    s.live.write_fields(new, fb.freq);
    s.live.which.store(1, Ordering::Relaxed);
    s.live.seq.store(seq + 2, Ordering::Release);
    Some(base_ns)
}

/// Which clock is running.
pub fn kind() -> Option<Clock> {
    STATE.get().map(|s| s.current().kind)
}

/// The clock's counter, with its frequency now, for calibrating other
/// counters against the clock itself.
pub fn counter() -> Option<Counter<'static>> {
    STATE.get().map(|s| match s.current().counter {
        Counter::TscAt(_) => Counter::TscAt(s.live.freq()),
        c => c,
    })
}

/// While the clock is the TSC: a reading of it, as the clock reads it
/// (crate::time::watchdog checks these).
pub fn tsc_raw() -> Option<u64> {
    let s = STATE.get()?;
    matches!(s.current().kind, Clock::Tsc).then(tsc)
}

/// While the clock is a 64-bit counter: its time at the reading `raw`.
pub fn ns_at(raw: u64) -> Option<u64> {
    let s = STATE.get()?;
    s.current()
        .narrow
        .is_none()
        .then(|| s.live.timeline().ns(raw))
}

#[cfg(feature = "ktest")]
mod tests;
