//! The kernel clock's arithmetic: nanoseconds since boot from a counter.
//!
//! A [`Timeline`] maps counter readings to nanoseconds: `base_ns` at the
//! reading `base_ticks`, then the counter's frequency through the fast
//! [`Scale`]. It is monotonic (a later reading never gives fewer
//! nanoseconds; readings before the base give the base) and, because the
//! Scale is within one nanosecond of exact for any span, it does not
//! drift however long it runs.
//!
//! A better frequency (the 10 s refinement) is taken with [`rebase`]: the
//! new timeline starts where the old one is at that reading, so time
//! neither jumps nor runs backwards, only its rate changes.
//!
//! [`rebase`]: Timeline::rebase

use crate::freq::{Freq, Scale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeline {
    pub base_ticks: u64,
    pub base_ns: u64,
    pub scale: Scale,
}

impl Timeline {
    /// 0 ns at the reading `start`.
    pub fn new(freq: Freq, start: u64) -> Timeline {
        Timeline {
            base_ticks: start,
            base_ns: 0,
            scale: freq.scale(),
        }
    }

    /// Nanoseconds at the reading `ticks`, saturating at u64::MAX (584
    /// years). A reading before the base gives the base: on another CPU
    /// whose counter is a little behind, or read just before a rebase.
    pub fn ns(self, ticks: u64) -> u64 {
        self.base_ns.saturating_add(
            self.scale
                .ticks_to_ns(ticks.saturating_sub(self.base_ticks)),
        )
    }

    /// The same clock from the reading `ticks` on, at frequency `freq`.
    /// Continuous: the new timeline gives exactly what this one does at
    /// `ticks` (or at the base, if `ticks` is before it), and monotonic
    /// across the change: for every reading r >= ticks, new.ns(r) >=
    /// self.ns(ticks), and every reading before gives the old value at the
    /// base or later.
    pub fn rebase(self, ticks: u64, freq: Freq) -> Timeline {
        let at = ticks.max(self.base_ticks);
        Timeline {
            base_ticks: at,
            base_ns: self.ns(at),
            scale: freq.scale(),
        }
    }
}
