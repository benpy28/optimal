//! Seeded structured fuzzing of the ACPI reader: a generator of random but
//! valid firmware table sets with the answers the reader must give, then
//! mutations of those sets that it must survive without panicking.

extern crate std;
use super::sim::{Mem, fix, table};
use super::*;
use crate::fuzzkit;
use crate::testrng::Rng;
use std::vec;
use std::vec::Vec;

/// What a correct reader reports for a generated system.
#[derive(Default)]
struct Want {
    ranges: Vec<(u64, u64)>,
    first: Vec<([u8; 4], u64, u32)>,
    lapic_addr: u64,
    pcat: bool,
    ioapics: Vec<u64>,
    nmis: Vec<LapicNmi>,
    /// (APIC id, UID of the first entry with that id).
    apic_uids: Vec<(u32, u32)>,
    dmar: Vec<u64>,
    ivrs: Vec<u64>,
}

/// Hands out non-overlapping addresses: low ones (below 4 GiB, so an RSDT
/// can point at them), high ones up to 2^52, and ones right at the top of
/// the 64-bit address space, where any unchecked `addr + offset` overflows.
struct Placer {
    used: Vec<(u64, u64)>,
}

impl Placer {
    fn place(&mut self, rng: &mut Rng, len: u64, low: bool) -> u64 {
        loop {
            let a = match (low, rng.below(4)) {
                (true, _) | (false, 0) => (0x1000 + rng.below(0xffff_0000 - len)) & !0xf,
                (false, 1) => u64::MAX - len + 1 - rng.below(64).min(u64::MAX - len),
                _ => rng.below(1 << 52) & !0xf,
            };
            if a == 0 || a.checked_add(len).is_none() {
                continue;
            }
            if self.used.iter().all(|&(s, l)| a + len <= s || s + l <= a) {
                self.used.push((a, len));
                return a;
            }
        }
    }
}

fn madt(rng: &mut Rng, w: &mut Want) -> Vec<u8> {
    let lapic = rng.next() as u32;
    let flags = rng.next() as u32;
    w.lapic_addr = lapic as u64;
    w.pcat = flags & 1 != 0;
    let mut body = Vec::new();
    body.extend_from_slice(&lapic.to_le_bytes());
    body.extend_from_slice(&flags.to_le_bytes());
    for _ in 0..rng.below(48) {
        match rng.below(8) {
            0 => {
                let (uid, id) = (rng.next() as u8, rng.next() as u8);
                body.extend_from_slice(&[0, 8, uid, id, 1, 0, 0, 0]);
                if !w.apic_uids.iter().any(|&(a, _)| a == id as u32) {
                    w.apic_uids.push((id as u32, uid as u32));
                }
            }
            1 => {
                let a = rng.next() as u32;
                body.extend_from_slice(&[1, 12, rng.next() as u8, 0]);
                body.extend_from_slice(&a.to_le_bytes());
                body.extend_from_slice(&(rng.next() as u32).to_le_bytes());
                if w.ioapics.len() < MAX_IOAPICS {
                    w.ioapics.push(a as u64);
                }
            }
            2 => {
                let (uid, fl, lint) = (rng.next() as u8, rng.next() as u16, rng.next() as u8);
                body.extend_from_slice(&[4, 6, uid]);
                body.extend_from_slice(&fl.to_le_bytes());
                body.push(lint);
                if w.nmis.len() < MAX_NMIS {
                    w.nmis.push(LapicNmi {
                        uid: if uid == 0xff { u32::MAX } else { uid as u32 },
                        flags: fl,
                        lint,
                    });
                }
            }
            3 => {
                let a = rng.next();
                body.extend_from_slice(&[5, 12, 0, 0]);
                body.extend_from_slice(&a.to_le_bytes());
                w.lapic_addr = a;
            }
            4 => {
                let (id, uid) = (rng.next() as u32, rng.next() as u32);
                body.extend_from_slice(&[9, 16, 0, 0]);
                body.extend_from_slice(&id.to_le_bytes());
                body.extend_from_slice(&1u32.to_le_bytes());
                body.extend_from_slice(&uid.to_le_bytes());
                if !w.apic_uids.iter().any(|&(a, _)| a == id) {
                    w.apic_uids.push((id, uid));
                }
            }
            5 => {
                let (fl, uid, lint) = (rng.next() as u16, rng.next() as u32, rng.next() as u8);
                body.extend_from_slice(&[0x0a, 12]);
                body.extend_from_slice(&fl.to_le_bytes());
                body.extend_from_slice(&uid.to_le_bytes());
                body.extend_from_slice(&[lint, 0, 0, 0]);
                if w.nmis.len() < MAX_NMIS {
                    w.nmis.push(LapicNmi {
                        uid,
                        flags: fl,
                        lint,
                    });
                }
            }
            _ => {
                // A structure type this reader skips (interrupt source
                // override, GICC, a future type): its length must be honoured.
                let typ = [2u8, 3, 0x0b, 0x14, 0x7f][rng.below(5) as usize];
                let len = 2 + rng.below(30) as u8;
                body.push(typ);
                body.push(len);
                body.extend((2..len).map(|_| rng.next() as u8));
            }
        }
    }
    table(b"APIC", &body)
}

