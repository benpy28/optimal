use proptest::prelude::*;

use crate::build_table;
use crate::checksum;
use crate::tables::*;
use crate::{Error, HEADER_LEN};

/// Physical memory made of separate readable regions. A read succeeds only
/// if it lies entirely inside one region.
#[derive(Default)]
struct Mem(Vec<(u64, Vec<u8>)>);

impl Mem {
    fn put(&mut self, at: u64, bytes: Vec<u8>) {
        self.0.push((at, bytes));
    }
}

impl PhysRead for Mem {
    fn read(&self, phys: u64, buf: &mut [u8]) -> bool {
        for (base, bytes) in &self.0 {
            if phys >= *base {
                let off = (phys - base) as usize;
                if let Some(src) = bytes.get(off..off + buf.len()) {
                    buf.copy_from_slice(src);
                    return true;
                }
            }
        }
        false
    }
}

fn rsdp_v1(rsdt: u32) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(b"RSD PTR ");
    r.push(0);
    r.extend_from_slice(b"OPTIML");
    r.push(0); // revision 0 = ACPI 1.0
    r.extend_from_slice(&rsdt.to_le_bytes());
    r[8] = 0u8.wrapping_sub(checksum(&r));
    r
}

fn rsdp_v2(rsdt: u32, xsdt: u64, len: u32) -> Vec<u8> {
    let mut r = rsdp_v1(rsdt);
    r[15] = 2;
    r[8] = 0;
    r[8] = 0u8.wrapping_sub(checksum(&r[..20]));
    r.extend_from_slice(&len.to_le_bytes());
    r.extend_from_slice(&xsdt.to_le_bytes());
    r.push(0); // extended checksum
    r.extend_from_slice(&[0, 0, 0]);
    r.resize(len.max(36) as usize, 0);
    r[32] = 0u8.wrapping_sub(checksum(&r));
    r
}

fn root(sig: &[u8; 4], entries: &[u64]) -> Vec<u8> {
    let mut body = Vec::new();
    for &e in entries {
        if sig == b"XSDT" {
            body.extend_from_slice(&e.to_le_bytes());
        } else {
            body.extend_from_slice(&(e as u32).to_le_bytes());
        }
    }
    build_table(sig, 1, &body)
}

#[test]
fn rsdp_v1_hand_worked() {
    let r = rsdp_v1(0x7fe1_0000);
    assert_eq!(r.len(), 20);
    assert_eq!(checksum(&r), 0);
    let p = Rsdp::parse(&r).unwrap();
    assert_eq!(p.revision, 0);
    assert_eq!(&p.oem_id, b"OPTIML");
    assert_eq!(p.rsdt, Some(0x7fe1_0000));
    assert_eq!(p.xsdt, None);
    assert!(!p.extended_broken);
}

#[test]
fn rsdp_errors() {
    let r = rsdp_v1(0x1000);
    assert_eq!(Rsdp::parse(&r[..19]), Err(Error::TooShort));
    let mut bad = r.clone();
    bad[0] = b'X';
    assert_eq!(Rsdp::parse(&bad), Err(Error::BadSignature));
    let mut bad = r.clone();
    bad[16] ^= 1;
    assert_eq!(Rsdp::parse(&bad), Err(Error::BadChecksum));
    // No root table at all.
    assert_eq!(Rsdp::parse(&rsdp_v1(0)), Err(Error::BadField));
}

#[test]
fn rsdp_v2_hand_worked() {
    let r = rsdp_v2(0x1000, 0x1_0000_2000, 36);
    let p = Rsdp::parse(&r).unwrap();
    assert_eq!(p.revision, 2);
    assert_eq!(p.rsdt, Some(0x1000));
    assert_eq!(p.xsdt, Some(0x1_0000_2000));
    assert!(!p.extended_broken);
    // Only an XSDT is fine.
    let p = Rsdp::parse(&rsdp_v2(0, 0x2000, 36)).unwrap();
    assert_eq!((p.rsdt, p.xsdt), (None, Some(0x2000)));
    // A zero XSDT address means none.
    let p = Rsdp::parse(&rsdp_v2(0x1000, 0, 36)).unwrap();
    assert_eq!((p.xsdt, p.extended_broken), (None, false));
    assert_eq!(Rsdp::parse(&rsdp_v2(0, 0, 36)), Err(Error::BadField));
}

