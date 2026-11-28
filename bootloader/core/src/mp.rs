//! The MP request's pure decisions (Limine protocol, "MP (Multiprocessor)
//! Feature", x86-64): which processors are reported and started, whether
//! the local APICs run in x2APIC mode, and the IPI commands that start an
//! application processor (Intel SDM Vol. 3A, 10.6.1 and 8.4.4).

/// One processor entry of the MADT (Processor Local APIC, type 0, or
/// Processor Local x2APIC, type 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MadtCpu {
    pub apic_id: u32,
    pub uid: u32,
    /// Flags bit 0. Online-capable (bit 1) processors are not running and
    /// are not started: they are the OS's to bring online.
    pub enabled: bool,
}

/// A processor as reported in the MP response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    /// The ACPI processor UID; u32::MAX for a BSP the MADT does not list.
    pub uid: u32,
}

/// The highest APIC id xAPIC mode can address (0xFF is the broadcast id).
pub const XAPIC_MAX_ID: u32 = 0xfe;

/// The processors to report: every enabled MADT entry in table order, each
/// APIC id once (the first entry wins), ids above 254 only in x2APIC mode
/// (an xAPIC interrupt command cannot name them). The BSP is always
/// reported; if the MADT does not list it, it comes first with UID
/// u32::MAX. Returns how many were written; entries beyond `out` are
/// dropped.
pub fn select(
    madt: impl IntoIterator<Item = MadtCpu>,
    x2apic: bool,
    bsp: u32,
    out: &mut [Cpu],
) -> usize {
    let mut n = 0;
    let mut saw_bsp = false;
    let push = |c: Cpu, out: &mut [Cpu], n: &mut usize| {
        if *n < out.len() && !out[..*n].iter().any(|o| o.apic_id == c.apic_id) {
            out[*n] = c;
            *n += 1;
        }
    };
    // Room for the BSP is kept back until we know whether the MADT lists it.
    let cap = out.len();
    if cap == 0 {
        return 0;
    }
    for m in madt {
        if !m.enabled || (!x2apic && m.apic_id > XAPIC_MAX_ID) {
            continue;
        }
        if m.apic_id == bsp {
            if !saw_bsp {
                saw_bsp = true;
                push(
                    Cpu {
                        apic_id: m.apic_id,
                        uid: m.uid,
                    },
                    out,
                    &mut n,
                );
            }
            continue;
        }
        // Keep one slot for the BSP while it has not been seen.
        if !saw_bsp && n + 1 >= cap {
            continue;
        }
        push(
            Cpu {
                apic_id: m.apic_id,
                uid: m.uid,
            },
            out,
            &mut n,
        );
    }
    if !saw_bsp {
        // Shift right by one and put the BSP first.
        let k = n.min(cap - 1);
        out.copy_within(0..k, 1);
        out[0] = Cpu {
            apic_id: bsp,
            uid: u32::MAX,
        };
        n = k + 1;
    }
    n
}

/// Why the APIC mode the kernel needs cannot be set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeError {
    /// The firmware left x2APIC on, the kernel did not ask for it, and it
    /// cannot be turned off: the platform locked it, or a processor has an
    /// APIC id that xAPIC mode cannot address.
    CannotDisableX2apic,
}

/// Whether the APICs run in x2APIC mode at entry. `asked` is the request's
/// flag bit 0, `cpu_x2` CPUID.1:ECX[21], `fw_x2` the firmware's mode,
/// `locked` IA32_XAPIC_DISABLE_STATUS bit 0, and `max_id` the highest
/// enabled APIC id in the MADT.
pub fn apic_mode(
    asked: bool,
    cpu_x2: bool,
    fw_x2: bool,
    locked: bool,
    max_id: u32,
) -> Result<bool, ModeError> {
    if asked && (cpu_x2 || fw_x2) {
        return Ok(true);
    }
    if fw_x2 {
        // The spec: "if firmware has already enabled x2APIC and bit 0 is
        // clear, the bootloader will try to disable x2APIC ... If the MP
        // request is present and x2APIC cannot be disabled, the bootloader
        // will fail to boot the executable."
        if locked || max_id > XAPIC_MAX_ID {
            return Err(ModeError::CannotDisableX2apic);
        }
    }
    Ok(false)
}

