//! Just enough ACPI table reading for the handoff: finding tables, listing
//! every table range (so they can be kept mapped), and reading the MADT
//! (interrupt controllers), DMAR (Intel VT-d) and IVRS (AMD-Vi).
//! Written from the ACPI 6.5, VT-d 4.1 and AMD IOMMU 3.x specifications.

/// Reads physical memory. Returns false if the range can't be read.
pub trait PhysRead {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool;

    fn u8(&self, p: u64) -> Option<u8> {
        let mut b = [0u8; 1];
        self.read(p, &mut b).then_some(b[0])
    }
    fn u16(&self, p: u64) -> Option<u16> {
        let mut b = [0u8; 2];
        self.read(p, &mut b).then_some(u16::from_le_bytes(b))
    }
    fn u32(&self, p: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(p, &mut b).then_some(u32::from_le_bytes(b))
    }
    fn u64(&self, p: u64) -> Option<u64> {
        let mut b = [0u8; 8];
        self.read(p, &mut b).then_some(u64::from_le_bytes(b))
    }
}

/// Upper bound on a single table's length; anything bigger is corrupt.
pub const MAX_TABLE_LEN: u32 = 16 << 20;
pub const MAX_TABLES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub len: u64,
}

/// True if `[addr, addr + len)` fits in the 64-bit address space. Every
/// range is checked before it is read, so no `addr + offset` below can wrap
/// (a wrapped address would read low memory as if it were the table).
fn fits(addr: u64, len: u64) -> bool {
    addr.checked_add(len).is_some()
}

fn checksum_ok(mem: &impl PhysRead, addr: u64, len: u32) -> bool {
    if !fits(addr, len as u64) {
        return false;
    }
    let mut sum = 0u8;
    let mut buf = [0u8; 256];
    let mut off = 0u64;
    while off < len as u64 {
        let n = (len as u64 - off).min(256) as usize;
        if !mem.read(addr + off, &mut buf[..n]) {
            return false;
        }
        sum = buf[..n].iter().fold(sum, |s, &b| s.wrapping_add(b));
        off += n as u64;
    }
    sum == 0
}

pub struct Acpi<'m, M: PhysRead> {
    mem: &'m M,
    pub rsdp: u64,
    pub rsdp_len: u32,
    pub revision: u8,
    /// (address, entry size) of the root table we use: XSDT (8) or RSDT (4).
    root: Option<(u64, u32, u64)>,
    /// Both root tables, for keeping them mapped.
    pub rsdt: Option<u64>,
    pub xsdt: Option<u64>,
}

impl<'m, M: PhysRead> Acpi<'m, M> {
    pub fn new(mem: &'m M, rsdp: u64) -> Option<Self> {
        let mut sig = [0u8; 8];
        if !fits(rsdp, 36) || !mem.read(rsdp, &mut sig) || &sig != b"RSD PTR " {
            return None;
        }
        if !checksum_ok(mem, rsdp, 20) {
            return None;
        }
        let revision = mem.u8(rsdp + 15)?;
        let rsdt = mem.u32(rsdp + 16)? as u64;
        let mut acpi = Acpi {
            mem,
            rsdp,
            rsdp_len: 20,
            revision,
            root: None,
            rsdt: (rsdt != 0).then_some(rsdt),
            xsdt: None,
        };
        if revision >= 2 {
            let len = mem.u32(rsdp + 20)?;
            if (36..=4096).contains(&len) && checksum_ok(mem, rsdp, len) {
                acpi.rsdp_len = len;
                let x = mem.u64(rsdp + 24)?;
                if x != 0 {
                    acpi.xsdt = Some(x);
                }
            }
        }
        // Prefer the XSDT, but fall back to the RSDT if the XSDT is broken
        // (seen on real firmware).
        if let Some(x) = acpi.xsdt
            && let Some(len) = acpi.header_len(x, b"XSDT")
            && checksum_ok(mem, x, len)
        {
            acpi.root = Some((x, len, 8));
        }
        if acpi.root.is_none()
            && let Some(r) = acpi.rsdt
            && let Some(len) = acpi.header_len(r, b"RSDT")
        {
            acpi.root = Some((r, len, 4));
        }
        Some(acpi)
    }

