use crate::pic::*;

/// Master at 0x20 (IRQ 0 to 7 on 0x20 to 0x27), slave at 0x28.
#[test]
fn remap_hand_worked() {
    assert_eq!(
        remap_and_mask(0x20, 0x28).unwrap(),
        [
            (0x21, 0xFF),
            (0xA1, 0xFF),
            (0x20, 0x11),
            (0x21, 0x20),
            (0x21, 0x04),
            (0x21, 0x01),
            (0x21, 0xFF),
            (0xA0, 0x11),
            (0xA1, 0x28),
            (0xA1, 0x02),
            (0xA1, 0x01),
            (0xA1, 0xFF),
        ]
    );
}

#[test]
fn refuses_exception_vectors_and_misaligned_bases() {
    // The BIOS defaults (0x08 and 0x70): the master would sit on #DF.
    assert_eq!(remap_and_mask(0x08, 0x70), None);
    assert_eq!(remap_and_mask(0x20, 0x18), None);
    assert_eq!(remap_and_mask(0x21, 0x28), None);
    assert_eq!(remap_and_mask(0x20, 0x2C), None);
    assert!(remap_and_mask(0xF8, 0xF0).is_some());
    assert!(remap_and_mask(0x20, 0x20).is_some());
}

#[test]
fn spurious_vectors() {
    assert_eq!(spurious_vector(0x20), 0x27);
    assert_eq!(spurious_vector(0x28), 0x2F);
    assert_eq!(PROBE, 0xFB);
    assert_eq!(OCW3_READ_ISR, 0x0B);
    assert_eq!(EOI, 0x20);
}

/// A model 8259 (datasheet section "Initialization Command Words"): a
/// command-port write with bit 4 set starts initialisation and clears the
/// mask register; the next data writes are ICW2, ICW3 (cascade mode) and
/// ICW4 (if ICW1 asked for it); after that data writes set the mask.
#[derive(Default, Debug)]
struct Model {
    step: u8, // 0 idle/initialised, 1..=3 waiting for ICW2..ICW4
    ready: bool,
    base: u8,
    icw3: u8,
    icw4: u8,
    imr: u8,
}

impl Model {
    fn command(&mut self, v: u8) {
        assert!(v & 0x10 != 0, "only ICW1 is used on the command port");
        assert_eq!(v & 0x01, 1, "ICW4 needed");
        assert_eq!(v & 0x02, 0, "cascade mode");
        self.step = 1;
        self.ready = false;
        self.imr = 0;
    }
    fn data(&mut self, v: u8) {
        match self.step {
            1 => {
                self.base = v;
                self.step = 2;
            }
            2 => {
                self.icw3 = v;
                self.step = 3;
            }
            3 => {
                self.icw4 = v;
                self.step = 0;
                self.ready = true;
            }
            _ => self.imr = v,
        }
    }
    /// Could this chip deliver an interrupt right now?
    fn live(&self) -> bool {
        self.ready && self.imr != 0xFF
    }
}

/// Run the sequence through two model chips, both starting initialised and
/// unmasked (the worst firmware). A chip is only ever live in the gap ICW1
/// forces (straight after its ICW4), and the very next write masks it; both
/// end programmed and masked.
#[test]
fn sequence_against_model() {
    for (mb, sb) in [(0x20u8, 0x28u8), (0x40, 0x48), (0xF0, 0xF8)] {
        let mut m = Model {
            ready: true,
            base: 0x08,
            ..Model::default()
        };
        let mut s = Model {
            ready: true,
            base: 0x70,
            ..Model::default()
        };
        let seq = remap_and_mask(mb, sb).unwrap();
        for (i, &(port, v)) in seq.iter().enumerate() {
            match port {
                MASTER_CMD => m.command(v),
                MASTER_DATA => m.data(v),
                SLAVE_CMD => s.command(v),
                SLAVE_DATA => s.data(v),
                _ => panic!("port {port:#x}"),
            }
            if i >= 1 {
                for (chip, data) in [(&m, MASTER_DATA), (&s, SLAVE_DATA)] {
                    if chip.live() {
                        assert_eq!(seq[i - 3].1, ICW1_INIT_ICW4, "live after write {i}");
                        assert_eq!(seq[i].0, data);
                        assert_eq!(seq[i + 1], (data, ALL_MASKED), "live after write {i}");
                    }
                }
            }
        }
        assert!(m.ready && s.ready);
        assert_eq!((m.base, s.base), (mb, sb));
        assert_eq!((m.icw3, s.icw3), (0x04, 0x02));
        assert_eq!((m.icw4, s.icw4), (0x01, 0x01));
        assert_eq!((m.imr, s.imr), (0xFF, 0xFF));
    }
}