#[test]
fn rsdp_v2_broken_extension_falls_back_to_rsdt() {
    // Bad extended checksum: the first 20 bytes are still right.
    let mut r = rsdp_v2(0x1000, 0x2000, 36);
    r[32] ^= 1;
    let p = Rsdp::parse(&r).unwrap();
    assert_eq!(
        (p.rsdt, p.xsdt, p.extended_broken),
        (Some(0x1000), None, true)
    );
    // Impossible extended lengths: below 36, above 4096, or past the bytes.
    for len in [0u32, 20, 35, 4097] {
        let mut r = rsdp_v2(0x1000, 0x2000, 36);
        r[20..24].copy_from_slice(&len.to_le_bytes());
        r[32] = 0;
        r[32] = 0u8.wrapping_sub(checksum(&r));
        assert!(Rsdp::parse(&r).unwrap().extended_broken, "len {len}");
    }
    let r = rsdp_v2(0x1000, 0x2000, 36);
    assert!(Rsdp::parse(&r[..35]).unwrap().extended_broken);
    // Bounds: exactly 36 and exactly 4096 are allowed.
    assert!(
        !Rsdp::parse(&rsdp_v2(0x1000, 0x2000, 4096))
            .unwrap()
            .extended_broken
    );
    // Broken extension and no RSDT: nothing usable.
    let mut r = rsdp_v2(0, 0x2000, 36);
    r[32] ^= 1;
    assert_eq!(Rsdp::parse(&r), Err(Error::BadField));
}

/// A QEMU-like layout: RSDP at 0xf5000, XSDT listing three tables.
fn machine() -> Mem {
    let mut m = Mem::default();
    m.put(0xf5000, rsdp_v2(0x7fe0_0000, 0x7fe0_1000, 36));
    m.put(0x7fe0_0000, root(b"RSDT", &[0x7fe0_2000]));
    m.put(
        0x7fe0_1000,
        root(b"XSDT", &[0x7fe0_2000, 0x7fe0_3000, 0x7fe0_4000]),
    );
    m.put(0x7fe0_2000, build_table(b"FACP", 6, &[0; 240]));
    m.put(0x7fe0_3000, build_table(b"APIC", 4, &[0; 8]));
    m.put(0x7fe0_4000, build_table(b"HPET", 1, &[0; 20]));
    m
}

#[test]
fn discover_uses_the_xsdt() {
    let m = machine();
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.root, Root::Xsdt);
    assert_eq!(t.root_phys, 0x7fe0_1000);
    assert!(t.root_checksum_ok);
    assert!(!t.xsdt_fell_back);
    assert_eq!((t.bad_entries, t.ignored_entries), (0, 0));
    assert_eq!(
        t.list,
        vec![
            TableRef {
                signature: *b"FACP",
                phys: 0x7fe0_2000,
                len: 276
            },
            TableRef {
                signature: *b"APIC",
                phys: 0x7fe0_3000,
                len: 44
            },
            TableRef {
                signature: *b"HPET",
                phys: 0x7fe0_4000,
                len: 56
            },
        ]
    );
    assert_eq!(t.find(b"APIC").unwrap().phys, 0x7fe0_3000);
    assert_eq!(t.find(b"MCFG"), None);
    assert_eq!(t.count(b"HPET"), 1);
    let apic = Tables::copy(&m, t.find(b"APIC").unwrap()).unwrap();
    assert_eq!(apic.len(), 44);
    assert_eq!(&apic[..4], b"APIC");
    // A table the copy can't read in full.
    let cut = TableRef {
        signature: *b"APIC",
        phys: 0x7fe0_3000,
        len: 45,
    };
    assert_eq!(Tables::copy(&m, cut), None);
}