fn dmar(rng: &mut Rng, w: &mut Want) -> Vec<u8> {
    let mut body = vec![0u8; 12];
    for _ in 0..rng.below(40) {
        let typ = rng.below(4) as u16;
        let scope = 8 * rng.below(3) as u16;
        let len = 16 + scope;
        let base = if rng.below(4) == 0 && !w.dmar.is_empty() {
            w.dmar[0]
        } else {
            rng.next() & !0xfff
        };
        body.extend_from_slice(&typ.to_le_bytes());
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&base.to_le_bytes());
        body.extend((0..scope).map(|_| rng.next() as u8));
        if typ == 0 && w.dmar.len() < MAX_IOMMUS && !w.dmar.contains(&base) {
            w.dmar.push(base);
        }
    }
    table(b"DMAR", &body)
}

fn ivrs(rng: &mut Rng, w: &mut Want) -> Vec<u8> {
    let mut body = vec![0u8; 12];
    for _ in 0..rng.below(40) {
        let typ = [0x10u8, 0x11, 0x40, 0x20, 0x21][rng.below(5) as usize];
        let len = if typ == 0x10 { 24 } else { 40 } + 4 * rng.below(4) as u16;
        let base = match rng.below(5) {
            0 if !w.ivrs.is_empty() => w.ivrs[0],
            1 => 0,
            _ => rng.next() & !0x3fff,
        };
        body.push(typ);
        body.push(0);
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&base.to_le_bytes());
        body.extend((16..len).map(|_| rng.next() as u8));
        if matches!(typ, 0x10 | 0x11 | 0x40)
            && base != 0
            && w.ivrs.len() < MAX_IOMMUS
            && !w.ivrs.contains(&base)
        {
            w.ivrs.push(base);
        }
    }
    table(b"IVRS", &body)
}

