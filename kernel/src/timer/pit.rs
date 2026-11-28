//! Timer events from PIT channel 0 in mode 0 on ISA IRQ 0: the last
//! resort (optimal_time::select::events). Each arming is three port
//! writes and covers at most 54.9 ms (optimal_time::oneshot::pit_count).
//!
//! The rate is measured on channel 2, whose gate software controls and
//! whose output reaches only the speaker (kept off); all three channels
//! count the same 1.193 MHz input clock (Intel 8254 datasheet; the PC/AT
//! wires one CLK to all three).

use alloc::format;
use alloc::string::String;

use optimal_time::calibrate::Estimate;

use crate::arch::idt::InterruptFrame;
use crate::clock;
use crate::irq;
use crate::time::{self, Counter};

/// Set channel 0 up for events on ISA IRQ 0, with the PIT's rate against
/// the clock. Or say why not.
pub fn setup() -> Result<Estimate, String> {
    let c = irq::controllers().ok_or("interrupts are off")?;
    let r = c.plan.isa_route(0).ok_or("ISA IRQ 0 is not routed")?;
    if r.entry.level {
        return Err(String::from("ISA IRQ 0 is level-triggered"));
    }
    let against = clock::counter().ok_or("no clock to measure the PIT against")?;
    // OUT held low: nothing reaches IRQ 0 until an arming.
    time::pit0_disarm();
    let port_b = time::pit_start();
    let rate = time::measure_to(
        against,
        Counter::Pit,
        super::LAPIC_GOAL_PPB,
        super::LAPIC_WINDOW_NS,
    );
    time::pit_stop(port_b);
    let rate = rate.map_err(|e| format!("could not measure the PIT: {e}"))?;
    // Refused while the HPET's legacy replacement holds ISA IRQ 0
    // (crate::timer::hpet).
    if !irq::try_set_handler(r.entry.vector, interrupt) {
        return Err(format!(
            "ISA IRQ 0's vector {:#x} already has a handler (the HPET's legacy replacement)",
            r.entry.vector
        ));
    }
    if let Err(e) = irq::isa_unmask(0) {
        irq::clear_handler(r.entry.vector);
        return Err(format!("ISA IRQ 0: {e:?}"));
    }
    Ok(rate)
}

/// Give channel 0 and ISA IRQ 0 back: masked, no handler, OUT low.
#[cfg(feature = "ktest")]
pub fn release() {
    let _ = irq::isa_mask(0);
    if let Some(r) = irq::controllers().and_then(|c| c.plan.isa_route(0)) {
        irq::clear_handler(r.entry.vector);
    }
    time::pit0_disarm();
}

fn interrupt(_frame: &mut InterruptFrame) {
    super::interrupt();
}
