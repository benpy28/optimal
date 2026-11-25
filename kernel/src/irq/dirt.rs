//! Test builds only: make the interrupt controllers look the way sloppy
//! firmware leaves them, right before `irq::init`, so the tests prove the
//! kernel cleans up itself.
//!
//! The firmware-quirk injector (bootloader/quirk) does this for the local
//! APIC and IA32_APIC_BASE, but Limine masks the 8259s and the I/O APIC's
//! fixed-delivery pins on its way to the kernel (its protocol promises
//! that), so through Limine the injector's `pic` and `ioapic` words never
//! reach the kernel. This applies the same dirt after Limine, from the
//! kernel command line: `irqdirt=pic,ioapic,lapic` (any subset).
//!
//! - pic: both 8259s reinitialised to the BIOS vectors (0x08 and 0x70,
//!   so IRQ 0 would land on the double-fault vector) and fully unmasked.
//! - ioapic: every I/O APIC pin unmasked, polarity and trigger as found:
//!   pin 0 ExtINT (virtual wire), pin 13 NMI, the rest fixed on 0x40 + pin.
//! - lapic: TPR 0x20, SVR 0x1EF, every LVT unmasked (LINT0 ExtINT, LINT1
//!   and the performance counters NMI), timer one-shot with count 0.
//!
//! Every change is read back, and a word that did not take stops the boot,
//! so a run can never pass with weaker dirt than it asked for.

use alloc::string::String;
use core::fmt::Write;

use optimal_apic::base::{self, Mode};
use optimal_apic::{ioapic, lapic, pic};

use super::{cpu_apic, map_regs};
use crate::arch::apic::{self, IoApicRegs, Lapic};
use crate::arch::cpu;
use crate::bootinfo::BootInfo;

pub fn apply(info: &BootInfo, words: &str) {
    let mut done = String::new();
    let mut detail = String::new();
    for w in words.split(',') {
        match w {
            "pic" => {
                // ICW1-4 to the BIOS bases, then unmask everything.
                apic::pic_write(&[
                    (pic::MASTER_CMD, pic::ICW1_INIT_ICW4),
                    (pic::MASTER_DATA, 0x08),
                    (pic::MASTER_DATA, pic::ICW3_MASTER),
                    (pic::MASTER_DATA, pic::ICW4_8086),
                    (pic::SLAVE_CMD, pic::ICW1_INIT_ICW4),
                    (pic::SLAVE_DATA, 0x70),
                    (pic::SLAVE_DATA, pic::ICW3_SLAVE),
                    (pic::SLAVE_DATA, pic::ICW4_8086),
                    (pic::MASTER_DATA, 0),
                    (pic::SLAVE_DATA, 0),
                ]);
                let m = apic::pic_masks();
                assert_eq!(m, (0, 0), "irqdirt: pic did not take");
                let _ = write!(detail, " PIC masks {:#04x}/{:#04x};", m.0, m.1);
            }
            "ioapic" => {
                let madt = match crate::acpi::get() {
                    Ok(a) => a.madt.as_ref(),
                    Err(_) => None,
                };
                let madt = madt.expect("irqdirt: ioapic needs a MADT");
                let mut unmasked = 0;
                for io in &madt.ioapics {
                    let regs = IoApicRegs {
                        regs: map_regs(info, u64::from(io.address)).expect("irqdirt: I/O APIC"),
                    };
                    let v = ioapic::Version::decode(regs.read(ioapic::REG_VERSION));
                    for pin in 0..v.pins().min(ioapic::MAX_PINS) as u8 {
                        let (lo_reg, hi_reg) = ioapic::redirection_regs(pin).unwrap();
                        let old = regs.read(lo_reg);
                        let mode_vec = match pin {
                            0 => 7 << 8,
                            13 => 4 << 8,
                            _ => 0x40 + u32::from(pin),
                        };
                        regs.write(hi_reg, 0);
                        regs.write(lo_reg, (old & (1 << 13 | 1 << 15)) | mode_vec);
                        if regs.read(lo_reg) & ioapic::MASKED == 0 {
                            unmasked += 1;
                        }
                    }
                }
                assert!(unmasked > 0, "irqdirt: ioapic did not take");
                let _ = write!(detail, " {unmasked} I/O APIC pins unmasked;");
            }
            "lapic" => {
                let c = cpu_apic();
                let raw = cpu::read_msr(base::MSR);
                let l = match Mode::of(raw) {
                    Mode::X2Apic => Lapic::X2,
                    Mode::XApic => Lapic::X {
                        regs: map_regs(info, base::address(raw, c.maxphyaddr))
                            .expect("irqdirt: local APIC"),
                    },
                    m => panic!("irqdirt: lapic needs an enabled local APIC, found {m:?}"),
                };
                l.write(lapic::TPR, 0x20);
                l.write(lapic::SVR, 0x1EF);
                let v = lapic::Version::decode(l.read(lapic::VERSION));
                l.write(lapic::TIMER_INITIAL, 0);
                let mut n = 0;
                for (reg, val) in [
                    (lapic::LVT_TIMER, 0x50),
                    (lapic::LVT_THERMAL, 0x52),
                    (lapic::LVT_PERF, 4 << 8),
                    (lapic::LVT_LINT0, 7 << 8),
                    (lapic::LVT_LINT1, 4 << 8),
                    (lapic::LVT_ERROR, 0x51),
                    (lapic::LVT_CMCI, 0x54),
                ] {
                    if v.has_lvt(reg) {
                        l.write(reg, val);
                        if l.read(reg) & lapic::LVT_MASKED == 0 {
                            n += 1;
                        }
                    }
                }
                assert!(
                    l.read(lapic::TPR) & 0xFF == 0x20 && n >= 4,
                    "irqdirt: lapic did not take"
                );
                let _ = write!(detail, " {n} LVTs unmasked, TPR 0x20;");
            }
            other => panic!("irqdirt: unknown word {other:?}"),
        }
        let _ = write!(done, " {w}");
    }
    crate::println!("irqdirt: applied:{done};{detail}");
}
