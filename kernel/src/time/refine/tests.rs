//! Kernel tests for the 10 s refinement: it finishes (or says why it did
//! not run), its result is what optimal_time::refine decides for this
//! machine, the clock never goes backwards while it lands, and what the
//! timer arms with moves with the clock.

use optimal_time::calibrate::Estimate;
use optimal_time::freq::Freq;
use optimal_time::refine::{self as rf, Boot, Outcome};
use optimal_time::select::{Clock, Events, Reference};
use optimal_time::watchdog as wd;

use super::{Skip, Status, WAIT_NS, WARN_PPB, report, status};
use crate::clock;
use crate::ktest;
use crate::time::{self, fake, watchdog};
use crate::timer;

/// Slack on top of the boot bound when checking a faked claim's offset
/// (see crate::time::tests): 20 ppm.
const SLACK_PPB: u64 = 20_000;

ktest!(refinement_finishes_with_the_right_result, {
    // Wait for it, reading the clock all the while: it must never go
    // backwards, including at the moment the refinement rebases it.
    let t0 = clock::now_ns();
    let mut last = t0;
    while matches!(status(), Status::NotStarted | Status::Running) {
        let n = clock::now_ns();
        assert!(n >= last, "the clock went back from {last} to {n}");
        last = n;
        assert!(
            n - t0 <= WAIT_NS,
            "the refinement did not finish in {WAIT_NS} ns"
        );
        core::hint::spin_loop();
    }
    report();
    let t = time::get().unwrap();
    let tm = timer::get().unwrap();
    match status() {
        Status::NotStarted | Status::Running => unreachable!(),
        Status::Skipped(Skip::ClockNotTsc(k)) => {
            assert_eq!(k, clock::kind());
            assert!(!matches!(k, Some(Clock::Tsc | Clock::TscNotInvariant)));
        }
        Status::Skipped(Skip::Pit(_)) => assert_eq!(t.reference, Some(Reference::Pit)),
        Status::Skipped(Skip::NoReference) => assert_eq!(t.reference, None),
        Status::Skipped(Skip::NoEvents) => assert!(tm.events.is_none()),
        Status::Failed(f) => panic!("the refinement failed: {f:?}"),
        Status::Done {
            boot,
            measured,
            outcome,
            ..
        } => {
            assert!(
                measured.ppb <= WARN_PPB,
                "only +/-{} ppb after 10 s",
                measured.ppb
            );
            match (fake::current().claim_ppm, outcome) {
                (Some(ppm), Outcome::ClaimWrong(off)) => {
                    // The claim was the boot measurement (within its
                    // bound, at most time::WARN_PPB without a warning)
                    // off by ppm, so the TSC runs about -ppm from it.
                    let want = -ppm * 1000;
                    let allowed = time::WARN_PPB + SLACK_PPB;
                    assert!(
                        off.abs_diff(want) <= allowed,
                        "off by {off} ppb from a claim faked {want} ppb away"
                    );
                }
                (Some(_), o) => panic!("a faked claim gave {o:?}"),
                (None, Outcome::Refined | Outcome::KeptBoot | Outcome::Confirmed(_)) => {}
                (None, o) => panic!("{o:?} on a steady TSC"),
            }
            // The decision is optimal_time's for these values.
            assert_eq!(rf::decide(boot, measured), outcome);
            let boot_f = t.tsc_estimate().unwrap().freq;
            let rates = timer::rates();
            // After a faked TSC fault the watchdog may have moved the
            // clock off the TSC, carrying the LAPIC timer's rate across
            // (crate::time::watchdog's tests check the rest).
            let fell = watchdog::tests::fell_back();
            let carried = |l: Option<Estimate>| match fell {
                Some(bound) => l.map(|l| wd::carry(l, bound)),
                None => l,
            };
            if outcome.adopts() {
                if fell.is_none() {
                    assert_eq!(clock::freq(), Some(measured.freq));
                }
                // The LAPIC timer's rate moved by the same factor.
                let want = tm
                    .lapic
                    .map(|l| rf::rescale(l, boot_f, measured.freq).unwrap());
                assert_eq!(rates.lapic, carried(want));
                // So did the HPET's or the PIT's, when they drive events.
                if let Some((_, _, h)) = tm.hpet {
                    let want = rf::rescale(h, boot_f, measured.freq).unwrap();
                    assert_eq!(rates.hpet, carried(Some(want)));
                }
                if let Some(p) = tm.pit {
                    let want = rf::rescale(p, boot_f, measured.freq).unwrap();
                    assert_eq!(rates.pit, carried(Some(want)));
                }
            } else {
                if fell.is_none() {
                    assert_eq!(clock::freq(), Some(boot_f));
                }
                assert_eq!(rates.lapic, carried(tm.lapic));
                if let Some((_, _, h)) = tm.hpet {
                    assert_eq!(rates.hpet, carried(Some(h)));
                }
                if let Some(p) = tm.pit {
                    assert_eq!(rates.pit, carried(Some(p)));
                }
            }
            // TSC-deadline mode arms with the clock's own frequency.
            if timer::events() == Some(Events::TscDeadline) && fell.is_none() {
                assert_eq!(rates.tsc, clock::freq());
            }
            crate::println!(
                "ktest: refinement {outcome:?}; boot {}",
                match boot {
                    Boot::Measured(e) => alloc::format!("measured {}", time::Mhz(e.freq)),
                    Boot::Claimed { freq, .. } => alloc::format!("claimed {}", time::Mhz(freq)),
                }
            );
        }
    }
});

/// `f` off by `ppb` (exactly representable steps of 1/1024 Hz).
fn nudge(f: Freq, ppb: u64) -> Freq {
    let e = Estimate {
        freq: f,
        ppb: 0,
        window: 0,
    };
    let g = 1_000_000_000;
    rf::rescale(e, Freq::hz(g).unwrap(), Freq::hz(g + ppb).unwrap())
        .unwrap()
        .freq
}

ktest!(a_rebase_is_continuous_and_monotonic, {
    if !matches!(clock::kind(), Some(Clock::Tsc | Clock::TscNotInvariant)) {
        assert!(!clock::rebase(Freq::hz(1_000_000_000).unwrap()));
        crate::println!("ktest: the clock is not the TSC; rebase refused");
        return;
    }
    // With the timer held, so nothing arms with the rates while the
    // clock is briefly 1000 ppm faster, then put back exactly.
    let (jump, worst) = timer::hold(|| {
        let f = clock::freq().unwrap();
        let fast = nudge(f, 1_000_000);
        let mut worst = 0;
        let mut jump = 0;
        for _ in 0..100 {
            let n0 = clock::now_ns();
            assert!(clock::rebase(fast));
            let n1 = clock::now_ns();
            assert!(n1 >= n0, "rebase moved the clock back from {n0} to {n1}");
            jump = jump.max(n1 - n0);
            assert_eq!(clock::freq(), Some(fast));
            let mut last = n1;
            for _ in 0..1000 {
                let n = clock::now_ns();
                assert!(n >= last);
                last = n;
            }
            assert!(clock::rebase(f));
            let n2 = clock::now_ns();
            assert!(
                n2 >= last,
                "rebase back moved the clock from {last} to {n2}"
            );
            worst = worst.max(n2 - last);
            assert_eq!(clock::freq(), Some(f));
        }
        (jump, worst)
    });
    // Two clock reads around a rebase are microseconds apart, even
    // emulated: a rebase never jumps the clock.
    assert!(jump < 1_000_000 && worst < 1_000_000, "{jump} / {worst} ns");
    crate::println!(
        "ktest: 200 rebases, at most {} ns between reads",
        jump.max(worst)
    );
});
