//! UEFI device paths (UEFI 2.10, chapter 10): measuring a firmware path,
//! reading the boot volume's partition and media nodes, and building the
//! path of a file next to us. Firmware hands out a bare pointer with no
//! length, so the loader first measures the path with `measure` (header
//! reads only, bounded by a cap), then passes the measured bytes here.
//!
//! Only the first instance is used: a path ends at its first End node,
//! whether End Entire (0x7f/0xff) or End Instance (0x7f/0x01), because every
//! path we touch (a volume's, a file's) names one device.

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DpError {
    /// A node header could not be read.
    Unreadable,
    /// A node shorter than its 4-byte header.
    ShortNode,
    /// An End node whose length is not 4.
    BadEnd,
    /// No End node within the cap.
    TooLong,
}

impl DpError {
    pub fn describe(&self) -> &'static str {
        match self {
            DpError::Unreadable => "device path cannot be read",
            DpError::ShortNode => "device path has a node shorter than its header",
            DpError::BadEnd => "device path's end node has the wrong length",
            DpError::TooLong => "device path has no end node within the size limit",
        }
    }
}

pub const END: u8 = 0x7f;
pub const END_ENTIRE: [u8; 4] = [0x7f, 0xff, 4, 0];
/// Largest path we accept, End node included.
pub const MAX_LEN: usize = 4096;

/// Length in bytes of the path's first instance, End node included.
/// `header(off)` returns the 4 header bytes of the node at `off`; it is only
/// called at node boundaries below `cap`, after the previous node was
/// checked, so a well-formed path is never read past its End node.
pub fn measure(
    mut header: impl FnMut(usize) -> Option<[u8; 4]>,
    cap: usize,
) -> Result<usize, DpError> {
    let mut off = 0usize;
    loop {
        if off + 4 > cap {
            return Err(DpError::TooLong);
        }
        let h = header(off).ok_or(DpError::Unreadable)?;
        let len = u16::from_le_bytes([h[2], h[3]]) as usize;
        if h[0] == END {
            if len != 4 {
                return Err(DpError::BadEnd);
            }
            return Ok(off + 4);
        }
        if len < 4 {
            return Err(DpError::ShortNode);
        }
        off += len;
    }
}

/// `measure` over a byte slice.
pub fn measure_slice(path: &[u8]) -> Result<usize, DpError> {
    measure(
        |o| path.get(o..o + 4).map(|b| b.try_into().unwrap()),
        path.len().min(MAX_LEN),
    )
}

/// A node: type, subtype and the whole node (header included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node<'a> {
    pub typ: u8,
    pub sub: u8,
    pub bytes: &'a [u8],
}

/// The nodes of a measured path's first instance, End node excluded.
/// Returns an error for anything `measure_slice` would refuse.
pub fn nodes(path: &[u8]) -> Result<Nodes<'_>, DpError> {
    let len = measure_slice(path)?;
    Ok(Nodes {
        rest: &path[..len - 4],
    })
}

pub struct Nodes<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Nodes<'a> {
    type Item = Node<'a>;
    fn next(&mut self) -> Option<Node<'a>> {
        // Lengths were checked by `nodes`: every node has len >= 4 and the
        // nodes tile `rest` exactly.
        let r = self.rest;
        if r.len() < 4 {
            return None;
        }
        let len = u16::from_le_bytes([r[2], r[3]]) as usize;
        let (node, rest) = r.split_at(len);
        self.rest = rest;
        Some(Node {
            typ: node[0],
            sub: node[1],
            bytes: node,
        })
    }
}

/// What the boot volume's device path says about it (for the Limine
/// executable-file request).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    pub optical: bool,
    pub partition_index: u32,
    pub mbr_disk_id: u32,
    /// Set for a GPT partition (Hard Drive node, signature type 2).
    pub gpt_part_uuid: Option<[u8; 16]>,
    /// Bytes of the path before the first Hard Drive or CD-ROM node: the
    /// path of the whole disk, used to find its GPT header.
    pub disk_prefix: usize,
}

