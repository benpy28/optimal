//! The 8254 PIT's channel 2 as a calibration reference (Intel 8254
//! datasheet; the gate and output bits in port 0x61 are the PC/AT's).
//!
//! Channel 2 is the only channel whose gate software controls, and its
//! output goes only to the PC speaker (kept off), so using it disturbs
//! nothing. In mode 0 with a count of 0xFFFF it counts down and, past
//! zero, wraps to 0xFFFF and keeps counting, so it is a 16-bit counter
//! that wraps every 55 ms and can be extended like any other.

use crate::freq::Freq;

/// 315/264 MHz = 1,193,181.8181... Hz (the 14.31818 MHz crystal over 12),
/// in lowest terms 13,125,000 / 11.
pub const FREQ: Freq = match Freq::new(13_125_000, 11) {
    Some(f) => f,
    None => panic!("the PIT frequency is a valid Freq"),
};

pub const CHANNEL2: u16 = 0x42;
pub const COMMAND: u16 = 0x43;
/// NMI status and control (PC/AT port B).
pub const PORT_B: u16 = 0x61;

/// Port B bit 0: channel 2's gate.
pub const GATE2: u8 = 0x01;
/// Port B bit 1: speaker data enable.
pub const SPEAKER: u8 = 0x02;
/// Port B bits 4 to 7 read back status and must be written as 0.
pub const PORT_B_WRITABLE: u8 = 0x0F;

/// Channel 2, low then high byte, mode 0 (interrupt on terminal count),
/// binary: 10 11 000 0.
pub const MODE0: u8 = 0b1011_0000;
/// Latch channel 2's count for reading: 10 00 0000.
pub const LATCH: u8 = 0b1000_0000;

/// Port B with channel 2's gate open and the speaker off.
pub const fn gate_open(port_b: u8) -> u8 {
    (port_b & PORT_B_WRITABLE & !SPEAKER) | GATE2
}

/// Port B as it was before, made writable.
pub const fn restore(port_b: u8) -> u8 {
    port_b & PORT_B_WRITABLE
}

/// The port writes that start channel 2 counting from 0xFFFF in mode 0,
/// after port B is set with [`gate_open`]: the mode, then the count low
/// and high byte.
pub const START: [(u16, u8); 3] = [(COMMAND, MODE0), (CHANNEL2, 0xFF), (CHANNEL2, 0xFF)];

/// A latched down-count (low byte, high byte) as an up-counter value, so
/// that [`crate::counter::delta`] with 16 bits gives ticks elapsed.
pub const fn up(low: u8, high: u8) -> u64 {
    (!u16::from_le_bytes([low, high])) as u64
}