    fn header_len(&self, addr: u64, want: &[u8; 4]) -> Option<u32> {
        let mut sig = [0u8; 4];
        if !fits(addr, 8) || !self.mem.read(addr, &mut sig) || &sig != want {
            return None;
        }
        let len = self.mem.u32(addr + 4)?;
        ((36..=MAX_TABLE_LEN).contains(&len) && fits(addr, len as u64)).then_some(len)
    }

    /// Calls `f(signature, address, length)` for each table in the root table.
    pub fn for_each_table(&self, mut f: impl FnMut([u8; 4], u64, u32)) {
        let Some((root, len, size)) = self.root else {
            return;
        };
        let count = ((len as u64 - 36) / size).min(MAX_TABLES as u64);
        for i in 0..count {
            let at = root + 36 + i * size;
            let addr = if size == 8 {
                self.mem.u64(at)
            } else {
                self.mem.u32(at).map(|v| v as u64)
            };
            let Some(addr) = addr.filter(|&a| a != 0) else {
                continue;
            };
            let mut sig = [0u8; 4];
            if !fits(addr, 8) || !self.mem.read(addr, &mut sig) {
                continue;
            }
            let Some(tlen) = self
                .mem
                .u32(addr + 4)
                .filter(|&l| (36..=MAX_TABLE_LEN).contains(&l) && fits(addr, l as u64))
            else {
                continue;
            };
            f(sig, addr, tlen);
        }
    }

    pub fn find(&self, sig: &[u8; 4]) -> Option<(u64, u32)> {
        let mut found = None;
        self.for_each_table(|s, a, l| {
            if &s == sig && found.is_none() {
                found = Some((a, l));
            }
        });
        found
    }

    /// Every byte range the ACPI guarantee of base revision 4 covers: RSDP,
    /// RSDT, XSDT, each table they list, and the FACS and DSDT from the FADT.
    pub fn for_each_range(&self, mut f: impl FnMut(Range)) {
        f(Range {
            start: self.rsdp,
            len: self.rsdp_len as u64,
        });
        if let Some(r) = self.rsdt
            && let Some(l) = self.header_len(r, b"RSDT")
        {
            f(Range {
                start: r,
                len: l as u64,
            });
        }
        if let Some(x) = self.xsdt
            && let Some(l) = self.header_len(x, b"XSDT")
        {
            f(Range {
                start: x,
                len: l as u64,
            });
        }
        let mut fadt = None;
        self.for_each_table(|sig, addr, len| {
            f(Range {
                start: addr,
                len: len as u64,
            });
            if &sig == b"FACP" {
                fadt = Some((addr, len));
            }
        });
        if let Some((addr, len)) = fadt {
            let field32 = |off: u64| {
                (len as u64 >= off + 4)
                    .then(|| self.mem.u32(addr + off))
                    .flatten()
                    .map(|v| v as u64)
            };
            let field64 = |off: u64| {
                (len as u64 >= off + 8)
                    .then(|| self.mem.u64(addr + off))
                    .flatten()
            };
            for p in [field32(36), field64(132)].into_iter().flatten() {
                // FACS: signature "FACS", length at +4 (no checksum header).
                // The signature is checked, so a stray pointer never makes
                // random memory look like a table that must stay mapped.
                let mut sig = [0u8; 4];
                if p != 0
                    && fits(p, 8)
                    && self.mem.read(p, &mut sig)
                    && &sig == b"FACS"
                    && let Some(l) = self
                        .mem
                        .u32(p + 4)
                        .filter(|&l| (64..=MAX_TABLE_LEN).contains(&l) && fits(p, l as u64))
                {
                    f(Range {
                        start: p,
                        len: l as u64,
                    });
                }
            }
            for p in [field32(40), field64(140)].into_iter().flatten() {
                if p != 0
                    && let Some(l) = self.header_len(p, b"DSDT")
                {
                    f(Range {
                        start: p,
                        len: l as u64,
                    });
                }
            }
        }
    }