#[test]
fn discover_falls_back_to_the_rsdt() {
    // XSDT has the wrong signature.
    let mut m = machine();
    m.0[2].1[0] = b'Y';
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.root, Root::Rsdt);
    assert_eq!(t.root_phys, 0x7fe0_0000);
    assert!(t.xsdt_fell_back);
    assert_eq!(t.list.len(), 1);
    assert_eq!(t.list[0].signature, *b"FACP");
    // XSDT unreadable.
    let mut m = machine();
    m.0.remove(2);
    assert_eq!(Tables::discover(&m, 0xf5000).unwrap().root, Root::Rsdt);
    // XSDT with an impossible length.
    let mut m = machine();
    m.0[2].1[4..8].copy_from_slice(&35u32.to_le_bytes());
    assert_eq!(Tables::discover(&m, 0xf5000).unwrap().root, Root::Rsdt);
    // XSDT length claims more than can be read.
    let mut m = machine();
    m.0[2].1[4..8].copy_from_slice(&4000u32.to_le_bytes());
    assert_eq!(Tables::discover(&m, 0xf5000).unwrap().root, Root::Rsdt);
    // Both broken: nothing to use.
    let mut m = machine();
    m.0[2].1[0] = b'Y';
    m.0[1].1[0] = b'Y';
    assert_eq!(Tables::discover(&m, 0xf5000), Err(Error::BadField));
}

#[test]
fn discover_with_acpi_1_rsdp_at_the_end_of_memory() {
    // Only the 20 bytes of a version 1 RSDP are readable.
    let mut m = Mem::default();
    m.put(0xf5000, rsdp_v1(0x7fe0_0000));
    m.put(0x7fe0_0000, root(b"RSDT", &[0x7fe0_2000]));
    m.put(0x7fe0_2000, build_table(b"FACP", 1, &[0; 80]));
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.root, Root::Rsdt);
    assert!(!t.xsdt_fell_back);
    assert!(!t.rsdp.extended_broken);
    assert_eq!(t.list[0].len, 116);
    // A version 2 RSDP cut at 20 bytes: only its RSDT can be used.
    let mut r = rsdp_v2(0x7fe0_0000, 0x7fe0_1000, 36);
    r.truncate(20);
    m.0[0].1 = r;
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(
        (t.root, t.rsdp.extended_broken, t.rsdp.xsdt),
        (Root::Rsdt, true, None)
    );
    // ... and with no RSDT either, nothing.
    let mut r = rsdp_v2(0, 0x7fe0_1000, 36);
    r.truncate(20);
    r[8] = 0;
    r[8] = 0u8.wrapping_sub(checksum(&r));
    m.0[0].1 = r;
    assert_eq!(Tables::discover(&m, 0xf5000), Err(Error::BadField));
    // Nothing readable at all.
    assert_eq!(Tables::discover(&m, 0x1234_5000), Err(Error::TooShort));
}

#[test]
fn discover_reads_a_longer_rsdp_in_full() {
    let mut m = machine();
    m.0[0].1 = rsdp_v2(0x7fe0_0000, 0x7fe0_1000, 40);
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.root, Root::Xsdt);
    // The longer RSDP can't be read in full: its extension is unusable.
    let mut m = machine();
    let mut r = rsdp_v2(0x7fe0_0000, 0x7fe0_1000, 40);
    r[20..24].copy_from_slice(&64u32.to_le_bytes());
    m.0[0].1 = r;
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!((t.root, t.rsdp.extended_broken), (Root::Rsdt, true));
}

