//! Kernel tests for the interrupt controllers on the real (virtual)
//! machine: the local APIC is in the mode and state the plan says,
//! interrupts arrive once, in priority order, and are retired; the
//! spurious and stray paths never retire someone else's interrupt; and a
//! real device interrupt (the PIT on ISA IRQ 0) arrives through its I/O
//! APIC route, with the override applied, only while unmasked.
//!
//! When the setup left interrupts off (no local APIC) there is nothing to
//! check and the tests say so; the tough-conditions script checks the
//! `interrupts:` line of every run.

use core::arch::asm;
use core::arch::x86_64::__cpuid_count;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use optimal_apic::base::{self, Mode, Reason};
use optimal_apic::lapic::{self, Ipi, Lvt};
use optimal_apic::{ioapic, is_set, vectors};

use super::{Controllers, clear_handler, controllers, count, isa_mask, isa_unmask, set_handler};
use crate::arch::idt::InterruptFrame;
use crate::arch::port::{inb, outb};
use crate::arch::{apic, cpu};
use crate::ktest;

fn ctl() -> Option<&'static Controllers> {
    let c = controllers();
    if c.is_none() {
        crate::println!("ktest: interrupts are off on this machine; nothing to check");
    }
    c
}

/// Spin until `f` holds, for at most about 50 million polls (seconds under
/// emulation; interrupts here arrive within microseconds).
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

ktest!(apic_mode_is_the_chosen_one, {
    let Some(c) = ctl() else { return };
    let raw = cpu::read_msr(base::MSR);
    assert_eq!(Mode::of(raw), c.mode);
    assert!(
        base::is_bsp(raw),
        "the boot CPU's IA32_APIC_BASE has no BSP bit"
    );
    let x2 = __cpuid_count(1, 0).ecx & (1 << 21) != 0;
    match c.reason {
        Reason::FirmwareX2Apic => assert_eq!(c.mode, Mode::X2Apic),
        Reason::Supported => assert!(x2 && c.mode == Mode::X2Apic),
        Reason::Unsupported => assert!(!x2 && c.mode == Mode::XApic),
        Reason::FirmwareOptOut => assert!(x2 && c.mode == Mode::XApic),
    }
    assert_eq!(matches!(c.lapic, apic::Lapic::X2), c.mode == Mode::X2Apic);
});

ktest!(lapic_id_matches_cpuid, {
    let Some(c) = ctl() else { return };
    let id = c.lapic.id();
    assert_eq!(id, c.lapic_id);
    if c.mode == Mode::X2Apic && __cpuid_count(0, 0).eax >= 0xB {
        assert_eq!(__cpuid_count(0xB, 0).edx, id, "x2APIC ID vs CPUID 0xB");
    } else {
        assert_eq!(__cpuid_count(1, 0).ebx >> 24, id, "xAPIC ID vs CPUID 1");
    }
});

// Every register setup() wrote reads back as written (in the bits the
// encoders define), the timer is stopped, and no error is latched.
ktest!(lapic_registers_as_planned, {
    let Some(c) = ctl() else { return };
    let l = c.lapic;
    for (reg, v) in lapic::setup(c.version, c.plan.lint[0], c.plan.lint[1]) {
        let got = l.read(reg);
        match reg {
            lapic::SVR => assert_eq!(got & 0x13FF, v, "SVR"),
            lapic::TPR => assert_eq!(got & 0xFF, 0, "TPR"),
            lapic::ESR => {}
            lapic::TIMER_INITIAL => assert_eq!(got, 0, "timer initial count"),
            r => {
                assert_eq!(got & 0x1_A7FF, v, "LVT {r:#x}");
                if r == lapic::LVT_TIMER {
                    assert_eq!(got & (3 << 17), 0, "timer mode");
                }
            }
        }
    }
    assert_eq!(l.esr(), 0, "local APIC error status");
    assert_eq!(l.read_array(lapic::ISR), [0; 8], "nothing in service");
});

static HITS: AtomicU64 = AtomicU64::new(0);
static WAS_IN_SERVICE: AtomicBool = AtomicBool::new(false);