/// INIT, level assert, to one processor (SDM 10.6.1: delivery mode 101b,
/// level 1, edge triggered, physical destination).
pub const ICR_INIT: u32 = 0x0000_4500;
/// Start-up, to one processor: delivery mode 110b, the vector is the page
/// number of the start-up code (below 1 MiB).
pub const ICR_STARTUP: u32 = 0x0000_4600;
/// xAPIC ICR low bit 12: the previous IPI has not been sent yet.
pub const ICR_PENDING: u32 = 1 << 12;

/// The interrupt command for `low` (ICR_INIT, or ICR_STARTUP | vector) to
/// the processor with `apic_id`: the x2APIC form is one 64-bit MSR value
/// (destination in bits 63:32); the xAPIC form is (ICR high, ICR low) with
/// the destination in bits 31:24 of the high word.
pub fn icr(x2apic: bool, apic_id: u32, low: u32) -> (u32, u32) {
    if x2apic {
        (apic_id, low)
    } else {
        (apic_id << 24, low)
    }
}

/// The start-up vector for code at `phys`: its page number, which must fit
/// in 8 bits, so the page is below 1 MiB. Page 0 is refused too (it holds
/// the real-mode interrupt vectors on PCs).
pub fn startup_vector(phys: u64) -> Option<u8> {
    (phys & 0xfff == 0 && (0x1000..0x10_0000).contains(&phys)).then_some((phys >> 12) as u8)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::fuzzkit;
    use std::vec::Vec;

    fn m(apic_id: u32, uid: u32, enabled: bool) -> MadtCpu {
        MadtCpu {
            apic_id,
            uid,
            enabled,
        }
    }
    fn c(apic_id: u32, uid: u32) -> Cpu {
        Cpu { apic_id, uid }
    }
    fn sel(madt: &[MadtCpu], x2: bool, bsp: u32, cap: usize) -> Vec<Cpu> {
        let mut out = std::vec![Cpu::default(); cap];
        let n = select(madt.iter().copied(), x2, bsp, &mut out);
        out.truncate(n);
        out
    }

    #[test]
    fn select_hand_worked() {
        let madt = [
            m(0, 0, true),
            m(1, 1, true),
            m(2, 2, false), // disabled: not started
            m(1, 9, true),  // duplicate id: first wins
            m(3, 3, true),
            m(300, 4, true), // x2APIC only
        ];
        assert_eq!(sel(&madt, false, 0, 16), [c(0, 0), c(1, 1), c(3, 3)]);
        assert_eq!(
            sel(&madt, true, 0, 16),
            [c(0, 0), c(1, 1), c(3, 3), c(300, 4)]
        );
        // BSP in the middle keeps table order.
        assert_eq!(sel(&madt, false, 3, 16), [c(0, 0), c(1, 1), c(3, 3)]);
        // BSP missing: reported first, UID unknown.
        assert_eq!(
            sel(&madt, false, 7, 16),
            [c(7, u32::MAX), c(0, 0), c(1, 1), c(3, 3)]
        );
        // BSP only reachable in x2APIC mode, and the MADT has it.
        assert_eq!(sel(&madt, true, 300, 2), [c(0, 0), c(300, 4)]);
        // No MADT at all: the BSP alone.
        assert_eq!(sel(&[], false, 5, 4), [c(5, u32::MAX)]);
        // Full output still has the BSP.
        assert_eq!(sel(&madt, false, 3, 2), [c(0, 0), c(3, 3)]);
        assert_eq!(sel(&madt, false, 7, 2), [c(7, u32::MAX), c(0, 0)]);
        assert_eq!(sel(&madt, false, 0, 0), []);
    }

    /// The obvious model: filter, dedupe, BSP guaranteed, capacity.
    fn model(madt: &[MadtCpu], x2: bool, bsp: u32, cap: usize) -> Vec<Cpu> {
        if cap == 0 {
            return Vec::new();
        }
        let mut all: Vec<Cpu> = Vec::new();
        for e in madt {
            if e.enabled
                && (x2 || e.apic_id <= XAPIC_MAX_ID)
                && !all.iter().any(|o| o.apic_id == e.apic_id)
            {
                all.push(c(e.apic_id, e.uid));
            }
        }
        let has_bsp = all.iter().any(|o| o.apic_id == bsp);
        if !has_bsp {
            all.insert(0, c(bsp, u32::MAX));
        }
        // Over capacity: the BSP and the first cap - 1 others, in order.
        let mut others = 0;
        all.retain(|o| {
            if o.apic_id == bsp {
                return true;
            }
            others += 1;
            others < cap
        });
        all
    }

    #[test]
    fn fuzz_select_matches_model() {
        fuzzkit::run("mp-select", 0x5e1e, |rng| {
            let len = rng.below(12) as usize;
            let ids = 1 + rng.below(8) as u32;
            let mut madt = Vec::new();
            for _ in 0..len {
                let id = match rng.below(4) {
                    0 => 0xfe + rng.below(4) as u32, // around the xAPIC limit
                    _ => rng.below(ids as u64) as u32,
                };
                madt.push(m(id, rng.below(16) as u32, rng.below(4) != 0));
            }
            let x2 = rng.below(2) == 0;
            let bsp = if rng.below(4) == 0 {
                0x100 + rng.below(2) as u32
            } else {
                rng.below(ids as u64) as u32
            };
            let cap = rng.below(8) as usize;
            let got = sel(&madt, x2, bsp, cap);
            let want = model(&madt, x2, bsp, cap);
            assert_eq!(got, want, "madt {madt:?} x2 {x2} bsp {bsp} cap {cap}");
            // Properties beyond the model: BSP present, ids unique.
            if cap > 0 {
                assert!(got.iter().any(|o| o.apic_id == bsp));
            }
            for (i, a) in got.iter().enumerate() {
                assert!(got[..i].iter().all(|b| b.apic_id != a.apic_id));
            }
        });
    }

    #[test]
    fn apic_mode_truth_table() {
        use ModeError::CannotDisableX2apic as E;
        // (asked, cpu_x2, fw_x2, locked, max_id) -> result
        let cases = [
            ((false, false, false, false, 3), Ok(false)),
            ((true, false, false, false, 3), Ok(false)), // asked, impossible
            ((true, true, false, false, 3), Ok(true)),
            ((false, true, false, false, 3), Ok(false)),
            ((false, true, true, false, 3), Ok(false)), // turned off
            ((false, true, true, true, 3), Err(E)),     // locked on
            ((false, true, true, false, 0xff), Err(E)), // id needs x2APIC
            ((true, true, true, true, 0x1000), Ok(true)),
            ((false, true, false, false, 0x1000), Ok(false)), // those CPUs are left out
        ];
        for ((a, c, f, l, id), want) in cases {
            assert_eq!(apic_mode(a, c, f, l, id), want, "{a} {c} {f} {l} {id}");
        }
    }

    #[test]
    fn ipi_encoding() {
        assert_eq!(icr(false, 3, ICR_INIT), (3 << 24, 0x4500));
        assert_eq!(icr(true, 0x1234, ICR_STARTUP | 0x9f), (0x1234, 0x469f));
        assert_eq!(startup_vector(0x9f000), Some(0x9f));
        assert_eq!(startup_vector(0x1000), Some(1));
        assert_eq!(startup_vector(0), None);
        assert_eq!(startup_vector(0x100000), None);
        assert_eq!(startup_vector(0x9f800), None);
    }
}
