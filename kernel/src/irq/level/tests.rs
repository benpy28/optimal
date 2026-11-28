//! Kernel tests for level lines and stuck Remote IRRs, on a real
//! level-triggered source: an HPET comparator in one-shot level mode,
//! routed to an I/O APIC input the plan leaves free (on QEMU's q35, GSI 16
//! to 23; its pc machine only offers GSI 2, which ISA IRQ 0 owns, so there
//! the tests say so and check the refusal instead).
//!
//! The comparator holds its line asserted from the match until its bit in
//! the HPET's interrupt status register is written (IA-PC HPET 1.0a,
//! 2.3.6 and 2.3.9), like any level device until its driver acknowledges
//! it. A stuck Remote IRR is made the way it happens on real machines:
//! the entry's vector changes while its interrupt is in service, so the
//! EOI (broadcast or directed, both by vector) never matches it.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use optimal_apic::eoi::{self, Method, Mode, Outcome};
use optimal_apic::ioapic::{self, Redirection};
use optimal_apic::plan::IoApicStatus;
use optimal_apic::{is_set, lapic};
use optimal_time::hpet;

use super::{ClaimError, Done, Line, NO_CLEAR, Stuck, claim, claim_isa, free};
use crate::arch::idt::InterruptFrame;
use crate::irq::{Controllers, IOAPIC_LOCK, clear_handler, controllers, set_handler};
use crate::ktest;
use crate::time::Hpet;

/// The level line's vector, and a stray one the tests move its entry to.
const VECTOR: u8 = 0x48;
const STRAY: u8 = 0x5E;

fn ctl() -> Option<&'static Controllers> {
    let c = controllers();
    if c.is_none() {
        crate::println!("ktest: interrupts are off on this machine; nothing to check");
    }
    c
}

fn wait_for(f: impl Fn() -> bool) -> bool {
    (0..50_000_000u64).any(|_| {
        core::hint::spin_loop();
        f()
    })
}