fn count_hit(f: &mut InterruptFrame) {
    HITS.fetch_add(1, Ordering::SeqCst);
    if let Some(c) = controllers() {
        WAS_IN_SERVICE.store(
            is_set(&c.lapic.read_array(lapic::ISR), f.vector as u8),
            Ordering::SeqCst,
        );
    }
}

// A self IPI is delivered exactly once, is in service while its handler
// runs, and is retired afterwards (EOI).
ktest!(self_ipi_delivered_once_then_retired, {
    let Some(c) = ctl() else { return };
    let v = 0x80;
    HITS.store(0, Ordering::SeqCst);
    WAS_IN_SERVICE.store(false, Ordering::SeqCst);
    set_handler(v, count_hit);
    let before = count(v);
    assert!(cpu::interrupts_enabled());
    assert!(c.lapic.send(Ipi::to_self(v)));
    assert!(
        wait_for(|| HITS.load(Ordering::SeqCst) > 0),
        "self IPI never arrived"
    );
    spin(100_000);
    assert_eq!(HITS.load(Ordering::SeqCst), 1);
    assert_eq!(count(v), before + 1);
    assert!(WAS_IN_SERVICE.load(Ordering::SeqCst));
    assert!(!is_set(&c.lapic.read_array(lapic::ISR), v), "not retired");
    assert!(!is_set(&c.lapic.read_array(lapic::IRR), v));
    clear_handler(v);
});

// With interrupts off, an IPI waits in the IRR and arrives as soon as
// they are back on.
ktest!(ipi_waits_for_interrupts_on, {
    let Some(c) = ctl() else { return };
    let v = 0x81;
    HITS.store(0, Ordering::SeqCst);
    set_handler(v, count_hit);
    let was = cpu::interrupts_disable();
    assert!(c.lapic.send(Ipi::to_self(v)));
    spin(100_000);
    assert_eq!(
        HITS.load(Ordering::SeqCst),
        0,
        "arrived with interrupts off"
    );
    assert!(is_set(&c.lapic.read_array(lapic::IRR), v), "not pending");
    cpu::interrupts_enable();
    assert!(wait_for(|| HITS.load(Ordering::SeqCst) == 1));
    assert!(!is_set(&c.lapic.read_array(lapic::IRR), v));
    if !was {
        cpu::interrupts_disable();
    }
    clear_handler(v);
});

static ORDER: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn record(f: &mut InterruptFrame) {
    let i = NEXT.fetch_add(1, Ordering::SeqCst);
    if i < ORDER.len() {
        ORDER[i].store(f.vector, Ordering::SeqCst);
    }
}

fn order() -> [u64; 4] {
    core::array::from_fn(|i| ORDER[i].load(Ordering::SeqCst))
}

fn reset_order() {
    NEXT.store(0, Ordering::SeqCst);
    for o in &ORDER {
        o.store(0, Ordering::SeqCst);
    }
}

// TPR 0x90 holds back priority class 9 and below: 0x95 waits, 0xA5 gets
// through, and lowering the TPR to 0 lets 0x95 in.
ktest!(tpr_holds_back_lower_classes, {
    let Some(c) = ctl() else { return };
    reset_order();
    set_handler(0x95, record);
    set_handler(0xA5, record);
    c.lapic.write(lapic::TPR, 0x90);
    assert!(c.lapic.send(Ipi::to_self(0x95)));
    spin(100_000);
    assert_eq!(NEXT.load(Ordering::SeqCst), 0, "class 9 got past TPR 0x90");
    assert!(is_set(&c.lapic.read_array(lapic::IRR), 0x95));
    assert!(c.lapic.send(Ipi::to_self(0xA5)));
    assert!(wait_for(|| NEXT.load(Ordering::SeqCst) == 1));
    c.lapic.write(lapic::TPR, 0);
    assert!(wait_for(|| NEXT.load(Ordering::SeqCst) == 2));
    assert_eq!(order(), [0xA5, 0x95, 0, 0]);
    clear_handler(0x95);
    clear_handler(0xA5);
});