    pub fn mem(&self) -> &M {
        self.mem
    }
}

pub const MAX_IOAPICS: usize = 32;
pub const MAX_NMIS: usize = 32;
pub const MAX_IOMMUS: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LapicNmi {
    /// ACPI processor UID it applies to; u32::MAX means every processor.
    pub uid: u32,
    pub flags: u16,
    pub lint: u8,
}

#[derive(Debug, Default)]
pub struct Madt {
    pub lapic_addr: u64,
    pub pcat_compat: bool,
    pub ioapics: [u64; MAX_IOAPICS],
    pub ioapic_count: usize,
    pub nmis: [LapicNmi; MAX_NMIS],
    pub nmi_count: usize,
    /// (APIC id, ACPI UID) pairs are looked up on demand by `uid_for_apic`.
    addr: u64,
    len: u32,
}

impl Madt {
    pub fn parse(mem: &impl PhysRead, addr: u64, len: u32) -> Option<Madt> {
        if len < 44 || !fits(addr, len as u64) {
            return None;
        }
        let mut m = Madt {
            lapic_addr: mem.u32(addr + 36)? as u64,
            pcat_compat: mem.u32(addr + 40)? & 1 != 0,
            addr,
            len,
            ..Default::default()
        };
        walk_madt(mem, addr, len, |typ, at, elen| match typ {
            1 if elen >= 12 && m.ioapic_count < MAX_IOAPICS => {
                if let Some(a) = mem.u32(at + 4) {
                    m.ioapics[m.ioapic_count] = a as u64;
                    m.ioapic_count += 1;
                }
            }
            4 if elen >= 6 && m.nmi_count < MAX_NMIS => {
                let uid = mem.u8(at + 2).unwrap_or(0);
                m.nmis[m.nmi_count] = LapicNmi {
                    uid: if uid == 0xff { u32::MAX } else { uid as u32 },
                    flags: mem.u16(at + 3).unwrap_or(0),
                    lint: mem.u8(at + 5).unwrap_or(0),
                };
                m.nmi_count += 1;
            }
            5 if elen >= 12 => {
                if let Some(a) = mem.u64(at + 4) {
                    m.lapic_addr = a;
                }
            }
            0x0a if elen >= 12 && m.nmi_count < MAX_NMIS => {
                m.nmis[m.nmi_count] = LapicNmi {
                    uid: mem.u32(at + 4).unwrap_or(0),
                    flags: mem.u16(at + 2).unwrap_or(0),
                    lint: mem.u8(at + 8).unwrap_or(0),
                };
                m.nmi_count += 1;
            }
            _ => {}
        });
        Some(m)
    }

    fn walk(&self, mem: &impl PhysRead, f: impl FnMut(u8, u64, u8)) {
        walk_madt(mem, self.addr, self.len, f)
    }

    /// The ACPI processor UID of the processor with this APIC id.
    pub fn uid_for_apic(&self, mem: &impl PhysRead, apic_id: u32) -> Option<u32> {
        let mut uid = None;
        self.walk(mem, |typ, at, elen| match typ {
            0 if elen >= 8 => {
                if mem.u8(at + 3).map(|v| v as u32) == Some(apic_id) && uid.is_none() {
                    uid = mem.u8(at + 2).map(|v| v as u32);
                }
            }
            9 if elen >= 16 && mem.u32(at + 4) == Some(apic_id) && uid.is_none() => {
                uid = mem.u32(at + 12);
            }
            _ => {}
        });
        uid
    }

