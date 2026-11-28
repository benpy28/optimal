//! Partition tables: the MBR (four primary entries) and GPT (UEFI 2.10,
//! chapter 5). The BIOS stage uses these to find its own boot volume, which
//! the installer recorded as a GPT unique partition GUID or an MBR
//! partition number plus start LBA.
//!
//! GPT is checked the way the spec asks: header CRC, entry array CRC,
//! MyLBA, and usable ranges. A bad primary falls back to the backup at the
//! last LBA (reported, so the user can repair it); both bad is a refusal.

use crate::crc32::Crc32;
use crate::disk::{BlockDevice, MAX_SECTOR, valid_sector_size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MbrEntry {
    pub bootable: bool,
    pub typ: u8,
    pub start_lba: u32,
    pub sectors: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mbr {
    pub disk_id: u32,
    pub entries: [MbrEntry; 4],
}

pub const MBR_TYPE_PROTECTIVE: u8 = 0xee;
pub const MBR_TYPE_EXTENDED: [u8; 3] = [0x05, 0x0f, 0x85];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartError {
    ReadFailed,
    BadSectorSize,
    NoMbrSignature,
    /// Overlapping or out-of-disk MBR entries.
    BadMbr,
    NoGpt,
    /// Both GPT headers (or their entry arrays) failed their checks.
    GptCorrupt,
    NotFound,
    /// Two entries claim the same unique GUID.
    Ambiguous,
    /// The partition is not where the installer recorded it.
    Moved,
    /// An extended or protective entry, or an empty one, was asked for.
    NotAVolume,
}

impl PartError {
    pub fn describe(&self) -> &'static str {
        match self {
            PartError::ReadFailed => "reading the partition table failed",
            PartError::BadSectorSize => "the disk reports a sector size we do not support",
            PartError::NoMbrSignature => "sector 0 has no MBR signature",
            PartError::BadMbr => "the MBR partition table is inconsistent",
            PartError::NoGpt => "the disk has no GPT",
            PartError::GptCorrupt => "both GPT headers are damaged",
            PartError::NotFound => "the boot partition is not on this disk any more",
            PartError::Ambiguous => "two GPT entries share the boot partition's GUID",
            PartError::Moved => "the boot partition moved since installation (reinstall)",
            PartError::NotAVolume => "the recorded MBR entry is not a data partition",
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Mbr {
    /// Parses sector 0. Empty entries (type 0) are kept, with zero sizes.
    /// Non-empty entries must lie inside the disk (when its size is known)
    /// and must not overlap each other.
    pub fn parse(s0: &[u8], disk_sectors: Option<u64>) -> Result<Mbr, PartError> {
        if s0.len() < 512 {
            return Err(PartError::ReadFailed);
        }
        if s0[510] != 0x55 || s0[511] != 0xaa {
            return Err(PartError::NoMbrSignature);
        }
        let mut m = Mbr {
            disk_id: u32_at(s0, 440),
            entries: [MbrEntry {
                bootable: false,
                typ: 0,
                start_lba: 0,
                sectors: 0,
            }; 4],
        };
        for i in 0..4 {
            let e = &s0[446 + 16 * i..446 + 16 * (i + 1)];
            if e[0] & 0x7f != 0 {
                return Err(PartError::BadMbr);
            }
            let typ = e[4];
            let (start, sectors) = (u32_at(e, 8), u32_at(e, 12));
            if typ == 0 {
                continue;
            }
            m.entries[i] = MbrEntry {
                bootable: e[0] == 0x80,
                typ,
                start_lba: start,
                sectors,
            };
        }
        for i in 0..4 {
            let a = m.entries[i];
            // A protective MBR may say 0xFFFFFFFF sectors for a big disk.
            if a.typ == 0 || a.typ == MBR_TYPE_PROTECTIVE {
                continue;
            }
            let end = a.start_lba as u64 + a.sectors as u64;
            if a.start_lba == 0 || a.sectors == 0 || disk_sectors.is_some_and(|n| end > n) {
                return Err(PartError::BadMbr);
            }
            for b in &m.entries[i + 1..] {
                if b.typ == 0 || b.typ == MBR_TYPE_PROTECTIVE {
                    continue;
                }
                let bend = b.start_lba as u64 + b.sectors as u64;
                if (a.start_lba as u64) < bend && (b.start_lba as u64) < end {
                    return Err(PartError::BadMbr);
                }
            }
        }
        Ok(m)
    }

    pub fn is_protective(&self) -> bool {
        self.entries.iter().any(|e| e.typ == MBR_TYPE_PROTECTIVE)
    }

    /// The data partition `index` (1 to 4), checked against the start LBA
    /// the installer recorded.
    pub fn volume(&self, index: u8, start_lba: u64) -> Result<MbrEntry, PartError> {
        if !(1..=4).contains(&index) {
            return Err(PartError::NotFound);
        }
        let e = self.entries[index as usize - 1];
        if e.typ == 0 || e.typ == MBR_TYPE_PROTECTIVE || MBR_TYPE_EXTENDED.contains(&e.typ) {
            return Err(PartError::NotAVolume);
        }
        if e.start_lba as u64 != start_lba {
            return Err(PartError::Moved);
        }
        Ok(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GptHeader {
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: [u8; 16],
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

/// Most entries we will walk: the spec's minimum array is 128; real disks
/// use 128 or 256. A header claiming millions is refused, not walked.
pub const MAX_GPT_ENTRIES: u32 = 4096;

/// Checks one GPT header sector read from `lba`.
pub fn parse_gpt_header(b: &[u8], lba: u64, disk_sectors: Option<u64>) -> Option<GptHeader> {
    let ss = b.len();
    if &b[0..8] != b"EFI PART" {
        return None;
    }
    // Revision 1.x: the major number is the high 16 bits.
    if u32_at(b, 8) >> 16 != 1 {
        return None;
    }
    let hsize = u32_at(b, 12) as usize;
    if hsize < 92 || hsize > ss {
        return None;
    }
    let want = u32_at(b, 16);
    let mut c = Crc32::new();
    c.update(&b[..16]);
    c.update(&[0; 4]);
    c.update(&b[20..hsize]);
    if c.finish() != want {
        return None;
    }
    let h = GptHeader {
        my_lba: u64_at(b, 24),
        alternate_lba: u64_at(b, 32),
        first_usable: u64_at(b, 40),
        last_usable: u64_at(b, 48),
        disk_guid: b[56..72].try_into().unwrap(),
        entries_lba: u64_at(b, 72),
        entry_count: u32_at(b, 80),
        entry_size: u32_at(b, 84),
        entries_crc: u32_at(b, 88),
    };
    if h.my_lba != lba || h.first_usable > h.last_usable {
        return None;
    }
    if h.entry_size < 128 || !h.entry_size.is_power_of_two() || h.entry_size as usize > ss {
        return None;
    }
    if h.entry_count == 0 || h.entry_count > MAX_GPT_ENTRIES {
        return None;
    }
    let array_sectors = (h.entry_count as u64 * h.entry_size as u64).div_ceil(ss as u64);
    let array_end = h.entries_lba.checked_add(array_sectors)?;
    // The array must not overlap the usable area or the header itself.
    let before = array_end <= h.first_usable && h.entries_lba > h.my_lba.min(1);
    let after = h.entries_lba > h.last_usable && array_end <= h.my_lba;
    if !(before || after) {
        return None;
    }
    if let Some(n) = disk_sectors
        && (h.last_usable >= n || array_end > n || h.my_lba >= n)
    {
        return None;
    }
    Some(h)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GptPartition {
    /// 1-based position in the entry array.
    pub index: u32,
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
    pub start_lba: u64,
    pub end_lba: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GptFound {
    pub disk_guid: [u8; 16],
    pub part: GptPartition,
    /// The primary header or array was bad and the backup was used.
    pub used_backup: bool,
}

/// Walks one header's entry array, checking its CRC as it goes, and finds
/// the single entry whose unique GUID is `want`.
fn search_array<D: BlockDevice>(
    dev: &mut D,
    h: &GptHeader,
    want: &[u8; 16],
) -> Result<Option<GptPartition>, PartError> {
    let ss = dev.sector_size();
    let mut buf = [0u8; MAX_SECTOR];
    let mut crc = Crc32::new();
    let per = ss / h.entry_size as usize;
    let mut left = h.entry_count as usize * h.entry_size as usize;
    let mut found: Option<GptPartition> = None;
    let mut dup = false;
    let mut index = 0u32;
    let mut lba = h.entries_lba;
    while left > 0 {
        if !dev.read(lba, &mut buf[..ss]) {
            return Err(PartError::ReadFailed);
        }
        let take = left.min(ss);
        crc.update(&buf[..take]);
        for k in 0..per {
            if index >= h.entry_count {
                break;
            }
            index += 1;
            let e = &buf[k * h.entry_size as usize..(k + 1) * h.entry_size as usize];
            let type_guid: [u8; 16] = e[0..16].try_into().unwrap();
            if type_guid == [0; 16] {
                continue;
            }
            let unique: [u8; 16] = e[16..32].try_into().unwrap();
            if &unique != want {
                continue;
            }
            let (start, end) = (u64_at(e, 32), u64_at(e, 40));
            let p = GptPartition {
                index,
                type_guid,
                unique_guid: unique,
                start_lba: start,
                end_lba: end,
            };
            if found.is_some() {
                dup = true;
            }
            found = Some(p);
        }
        left -= take;
        lba += 1;
    }
    if crc.finish() != h.entries_crc {
        return Err(PartError::GptCorrupt);
    }
    if dup {
        return Err(PartError::Ambiguous);
    }
    if let Some(p) = found
        && (p.start_lba > p.end_lba || p.start_lba < h.first_usable || p.end_lba > h.last_usable)
    {
        return Err(PartError::GptCorrupt);
    }
    Ok(found)
}

/// Finds the partition with unique GUID `want`, checking it still starts at
/// `start_lba`. Uses the primary GPT, or the backup when the primary (or
/// its array) fails its checks.
pub fn gpt_find<D: BlockDevice>(
    dev: &mut D,
    want: &[u8; 16],
    start_lba: u64,
) -> Result<GptFound, PartError> {
    let ss = dev.sector_size();
    if !valid_sector_size(ss) {
        return Err(PartError::BadSectorSize);
    }
    let total = dev.sectors();
    let mut buf = [0u8; MAX_SECTOR];
    if !dev.read(1, &mut buf[..ss]) {
        return Err(PartError::ReadFailed);
    }
    let primary = parse_gpt_header(&buf[..ss], 1, total);
    let mut saw_gpt = &buf[0..8] == b"EFI PART";
    let mut first_err = None;
    if let Some(h) = &primary {
        match search_array(dev, h, want) {
            Ok(Some(p)) => return finish(h, p, false, start_lba),
            Ok(None) => return Err(PartError::NotFound),
            Err(PartError::ReadFailed) => return Err(PartError::ReadFailed),
            Err(e) => first_err = Some(e),
        }
    }
    // The backup: where the primary says, else the disk's last LBA.
    let backup_lba = match (&primary, total) {
        (Some(h), _) => Some(h.alternate_lba),
        (None, Some(n)) if n >= 2 => Some(n - 1),
        _ => None,
    };
    if let Some(lba) = backup_lba
        && dev.read(lba, &mut buf[..ss])
    {
        saw_gpt |= &buf[0..8] == b"EFI PART";
        if let Some(h) = parse_gpt_header(&buf[..ss], lba, total) {
            match search_array(dev, &h, want) {
                Ok(Some(p)) => return finish(&h, p, true, start_lba),
                Ok(None) => return Err(PartError::NotFound),
                Err(PartError::ReadFailed) => return Err(PartError::ReadFailed),
                Err(e) => first_err = first_err.or(Some(e)),
            }
        }
    }
    match first_err {
        Some(PartError::Ambiguous) => Err(PartError::Ambiguous),
        _ if saw_gpt => Err(PartError::GptCorrupt),
        _ => Err(PartError::NoGpt),
    }
}

fn finish(
    h: &GptHeader,
    p: GptPartition,
    used_backup: bool,
    start_lba: u64,
) -> Result<GptFound, PartError> {
    if p.start_lba != start_lba {
        return Err(PartError::Moved);
    }
    Ok(GptFound {
        disk_guid: h.disk_guid,
        part: p,
        used_backup,
    })
}

/// CRC of a GPT header as written (for tools and tests).
pub fn gpt_header_crc(header: &[u8]) -> u32 {
    let hsize = (u32_at(header, 12) as usize).clamp(20, header.len());
    let mut c = Crc32::new();
    c.update(&header[..16]);
    c.update(&[0; 4]);
    c.update(&header[20..hsize]);
    c.finish()
}

/// Returns u16 at `o`, for callers inspecting raw sectors.
pub fn le16(b: &[u8], o: usize) -> u16 {
    u16_at(b, o)
}

#[cfg(test)]
pub mod build {
    //! Writes MBR and GPT layouts into a sparse disk, as a reference for
    //! the parser tests (laid out by hand from UEFI 2.10, 5.3).
    extern crate std;
    use super::*;
    use crate::crc32::crc32;
    use crate::disk::sim::Sparse;
    use std::vec;
    use std::vec::Vec;

    pub struct GptPart {
        pub type_guid: [u8; 16],
        pub unique: [u8; 16],
        pub start: u64,
        pub end: u64,
    }

    pub const DISK_GUID: [u8; 16] = [0xd1; 16];

    /// Header for an array at `entries_lba`, written at `my_lba`.
    fn header(
        d: &Sparse,
        my_lba: u64,
        alt: u64,
        entries_lba: u64,
        count: u32,
        array_crc: u32,
    ) -> Vec<u8> {
        let ss = d.ss;
        let total = d.total;
        let array_sectors = (count as u64 * 128).div_ceil(ss as u64);
        let mut h = vec![0u8; ss];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my_lba.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + array_sectors).to_le_bytes());
        h[48..56].copy_from_slice(&(total - 2 - array_sectors).to_le_bytes());
        h[56..72].copy_from_slice(&DISK_GUID);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&count.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&array_crc.to_le_bytes());
        let c = crc32(&h[..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    }

    /// A protective MBR and both GPT copies holding `parts` in a 128-entry array.
    pub fn gpt(d: &mut Sparse, parts: &[GptPart]) {
        let count = 128u32;
        let mut array = vec![0u8; count as usize * 128];
        for (i, p) in parts.iter().enumerate() {
            let e = &mut array[i * 128..(i + 1) * 128];
            e[0..16].copy_from_slice(&p.type_guid);
            e[16..32].copy_from_slice(&p.unique);
            e[32..40].copy_from_slice(&p.start.to_le_bytes());
            e[40..48].copy_from_slice(&p.end.to_le_bytes());
        }
        let acrc = crc32(&array);
        let ss = d.ss as u64;
        let array_sectors = (array.len() as u64).div_ceil(ss);
        let last = d.total - 1;
        let mut mbr = vec![0u8; 512];
        mbr[446 + 4] = MBR_TYPE_PROTECTIVE;
        mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        mbr[446 + 12..446 + 16]
            .copy_from_slice(&((d.total - 1).min(0xffff_ffff) as u32).to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xaa;
        d.write(0, &mbr);
        let ph = header(d, 1, last, 2, count, acrc);
        d.write(ss, &ph);
        d.write(2 * ss, &array);
        let backup_array = last - array_sectors;
        let bh = header(d, last, 1, backup_array, count, acrc);
        d.write(backup_array * ss, &array);
        d.write(last * ss, &bh);
    }

    pub fn mbr(d: &mut Sparse, disk_id: u32, parts: &[(u8, u32, u32)]) {
        let mut m = vec![0u8; 512];
        m[440..444].copy_from_slice(&disk_id.to_le_bytes());
        for (i, &(typ, start, n)) in parts.iter().enumerate() {
            let e = &mut m[446 + 16 * i..446 + 16 * (i + 1)];
            e[4] = typ;
            e[8..12].copy_from_slice(&start.to_le_bytes());
            e[12..16].copy_from_slice(&n.to_le_bytes());
        }
        m[510] = 0x55;
        m[511] = 0xaa;
        d.write(0, &m);
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;
    use crate::disk::sim::Sparse;
    extern crate std;

    const ESP: [u8; 16] = [0xc1; 16];
    const ME: [u8; 16] = [0x0b; 16];
    const OTHER: [u8; 16] = [0x77; 16];

    fn disk(ss: usize) -> Sparse {
        let mut d = Sparse::new(ss, 8192);
        gpt(
            &mut d,
            &[
                GptPart {
                    type_guid: ESP,
                    unique: OTHER,
                    start: 40,
                    end: 99,
                },
                GptPart {
                    type_guid: ESP,
                    unique: ME,
                    start: 2048,
                    end: 4095,
                },
            ],
        );
        d
    }

    #[test]
    fn finds_the_recorded_gpt_partition() {
        for ss in [512, 4096] {
            let mut d = disk(ss);
            let f = gpt_find(&mut d, &ME, 2048).unwrap();
            assert_eq!(f.part.index, 2);
            assert_eq!((f.part.start_lba, f.part.end_lba), (2048, 4095));
            assert_eq!(f.disk_guid, DISK_GUID);
            assert!(!f.used_backup);
        }
    }

    #[test]
    fn gpt_refusals() {
        let mut d = disk(512);
        assert_eq!(gpt_find(&mut d, &[9; 16], 2048), Err(PartError::NotFound));
        assert_eq!(gpt_find(&mut d, &ME, 2049), Err(PartError::Moved));
        let mut blank = Sparse::new(512, 100);
        assert_eq!(gpt_find(&mut blank, &ME, 2048), Err(PartError::NoGpt));
    }

    #[test]
    fn damaged_primary_falls_back_to_backup() {
        // Header CRC broken.
        let mut d = disk(512);
        let mut h = d.read_bytes(512, 512);
        h[60] ^= 1;
        d.write(512, &h);
        let f = gpt_find(&mut d, &ME, 2048).unwrap();
        assert!(f.used_backup);
        // Array CRC broken (header intact): also falls back.
        let mut d = disk(512);
        let mut a = d.read_bytes(2 * 512, 512);
        a[100] ^= 1;
        d.write(2 * 512, &a);
        assert!(gpt_find(&mut d, &ME, 2048).unwrap().used_backup);
        // Both broken: refused.
        let last = d.total - 1;
        let mut b = d.read_bytes(last * 512, 512);
        b[60] ^= 1;
        d.write(last * 512, &b);
        assert_eq!(gpt_find(&mut d, &ME, 2048), Err(PartError::GptCorrupt));
    }

    #[test]
    fn duplicate_guid_is_refused() {
        let mut d = Sparse::new(512, 8192);
        gpt(
            &mut d,
            &[
                GptPart {
                    type_guid: ESP,
                    unique: ME,
                    start: 40,
                    end: 99,
                },
                GptPart {
                    type_guid: ESP,
                    unique: ME,
                    start: 2048,
                    end: 4095,
                },
            ],
        );
        assert_eq!(gpt_find(&mut d, &ME, 2048), Err(PartError::Ambiguous));
    }

    #[test]
    fn entry_outside_usable_range_is_corrupt() {
        let mut d = Sparse::new(512, 8192);
        gpt(
            &mut d,
            &[GptPart {
                type_guid: ESP,
                unique: ME,
                start: 2048,
                end: 9000,
            }],
        );
        assert_eq!(gpt_find(&mut d, &ME, 2048), Err(PartError::GptCorrupt));
    }

    #[test]
    fn header_field_checks() {
        let d = disk(512);
        let good = d.read_bytes(512, 512);
        assert!(parse_gpt_header(&good, 1, Some(8192)).is_some());
        // Read from the wrong LBA (MyLBA mismatch).
        assert!(parse_gpt_header(&good, 2, Some(8192)).is_none());
        // Disk smaller than the header claims.
        assert!(parse_gpt_header(&good, 1, Some(4000)).is_none());
        // Each field lie, with the CRC fixed up so only the field check can catch it.
        let lie = |off: usize, v: u32| {
            let mut h = good.clone();
            h[off..off + 4].copy_from_slice(&v.to_le_bytes());
            let c = gpt_header_crc(&h);
            h[16..20].copy_from_slice(&c.to_le_bytes());
            parse_gpt_header(&h, 1, Some(8192))
        };
        assert!(lie(12, 91).is_none()); // header size below 92
        assert!(lie(12, 513).is_none()); // header bigger than the sector
        assert!(lie(84, 96).is_none()); // entry size not a power of two
        assert!(lie(84, 64).is_none()); // entry size below 128
        assert!(lie(80, 0).is_none()); // no entries
        assert!(lie(80, MAX_GPT_ENTRIES + 1).is_none());
        assert!(lie(8, 0x0002_0000).is_none()); // revision 2.0
        assert!(lie(72, 40).is_none()); // array inside the usable area
        assert!(lie(72, 1).is_none()); // array on top of the header
        assert!(lie(8, 0x0001_0001).is_some()); // revision 1.1 is fine
    }

    #[test]
    fn mbr_volumes() {
        let mut d = Sparse::new(512, 10000);
        build::mbr(
            &mut d,
            0xdead_beef,
            &[(0x0c, 2048, 4000), (0x05, 6048, 100), (0x83, 7000, 100)],
        );
        let s0 = d.read_bytes(0, 512);
        let m = Mbr::parse(&s0, Some(10000)).unwrap();
        assert_eq!(m.disk_id, 0xdead_beef);
        assert_eq!(m.volume(1, 2048).unwrap().sectors, 4000);
        assert_eq!(m.volume(1, 2047), Err(PartError::Moved));
        assert_eq!(m.volume(2, 6048), Err(PartError::NotAVolume)); // extended
        assert_eq!(m.volume(4, 0), Err(PartError::NotAVolume)); // empty
        assert_eq!(m.volume(0, 0), Err(PartError::NotFound));
        assert_eq!(m.volume(5, 0), Err(PartError::NotFound));
        assert!(!m.is_protective());
    }

    #[test]
    fn mbr_refusals() {
        let mk = |parts: &[(u8, u32, u32)]| {
            let mut d = Sparse::new(512, 10000);
            build::mbr(&mut d, 1, parts);
            Mbr::parse(&d.read_bytes(0, 512), Some(10000))
        };
        assert_eq!(
            mk(&[(0x0c, 100, 200), (0x0c, 250, 10)]),
            Err(PartError::BadMbr)
        ); // overlap
        assert_eq!(mk(&[(0x0c, 9990, 20)]), Err(PartError::BadMbr)); // past the end
        assert_eq!(mk(&[(0x0c, 0, 20)]), Err(PartError::BadMbr)); // on top of the MBR
        assert_eq!(mk(&[(0x0c, 10, 0)]), Err(PartError::BadMbr)); // empty but typed
        assert!(mk(&[(0xee, 1, 0xffff_ffff)]).unwrap().is_protective());
        let mut s0 = [0u8; 512];
        assert_eq!(Mbr::parse(&s0, None), Err(PartError::NoMbrSignature));
        s0[510] = 0x55;
        s0[511] = 0xaa;
        s0[446] = 0x81; // boot flag must be 0x00 or 0x80
        s0[446 + 4] = 0x0c;
        assert_eq!(Mbr::parse(&s0, None), Err(PartError::BadMbr));
    }

    #[test]
    fn fuzz_gpt() {
        // Mutated GPT sectors: never a panic, never an out-of-disk result,
        // and a found partition is always the one asked for.
        crate::fuzzkit::run("gpt", 0x6b7, |rng| {
            let mut d = disk(512);
            let sector = [1u64, 2, 3, d.total - 1, d.total - 33][rng.below(5) as usize];
            let mut b = d.read_bytes(sector * 512, 512);
            crate::fuzzkit::mutate(rng, &mut b);
            b.resize(512, 0);
            d.write(sector * 512, &b);
            if rng.below(4) == 0 {
                // Re-seal the header CRC so field checks get exercised too.
                for lba in [1, d.total - 1] {
                    let mut h = d.read_bytes(lba * 512, 512);
                    let hs = u32_at(&h, 12) as usize;
                    if (92..=512).contains(&hs) {
                        let c = gpt_header_crc(&h);
                        h[16..20].copy_from_slice(&c.to_le_bytes());
                        d.write(lba * 512, &h);
                    }
                }
            }
            if let Ok(f) = gpt_find(&mut d, &ME, 2048) {
                assert_eq!(f.part.unique_guid, ME);
                assert_eq!(f.part.start_lba, 2048);
                assert!(f.part.end_lba < d.total);
            }
            assert!(d.reads < 200, "GPT walk read {} sectors", d.reads);
        });
    }

    #[test]
    fn fuzz_mbr() {
        crate::fuzzkit::run("mbr", 0x3b7, |rng| {
            let mut d = Sparse::new(512, 10000);
            build::mbr(&mut d, 7, &[(0x0c, 2048, 4000), (0x83, 7000, 100)]);
            let mut b = d.read_bytes(0, 512);
            crate::fuzzkit::mutate(rng, &mut b);
            if let Ok(m) = Mbr::parse(&b, Some(10000)) {
                for i in 1..=4u8 {
                    if let Ok(e) = m.volume(i, m.entries[i as usize - 1].start_lba as u64) {
                        assert!(e.start_lba as u64 + e.sectors as u64 <= 10000);
                        assert!(e.start_lba > 0 && e.sectors > 0);
                    }
                }
            }
        });
    }
}
