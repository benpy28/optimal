//! Ending level-triggered interrupts at the I/O APIC, and clearing a
//! Remote IRR that stayed set.
//!
//! When an I/O APIC delivers a level-triggered interrupt it sets the
//! entry's Remote IRR bit and sends nothing more on that pin until an EOI
//! for the entry's vector reaches it (82093AA datasheet 3.2.4). There are
//! two ways an EOI gets there:
//!
//! - Broadcast: the local APIC, on an EOI for a vector its TMR marks
//!   level-triggered, sends the vector to every I/O APIC (SDM 11.8.5).
//! - Directed: the local APIC's SVR bit 12 suppresses that broadcast and
//!   the kernel writes the vector to the one I/O APIC's EOI register
//!   (version 0x20 and later). Once vectors are allocated per CPU (SMP), a
//!   broadcast from one CPU can clear Remote IRR on a pin aimed at another
//!   CPU that uses the same vector; a directed EOI cannot.
//!
//! [`choose`] picks directed EOI only when the local APIC can suppress
//! broadcasts and every used I/O APIC has a reachable EOI register (one
//! without it would never hear an EOI again). Otherwise broadcast.
//!
//! A Remote IRR can stay set anyway: firmware leaves one, an entry's
//! vector changes while its interrupt is in service (the EOI then names
//! the old vector), or the local APIC accepts a level interrupt as edge
//! (its TMR bit clear), so its EOI is never broadcast. The pin is then
//! dead. [`settle`] clears it: the EOI register where there is one, else
//! the trigger toggle (mask, edge, level: Intel's I/O APICs, QEMU's and
//! KVM's clear Remote IRR when an entry is written edge-triggered, which
//! is also what Linux relies on for the 82093AA), and if neither works,
//! the pin is left masked and reported stuck.

use crate::ioapic::{self, Redirection};
use crate::lapic;

/// How EOIs reach the I/O APICs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The local APIC broadcasts each level EOI to every I/O APIC.
    Broadcast(Why),
    /// Broadcasts suppressed (SVR bit 12); the kernel writes the vector
    /// to the I/O APIC's EOI register.
    Directed,
}

/// Why EOIs are broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The local APIC's version register has no suppression bit (24).
    LapicCannotSuppress,
    /// No I/O APIC is in use, so there is nothing to direct EOIs to.
    NoIoApic,
    /// This used I/O APIC (MADT index) has no EOI register (version below
    /// 0x20), so it would never get an EOI if broadcasts were suppressed.
    NoEoiRegister { index: usize, version: u8 },
    /// This used I/O APIC's EOI register is past the end of its page.
    EoiNotInPage { index: usize },
    /// The local APIC said it could suppress broadcasts, but SVR bit 12
    /// did not stay set when written.
    SuppressDidNotStick,
}

/// A used I/O APIC, as [`choose`] sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoApic {
    /// MADT index.
    pub index: usize,
    pub version: ioapic::Version,
    /// Physical address of its registers.
    pub address: u64,
}

impl IoApic {
    /// Can the kernel use this I/O APIC's EOI register?
    pub fn has_eoi(&self) -> bool {
        self.version.has_eoi() && ioapic::eoi_in_page(self.address)
    }
}

/// The EOI mode for a local APIC of version `lapic` and the I/O APICs in
/// use. The kernel then sets SVR bit 12 for [`Mode::Directed`] and reads
/// it back (falling back to [`Why::SuppressDidNotStick`]).
pub fn choose(lapic: lapic::Version, used: &[IoApic]) -> Mode {
    if !lapic.eoi_broadcast_suppression {
        return Mode::Broadcast(Why::LapicCannotSuppress);
    }
    if used.is_empty() {
        return Mode::Broadcast(Why::NoIoApic);
    }
    for io in used {
        if !io.version.has_eoi() {
            return Mode::Broadcast(Why::NoEoiRegister {
                index: io.index,
                version: io.version.version,
            });
        }
        if !io.has_eoi() {
            return Mode::Broadcast(Why::EoiNotInPage { index: io.index });
        }
    }
    Mode::Directed
}

/// One I/O APIC pin, as [`settle`] drives it.
pub trait Pin {
    /// The low dword of the pin's redirection entry.
    fn read_low(&mut self) -> u32;
    /// Write the low dword (the high dword, the destination, is left as
    /// it is).
    fn write_low(&mut self, value: u32);
    /// Write `vector` to the I/O APIC's EOI register. Only called when
    /// [`settle`] was told the I/O APIC has a usable one.
    fn eoi(&mut self, vector: u8);
}

/// How [`settle`] found and left the pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Remote IRR was clear; the entry is now `wanted`.
    Clear,
    /// Remote IRR was set and this cleared it; the entry is now `wanted`.
    Cleared(Method),
    /// Remote IRR is still set after every method: the entry is `wanted`
    /// but masked, because the pin can deliver nothing until it clears.
    Stuck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    EoiRegister,
    Toggle,
}

/// Leave the pin's entry as `wanted` with Remote IRR clear, clearing it if
/// needed. `eoi_register`: the I/O APIC has a usable EOI register.
/// `rereads`: extra reads after each method before trying the next, since
/// an EOI register write may take effect a little later than the write.
///
/// Every write but the last keeps the pin masked, so nothing can be
/// delivered from a half-done entry, and the last write is `wanted`
/// itself (masked when the pin stays stuck). While Remote IRR is set the
/// pin is held masked and level-triggered on the vector it has now, which
/// is the vector the EOI register must be given (it may differ from
/// `wanted`'s, if something changed it while an interrupt was in service).
///
/// The caller holds the I/O APIC's register lock and has already written
/// the high dword `wanted` needs.
pub fn settle(
    pin: &mut impl Pin,
    wanted: Redirection,
    eoi_register: bool,
    rereads: u32,
) -> Outcome {
    let now = Redirection::decode(pin.read_low(), 0);
    if !now.remote_irr {
        pin.write_low(wanted.encode().0);
        return Outcome::Clear;
    }
    let hold = Redirection {
        masked: true,
        level: true,
        ..now.entry
    };
    pin.write_low(hold.encode().0);
    let mut method = None;
    if eoi_register {
        pin.eoi(now.entry.vector);
        if cleared(pin, rereads) {
            method = Some(Method::EoiRegister);
        }
    }
    if method.is_none() {
        pin.write_low(
            Redirection {
                level: false,
                ..hold
            }
            .encode()
            .0,
        );
        pin.write_low(hold.encode().0);
        if cleared(pin, rereads) {
            method = Some(Method::Toggle);
        }
    }
    match method {
        Some(m) => {
            pin.write_low(wanted.encode().0);
            Outcome::Cleared(m)
        }
        None => {
            pin.write_low(
                Redirection {
                    masked: true,
                    ..wanted
                }
                .encode()
                .0,
            );
            Outcome::Stuck
        }
    }
}

/// Remote IRR reads clear within `1 + rereads` reads.
fn cleared(pin: &mut impl Pin, rereads: u32) -> bool {
    (0..=rereads).any(|_| pin.read_low() & ioapic::REMOTE_IRR == 0)
}