// Pending interrupts are served highest vector first, within a class too.
ktest!(highest_vector_served_first, {
    let Some(c) = ctl() else { return };
    reset_order();
    for v in [0x91, 0x92, 0xB1] {
        set_handler(v, record);
    }
    let was = cpu::interrupts_disable();
    for v in [0x91, 0xB1, 0x92] {
        assert!(c.lapic.send(Ipi::to_self(v)));
    }
    spin(100_000);
    cpu::interrupts_enable();
    assert!(wait_for(|| NEXT.load(Ordering::SeqCst) == 3));
    assert_eq!(order(), [0xB1, 0x92, 0x91, 0]);
    if !was {
        cpu::interrupts_disable();
    }
    for v in [0x91, 0x92, 0xB1] {
        clear_handler(v);
    }
});

static STILL: [AtomicBool; 3] = [const { AtomicBool::new(false) }; 3];

/// Runs while 0x84 is in service. A spurious interrupt, a stray vector and
/// a stray 8259 vector arrive (as software interrupts, which are never in
/// service); if any of their paths sent an EOI it would retire 0x84.
fn in_service_handler(_: &mut InterruptFrame) {
    let c = controllers().expect("only registered while on");
    let in_service = || is_set(&c.lapic.read_array(lapic::ISR), 0x84);
    // SAFETY: software interrupts on vectors the IDT routes to
    // irq::dispatch, which returns.
    unsafe { asm!("int 0xff") };
    STILL[0].store(in_service(), Ordering::SeqCst);
    unsafe { asm!("int 0x86") };
    STILL[1].store(in_service(), Ordering::SeqCst);
    unsafe { asm!("int 0x27") };
    STILL[2].store(in_service(), Ordering::SeqCst);
    HITS.fetch_add(1, Ordering::SeqCst);
}

ktest!(spurious_and_stray_paths_never_retire_another, {
    let Some(c) = ctl() else { return };
    HITS.store(0, Ordering::SeqCst);
    for s in &STILL {
        s.store(false, Ordering::SeqCst);
    }
    let counts = [count(0xFF), count(0x86), count(0x27)];
    set_handler(0x84, in_service_handler);
    assert!(c.lapic.send(Ipi::to_self(0x84)));
    assert!(wait_for(|| HITS.load(Ordering::SeqCst) == 1));
    assert!(STILL[0].load(Ordering::SeqCst), "spurious path sent an EOI");
    assert!(STILL[1].load(Ordering::SeqCst), "stray path sent an EOI");
    assert!(STILL[2].load(Ordering::SeqCst), "8259 path sent an EOI");
    assert_eq!(
        [count(0xFF), count(0x86), count(0x27)],
        counts.map(|n| n + 1)
    );
    // The registered handler's own EOI did retire 0x84.
    assert_eq!(c.lapic.read_array(lapic::ISR), [0; 8]);
    clear_handler(0x84);
});

// A stray hardware interrupt (an IPI on a vector with no handler) is
// counted and retired, not fatal.
ktest!(stray_interrupt_is_counted_and_retired, {
    let Some(c) = ctl() else { return };
    let v = 0x88;
    let before = count(v);
    assert!(c.lapic.send(Ipi::to_self(v)));
    assert!(wait_for(|| count(v) == before + 1));
    assert_eq!(c.lapic.read_array(lapic::ISR), [0; 8]);
});

// Every pin of every used I/O APIC holds what the plan says (masked
// state included: every test puts its line back).
ktest!(ioapic_tables_as_planned, {
    let Some(c) = ctl() else { return };
    for (i, regs) in c.ioapics.iter().enumerate() {
        let table = c.plan.table(i);
        if table.is_empty() {
            continue;
        }
        let regs = regs.expect("a used I/O APIC is mapped");
        for (pin, want) in table.iter().enumerate() {
            let (lo, hi) = regs.entry(pin as u8);
            let got = ioapic::Redirection::decode(lo, hi);
            assert_eq!(got.entry, *want, "I/O APIC {i} pin {pin}");
            assert!(!got.remote_irr, "I/O APIC {i} pin {pin} awaits an EOI");
        }
    }
});

ktest!(pics_masked, {
    let Some(c) = ctl() else { return };
    if c.pic {
        assert_eq!(apic::pic_masks(), (0xFF, 0xFF));
    }
});

// The PIT (8254) on ISA IRQ 0: mode 0 raises its output once, when the
// count runs out, which is one edge on IRQ 0.
const PIT_CMD: u16 = 0x43;
const PIT_CH0: u16 = 0x40;

