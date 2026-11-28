//! Working out what to write, from the disk's partition table and the BIOS
//! stage's ELF, without writing anything. Kept apart from the file I/O so
//! every check is unit tested on in-memory disks.

use optimal_boot_core::crc32::{Crc32, crc32};
use optimal_boot_core::part::{MBR_TYPE_EXTENDED, MBR_TYPE_PROTECTIVE, Mbr, parse_gpt_header};

pub trait Disk {
    fn len(&self) -> u64;
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<(), String>;
}

/// "Hah!IdontNeedEFI": the BIOS boot partition type GUID
/// 21686148-6449-6E6F-744E-656564454649, as GPT stores it.
pub const BIOS_BOOT_GUID: [u8; 16] = *b"Hah!IdontNeedEFI";

/// Stage 1's bytes (the 440 bytes of MBR code) and stage 2's file image.
#[derive(Debug)]
pub struct Stages {
    pub stage1: Vec<u8>,
    pub stage2: Vec<u8>,
}

pub const STAGE1_LEN: usize = 440;
const STAGE1_LBA_FIELD: usize = 0x1a8;
const STAGE1_COUNT_FIELD: usize = 0x1b0;
/// Stage 2 runs from 0x8000 and must end below the bounce buffer at
/// 0x70000 (its .bss included, which the linker script checks).
pub const STAGE2_MAX: usize = 0x70000 - 0x8000;
const HEADER_LEN: usize = 0x40;

