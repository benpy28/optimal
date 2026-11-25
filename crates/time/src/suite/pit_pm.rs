use crate::counter::delta;
use crate::{pit, pm};

#[test]
fn pm_widths() {
    assert_eq!(pm::bits(false), 24);
    assert_eq!(pm::bits(true), 32);
}

#[test]
fn pit_port_b() {
    // Every value port B can read back: bits 4 to 7 never written, the
    // speaker off, the gate open; restore keeps the low four bits.
    for b in 0..=255u8 {
        let o = pit::gate_open(b);
        assert_eq!(o & 0xF0, 0);
        assert_eq!(o & pit::SPEAKER, 0);
        assert_eq!(o & pit::GATE2, pit::GATE2);
        assert_eq!(o & 0x0C, b & 0x0C);
        assert_eq!(pit::restore(b), b & 0x0F);
    }
    // Hand-worked: 0x30 (status bits, gate closed) opens as 0x01; 0x23
    // (OUT2, speaker on, gate open) as 0x01; 0x0C as 0x0D.
    assert_eq!(pit::gate_open(0x30), 0x01);
    assert_eq!(pit::gate_open(0x23), 0x01);
    assert_eq!(pit::gate_open(0x0C), 0x0D);
}

#[test]
fn pit_commands() {
    // Channel 2 (10), low then high byte (11), mode 0 (000), binary (0).
    assert_eq!(pit::MODE0, 0xB0);
    // Channel 2 (10), latch (00).
    assert_eq!(pit::LATCH, 0x80);
    assert_eq!(pit::START, [(0x43, 0xB0), (0x42, 0xFF), (0x42, 0xFF)]);
}

#[test]
fn pit_counts_up_when_inverted() {
    // The down-count 0xFFFF is up-count 0; 0xFFFE (one tick later) is 1;
    // 0x0000 is 0xFFFF; then it wraps back to 0xFFFF, up-count 0 again.
    assert_eq!(pit::up(0xFF, 0xFF), 0);
    assert_eq!(pit::up(0xFE, 0xFF), 1);
    assert_eq!(pit::up(0x00, 0x00), 0xFFFF);
    assert_eq!(pit::up(0x34, 0x12), 0xFFFF - 0x1234);
    assert_eq!(delta(pit::up(0x00, 0x00), pit::up(0xFF, 0xFF), 16), 1);
    assert_eq!(delta(pit::up(0x10, 0x00), pit::up(0xF0, 0xFF), 16), 0x20);
}