fn pit_one_shot(count: u16) {
    // SAFETY: the 8254's command and channel 0 ports. Channel 0, low then
    // high byte, mode 0 (interrupt on terminal count), binary: 0x30.
    unsafe {
        outb(PIT_CMD, 0x30);
        outb(PIT_CH0, count as u8);
        outb(PIT_CH0, (count >> 8) as u8);
    }
}

/// Channel 0's OUT pin, by the read-back command (latch status only).
fn pit_out() -> bool {
    // SAFETY: read-back of channel 0's status; changes nothing.
    unsafe {
        outb(PIT_CMD, 0xE2);
        inb(PIT_CH0) & 0x80 != 0
    }
}

fn pic_vector_counts() -> [u64; 16] {
    core::array::from_fn(|i| count(vectors::PIC_MASTER + i as u8))
}

// While IRQ 0 is masked, the PIT's edge is ignored, and stays ignored
// after unmasking (an I/O APIC drops edges on masked pins).
ktest!(masked_isa_line_stays_quiet, {
    let Some(c) = ctl() else { return };
    let Some(r) = c.plan.isa_route(0) else {
        crate::println!("ktest: ISA IRQ 0 not routed here; nothing to check");
        return;
    };
    let v = r.entry.vector;
    HITS.store(0, Ordering::SeqCst);
    set_handler(v, count_hit);
    pit_one_shot(0x100);
    assert!(wait_for(pit_out), "PIT output never rose");
    spin(1_000_000);
    assert_eq!(HITS.load(Ordering::SeqCst), 0, "masked IRQ 0 delivered");
    isa_unmask(0).unwrap();
    spin(1_000_000);
    assert_eq!(
        HITS.load(Ordering::SeqCst),
        0,
        "masked edge kept and delivered"
    );
    isa_mask(0).unwrap();
    clear_handler(v);
});

// Unmasked, the PIT's one edge arrives exactly once on IRQ 0's vector,
// through the pin the plan chose (GSI 2 on QEMU, by override), and nothing
// arrives on an 8259 vector.
ktest!(pit_interrupt_arrives_through_its_route, {
    let Some(c) = ctl() else { return };
    let Some(r) = c.plan.isa_route(0) else {
        crate::println!("ktest: ISA IRQ 0 not routed here; nothing to check");
        return;
    };
    let v = r.entry.vector;
    assert_eq!(v, vectors::isa(0));
    let pic_before = pic_vector_counts();
    HITS.store(0, Ordering::SeqCst);
    WAS_IN_SERVICE.store(false, Ordering::SeqCst);
    set_handler(v, count_hit);
    isa_unmask(0).unwrap();
    pit_one_shot(0x2000);
    assert!(
        wait_for(|| HITS.load(Ordering::SeqCst) > 0),
        "IRQ 0 never arrived"
    );
    spin(2_000_000);
    isa_mask(0).unwrap();
    assert_eq!(HITS.load(Ordering::SeqCst), 1, "one edge, one interrupt");
    assert!(WAS_IN_SERVICE.load(Ordering::SeqCst));
    assert!(pit_out());
    assert_eq!(pic_vector_counts(), pic_before, "an 8259 vector fired");
    let regs = c.ioapics[r.ioapic].unwrap();
    let (lo, hi) = regs.entry(r.pin);
    let e = ioapic::Redirection::decode(lo, hi);
    assert!(e.entry.masked && !e.remote_irr && !e.pending);
    assert_eq!(c.lapic.read_array(lapic::ISR), [0; 8]);
    clear_handler(v);
});

// The LINT entries are what the plan chose, and the plan followed the
// MADT: an NMI pin is unmasked NMI, anything else masked.
ktest!(lint_pins_as_planned, {
    let Some(c) = ctl() else { return };
    for (i, reg) in [lapic::LVT_LINT0, lapic::LVT_LINT1].into_iter().enumerate() {
        let got = Lvt::decode(c.lapic.read(reg));
        assert_eq!(got, c.plan.lint[i], "LINT{i}");
        assert!(got.masked || got.delivery == lapic::Delivery::Nmi);
    }
});
