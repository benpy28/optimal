//! ACPI table parsing for the Optimal kernel.
//!
//! Written from the ACPI 6.5 specification (sections 5.2.5 to 5.2.12 and
//! 5.2.9 for the FADT), the IA-PC HPET specification 1.0a, the PCI
//! Firmware Specification 3.3 (the MCFG) and the Intel VT-d specification
//! 4.1 (the DMAR header). Plain safe Rust with no hardware
//! access: the kernel copies each table out of firmware memory and hands
//! the bytes in, so every parser works on a byte slice and is tested on the
//! host.
//!
//! Policy for broken firmware (real machines ship all of these):
//! - A table whose checksum is wrong is still parsed, because many shipping
//!   BIOSes get it wrong and refusing would leave the machine unusable. The
//!   result says so (`checksum_ok`), and the kernel reports it.
//! - Anything that would make a parser read outside the table, or that
//!   breaks the table's structure, is an error or a skipped entry, never a
//!   panic and never a guess. Every skipped or ignored item is counted in
//!   the result, so the kernel can report it.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod dmar;
pub mod fadt;
pub mod hpet;
pub mod madt;
pub mod mcfg;
pub mod reach;
pub mod tables;

/// Length of the common header every system description table starts with.
pub const HEADER_LEN: usize = 36;

/// Upper bound on one table's length; anything bigger is corrupt.
pub const MAX_TABLE_LEN: u32 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the structure needs.
    TooShort,
    /// The signature isn't the one asked for.
    BadSignature,
    /// The table's length field is below the minimum, above
    /// `MAX_TABLE_LEN`, or larger than the bytes supplied.
    BadLength,
    /// A checksum that must be right (the RSDP's) is wrong.
    BadChecksum,
    /// A required field holds a value the specification forbids.
    BadField,
}

/// Sum of all bytes, modulo 256. ACPI checksums make this zero.
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b))
}

pub(crate) fn u8_at(b: &[u8], off: usize) -> Option<u8> {
    b.get(off).copied()
}

pub(crate) fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(off..off.checked_add(2)?)?.try_into().ok()?,
    ))
}

pub(crate) fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}

pub(crate) fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// A checked system description table: `bytes` is exactly the table
/// (`length` bytes), whose signature matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sdt<'a> {
    pub bytes: &'a [u8],
    pub revision: u8,
    pub checksum_ok: bool,
}

impl<'a> Sdt<'a> {
    /// Checks the common header of the table at the start of `bytes`.
    /// `bytes` may be longer than the table; the result is cut to its length.
    pub fn new(bytes: &'a [u8], sig: &[u8; 4], min_len: usize) -> Result<Self, Error> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::TooShort);
        }
        if &bytes[..4] != sig {
            return Err(Error::BadSignature);
        }
        let len = u32_at(bytes, 4).ok_or(Error::TooShort)?;
        if len > MAX_TABLE_LEN || (len as usize) < min_len.max(HEADER_LEN) {
            return Err(Error::BadLength);
        }
        let bytes = bytes.get(..len as usize).ok_or(Error::BadLength)?;
        Ok(Sdt {
            bytes,
            revision: bytes[8],
            checksum_ok: checksum(bytes) == 0,
        })
    }

    /// The table's body, after the common header.
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[HEADER_LEN..]
    }
}

/// Generic Address Structure (ACPI 6.5 section 5.2.3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gas {
    pub space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    pub access_size: u8,
    pub address: u64,
}

impl Gas {
    pub const SYSTEM_MEMORY: u8 = 0;
    pub const SYSTEM_IO: u8 = 1;

    pub(crate) fn at(b: &[u8], off: usize) -> Option<Gas> {
        Some(Gas {
            space: u8_at(b, off)?,
            bit_width: u8_at(b, off + 1)?,
            bit_offset: u8_at(b, off + 2)?,
            access_size: u8_at(b, off + 3)?,
            address: u64_at(b, off + 4)?,
        })
    }
}

/// Property-test case count: `PROPTEST_CASES` if set (CI runs far more),
/// otherwise the given default.
#[cfg(test)]
pub(crate) fn test_cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Test helper: builds a table with a correct length and checksum.
#[cfg(test)]
pub(crate) fn build_table(sig: &[u8; 4], revision: u8, body: &[u8]) -> std::vec::Vec<u8> {
    let mut t = std::vec::Vec::with_capacity(HEADER_LEN + body.len());
    t.extend_from_slice(sig);
    t.extend_from_slice(&((HEADER_LEN + body.len()) as u32).to_le_bytes());
    t.push(revision);
    t.push(0); // checksum, fixed below
    t.extend_from_slice(b"OPTIML"); // OEM ID
    t.extend_from_slice(b"TESTTABL"); // OEM table ID
    t.extend_from_slice(&1u32.to_le_bytes()); // OEM revision
    t.extend_from_slice(b"OPTM"); // creator ID
    t.extend_from_slice(&1u32.to_le_bytes()); // creator revision
    t.extend_from_slice(body);
    t[9] = 0u8.wrapping_sub(checksum(&t));
    t
}