#[derive(Debug)]
pub struct WriteOp {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct Plan {
    pub writes: Vec<WriteOp>,
    pub summary: String,
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

/// Pulls stage 1 (the PT_LOAD at 0x7C00) and stage 2 (the one at 0x8000)
/// out of the linked BIOS stage.
pub fn stages_from_elf(elf: &[u8]) -> Result<Stages, String> {
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return Err("not a little-endian ELF64 file".into());
    }
    if u16_at(elf, 18) != 62 {
        return Err("not an x86-64 ELF".into());
    }
    let phoff = u64_at(elf, 32) as usize;
    let phentsize = u16_at(elf, 54) as usize;
    let phnum = u16_at(elf, 56) as usize;
    if phentsize < 56
        || phoff
            .checked_add(phentsize * phnum)
            .is_none_or(|e| e > elf.len())
    {
        return Err("bad program headers".into());
    }
    let (mut s1, mut s2) = (None, None);
    for i in 0..phnum {
        let p = &elf[phoff + i * phentsize..];
        if u32_at(p, 0) != 1 {
            continue; // not PT_LOAD
        }
        let off = u64_at(p, 8) as usize;
        let vaddr = u64_at(p, 16);
        let filesz = u64_at(p, 32) as usize;
        if filesz == 0 {
            continue;
        }
        let bytes = elf
            .get(off..off.checked_add(filesz).ok_or("bad segment")?)
            .ok_or("segment runs past the file")?
            .to_vec();
        match vaddr {
            0x7c00 => s1 = Some(bytes),
            0x8000 => s2 = Some(bytes),
            _ => return Err(format!("unexpected segment at {vaddr:#x}")),
        }
    }
    let (Some(stage1), Some(stage2)) = (s1, s2) else {
        return Err("no stage 1 or stage 2 segment: is this optimal-boot-bios?".into());
    };
    if stage1.len() != STAGE1_LEN {
        return Err(format!(
            "stage 1 is {} bytes, not {STAGE1_LEN}",
            stage1.len()
        ));
    }
    if stage2.len() < HEADER_LEN || &stage2[0..8] != b"OBSTAGE2" || u32_at(&stage2, 0x30) != 1 {
        return Err("stage 2 has no version-1 OBSTAGE2 header".into());
    }
    if stage2.len() > STAGE2_MAX {
        return Err(format!(
            "stage 2 is {} bytes, more than {STAGE2_MAX}",
            stage2.len()
        ));
    }
    Ok(Stages { stage1, stage2 })
}

enum Volume {
    Gpt { guid: [u8; 16], start: u64 },
    Mbr { index: u8, start: u64 },
}

/// Fills in stage 2's header and pads it to whole 512-byte sectors.
pub fn patch_stage2(
    stage2: &[u8],
    vol_kind: u8,
    mbr_index: u8,
    start: u64,
    guid: [u8; 16],
) -> Vec<u8> {
    let mut s = stage2.to_vec();
    s[0x08..0x0c].copy_from_slice(&(stage2.len() as u32).to_le_bytes());
    let crc = crc32(&stage2[HEADER_LEN..]);
    s[0x0c..0x10].copy_from_slice(&crc.to_le_bytes());
    s[0x10] = vol_kind;
    s[0x11] = mbr_index;
    s[0x18..0x20].copy_from_slice(&start.to_le_bytes());
    s[0x20..0x30].copy_from_slice(&guid);
    s.resize(stage2.len().div_ceil(512) * 512, 0);
    s
}

pub fn plan(d: &mut impl Disk, st: &Stages, partition: u32, ss: usize) -> Result<Plan, String> {
    // Stage 1 reads 512-byte sectors; on a 4Kn disk that is not what INT 13h
    // counts, so refuse rather than install something that cannot boot.
    if ss != 512 {
        return Err("stage 1 reads 512-byte sectors: 4 KiB-sector disks are not supported by the BIOS stage yet".into());
    }
    let total = d.len() / ss as u64;
    let mut s0 = vec![0u8; ss.max(512)];
    d.read_at(0, &mut s0)?;
    let mbr = Mbr::parse(&s0, Some(total)).map_err(|e| e.describe().to_string())?;
    let stage2_sectors_512 = st.stage2.len().div_ceil(512) as u64;
    // Where stage 2 goes, in 512-byte units (what stage 1 reads in).
    let (stage2_lba512, room512, vol, where_) = if mbr.is_protective() {
        let mut h = vec![0u8; ss];
        d.read_at(ss as u64, &mut h)?;
        let hdr = parse_gpt_header(&h, 1, Some(total))
            .ok_or("the primary GPT header is damaged (repair the disk first)")?;
        let mut array = vec![0u8; hdr.entry_count as usize * hdr.entry_size as usize];
        d.read_at(hdr.entries_lba * ss as u64, &mut array)?;
        if crc32(&array) != hdr.entries_crc {
            return Err("the GPT entry array is damaged (repair the disk first)".into());
        }
        let mut bios_boot = Vec::new();
        let mut target = None;
        for (i, e) in array.chunks_exact(hdr.entry_size as usize).enumerate() {
            let typ: [u8; 16] = e[0..16].try_into().unwrap();
            if typ == [0; 16] {
                continue;
            }
            let (start, end) = (u64_at(e, 32), u64_at(e, 40));
            if typ == BIOS_BOOT_GUID {
                bios_boot.push((i + 1, start, end));
            }
            if i as u32 + 1 == partition {
                target = Some((e[16..32].try_into().unwrap(), start));
            }
        }
        let Some((guid, start)) = target else {
            return Err(format!("GPT partition {partition} does not exist"));
        };
        let guid: [u8; 16] = guid;
        if bios_boot.iter().any(|&(i, ..)| i as u32 == partition) {
            return Err(format!(
                "partition {partition} is the BIOS boot partition, not a FAT volume"
            ));
        }
        let &[(bi, bs, be)] = &bios_boot[..] else {
            return Err(format!(
                "the disk needs exactly one BIOS boot partition (type 21686148-6449-6E6F-744E-656564454649); it has {}",
                bios_boot.len()
            ));
        };
        if be < bs {
            return Err("the BIOS boot partition's end is before its start".into());
        }
        let per = ss as u64 / 512;
        (
            bs * per,
            (be - bs + 1) * per,
            Volume::Gpt { guid, start },
            format!("GPT BIOS boot partition {bi}"),
        )
    } else {
        if !(1..=4).contains(&partition) {
            return Err(format!("MBR partition {partition} does not exist (1 to 4)"));
        }
        let e = mbr.entries[partition as usize - 1];
        if e.typ == 0 || MBR_TYPE_EXTENDED.contains(&e.typ) || e.typ == MBR_TYPE_PROTECTIVE {
            return Err(format!("MBR partition {partition} is empty or extended"));
        }
        let first = mbr
            .entries
            .iter()
            .filter(|x| x.typ != 0)
            .map(|x| x.start_lba as u64)
            .min()
            .unwrap();
        let per = ss as u64 / 512;
        (
            per,
            (first - 1) * per,
            Volume::Mbr {
                index: partition as u8,
                start: e.start_lba as u64,
            },
            "the gap after the MBR".to_string(),
        )
    };
    if stage2_sectors_512 > room512 {
        return Err(format!(
            "{where_} has {} KiB; stage 2 needs {} KiB",
            room512 / 2,
            stage2_sectors_512.div_ceil(2)
        ));
    }
    if stage2_lba512 + stage2_sectors_512 > 1 << 48 {
        return Err("stage 2 would sit past the 48-bit LBA limit".into());
    }
    let (kind, idx, start, guid) = match vol {
        Volume::Gpt { guid, start } => (1u8, 0u8, start, guid),
        Volume::Mbr { index, start } => (2u8, index, start, [0; 16]),
    };
    let s2 = patch_stage2(&st.stage2, kind, idx, start, guid);
    let mut s1 = st.stage1.clone();
    s1[STAGE1_LBA_FIELD..STAGE1_LBA_FIELD + 8].copy_from_slice(&stage2_lba512.to_le_bytes());
    s1[STAGE1_COUNT_FIELD..STAGE1_COUNT_FIELD + 2]
        .copy_from_slice(&(stage2_sectors_512 as u16).to_le_bytes());
    let mut c = Crc32::new();
    c.update(&s2[HEADER_LEN..st.stage2.len()]);
    debug_assert_eq!(c.finish(), u32_at(&s2, 0x0c));
    let summary = format!(
        "Installed: stage 1 in sector 0, stage 2 ({} bytes) at LBA {stage2_lba512} in {where_}; boot volume partition {partition}",
        st.stage2.len()
    );
    Ok(Plan {
        writes: vec![
            WriteOp {
                offset: stage2_lba512 * 512,
                bytes: s2,
            },
            WriteOp {
                offset: 0,
                bytes: s1,
            },
        ],
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<u8>);
    impl Disk for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<(), String> {
            let o = off as usize;
            let src = self.0.get(o..o + buf.len()).ok_or("past the end")?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    fn stages(s2_len: usize) -> Stages {
        let mut stage2 = vec![0xabu8; s2_len];
        stage2[0..8].copy_from_slice(b"OBSTAGE2");
        stage2[8..0x40].fill(0);
        stage2[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        Stages {
            stage1: vec![0x90; STAGE1_LEN],
            stage2,
        }
    }

    fn mbr_disk(parts: &[(u8, u32, u32)]) -> Mem {
        let mut d = vec![0u8; 8 << 20];
        d[440..444].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        for (i, &(t, s, n)) in parts.iter().enumerate() {
            let e = &mut d[446 + 16 * i..446 + 16 * (i + 1)];
            e[4] = t;
            e[8..12].copy_from_slice(&s.to_le_bytes());
            e[12..16].copy_from_slice(&n.to_le_bytes());
        }
        d[510] = 0x55;
        d[511] = 0xaa;
        Mem(d)
    }

    /// A GPT disk: protective MBR, primary header and a 128-entry array.
    fn gpt_disk(parts: &[([u8; 16], [u8; 16], u64, u64)]) -> Mem {
        let total = 16384u64;
        let mut d = vec![0u8; total as usize * 512];
        d[446 + 4] = 0xee;
        d[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
        d[446 + 12..446 + 16].copy_from_slice(&((total - 1) as u32).to_le_bytes());
        d[510] = 0x55;
        d[511] = 0xaa;
        let mut array = vec![0u8; 128 * 128];
        for (i, (t, u, s, e)) in parts.iter().enumerate() {
            let x = &mut array[i * 128..(i + 1) * 128];
            x[0..16].copy_from_slice(t);
            x[16..32].copy_from_slice(u);
            x[32..40].copy_from_slice(&s.to_le_bytes());
            x[40..48].copy_from_slice(&e.to_le_bytes());
        }
        d[1024..1024 + array.len()].copy_from_slice(&array);
        let h = &mut d[512..1024];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[32..40].copy_from_slice(&(total - 1).to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(total - 34).to_le_bytes());
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(&array).to_le_bytes());
        let c = crc32(&h[..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        Mem(d)
    }

    const ESP: [u8; 16] = [0xc1; 16];
    const ME: [u8; 16] = [0x0b; 16];

    #[test]
    fn gpt_install() {
        let mut d = gpt_disk(&[(BIOS_BOOT_GUID, [1; 16], 34, 2047), (ESP, ME, 2048, 16000)]);
        let st = stages(150_000);
        let p = plan(&mut d, &st, 2, 512).unwrap();
        let s2 = &p.writes[0];
        assert_eq!(s2.offset, 34 * 512);
        assert_eq!(s2.bytes.len(), 150_016); // padded to sectors
        assert_eq!(u32_at(&s2.bytes, 8), 150_000);
        assert_eq!(u32_at(&s2.bytes, 12), crc32(&st.stage2[0x40..]));
        assert_eq!(s2.bytes[0x10], 1);
        assert_eq!(u64_at(&s2.bytes, 0x18), 2048);
        assert_eq!(&s2.bytes[0x20..0x30], &ME);
        let s1 = &p.writes[1];
        assert_eq!((s1.offset, s1.bytes.len()), (0, 440));
        assert_eq!(u64_at(&s1.bytes, 0x1a8), 34);
        assert_eq!(u16_at(&s1.bytes, 0x1b0), 293);
    }

    #[test]
    fn gpt_refusals() {
        let st = stages(150_000);
        // No BIOS boot partition.
        let mut d = gpt_disk(&[(ESP, ME, 2048, 16000)]);
        assert!(
            plan(&mut d, &st, 1, 512)
                .unwrap_err()
                .contains("exactly one BIOS boot")
        );
        // Two of them.
        let mut d = gpt_disk(&[
            (BIOS_BOOT_GUID, [1; 16], 34, 1000),
            (BIOS_BOOT_GUID, [2; 16], 1001, 2047),
            (ESP, ME, 2048, 16000),
        ]);
        assert!(
            plan(&mut d, &st, 3, 512)
                .unwrap_err()
                .contains("exactly one")
        );
        // Too small for stage 2 (100 sectors = 50 KiB).
        let mut d = gpt_disk(&[(BIOS_BOOT_GUID, [1; 16], 34, 133), (ESP, ME, 2048, 16000)]);
        assert!(
            plan(&mut d, &st, 2, 512)
                .unwrap_err()
                .contains("needs 147 KiB")
        );
        // The boot volume is the BIOS boot partition itself, or missing.
        let mut d = gpt_disk(&[(BIOS_BOOT_GUID, [1; 16], 34, 2047), (ESP, ME, 2048, 16000)]);
        assert!(
            plan(&mut d, &st, 1, 512)
                .unwrap_err()
                .contains("BIOS boot partition, not")
        );
        assert!(
            plan(&mut d, &st, 3, 512)
                .unwrap_err()
                .contains("does not exist")
        );
        // A damaged entry array.
        d.0[1024 + 40] ^= 1;
        assert!(
            plan(&mut d, &st, 2, 512)
                .unwrap_err()
                .contains("entry array is damaged")
        );
        // A damaged header.
        let mut d = gpt_disk(&[(BIOS_BOOT_GUID, [1; 16], 34, 2047), (ESP, ME, 2048, 16000)]);
        d.0[512 + 50] ^= 1;
        assert!(
            plan(&mut d, &st, 2, 512)
                .unwrap_err()
                .contains("header is damaged")
        );
    }

    #[test]
    fn mbr_install_keeps_the_table() {
        let mut d = mbr_disk(&[(0x0c, 2048, 10000)]);
        let st = stages(150_000);
        let p = plan(&mut d, &st, 1, 512).unwrap();
        assert_eq!(p.writes[0].offset, 512);
        assert_eq!(p.writes[0].bytes[0x10], 2);
        assert_eq!(p.writes[0].bytes[0x11], 1);
        assert_eq!(u64_at(&p.writes[0].bytes, 0x18), 2048);
        assert_eq!(&p.writes[0].bytes[0x20..0x30], &[0; 16]);
        // Stage 1 covers bytes 0..440 only: signature and table untouched.
        assert_eq!(p.writes[1].bytes.len(), 440);
        assert_eq!(u64_at(&p.writes[1].bytes, 0x1a8), 1);
    }

    #[test]
    fn mbr_refusals() {
        let st = stages(150_000);
        // Gap too small: first partition at LBA 63 (old DOS layout).
        let mut d = mbr_disk(&[(0x0c, 63, 10000)]);
        assert!(
            plan(&mut d, &st, 1, 512)
                .unwrap_err()
                .contains("gap after the MBR has 31 KiB")
        );
        let mut d = mbr_disk(&[(0x0c, 2048, 10000), (0x05, 12048, 100)]);
        assert!(plan(&mut d, &st, 2, 512).unwrap_err().contains("extended"));
        assert!(plan(&mut d, &st, 3, 512).unwrap_err().contains("empty"));
        assert!(plan(&mut d, &st, 5, 512).unwrap_err().contains("1 to 4"));
        // No MBR at all.
        let mut d = Mem(vec![0u8; 1 << 20]);
        assert!(
            plan(&mut d, &st, 1, 512)
                .unwrap_err()
                .contains("MBR signature")
        );
        // 4 KiB sectors: refused, not half-installed.
        let mut d = mbr_disk(&[(0x0c, 256, 1000)]);
        assert!(
            plan(&mut d, &st, 1, 4096)
                .unwrap_err()
                .contains("4 KiB-sector")
        );
    }

    #[test]
    fn elf_extraction_refusals() {
        assert!(stages_from_elf(b"not an elf").is_err());
        let mut e = vec![0u8; 64];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[4] = 2;
        e[5] = 1;
        e[18] = 62;
        assert!(
            stages_from_elf(&e)
                .unwrap_err()
                .contains("bad program headers")
        );
        e[54] = 56; // no program headers at all
        assert!(stages_from_elf(&e).unwrap_err().contains("no stage 1"));
        e[56] = 1; // one, past the end of the file
        e[32] = 64;
        assert!(
            stages_from_elf(&e)
                .unwrap_err()
                .contains("bad program headers")
        );
        e[18] = 3;
        assert!(stages_from_elf(&e).unwrap_err().contains("x86-64"));
    }
}
