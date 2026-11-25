//! Finding the tables: the RSDP (ACPI 6.5 section 5.2.5) and the root
//! table it points to, the XSDT (5.2.8) or the older RSDT (5.2.7).

use alloc::vec::Vec;

use crate::{Error, HEADER_LEN, MAX_TABLE_LEN, Sdt, checksum, u32_at, u64_at};

/// Reads physical memory. Returns false if the range can't be read.
pub trait PhysRead {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool;
}

/// Root table entries beyond this many are ignored (and counted): a real
/// machine has a few dozen tables, so more means a corrupt length.
pub const MAX_TABLES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsdp {
    pub revision: u8,
    pub oem_id: [u8; 6],
    /// RSDT address, if not zero.
    pub rsdt: Option<u64>,
    /// XSDT address, if the RSDP is version 2 or later, its extended
    /// checksum is right and the address is not zero.
    pub xsdt: Option<u64>,
    /// Version 2 or later, but the extended part was unusable (bad length
    /// or extended checksum), so only the RSDT can be used.
    pub extended_broken: bool,
}

impl Rsdp {
    /// Parses an RSDP. `bytes` may be longer than the structure.
    pub fn parse(bytes: &[u8]) -> Result<Rsdp, Error> {
        if bytes.len() < 20 {
            return Err(Error::TooShort);
        }
        if &bytes[..8] != b"RSD PTR " {
            return Err(Error::BadSignature);
        }
        // The first 20 bytes (the ACPI 1.0 structure) must sum to zero:
        // that is how firmware and loaders tell a real RSDP from noise.
        if checksum(&bytes[..20]) != 0 {
            return Err(Error::BadChecksum);
        }
        let revision = bytes[15];
        let mut oem_id = [0u8; 6];
        oem_id.copy_from_slice(&bytes[9..15]);
        let rsdt = u32_at(bytes, 16).filter(|&a| a != 0).map(u64::from);
        let mut r = Rsdp {
            revision,
            oem_id,
            rsdt,
            xsdt: None,
            extended_broken: false,
        };
        if revision >= 2 {
            let ext = u32_at(bytes, 20)
                .filter(|&len| (36..=4096).contains(&len))
                .and_then(|len| bytes.get(..len as usize))
                .filter(|ext| checksum(ext) == 0);
            match ext {
                Some(ext) => r.xsdt = u64_at(ext, 24).filter(|&a| a != 0),
                None => r.extended_broken = true,
            }
        }
        if r.rsdt.is_none() && r.xsdt.is_none() {
            return Err(Error::BadField);
        }
        Ok(r)
    }
}

/// One table listed in the root table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRef {
    pub signature: [u8; 4],
    pub phys: u64,
    pub len: u32,
}

/// Which root table the list came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    Xsdt,
    Rsdt,
}

/// Every table the root table lists, plus what was wrong on the way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tables {
    pub rsdp: Rsdp,
    pub root: Root,
    pub root_phys: u64,
    pub root_checksum_ok: bool,
    /// An XSDT was given but unusable, so the RSDT was used instead.
    pub xsdt_fell_back: bool,
    pub list: Vec<TableRef>,
    /// Root entries that were zero, unreadable, or whose header had an
    /// impossible length.
    pub bad_entries: usize,
    /// Root entries past `MAX_TABLES`, or a root length that isn't a whole
    /// number of entries.
    pub ignored_entries: usize,
}

fn read_vec(mem: &impl PhysRead, phys: u64, len: usize) -> Option<Vec<u8>> {
    let mut v = alloc::vec![0u8; len];
    mem.read(phys, &mut v).then_some(v)
}

/// The header of the table at `phys`: (signature, length), if readable and
/// the length is possible.
fn header(mem: &impl PhysRead, phys: u64) -> Option<([u8; 4], u32)> {
    let mut h = [0u8; HEADER_LEN];
    if !mem.read(phys, &mut h) {
        return None;
    }
    let len = u32_at(&h, 4)?;
    if !(HEADER_LEN as u32..=MAX_TABLE_LEN).contains(&len) {
        return None;
    }
    Some(([h[0], h[1], h[2], h[3]], len))
}