#[cfg(test)]
mod suite;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_byte_sum_mod_256() {
        assert_eq!(checksum(&[]), 0);
        assert_eq!(checksum(&[0xff, 0x01]), 0);
        assert_eq!(checksum(&[0x80, 0x80, 0x05]), 5);
    }

    #[test]
    fn little_endian_reads_and_bounds() {
        let b = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(u8_at(&b, 8), Some(9));
        assert_eq!(u8_at(&b, 9), None);
        assert_eq!(u16_at(&b, 0), Some(0x0201));
        assert_eq!(u16_at(&b, 8), None);
        assert_eq!(u32_at(&b, 5), Some(0x0908_0706));
        assert_eq!(u32_at(&b, 6), None);
        assert_eq!(u64_at(&b, 1), Some(0x0908_0706_0504_0302));
        assert_eq!(u64_at(&b, 2), None);
        assert_eq!(u64_at(&b, usize::MAX), None);
    }

    #[test]
    fn header_hand_worked() {
        let t = build_table(b"TEST", 3, &[0xaa, 0xbb]);
        assert_eq!(t.len(), 38);
        assert_eq!(checksum(&t), 0);
        let s = Sdt::new(&t, b"TEST", 0).unwrap();
        assert_eq!(s.revision, 3);
        assert!(s.checksum_ok);
        assert_eq!(s.body(), &[0xaa, 0xbb]);
        // Extra bytes after the table are cut off.
        let mut longer = t.clone();
        longer.extend_from_slice(&[1, 2, 3]);
        assert_eq!(Sdt::new(&longer, b"TEST", 0).unwrap().bytes.len(), 38);
        // Exactly the minimum length is fine; one more is too long.
        assert!(Sdt::new(&t, b"TEST", 38).is_ok());
        assert_eq!(Sdt::new(&t, b"TEST", 39), Err(Error::BadLength));
        assert_eq!(Sdt::new(&t, b"TESX", 0), Err(Error::BadSignature));
        assert_eq!(Sdt::new(&t[..35], b"TEST", 0), Err(Error::TooShort));
        assert_eq!(Sdt::new(&t[..36], b"TEST", 0), Err(Error::BadLength));
        // A wrong checksum is reported, not refused.
        let mut bad = t.clone();
        bad[36] ^= 1;
        let s = Sdt::new(&bad, b"TEST", 0).unwrap();
        assert!(!s.checksum_ok);
    }

    #[test]
    fn header_length_limits() {
        let mut t = build_table(b"TEST", 1, &[]);
        // Length below the header itself.
        t[4..8].copy_from_slice(&35u32.to_le_bytes());
        assert_eq!(Sdt::new(&t, b"TEST", 0), Err(Error::BadLength));
        // Exactly the header is a valid, empty table.
        t[4..8].copy_from_slice(&36u32.to_le_bytes());
        assert!(Sdt::new(&t, b"TEST", 0).is_ok());
        // Over the cap, even if the bytes were there.
        t[4..8].copy_from_slice(&(MAX_TABLE_LEN + 1).to_le_bytes());
        assert_eq!(Sdt::new(&t, b"TEST", 0), Err(Error::BadLength));
        let big = {
            let mut v = vec![0u8; MAX_TABLE_LEN as usize];
            v[..4].copy_from_slice(b"TEST");
            v[4..8].copy_from_slice(&MAX_TABLE_LEN.to_le_bytes());
            v
        };
        assert!(Sdt::new(&big, b"TEST", 0).is_ok());
    }

    #[test]
    fn gas_hand_worked() {
        let b = [1u8, 32, 0, 3, 0x08, 0x04, 0, 0, 0, 0, 0, 0, 0xee];
        assert_eq!(
            Gas::at(&b, 0),
            Some(Gas {
                space: 1,
                bit_width: 32,
                bit_offset: 0,
                access_size: 3,
                address: 0x408
            })
        );
        assert_eq!(Gas::at(&b, 2), None);
    }
}