/// A random, valid table set and what the reader must report for it.
fn system(rng: &mut Rng) -> (Mem, u64, Want) {
    let mut w = Want::default();
    let mut p = Placer { used: Vec::new() };
    let mut regions: Vec<(u64, Vec<u8>)> = Vec::new();
    let rev2 = rng.below(4) != 0;
    // Without an XSDT everything must sit below 4 GiB.
    let low = !rev2 || rng.below(3) == 0;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let with_madt = rng.below(8) != 0;
    if with_madt {
        tables.push((*b"APIC", madt(rng, &mut w)));
    }
    if rng.below(2) == 0 {
        tables.push((*b"DMAR", dmar(rng, &mut w)));
    }
    if rng.below(2) == 0 {
        tables.push((*b"IVRS", ivrs(rng, &mut w)));
    }
    for _ in 0..rng.below(6) {
        let body: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
        tables.push((*b"SSDT", table(b"SSDT", &body)));
    }
    // A second MADT: `find` must return the first one.
    if with_madt && rng.below(8) == 0 {
        let mut ignored = Want::default();
        tables.push((*b"APIC", madt(rng, &mut ignored)));
    }
    let mut addrs = Vec::new();
    // The FADT, with 32-bit and/or 64-bit FACS and DSDT pointers.
    let fadt_len = [116u64, 132, 148, 244, 276][rng.below(5) as usize];
    let with_fadt = rng.below(4) != 0;
    if with_fadt {
        let mut fb = vec![0u8; fadt_len as usize - 36];
        let facs_len = 64 + 4 * rng.below(8);
        let mut facs = vec![0u8; facs_len as usize];
        facs[0..4].copy_from_slice(b"FACS");
        facs[4..8].copy_from_slice(&(facs_len as u32).to_le_bytes());
        let dsdt_body: Vec<u8> = (0..rng.below(200)).map(|_| rng.next() as u8).collect();
        let dsdt = table(b"DSDT", &dsdt_body);
        let fa = p.place(rng, facs_len, true);
        let da = p.place(rng, dsdt.len() as u64, true);
        let wide = fadt_len >= 148 && rng.below(2) == 0;
        if wide {
            fb[132 - 36..140 - 36].copy_from_slice(&fa.to_le_bytes());
            fb[140 - 36..148 - 36].copy_from_slice(&da.to_le_bytes());
        } else {
            fb[0..4].copy_from_slice(&(fa as u32).to_le_bytes());
            fb[4..8].copy_from_slice(&(da as u32).to_le_bytes());
        }
        w.ranges.push((fa, facs_len));
        w.ranges.push((da, dsdt.len() as u64));
        regions.push((fa, facs));
        regions.push((da, dsdt));
        let at = rng.below(tables.len() as u64 + 1) as usize;
        tables.insert(at, (*b"FACP", table(b"FACP", &fb)));
    }
    for (sig, t) in tables {
        let a = p.place(rng, t.len() as u64, low);
        if !w.first.iter().any(|f| f.0 == sig) {
            w.first.push((sig, a, t.len() as u32));
        }
        w.ranges.push((a, t.len() as u64));
        addrs.push(a);
        regions.push((a, t));
    }
    // A zero entry in the root table is skipped.
    if rng.below(8) == 0 {
        let at = rng.below(addrs.len() as u64 + 1) as usize;
        addrs.insert(at, 0);
    }
    let rsdt_addr = if low || rng.below(2) == 0 {
        let body: Vec<u8> = addrs
            .iter()
            .flat_map(|&a| (a as u32).to_le_bytes())
            .collect();
        let t = table(b"RSDT", &body);
        let a = p.place(rng, t.len() as u64, true);
        w.ranges.push((a, t.len() as u64));
        regions.push((a, t));
        a
    } else {
        0
    };
    let xsdt_addr = if rev2 && (!low || rng.below(2) == 0) || rsdt_addr == 0 {
        let body: Vec<u8> = addrs.iter().flat_map(|&a| a.to_le_bytes()).collect();
        let t = table(b"XSDT", &body);
        let a = p.place(rng, t.len() as u64, false);
        w.ranges.push((a, t.len() as u64));
        regions.push((a, t));
        a
    } else {
        0
    };
    let mut r = super::sim::rsdp(xsdt_addr, rsdt_addr as u32);
    if !rev2 {
        r[15] = 0;
        r[24..32].fill(0);
        let s = r[..20].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[8] = r[8].wrapping_sub(s);
        fix(&mut r, 32);
    }
    let ra = p.place(rng, r.len() as u64, false);
    w.ranges.push((ra, if rev2 { 36 } else { 20 }));
    regions.push((ra, r));
    (Mem(regions), ra, w)
}