/// Reads and checks a root table: Some((bytes, checksum_ok)).
fn root_table(mem: &impl PhysRead, phys: u64, sig: &[u8; 4]) -> Option<(Vec<u8>, bool)> {
    let (s, len) = header(mem, phys)?;
    if &s != sig {
        return None;
    }
    let bytes = read_vec(mem, phys, len as usize)?;
    let ok = Sdt::new(&bytes, sig, HEADER_LEN).ok()?.checksum_ok;
    Some((bytes, ok))
}

impl Tables {
    /// Finds every table from the RSDP at `rsdp_phys`. The XSDT is used
    /// when there is one and it is readable with the right signature and a
    /// possible length; otherwise the RSDT. A wrong root checksum is
    /// reported, not refused.
    pub fn discover(mem: &impl PhysRead, rsdp_phys: u64) -> Result<Tables, Error> {
        let mut raw = [0u8; 36];
        // A version 1 RSDP is only 20 bytes and may sit at the very end of
        // readable memory, so fall back to reading just those.
        let rsdp = if mem.read(rsdp_phys, &mut raw) {
            // The extended structure is 36 bytes today, but its length
            // field allows more; read all of it if so.
            match u32_at(&raw, 20) {
                Some(len) if raw[15] >= 2 && (37..=4096).contains(&len) => {
                    match read_vec(mem, rsdp_phys, len as usize) {
                        Some(v) => Rsdp::parse(&v)?,
                        None => Rsdp::parse(&raw)?,
                    }
                }
                _ => Rsdp::parse(&raw)?,
            }
        } else if mem.read(rsdp_phys, &mut raw[..20]) {
            // Parsing just 20 bytes leaves a version 2 RSDP's extension
            // unusable (and refuses it if it has no RSDT).
            Rsdp::parse(&raw[..20])?
        } else {
            return Err(Error::TooShort);
        };
        let xsdt = rsdp
            .xsdt
            .and_then(|p| Some((p, root_table(mem, p, b"XSDT")?)));
        let (root, root_phys, (bytes, root_checksum_ok)) = match xsdt {
            Some((p, t)) => (Root::Xsdt, p, t),
            None => {
                let p = rsdp.rsdt.ok_or(Error::BadField)?;
                (
                    Root::Rsdt,
                    p,
                    root_table(mem, p, b"RSDT").ok_or(Error::BadField)?,
                )
            }
        };
        let width = if root == Root::Xsdt { 8 } else { 4 };
        let body = &bytes[HEADER_LEN..];
        let mut t = Tables {
            rsdp,
            root,
            root_phys,
            root_checksum_ok,
            xsdt_fell_back: rsdp.xsdt.is_some() && root == Root::Rsdt,
            list: Vec::new(),
            bad_entries: 0,
            ignored_entries: usize::from(body.len() % width != 0),
        };
        for (i, e) in body.chunks_exact(width).enumerate() {
            if i >= MAX_TABLES {
                t.ignored_entries += 1;
                continue;
            }
            let phys = if width == 8 {
                u64_at(e, 0)
            } else {
                u32_at(e, 0).map(u64::from)
            };
            match phys
                .filter(|&p| p != 0)
                .and_then(|p| Some((p, header(mem, p)?)))
            {
                Some((phys, (signature, len))) => t.list.push(TableRef {
                    signature,
                    phys,
                    len,
                }),
                None => t.bad_entries += 1,
            }
        }
        Ok(t)
    }

    /// The first table with this signature.
    pub fn find(&self, sig: &[u8; 4]) -> Option<TableRef> {
        self.list.iter().copied().find(|t| &t.signature == sig)
    }

    /// How many tables have this signature (some firmware lists one twice).
    pub fn count(&self, sig: &[u8; 4]) -> usize {
        self.list.iter().filter(|t| &t.signature == sig).count()
    }

    /// Copies a table out of firmware memory.
    pub fn copy(mem: &impl PhysRead, t: TableRef) -> Option<Vec<u8>> {
        read_vec(mem, t.phys, t.len as usize)
    }
}