    pub fn ioapics(&self) -> &[u64] {
        &self.ioapics[..self.ioapic_count]
    }
    pub fn nmis(&self) -> &[LapicNmi] {
        &self.nmis[..self.nmi_count]
    }
}

fn walk_madt(mem: &impl PhysRead, addr: u64, len: u32, mut f: impl FnMut(u8, u64, u8)) {
    if !fits(addr, len as u64) {
        return;
    }
    let mut off = 44u64;
    while off + 2 <= len as u64 {
        let (Some(typ), Some(elen)) = (mem.u8(addr + off), mem.u8(addr + off + 1)) else {
            return;
        };
        if elen < 2 || off + elen as u64 > len as u64 {
            return;
        }
        f(typ, addr + off, elen);
        off += elen as u64;
    }
}

/// Register base addresses of every remapping unit in a DMAR (Intel VT-d).
pub fn dmar_units(mem: &impl PhysRead, addr: u64, len: u32, out: &mut [u64; MAX_IOMMUS]) -> usize {
    let mut n = 0;
    if !fits(addr, len as u64) {
        return 0;
    }
    let mut off = 48u64;
    while off + 4 <= len as u64 {
        let (Some(typ), Some(slen)) = (mem.u16(addr + off), mem.u16(addr + off + 2)) else {
            break;
        };
        if slen < 4 || off + slen as u64 > len as u64 {
            break;
        }
        if typ == 0
            && slen >= 16
            && let Some(base) = mem.u64(addr + off + 8)
            && n < MAX_IOMMUS
            && !out[..n].contains(&base)
        {
            out[n] = base;
            n += 1;
        }
        off += slen as u64;
    }
    n
}

/// MMIO base addresses of every IOMMU in an IVRS (AMD-Vi). The same IOMMU
/// can be described by several IVHD types, so addresses are de-duplicated.
pub fn ivrs_units(mem: &impl PhysRead, addr: u64, len: u32, out: &mut [u64; MAX_IOMMUS]) -> usize {
    let mut n = 0;
    if !fits(addr, len as u64) {
        return 0;
    }
    let mut off = 48u64;
    while off + 4 <= len as u64 {
        let (Some(typ), Some(blen)) = (mem.u8(addr + off), mem.u16(addr + off + 2)) else {
            break;
        };
        if blen < 4 || off + blen as u64 > len as u64 {
            break;
        }
        if matches!(typ, 0x10 | 0x11 | 0x40)
            && blen >= 24
            && let Some(base) = mem.u64(addr + off + 8)
            && base != 0
            && n < MAX_IOMMUS
            && !out[..n].contains(&base)
        {
            out[n] = base;
            n += 1;
        }
        off += blen as u64;
    }
    n
}

#[cfg(test)]
pub mod sim {
    extern crate std;
    use super::PhysRead;
    use std::vec::Vec;

    /// Physical memory made of byte vectors at fixed addresses.
    pub struct Mem(pub Vec<(u64, Vec<u8>)>);

    impl PhysRead for Mem {
        fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
            let Some(end) = phys.checked_add(buf.len() as u64) else {
                return false;
            };
            for (base, bytes) in &self.0 {
                if phys >= *base && end <= base + bytes.len() as u64 {
                    let o = (phys - base) as usize;
                    buf.copy_from_slice(&bytes[o..o + buf.len()]);
                    return true;
                }
            }
            false
        }
    }

    pub fn table(sig: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(sig);
        t.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
        t.extend_from_slice(&[1, 0]); // revision, checksum placeholder
        t.extend_from_slice(b"OPTIML");
        t.extend_from_slice(b"TESTTBL ");
        t.extend_from_slice(&[0; 12]);
        t.extend_from_slice(body);
        fix(&mut t, 9);
        t
    }

    pub fn fix(t: &mut [u8], at: usize) {
        t[at] = 0;
        let s = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        t[at] = 0u8.wrapping_sub(s);
    }

    pub fn rsdp(xsdt: u64, rsdt: u32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(b"RSD PTR ");
        r.push(0);
        r.extend_from_slice(b"OPTIML");
        r.push(2);
        r.extend_from_slice(&rsdt.to_le_bytes());
        r.extend_from_slice(&36u32.to_le_bytes());
        r.extend_from_slice(&xsdt.to_le_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);
        let s = r[..20].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[8] = 0u8.wrapping_sub(s);
        fix(&mut r, 32);
        r
    }
}

