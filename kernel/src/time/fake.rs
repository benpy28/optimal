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
//!
//! The kernel prints what it applied, so a scenario can check the fake was
//! in force.

use optimal_time::calibrate::Estimate;
use optimal_time::cpuid::{Claimed, Source};
use optimal_time::freq::Freq;
use optimal_time::refine;

use super::Time;
use crate::bootinfo::BootInfo;
use crate::sync::Once;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fake {
    pub invariant: bool,
    pub claim_ppm: Option<i64>,
}

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
        } else {
            panic!("tscfake: unknown word {w:?}");
        }
    }
    crate::println!(
        "tscfake: applied:{}{}",
        if f.invariant { " invariant" } else { "" },
        f.claim_ppm
            .map(|p| alloc::format!(" claim{p:+}"))
            .unwrap_or_default()
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