fn spin(n: u64) {
    for _ in 0..n {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// The HPET comparator as a level device.

struct Dev {
    hpet: &'static Hpet,
    n: u8,
    gsi: u32,
    /// The comparator's configuration as found (quiet, from time::init).
    config: u64,
    ticks: u64,
}

static COMPARATOR: AtomicU8 = AtomicU8::new(0);
/// The counter when the comparator was last armed (for failure reports).
static ARMED: AtomicU64 = AtomicU64::new(0);
/// One-shot armings the HPET never matched (see Dev::fire_until).
static MISSES: AtomicU64 = AtomicU64::new(0);
static FIRES: AtomicU64 = AtomicU64::new(0);
static SAW_REMOTE_IRR: AtomicBool = AtomicBool::new(false);
static SAW_MASKED: AtomicBool = AtomicBool::new(false);
static SAW_TMR: AtomicBool = AtomicBool::new(false);
static SAW_ISR: AtomicBool = AtomicBool::new(false);
static LINE_AT: AtomicU32 = AtomicU32::new(0);
static BEHAVIOUR: AtomicU8 = AtomicU8::new(ACK);
/// Acknowledge the device and say done.
const ACK: u8 = 0;
/// Leave the device asserting and say later.
const HOLD: u8 = 1;
/// Acknowledge, but move the entry to STRAY first (the EOI then misses).
const REVECTOR: u8 = 2;

fn device(c: &Controllers) -> Result<Dev, &'static str> {
    let t = crate::time::get().ok_or("no time setup")?;
    let h = t.hpet.as_ref().ok_or("no HPET")?;
    // Comparators the timer took for its events are left out (a zero
    // configuration has no routes).
    let taken = h.claimed();
    let configs: Vec<u64> = (0..h.caps.comparators)
        .map(|n| {
            if taken & (1 << n) != 0 {
                0
            } else {
                h.read(hpet::timer_config(n))
            }
        })
        .collect();
    let (n, gsi) = hpet::pick_level(&configs, |g| free(c, g).is_ok())
        .ok_or("no HPET comparator reaches a free I/O APIC input")?;
    Ok(Dev {
        hpet: h,
        n,
        gsi,
        config: configs[usize::from(n)],
        // 1 ms: far enough ahead that the counter cannot pass the
        // comparator before it is enabled, even under TCG.
        ticks: h.caps.freq().ns_to_ticks_ceil(1_000_000),
    })
}

impl Dev {
    /// Arm the comparator `ticks` ahead (one-shot, level). Returns the
    /// deadline.
    fn arm(&self) -> u64 {
        let h = self.hpet;
        let off = hpet::level_one_shot(self.config, self.gsi, false).expect("picked from the cap");
        let on = hpet::level_one_shot(self.config, self.gsi, true).expect("picked from the cap");
        h.write(hpet::timer_config(self.n), off);
        let now = h.counter();
        let d = hpet::deadline(now, self.ticks, self.config);
        h.write(hpet::timer_comparator(self.n), d);
        h.write(hpet::timer_config(self.n), on);
        ARMED.store(now, Ordering::SeqCst);
        d
    }

    /// Spin until `f` holds or the counter is `grace` ticks past
    /// `deadline`; then whether `f` holds.
    fn wait_past(&self, deadline: u64, f: impl Fn() -> bool) -> bool {
        let limit = deadline.wrapping_add(self.ticks * 100);
        loop {
            if f() {
                return true;
            }
            if hpet::passed(self.hpet.counter(), limit, self.config) {
                return f();
            }
            core::hint::spin_loop();
        }
    }

    /// Arm, and wait until the comparator asserts its line (status bit
    /// set) or `done` holds. Under QEMU 8.2 with multi-threaded TCG, an
    /// earlier version of this test that waited by spinning alone saw an
    /// arming whose status bit never set within 10 s of HPET time in 4 of
    /// 4 traced boots (once in about 100 armings); waiting while reading
    /// the HPET's counter, as this does, saw none in 6 boots. A comparator
    /// that never matches is the device's fault, not the kernel's, and
    /// nothing reached the I/O APIC to lose, so it is counted, printed and
    /// re-armed, at most 4 times per call. A status bit that is set while
    /// the interrupt never arrives is the kernel's, and fails at once.
    fn fire_until(&self, done: impl Fn() -> bool) {
        for _ in 0..5 {
            let d = self.arm();
            if self.wait_past(d, &done) {
                return;
            }
            if self.asserted() {
                fail_lost("the line was asserted but the interrupt never came");
            }
            MISSES.fetch_add(1, Ordering::SeqCst);
        }
        fail_lost("the HPET comparator missed 5 armings in a row");
    }

    /// Arm and wait for the handler's next run.
    fn fire(&self) {
        let want = FIRES.load(Ordering::SeqCst) + 1;
        self.fire_until(|| FIRES.load(Ordering::SeqCst) >= want);
    }

    /// Arm and wait for the line to assert, without the handler running
    /// (the pin is masked or held).
    fn assert_line(&self) {
        self.fire_until(|| self.asserted());
    }

    fn quiet(&self) {
        self.hpet
            .write(hpet::timer_config(self.n), hpet::quiet(self.config));
        ack(self.hpet, self.n);
    }

    fn asserted(&self) -> bool {
        self.hpet.read(hpet::INTERRUPT_STATUS) & (1 << self.n) != 0
    }
}

fn ack(h: &Hpet, n: u8) {
    h.write(hpet::INTERRUPT_STATUS, 1 << n);
}

fn entry_of(c: &Controllers, ioapic: usize, pin: u8) -> ioapic::Readback {
    let regs = c.ioapics[ioapic].unwrap();
    let _g = IOAPIC_LOCK.lock();
    let (lo, hi) = regs.entry(pin);
    Redirection::decode(lo, hi)
}

fn handler(frame: &mut InterruptFrame) -> Done {
    FIRES.fetch_add(1, Ordering::SeqCst);
    let c = controllers().unwrap();
    let at = LINE_AT.load(Ordering::SeqCst);
    let (ioapic, pin) = ((at >> 8) as usize, at as u8);
    let r = entry_of(c, ioapic, pin);
    SAW_REMOTE_IRR.store(r.remote_irr, Ordering::SeqCst);
    SAW_MASKED.store(r.entry.masked, Ordering::SeqCst);
    let v = frame.vector as u8;
    SAW_TMR.store(is_set(&c.lapic.read_array(lapic::TMR), v), Ordering::SeqCst);
    SAW_ISR.store(is_set(&c.lapic.read_array(lapic::ISR), v), Ordering::SeqCst);
    let h = crate::time::get().unwrap().hpet.as_ref().unwrap();
    let n = COMPARATOR.load(Ordering::SeqCst);
    match BEHAVIOUR.load(Ordering::SeqCst) {
        HOLD => Done::Later,
        REVECTOR => {
            ack(h, n);
            let regs = c.ioapics[ioapic].unwrap();
            let moved = Redirection {
                vector: STRAY,
                masked: true,
                ..r.entry
            };
            let _g = IOAPIC_LOCK.lock();
            regs.write(ioapic::redirection_regs(pin).unwrap().0, moved.encode().0);
            Done::Now
        }
        _ => {
            ack(h, n);
            Done::Now
        }
    }
}

/// Claim the device's line and reset the test state. None (with the
/// reason printed) where there is no device to test with.
fn setup() -> Option<(&'static Controllers, Dev, Line)> {
    let c = ctl()?;
    let dev = match device(c) {
        Ok(d) => d,
        Err(why) => {
            crate::println!("ktest: {why}; nothing to check");
            return None;
        }
    };
    dev.quiet();
    let line = claim(dev.gsi, false, VECTOR, handler).expect("the picked GSI is free");
    COMPARATOR.store(dev.n, Ordering::SeqCst);
    LINE_AT.store(
        (line.ioapic as u32) << 8 | u32::from(line.pin),
        Ordering::SeqCst,
    );
    FIRES.store(0, Ordering::SeqCst);
    BEHAVIOUR.store(ACK, Ordering::SeqCst);
    for f in [&SAW_REMOTE_IRR, &SAW_MASKED, &SAW_TMR, &SAW_ISR] {
        f.store(false, Ordering::SeqCst);
    }
    Some((c, dev, line))
}

fn teardown(dev: Dev, line: Line) {
    dev.quiet();
    line.release();
    BEHAVIOUR.store(ACK, Ordering::SeqCst);
    NO_CLEAR.store(false, Ordering::SeqCst);
}

fn counters() -> (u64, u64, u64) {
    (
        super::CLEARED_EOI.load(Ordering::SeqCst),
        super::CLEARED_TOGGLE.load(Ordering::SeqCst),
        super::STUCK_PINS.load(Ordering::SeqCst),
    )
}

/// The method that clears a stuck Remote IRR on this line's I/O APIC.
fn expected_method(c: &Controllers, line: &Line) -> Method {
    if c.eoi_register[line.ioapic] {
        Method::EoiRegister
    } else {
        Method::Toggle
    }
}

/// Wait for the handler's `n`th run in all, after something other than
/// arming made it due (an unmask or done() on an asserted line, which the
/// I/O APIC delivers at once).
fn wait_fires(n: u64) {
    if !wait_for(|| FIRES.load(Ordering::SeqCst) >= n) {
        fail_lost("the level interrupt never arrived");
    }
}

fn fail_lost(what: &str) -> ! {
    let c = controllers().unwrap();
    let at = LINE_AT.load(Ordering::SeqCst);
    let h = crate::time::get().unwrap().hpet.as_ref().unwrap();
    let n = COMPARATOR.load(Ordering::SeqCst);
    panic!(
        "{what}: fires {}, entry {:?}, HPET status {:#x}, config {:#x}, comparator {:#x}, armed at {:#x}, counter {:#x}, IRR {:x?}, ISR {:x?}",
        FIRES.load(Ordering::SeqCst),
        entry_of(c, (at >> 8) as usize, at as u8),
        h.read(hpet::INTERRUPT_STATUS),
        h.read(hpet::timer_config(n)),
        h.read(hpet::timer_comparator(n)),
        ARMED.load(Ordering::SeqCst),
        h.counter(),
        c.lapic.read_array(lapic::IRR),
        c.lapic.read_array(lapic::ISR),
    );
}

// ---------------------------------------------------------------------------
// Tests.

// The EOI mode is what optimal_apic::eoi::choose says for this local APIC
// and these I/O APICs, and SVR bit 12 is set exactly in directed mode.
ktest!(eoi_mode_follows_the_rule, {
    let Some(c) = ctl() else { return };
    let used: Vec<eoi::IoApic> = c
        .plan
        .ioapics
        .iter()
        .enumerate()
        .filter(|(_, io)| io.status == IoApicStatus::Used)
        .map(|(i, io)| eoi::IoApic {
            index: i,
            version: c.ioapic_versions[i].unwrap(),
            address: u64::from(io.address),
        })
        .collect();
    let rule = eoi::choose(c.version, &used);
    if c.eoi != Mode::Broadcast(eoi::Why::SuppressDidNotStick) {
        assert_eq!(c.eoi, rule);
    } else {
        assert_eq!(rule, Mode::Directed);
    }
    let svr = c.lapic.read(lapic::SVR);
    assert_eq!(
        svr & lapic::SVR_SUPPRESS_EOI_BROADCAST != 0,
        c.eoi == Mode::Directed,
        "SVR {svr:#x}"
    );
    for (i, io) in used.iter().enumerate() {
        assert_eq!(c.eoi_register[io.index], io.has_eoi(), "I/O APIC {i}");
    }
});

// After setup no used pin has Remote IRR set.
ktest!(no_pin_left_with_remote_irr, {
    let Some(c) = ctl() else { return };
    for (i, io) in c.plan.ioapics.iter().enumerate() {
        if io.status != IoApicStatus::Used {
            continue;
        }
        for pin in 0..io.pins.min(ioapic::MAX_PINS) as u8 {
            assert!(!entry_of(c, i, pin).remote_irr, "I/O APIC {i} pin {pin}");
        }
    }
});

// Every refusal, by name, and nothing is left claimed by a refused call.
ktest!(level_claims_are_refused_by_name, {
    let Some(c) = ctl() else { return };
    let h: super::LevelHandler = |_| Done::Now;
    for v in [0x30, 0x3F, 0xE0, 0xF0, 0xFF] {
        assert_eq!(claim(16, false, v, h), Err(ClaimError::NotDeviceVector(v)));
    }
    assert_eq!(
        claim(0xFFFF_FFF0, false, VECTOR, h),
        Err(ClaimError::NoIoApic { gsi: 0xFFFF_FFF0 })
    );
    if let Some(r) = c.plan.isa_route(0) {
        assert_eq!(
            claim(r.gsi, false, VECTOR, h),
            Err(ClaimError::PlanUses { gsi: r.gsi })
        );
    }
    if let Some(r) = c.plan.isa_route(1) {
        assert!(!r.entry.level);
        assert_eq!(claim_isa(1, h), Err(ClaimError::NotLevel { irq: 1 }));
    }
    assert!(matches!(claim_isa(2, h), Err(ClaimError::NotRouted(_))));
    // A free GSI: once claimed, its pin and its vector are both taken.
    let free_gsi = c.plan.ioapics.iter().find_map(|io| {
        (io.status == IoApicStatus::Used)
            .then(|| (io.gsi_base..io.gsi_base + u32::from(io.pins)).find(|&g| free(c, g).is_ok()))
            .flatten()
    });
    let Some(g) = free_gsi else {
        crate::println!("ktest: no free GSI here; claim and busy not checked");
        return;
    };
    let line = claim(g, false, VECTOR, h).unwrap();
    assert_eq!(
        claim(g, false, VECTOR + 1, h),
        Err(ClaimError::PinBusy { gsi: g })
    );
    if let Some(g2) = (g + 1..g + 8).find(|&x| free(c, x).is_ok()) {
        assert_eq!(
            claim(g2, false, VECTOR, h),
            Err(ClaimError::VectorBusy(VECTOR))
        );
    }
    // A vector with an edge handler is busy too.
    set_handler(VECTOR + 2, |_| {});
    if let Some(g2) = (g + 1..g + 8).find(|&x| free(c, x).is_ok()) {
        assert_eq!(
            claim(g2, false, VECTOR + 2, h),
            Err(ClaimError::VectorBusy(VECTOR + 2))
        );
    }
    clear_handler(VECTOR + 2);
    // Claimed lines start masked, level, on their vector.
    let e = entry_of(c, line.ioapic, line.pin);
    assert!(e.entry.masked && e.entry.level && !e.remote_irr);
    assert_eq!(e.entry.vector, VECTOR);
    line.release();
    let e = entry_of(c, line.ioapic, line.pin);
    assert_eq!(
        e.entry,
        optimal_apic::plan::unrouted(c.plan.destination.unwrap())
    );
    assert!(free(c, g).is_ok());
});

// 25 level interrupts, each arriving once: in the handler the pin is
// masked with Remote IRR set, the local APIC has the vector in service and
// its TMR says level; afterwards Remote IRR is clear, the pin is unmasked,
// nothing is in service, and no stuck Remote IRR was needed.
ktest!(level_interrupts_arrive_once_and_retire, {
    let Some((c, dev, line)) = setup() else {
        return;
    };
    line.unmask().unwrap();
    let before = counters();
    let seen = crate::irq::count(VECTOR);
    for k in 1..=25u64 {
        dev.fire();
        // No second delivery: the device was acknowledged.
        spin(200_000);
        assert_eq!(FIRES.load(Ordering::SeqCst), k, "round {k}");
        assert!(
            SAW_REMOTE_IRR.load(Ordering::SeqCst),
            "round {k}: no Remote IRR"
        );
        assert!(
            SAW_MASKED.load(Ordering::SeqCst),
            "round {k}: not masked in the handler"
        );
        assert!(
            SAW_TMR.load(Ordering::SeqCst),
            "round {k}: the local APIC took it as edge"
        );
        assert!(SAW_ISR.load(Ordering::SeqCst), "round {k}: not in service");
        let e = entry_of(c, line.ioapic, line.pin);
        assert!(!e.remote_irr && !e.entry.masked, "round {k}: {e:?}");
        assert!(!dev.asserted());
        assert_eq!(c.lapic.read_array(lapic::ISR), [0; 8], "round {k}");
    }
    assert_eq!(counters(), before, "a Remote IRR needed clearing");
    // Masked, an asserted level line waits; unmasked, it arrives once.
    line.mask();
    dev.assert_line();
    spin(1_000_000);
    assert_eq!(FIRES.load(Ordering::SeqCst), 25, "delivered while masked");
    line.unmask().unwrap();
    wait_fires(26);
    spin(200_000);
    assert_eq!(FIRES.load(Ordering::SeqCst), 26);
    assert_eq!(crate::irq::count(VECTOR) - seen, 26);
    crate::println!(
        "ktest: one-shot armings the HPET never matched, re-armed: {}",
        MISSES.load(Ordering::SeqCst)
    );
    crate::println!(
        "level: hpet comparator {} on GSI {} (I/O APIC {} pin {}): 26 interrupts, {}",
        dev.n,
        dev.gsi,
        line.ioapic,
        line.pin,
        match c.eoi {
            Mode::Directed => "directed EOI",
            Mode::Broadcast(_) => "broadcast EOI",
        }
    );
    teardown(dev, line);
});

// A handler that is not done keeps the line masked however long the
// device asserts it; done() while it still asserts delivers it again (it
// is level), and done() after the device lets go delivers nothing.
ktest!(held_level_line_waits_for_done, {
    let Some((c, dev, line)) = setup() else {
        return;
    };
    line.unmask().unwrap();
    BEHAVIOUR.store(HOLD, Ordering::SeqCst);
    dev.fire();
    spin(2_000_000);
    assert_eq!(
        FIRES.load(Ordering::SeqCst),
        1,
        "a held line was delivered again"
    );
    assert!(dev.asserted());
    let e = entry_of(c, line.ioapic, line.pin);
    assert!(e.entry.masked && !e.remote_irr, "{e:?}");
    // Still asserted: done() re-delivers once, and the handler holds again.
    line.done().unwrap();
    wait_fires(2);
    spin(2_000_000);
    assert_eq!(FIRES.load(Ordering::SeqCst), 2);
    // Let go, then done(): quiet.
    ack(dev.hpet, dev.n);
    BEHAVIOUR.store(ACK, Ordering::SeqCst);
    line.done().unwrap();
    spin(2_000_000);
    assert_eq!(
        FIRES.load(Ordering::SeqCst),
        2,
        "delivered after the device let go"
    );
    let e = entry_of(c, line.ioapic, line.pin);
    assert!(!e.entry.masked && !e.remote_irr);
    teardown(dev, line);
});

// The entry's vector changes while its interrupt is in service, so the
// EOI misses and Remote IRR stays set. The kernel finds it when it
// unmasks, clears it with the EOI register (version 0x20 I/O APICs) or the
// trigger toggle (KVM's, the 82093AA), and puts the vector back; the next
// interrupt arrives normally.
ktest!(stuck_remote_irr_is_cleared, {
    let Some((c, dev, line)) = setup() else {
        return;
    };
    line.unmask().unwrap();
    let (eoi0, toggle0, stuck0) = counters();
    BEHAVIOUR.store(REVECTOR, Ordering::SeqCst);
    dev.fire();
    spin(200_000);
    let (eoi1, toggle1, stuck1) = counters();
    assert_eq!(stuck1, stuck0);
    match expected_method(c, &line) {
        Method::EoiRegister => assert_eq!((eoi1 - eoi0, toggle1 - toggle0), (1, 0)),
        Method::Toggle => assert_eq!((eoi1 - eoi0, toggle1 - toggle0), (0, 1)),
    }
    let e = entry_of(c, line.ioapic, line.pin);
    assert!(!e.remote_irr && !e.entry.masked, "{e:?}");
    assert_eq!(e.entry.vector, VECTOR);
    assert!(!line.stuck());
    assert_eq!(crate::irq::count(STRAY), 0);
    BEHAVIOUR.store(ACK, Ordering::SeqCst);
    dev.fire();
    spin(200_000);
    assert_eq!(FIRES.load(Ordering::SeqCst), 2);
    assert_eq!(counters(), (eoi1, toggle1, stuck1));
    teardown(dev, line);
});

// A pin whose Remote IRR nothing clears (an I/O APIC that ignores both
// the EOI register and the toggle, modelled by NO_CLEAR) is left masked
// and reported stuck, unmask() keeps refusing, and once the I/O APIC
// behaves the setup sweep's settle clears it and names the method.
ktest!(pin_that_stays_stuck_is_left_masked, {
    let Some((c, dev, line)) = setup() else {
        return;
    };
    line.unmask().unwrap();
    let (_, _, stuck0) = counters();
    NO_CLEAR.store(true, Ordering::SeqCst);
    BEHAVIOUR.store(REVECTOR, Ordering::SeqCst);
    dev.fire();
    spin(200_000);
    assert!(line.stuck());
    assert_eq!(counters().2, stuck0 + 1);
    let e = entry_of(c, line.ioapic, line.pin);
    assert!(e.remote_irr && e.entry.masked, "{e:?}");
    assert_eq!(e.entry.vector, VECTOR, "the wanted vector is kept, masked");
    assert_eq!(line.unmask(), Err(Stuck));
    assert_eq!(counters().2, stuck0 + 2);
    // A masked pin delivers nothing however often the device asks.
    BEHAVIOUR.store(ACK, Ordering::SeqCst);
    dev.assert_line();
    spin(2_000_000);
    assert_eq!(FIRES.load(Ordering::SeqCst), 1);
    ack(dev.hpet, dev.n);
    // The setup sweep's path, on this pin.
    NO_CLEAR.store(false, Ordering::SeqCst);
    let regs = c.ioapics[line.ioapic].unwrap();
    let masked = Redirection {
        masked: true,
        ..line.entry
    };
    let out = {
        let _g = IOAPIC_LOCK.lock();
        super::settle(regs, line.pin, masked, c.eoi_register[line.ioapic])
    };
    assert_eq!(out, Outcome::Cleared(expected_method(c, &line)));
    let msg = crate::irq::sweep_message(line.ioapic, line.pin, out);
    assert!(
        msg.ends_with(match expected_method(c, &line) {
            Method::EoiRegister => "cleared by the EOI register",
            Method::Toggle => "cleared by the trigger toggle",
        }),
        "{msg}"
    );
    line.unmask().unwrap();
    assert!(!line.stuck());
    dev.fire();
    assert_eq!(FIRES.load(Ordering::SeqCst), 2);
    teardown(dev, line);
});

// Where no comparator reaches a free input (QEMU's pc machine: GSI 2
// only, which is ISA IRQ 0's), the device search says so, and a claim of
// that input is refused because the plan uses it.
ktest!(level_device_search_is_honest, {
    let Some(c) = ctl() else { return };
    let Some(h) = crate::time::get().and_then(|t| t.hpet.as_ref()) else {
        crate::println!("ktest: no HPET; nothing to check");
        return;
    };
    let configs: Vec<u64> = (0..h.caps.comparators)
        .map(|n| h.read(hpet::timer_config(n)))
        .collect();
    let all: u32 = configs
        .iter()
        .map(|&c| hpet::route_cap(c))
        .fold(0, |a, b| a | b);
    let free_in_cap = (0..32)
        .filter(|&g| all & (1 << g) != 0 && free(c, g).is_ok())
        .count();
    assert_eq!(device(c).is_ok(), free_in_cap > 0);
    for g in (0..32).filter(|&g| all & (1 << g) != 0) {
        match free(c, g) {
            Ok(_) => {}
            Err(e) => assert!(
                matches!(
                    e,
                    ClaimError::PlanUses { .. }
                        | ClaimError::NoIoApic { .. }
                        | ClaimError::PinBusy { .. }
                ),
                "GSI {g}: {e:?}"
            ),
        }
    }
    crate::println!("ktest: HPET route capability {all:#010x}; {free_in_cap} free inputs");
});