#[cfg(test)]
#[path = "acpi_fuzz.rs"]
mod fuzz;

#[cfg(test)]
mod tests {
    use super::sim::*;
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn system() -> Mem {
        // MADT: LAPIC (apic 0 uid 5), IOAPIC at 0xfec00000, LAPIC NMI all LINT1.
        let mut body = Vec::new();
        body.extend_from_slice(&0xfee0_0000u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&[0, 8, 5, 0, 1, 0, 0, 0]);
        body.extend_from_slice(&[1, 12, 0, 0]);
        body.extend_from_slice(&0xfec0_0000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&[4, 6, 0xff, 5, 0, 1]);
        let madt = table(b"APIC", &body);
        // FADT pointing at FACS and DSDT.
        let mut fbody = vec![0u8; 276 - 36];
        fbody[0..4].copy_from_slice(&0x6000u32.to_le_bytes()); // FIRMWARE_CTRL
        fbody[4..8].copy_from_slice(&0x7000u32.to_le_bytes()); // DSDT
        let fadt = table(b"FACP", &fbody);
        let mut facs = vec![0u8; 64];
        facs[0..4].copy_from_slice(b"FACS");
        facs[4..8].copy_from_slice(&64u32.to_le_bytes());
        let dsdt = table(b"DSDT", &[0xAA; 10]);
        // DMAR with two DRHDs (one duplicate).
        let mut dbody = vec![0u8; 12];
        for base in [0xfed9_0000u64, 0xfed9_1000, 0xfed9_0000] {
            dbody.extend_from_slice(&0u16.to_le_bytes());
            dbody.extend_from_slice(&16u16.to_le_bytes());
            dbody.extend_from_slice(&[0, 0, 0, 0]);
            dbody.extend_from_slice(&base.to_le_bytes());
        }
        let dmar = table(b"DMAR", &dbody);
        let mut xbody = Vec::new();
        for a in [0x2000u64, 0x3000, 0x8000] {
            xbody.extend_from_slice(&a.to_le_bytes());
        }
        let xsdt = table(b"XSDT", &xbody);
        Mem(vec![
            (0xe0000, rsdp(0x1000, 0)),
            (0x1000, xsdt),
            (0x2000, madt),
            (0x3000, fadt),
            (0x6000, facs),
            (0x7000, dsdt),
            (0x8000, dmar),
        ])
    }

    #[test]
    fn reads_tables_and_ranges() {
        let mem = system();
        let acpi = Acpi::new(&mem, 0xe0000).unwrap();
        assert_eq!(acpi.revision, 2);
        let (madt_addr, madt_len) = acpi.find(b"APIC").unwrap();
        assert_eq!(madt_addr, 0x2000);
        let madt = Madt::parse(&mem, madt_addr, madt_len).unwrap();
        assert!(madt.pcat_compat);
        assert_eq!(madt.ioapics(), &[0xfec0_0000]);
        assert_eq!(
            madt.nmis(),
            &[LapicNmi {
                uid: u32::MAX,
                flags: 5,
                lint: 1
            }]
        );
        assert_eq!(madt.uid_for_apic(&mem, 0), Some(5));
        assert_eq!(madt.uid_for_apic(&mem, 1), None);
        let mut ranges = Vec::new();
        acpi.for_each_range(|r| ranges.push(r.start));
        for want in [0xe0000, 0x1000, 0x2000, 0x3000, 0x6000, 0x7000, 0x8000] {
            assert!(ranges.contains(&want), "{want:#x} in {ranges:x?}");
        }
        let (da, dl) = acpi.find(b"DMAR").unwrap();
        let mut units = [0u64; MAX_IOMMUS];
        let n = dmar_units(&mem, da, dl, &mut units);
        assert_eq!(&units[..n], &[0xfed9_0000, 0xfed9_1000]);
    }