#[test]
fn bad_and_ignored_root_entries() {
    let mut m = machine();
    // Entries: zero, unreadable, a header with length 35, then a good one.
    m.put(0x7fe0_5000, {
        let mut t = build_table(b"SSDT", 1, &[]);
        t[4..8].copy_from_slice(&35u32.to_le_bytes());
        t
    });
    m.0[2].1 = root(b"XSDT", &[0, 0xdead_0000, 0x7fe0_5000, 0x7fe0_3000]);
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.bad_entries, 3);
    assert_eq!(t.list.len(), 1);
    // A root length that isn't a whole number of entries: 4 extra bytes.
    let mut x = root(b"XSDT", &[0x7fe0_3000]);
    x.extend_from_slice(&[0; 4]);
    let n = x.len() as u32;
    x[4..8].copy_from_slice(&n.to_le_bytes());
    x[9] = 0;
    x[9] = 0u8.wrapping_sub(checksum(&x));
    m.0[2].1 = x;
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(
        (t.list.len(), t.ignored_entries, t.root_checksum_ok),
        (1, 1, true)
    );
    // A wrong root checksum is reported, the tables still used.
    m.0[2].1 = root(b"XSDT", &[0x7fe0_3000]);
    m.0[2].1[9] ^= 0x10;
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(
        (t.root, t.root_checksum_ok, t.list.len()),
        (Root::Xsdt, false, 1)
    );
}

#[test]
fn more_than_max_tables_are_ignored() {
    let mut m = machine();
    let entries = vec![0x7fe0_3000u64; MAX_TABLES + 3];
    m.0[2].1 = root(b"XSDT", &entries);
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!(t.list.len(), MAX_TABLES);
    assert_eq!(t.ignored_entries, 3);
    assert_eq!(t.count(b"APIC"), MAX_TABLES);
    // Exactly MAX_TABLES: none ignored.
    m.0[2].1 = root(b"XSDT", &entries[..MAX_TABLES]);
    assert_eq!(Tables::discover(&m, 0xf5000).unwrap().ignored_entries, 0);
}

#[test]
fn rsdt_entries_are_32_bit() {
    let mut m = Mem::default();
    m.put(0xf5000, rsdp_v1(0x7fe0_0000));
    // In an RSDT these are two 4-byte entries.
    m.put(0x7fe0_0000, root(b"RSDT", &[0x7fe0_2000, 0x7fe0_3000]));
    m.put(0x7fe0_2000, build_table(b"FACP", 1, &[0; 80]));
    m.put(0x7fe0_3000, build_table(b"APIC", 1, &[0; 8]));
    let t = Tables::discover(&m, 0xf5000).unwrap();
    let sigs: Vec<_> = t.list.iter().map(|t| t.signature).collect();
    assert_eq!(sigs, vec![*b"FACP", *b"APIC"]);
    assert_eq!(t.list[1].phys, 0x7fe0_3000);
    assert_eq!(HEADER_LEN + 8, t.list[1].len as usize);
}

proptest! {
    #![proptest_config(super::config(2000))]

    #[test]
    fn rsdp_parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
        let _ = Rsdp::parse(&bytes);
    }

    /// Any byte of the machine's firmware memory corrupted: discovery
    /// never panics, and anything it lists is readable.
    #[test]
    fn discover_survives_any_single_corruption(region in 0usize..6, at in 0usize..300, bit in 0u8..8) {
        let mut m = machine();
        let r = &mut m.0[region].1;
        let at = at % r.len();
        r[at] ^= 1 << bit;
        if let Ok(t) = Tables::discover(&m, 0xf5000) {
            for e in &t.list {
                let mut h = [0u8; HEADER_LEN];
                prop_assert!(m.read(e.phys, &mut h));
                prop_assert!(e.len as usize >= HEADER_LEN);
            }
        }
    }
}

#[test]
fn rsdp_extended_length_too_small_to_reread() {
    // A version 2 RSDP whose length field says 10: the extension is
    // unusable, but the 36 bytes already read still give the RSDT. Reading
    // just 10 bytes again would lose even that.
    let mut m = machine();
    let mut r = rsdp_v2(0x7fe0_0000, 0x7fe0_1000, 36);
    r[20..24].copy_from_slice(&10u32.to_le_bytes());
    m.0[0].1 = r;
    let t = Tables::discover(&m, 0xf5000).unwrap();
    assert_eq!((t.root, t.rsdp.extended_broken), (Root::Rsdt, true));
}