#[test]
fn generated_systems_read_exactly() {
    fuzzkit::run("acpi-valid", 1, |rng| {
        let (mem, rsdp, w) = system(rng);
        let acpi = Acpi::new(&mem, rsdp).expect("valid RSDP");
        let mut ranges = Vec::new();
        acpi.for_each_range(|r| ranges.push((r.start, r.len)));
        ranges.sort();
        let mut want = w.ranges.clone();
        want.sort();
        assert_eq!(ranges, want);
        for &(sig, a, l) in &w.first {
            assert_eq!(acpi.find(&sig), Some((a, l)), "{:?}", sig);
        }
        if let Some((a, l)) = acpi.find(b"APIC") {
            let m = Madt::parse(&mem, a, l).expect("valid MADT");
            assert_eq!(m.lapic_addr, w.lapic_addr);
            assert_eq!(m.pcat_compat, w.pcat);
            assert_eq!(m.ioapics(), &w.ioapics[..]);
            assert_eq!(m.nmis(), &w.nmis[..]);
            for &(id, uid) in &w.apic_uids {
                assert_eq!(m.uid_for_apic(&mem, id), Some(uid), "apic {id}");
            }
        }
        let mut u = [0u64; MAX_IOMMUS];
        match acpi.find(b"DMAR") {
            Some((a, l)) => {
                let n = dmar_units(&mem, a, l, &mut u);
                assert_eq!(&u[..n], &w.dmar[..]);
            }
            None => assert!(w.dmar.is_empty()),
        }
        match acpi.find(b"IVRS") {
            Some((a, l)) => {
                let n = ivrs_units(&mem, a, l, &mut u);
                assert_eq!(&u[..n], &w.ivrs[..]);
            }
            None => assert!(w.ivrs.is_empty()),
        }
    });
}

#[test]
fn mutated_systems_never_panic() {
    fuzzkit::run("acpi-mutated", 2, |rng| {
        let (mut mem, mut rsdp, _) = system(rng);
        for _ in 0..1 + rng.below(3) {
            let t = rng.below(mem.0.len() as u64) as usize;
            if rng.below(8) == 0 {
                // Move a whole table to the very top of the address space.
                let len = mem.0[t].1.len() as u64;
                mem.0[t].0 = (u64::MAX - len).saturating_sub(rng.below(16));
                continue;
            }
            fuzzkit::mutate(rng, &mut mem.0[t].1);
            // Often repair the checksum, so the mutation gets past it.
            if rng.below(2) == 0 && mem.0[t].1.len() >= 36 {
                if &mem.0[t].1[..8] == b"RSD PTR " {
                    let s = mem.0[t].1[..20].iter().fold(0u8, |a, &b| a.wrapping_add(b));
                    mem.0[t].1[8] = mem.0[t].1[8].wrapping_sub(s);
                    fix(&mut mem.0[t].1[..36], 32);
                } else {
                    let len = mem.0[t].1.len();
                    fix(&mut mem.0[t].1[..len], 9);
                }
            }
        }
        if rng.below(32) == 0 {
            rsdp = u64::MAX - rng.below(40);
        }
        // Keep regions inside the address space (the simulator needs it).
        mem.0
            .retain(|(b, v)| b.checked_add(v.len() as u64).is_some());
        let Some(acpi) = Acpi::new(&mem, rsdp) else {
            return;
        };
        acpi.for_each_range(|r| {
            assert!(r.len >= 20 && r.len <= MAX_TABLE_LEN as u64, "{r:?}");
            assert!(mem.u8(r.start).is_some(), "{r:?} unreadable");
        });
        let mut tables = 0;
        acpi.for_each_table(|_, a, l| {
            tables += 1;
            assert!((36..=MAX_TABLE_LEN).contains(&l));
            assert!(mem.u32(a).is_some());
        });
        assert!(tables <= MAX_TABLES);
        acpi.for_each_table(|sig, a, l| {
            if &sig == b"APIC"
                && let Some(m) = Madt::parse(&mem, a, l)
            {
                assert!(m.ioapics().len() <= MAX_IOAPICS && m.nmis().len() <= MAX_NMIS);
                for id in [0, 1, 0xff, u32::MAX] {
                    let _ = m.uid_for_apic(&mem, id);
                }
            }
            let mut u = [0u64; MAX_IOMMUS];
            let n = dmar_units(&mem, a, l, &mut u);
            assert!(u[..n].iter().enumerate().all(|(i, x)| !u[..i].contains(x)));
            let n = ivrs_units(&mem, a, l, &mut u);
            assert!(u[..n].iter().all(|&x| x != 0));
        });
    });
}