/// Reads the first Hard Drive (4/1) or CD-ROM (4/2) media node. A Hard
/// Drive node shorter than its fixed 42 bytes is ignored, not guessed at:
/// the volume is then reported without partition details.
pub fn volume(path: &[u8]) -> Result<Volume, DpError> {
    let mut v = Volume::default();
    let mut off = 0;
    for n in nodes(path)? {
        if n.typ == 4 && (n.sub == 1 || n.sub == 2) {
            v.disk_prefix = off;
            if n.sub == 2 {
                v.optical = true;
            } else if n.bytes.len() >= 42 {
                let b = n.bytes;
                v.partition_index = u32::from_le_bytes(b[4..8].try_into().unwrap());
                let sig = &b[24..40];
                match b[41] {
                    1 => v.mbr_disk_id = u32::from_le_bytes(sig[0..4].try_into().unwrap()),
                    2 => v.gpt_part_uuid = Some(sig.try_into().unwrap()),
                    _ => {}
                }
            }
            return Ok(v);
        }
        off += n.bytes.len();
    }
    v.disk_prefix = off;
    Ok(v)
}

/// Writes `path`'s first instance, then a Media File Path node (4/4) for
/// `file` (UCS-2, NUL included), then End Entire into `out`. Returns the
/// length written, or None if it does not fit.
pub fn with_file(path: &[u8], file: &[u16], out: &mut [u8]) -> Result<Option<usize>, DpError> {
    let inst = measure_slice(path)? - 4;
    let node = 4 + 2 * file.len();
    let total = inst + node + 4;
    if node > u16::MAX as usize || total > out.len() {
        return Ok(None);
    }
    out[..inst].copy_from_slice(&path[..inst]);
    out[inst..inst + 4].copy_from_slice(&[4, 4, node as u8, (node >> 8) as u8]);
    for (i, c) in file.iter().enumerate() {
        out[inst + 4 + 2 * i..inst + 6 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    out[inst + node..total].copy_from_slice(&END_ENTIRE);
    Ok(Some(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn node(typ: u8, sub: u8, data: &[u8]) -> Vec<u8> {
        let len = 4 + data.len();
        let mut n = vec![typ, sub, len as u8, (len >> 8) as u8];
        n.extend_from_slice(data);
        n
    }

    fn hard_drive(index: u32, sig_type: u8, sig: [u8; 16]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&index.to_le_bytes());
        d.extend_from_slice(&2048u64.to_le_bytes()); // start LBA
        d.extend_from_slice(&4096u64.to_le_bytes()); // size
        d.extend_from_slice(&sig);
        d.push(if sig_type == 2 { 2 } else { 1 }); // MBR type
        d.push(sig_type);
        node(4, 1, &d)
    }

    /// PciRoot(0)/Pci(1f,2)/Sata(0,ffff,0)/HD(3,GPT,uuid,...): the shape OVMF
    /// gives a SATA disk. Worked by hand: ACPI node 12 bytes, PCI 6, SATA
    /// 10, HD 42, End 4; disk prefix = 12 + 6 + 10 = 28; total 74.
    fn sata_gpt() -> Vec<u8> {
        let mut p = node(2, 1, &[0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0]);
        p.extend(node(1, 1, &[2, 0x1f]));
        p.extend(node(3, 0x12, &[0, 0, 0xff, 0xff, 0, 0]));
        p.extend(hard_drive(3, 2, [0xab; 16]));
        p.extend_from_slice(&END_ENTIRE);
        p
    }

    #[test]
    fn hand_worked_paths() {
        let p = sata_gpt();
        assert_eq!(measure_slice(&p), Ok(74));
        let v = volume(&p).unwrap();
        assert_eq!(
            v,
            Volume {
                optical: false,
                partition_index: 3,
                mbr_disk_id: 0,
                gpt_part_uuid: Some([0xab; 16]),
                disk_prefix: 28,
            }
        );
        // MBR partition: the disk id is the first 4 signature bytes.
        let mut m = node(2, 1, &[0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0]);
        m.extend(hard_drive(
            1,
            1,
            [0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ));
        m.extend_from_slice(&END_ENTIRE);
        let v = volume(&m).unwrap();
        assert_eq!(
            (v.partition_index, v.mbr_disk_id, v.gpt_part_uuid),
            (1, 0x1234_5678, None)
        );
        // CD-ROM: optical, prefix is everything before it.
        let mut c = node(2, 1, &[0; 8]);
        c.extend(node(4, 2, &[0; 20]));
        c.extend_from_slice(&END_ENTIRE);
        let v = volume(&c).unwrap();
        assert!(v.optical);
        assert_eq!(v.disk_prefix, 12);
        // A truncated Hard Drive node (41 bytes) is ignored, not read.
        let mut t = node(2, 1, &[0; 8]);
        let hd = hard_drive(3, 2, [1; 16]);
        t.extend(node(4, 1, &hd[4..hd.len() - 1]));
        t.extend_from_slice(&END_ENTIRE);
        let v = volume(&t).unwrap();
        assert_eq!(
            (v.partition_index, v.gpt_part_uuid, v.disk_prefix),
            (0, None, 12)
        );
    }

    #[test]
    fn only_the_first_instance_counts() {
        // Instance 1 / End Instance / instance 2 (a CD-ROM) / End Entire.
        let mut p = node(2, 1, &[0; 8]);
        p.extend_from_slice(&[0x7f, 0x01, 4, 0]);
        p.extend(node(4, 2, &[0; 20]));
        p.extend_from_slice(&END_ENTIRE);
        assert_eq!(measure_slice(&p), Ok(16));
        assert!(!volume(&p).unwrap().optical);
        // The file path is appended to instance 1 only.
        let mut out = [0u8; 64];
        let n = with_file(&p, &[b'a' as u16, 0], &mut out).unwrap().unwrap();
        assert_eq!(
            &out[..n],
            &[&p[..12], &[4, 4, 8, 0, b'a', 0, 0, 0], &END_ENTIRE[..]].concat()[..]
        );
    }

    #[test]
    fn refusals() {
        assert_eq!(measure_slice(&[]), Err(DpError::TooLong));
        assert_eq!(
            measure_slice(&[1, 1, 3, 0, 0x7f, 0xff, 4, 0]),
            Err(DpError::ShortNode)
        );
        assert_eq!(measure_slice(&[1, 1, 0, 0]), Err(DpError::ShortNode));
        assert_eq!(measure_slice(&[0x7f, 0xff, 5, 0, 0]), Err(DpError::BadEnd));
        // A node whose length runs past the buffer: the next header is not
        // there.
        assert_eq!(
            measure_slice(&[1, 1, 200, 0, 0x7f, 0xff, 4, 0]),
            Err(DpError::TooLong)
        );
        // No End node at all.
        assert_eq!(measure_slice(&node(1, 1, &[0; 8])), Err(DpError::TooLong));
        // Over the cap: a chain of nodes that never ends.
        let long: Vec<u8> = (0..MAX_LEN / 8 + 1)
            .flat_map(|_| node(1, 1, &[0; 4]))
            .collect();
        let mut l = long.clone();
        l.extend_from_slice(&END_ENTIRE);
        assert_eq!(measure_slice(&l), Err(DpError::TooLong));
        // The header reader is never asked beyond the End node.
        let p = sata_gpt();
        let mut asked = Vec::new();
        let r = measure(
            |o| {
                asked.push(o);
                p.get(o..o + 4).map(|b| b.try_into().unwrap())
            },
            MAX_LEN,
        );
        assert_eq!(r, Ok(74));
        assert_eq!(asked, [0, 12, 18, 28, 70]);
        // with_file refuses a result that does not fit.
        let mut small = [0u8; 80];
        assert_eq!(with_file(&p, &[0x41, 0], &mut small), Ok(None));
    }

    /// A random valid path and what `volume` must report for it.
    fn random_path(rng: &mut crate::testrng::Rng) -> (Vec<u8>, Volume) {
        let mut p = Vec::new();
        let mut want = Volume::default();
        let mut media = false;
        for _ in 0..rng.below(12) {
            let pick = rng.below(8);
            if pick < 2 && !media {
                media = true;
                want.disk_prefix = p.len();
                if pick == 0 {
                    let mut sig = [0u8; 16];
                    sig.iter_mut().for_each(|b| *b = rng.next() as u8);
                    let st = 1 + rng.below(2) as u8;
                    let idx = rng.next() as u32;
                    p.extend(hard_drive(idx, st, sig));
                    want.partition_index = idx;
                    if st == 2 {
                        want.gpt_part_uuid = Some(sig);
                    } else {
                        want.mbr_disk_id = u32::from_le_bytes(sig[0..4].try_into().unwrap());
                    }
                } else {
                    want.optical = true;
                    p.extend(node(4, 2, &[0; 20]));
                }
                continue;
            }
            let typ = [1u8, 2, 3, 4, 5][rng.below(5) as usize];
            let sub = 3 + rng.below(30) as u8; // never 4/1 or 4/2
            let data: Vec<u8> = (0..rng.below(40)).map(|_| rng.next() as u8).collect();
            p.extend(node(typ, sub, &data));
        }
        if !media {
            want.disk_prefix = p.len();
        }
        if rng.below(4) == 0 {
            p.extend_from_slice(&[0x7f, 0x01, 4, 0]);
            p.extend(node(4, 2, &[0; 20]));
        }
        p.extend_from_slice(&END_ENTIRE);
        (p, want)
    }

    #[test]
    fn fuzz_random_paths_read_exactly() {
        crate::fuzzkit::run("devpath-valid", 6, |rng| {
            let (p, want) = random_path(rng);
            assert_eq!(volume(&p), Ok(want));
            let inst = measure_slice(&p).unwrap();
            let total: usize = nodes(&p).unwrap().map(|n| n.bytes.len()).sum();
            assert_eq!(total + 4, inst);
            let file: Vec<u16> = (0..rng.below(40)).map(|_| rng.next() as u16).collect();
            let mut out = vec![0u8; inst + 4 + 2 * file.len()];
            let n = with_file(&p, &file, &mut out).unwrap().unwrap();
            assert_eq!(n, out.len());
            // The result is itself a valid single-instance path.
            assert_eq!(measure_slice(&out), Ok(n));
            // With no media node, the "disk" prefix is the whole instance,
            // which now includes the file node.
            let mut w2 = want;
            if want.disk_prefix == inst - 4 {
                w2.disk_prefix = n - 4;
            }
            assert_eq!(volume(&out).unwrap(), w2);
        });
    }

    #[test]
    fn fuzz_mutated_paths_never_overrun() {
        crate::fuzzkit::run("devpath-mutated", 7, |rng| {
            let (mut p, _) = random_path(rng);
            crate::fuzzkit::mutate(rng, &mut p);
            let mut max_asked = 0;
            let m = measure(
                |o| {
                    max_asked = max_asked.max(o);
                    p.get(o..o + 4).map(|b| b.try_into().unwrap())
                },
                MAX_LEN,
            );
            assert!(max_asked < MAX_LEN);
            match m {
                Ok(len) => {
                    assert!(len <= p.len() && len >= 4);
                    let v = volume(&p).unwrap();
                    assert!(v.disk_prefix + 4 <= len);
                    let total: usize = nodes(&p).unwrap().map(|n| n.bytes.len()).sum();
                    assert_eq!(total + 4, len);
                    let mut out = [0u8; 256];
                    if let Some(n) = with_file(&p, &[0x41, 0], &mut out).unwrap() {
                        assert_eq!(measure_slice(&out[..n]), Ok(n));
                    }
                }
                Err(_) => {
                    assert!(volume(&p).is_err() && nodes(&p).is_err());
                }
            }
        });
    }
}