    /// Found by the fuzzer: a table at the very top of the address space
    /// whose length runs past 2^64. `addr + offset` used to wrap (a panic in
    /// debug builds; in the release loader, low memory read as if it were
    /// the table). Now any range that does not fit is refused before reading.
    #[test]
    fn tables_that_wrap_the_address_space_are_refused() {
        let top = u64::MAX - 63; // 64 readable bytes: [top, 2^64)
        let mut t = table(b"DMAR", &[0u8; 12]);
        t.resize(64, 0);
        let mem = Mem(vec![(top, t)]);
        let mut u = [0u64; MAX_IOMMUS];
        // len 100 claims 36 bytes beyond the end of the address space.
        assert_eq!(dmar_units(&mem, top, 100, &mut u), 0);
        assert_eq!(ivrs_units(&mem, top, 100, &mut u), 0);
        assert!(Madt::parse(&mem, top, 100).is_none());
        // An RSDP whose 36 bytes would wrap is not read at all.
        assert!(Acpi::new(&mem, u64::MAX - 10).is_none());
        // An XSDT entry pointing 2 bytes below 2^64 is skipped, not wrapped.
        let mut mem = system();
        let xsdt = &mut mem.0[1].1;
        xsdt[36..44].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
        fix(xsdt, 9);
        let acpi = Acpi::new(&mem, 0xe0000).unwrap();
        assert_eq!(acpi.find(b"APIC"), None);
        assert!(acpi.find(b"FACP").is_some());
    }

    /// Found by the fuzzer: a FACS pointer was followed on its length field
    /// alone, so a stray pointer made random memory a range to keep mapped.
    #[test]
    fn facs_needs_its_signature() {
        let mut mem = system();
        let facs = mem.0.iter_mut().find(|r| r.0 == 0x6000).unwrap();
        facs.1[0..4].copy_from_slice(b"XXXX");
        let acpi = Acpi::new(&mem, 0xe0000).unwrap();
        let mut ranges = Vec::new();
        acpi.for_each_range(|r| ranges.push(r.start));
        assert!(!ranges.contains(&0x6000), "{ranges:x?}");
        assert!(ranges.contains(&0x7000));
    }

    #[test]
    fn bad_rsdp_is_rejected_and_corrupt_tables_are_survived() {
        let mut mem = system();
        mem.0[0].1[8] ^= 1;
        assert!(Acpi::new(&mem, 0xe0000).is_none());
        let mut rng = crate::testrng::Rng::new(5);
        for _ in 0..5000 {
            let mut mem = system();
            for _ in 0..4 {
                let t = rng.below(mem.0.len() as u64) as usize;
                let len = mem.0[t].1.len() as u64;
                let at = rng.below(len) as usize;
                mem.0[t].1[at] = rng.next() as u8;
            }
            if let Some(acpi) = Acpi::new(&mem, 0xe0000) {
                acpi.for_each_range(|_| {});
                if let Some((a, l)) = acpi.find(b"APIC")
                    && let Some(m) = Madt::parse(&mem, a, l)
                {
                    let _ = m.uid_for_apic(&mem, 0);
                }
                if let Some((a, l)) = acpi.find(b"DMAR") {
                    let mut u = [0; MAX_IOMMUS];
                    dmar_units(&mem, a, l, &mut u);
                    ivrs_units(&mem, a, l, &mut u);
                }
            }
        }
    }
}
