//! The superblock: the root of trust for a commit.
//!
//! There are four slots: blocks 0 and 1 at the front of the device and the
//! last two blocks at the back. Commit `g` writes slots `g % 2` and
//! `2 + g % 2`, so the two newest commits are always each stored twice, at
//! opposite ends of the device. Mount picks the newest copy that verifies.
//!
//! Layout (4096 bytes):
//! - `0..512` header, in the clear: identity, geometry, cipher suite, KDF
//!   parameters and key slots (needed before any key is known).
//! - `512..536` body nonce, `536..552` body MAC.
//! - `552..4064` body: the commit's root pointers and counters. Sealed as a
//!   block of kind `Superblock` (encrypted on encrypted volumes), with the
//!   header's BLAKE3 hash inside it, so the header is authenticated too.
//! - `4064..4096` BLAKE3 of bytes `0..4064`, unkeyed: detects torn or
//!   garbage copies before any key is tried.

use crate::codec::{Reader, Writer};
use crate::crypto::{
    self, BlockKind, Entropy, KEY_LEN, KdfParams, MAC_LEN, NONCE_LEN, Sealer, SecretKey,
};
use crate::device::{BLOCK_SIZE, BlockDevice};
use crate::error::{Error, Result};
use crate::io::{Io, zero_block};
use crate::ptr::BlockPtr;
use alloc::vec::Vec;

pub const MAGIC: [u8; 8] = *b"OPTMLFS\x01";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 512;
const BODY_OFF: usize = 552;
const SUM_OFF: usize = BLOCK_SIZE - 32;
const BODY_LEN: usize = SUM_OFF - BODY_OFF;
pub const KEYSLOTS: usize = 4;

pub const SUITE_PLAIN: u8 = 0;
pub const SUITE_XCHACHA20POLY1305: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeySlot {
    pub used: bool,
    pub nonce: [u8; NONCE_LEN],
    pub wrapped: [u8; KEY_LEN],
    pub tag: [u8; MAC_LEN],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub uuid: [u8; 16],
    pub total_blocks: u64,
    pub suite: u8,
    pub generation: u64,
    pub slot: u32,
    pub kdf: KdfParams,
    pub salt: [u8; 32],
    pub keyslots: [KeySlot; KEYSLOTS],
}

/// The commit state a superblock records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    pub generation: u64,
    pub fs_root: BlockPtr,
    pub meta_root: BlockPtr,
    pub space_root: BlockPtr,
    pub free_log: BlockPtr,
    pub next_ino: u64,
    pub next_snap_id: u64,
    pub live_deadlist: u64,
    pub next_deadlist: u64,
    /// Generation of the newest snapshot (0 if none).
    pub latest_snap_gen: u64,
    pub commit_time_ns: i64,
}

pub fn slot_addr(slot: u32, total: u64) -> u64 {
    match slot {
        0 => 0,
        1 => 1,
        2 => total - 1,
        _ => total - 2,
    }
}

pub fn slots_for_gen(generation: u64) -> [u32; 2] {
    let p = (generation % 2) as u32;
    [p, 2 + p]
}

impl Header {
    fn encode(&self, w: &mut Writer) {
        w.bytes(&MAGIC);
        w.u32(VERSION);
        w.u32(0); // incompatible feature flags: none defined
        w.bytes(&self.uuid);
        w.u64(self.total_blocks);
        w.u32(BLOCK_SIZE as u32);
        w.u8(self.suite);
        w.zeros(3);
        w.u64(self.generation);
        w.u32(self.slot);
        w.u32(self.kdf.mem_kib);
        w.u32(self.kdf.iterations);
        w.u32(self.kdf.lanes);
        w.bytes(&self.salt);
        for ks in &self.keyslots {
            w.u8(ks.used as u8);
            w.zeros(7);
            w.bytes(&ks.nonce);
            w.bytes(&ks.wrapped);
            w.bytes(&ks.tag);
        }
    }

    fn decode(b: &[u8], at: u64) -> Result<Header> {
        let mut r = Reader::new(b, "superblock header", at);
        if r.array::<8>()? != MAGIC {
            return Err(Error::NoFilesystem);
        }
        if r.u32()? != VERSION {
            return Err(Error::Unsupported("superblock version"));
        }
        if r.u32()? != 0 {
            return Err(Error::Unsupported("incompatible feature flags"));
        }
        let uuid = r.array()?;
        let total_blocks = r.u64()?;
        if r.u32()? != BLOCK_SIZE as u32 {
            return Err(Error::Unsupported("block size"));
        }
        let suite = r.u8()?;
        if suite > SUITE_XCHACHA20POLY1305 {
            return Err(Error::Unsupported("cipher suite"));
        }
        r.skip(3)?;
        let generation = r.u64()?;
        let slot = r.u32()?;
        let kdf = KdfParams {
            mem_kib: r.u32()?,
            iterations: r.u32()?,
            lanes: r.u32()?,
        };
        let salt = r.array()?;
        let mut keyslots = [KeySlot::default(); KEYSLOTS];
        for ks in &mut keyslots {
            let used = r.u8()?;
            if used > 1 {
                return Err(r.err());
            }
            ks.used = used == 1;
            r.skip(7)?;
            ks.nonce = r.array()?;
            ks.wrapped = r.array()?;
            ks.tag = r.array()?;
        }
        Ok(Header {
            uuid,
            total_blocks,
            suite,
            generation,
            slot,
            kdf,
            salt,
            keyslots,
        })
    }

    fn keyslot_ad(&self, i: usize) -> [u8; 24] {
        let mut ad = [0u8; 24];
        ad[..4].copy_from_slice(b"OFSK");
        ad[4..20].copy_from_slice(&self.uuid);
        ad[20..24].copy_from_slice(&(i as u32).to_le_bytes());
        ad
    }

    /// Unlocks the master key with a passphrase.
    pub fn unlock(&self, pass: &[u8]) -> Result<SecretKey> {
        let kek = crypto::derive_kek(pass, &self.salt, self.kdf)?;
        for (i, ks) in self.keyslots.iter().enumerate() {
            if ks.used
                && let Some(m) =
                    crypto::unwrap_key(&kek, &ks.nonce, &self.keyslot_ad(i), &ks.wrapped, &ks.tag)
            {
                return Ok(m);
            }
        }
        Err(Error::BadKey)
    }

    /// Stores the master key in slot `i`, protected by `pass`.
    pub fn set_keyslot(
        &mut self,
        i: usize,
        pass: &[u8],
        master: &SecretKey,
        rng: &mut dyn Entropy,
    ) -> Result<()> {
        let kek = crypto::derive_kek(pass, &self.salt, self.kdf)?;
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill(&mut nonce);
        let (wrapped, tag) = crypto::wrap_key(&kek, &nonce, &self.keyslot_ad(i), master);
        self.keyslots[i] = KeySlot {
            used: true,
            nonce,
            wrapped,
            tag,
        };
        Ok(())
    }
}

impl Body {
    fn encode(&self, header_hash: &[u8; 32], w: &mut Writer) {
        w.bytes(header_hash);
        w.u64(self.generation);
        for p in [
            &self.fs_root,
            &self.meta_root,
            &self.space_root,
            &self.free_log,
        ] {
            p.encode(w);
        }
        w.u64(self.next_ino);
        w.u64(self.next_snap_id);
        w.u64(self.live_deadlist);
        w.u64(self.next_deadlist);
        w.u64(self.latest_snap_gen);
        w.i64(self.commit_time_ns);
    }

    fn decode(b: &[u8], at: u64) -> Result<([u8; 32], Body)> {
        let mut r = Reader::new(b, "superblock body", at);
        let hh = r.array()?;
        let generation = r.u64()?;
        let fs_root = BlockPtr::decode(&mut r)?;
        let meta_root = BlockPtr::decode(&mut r)?;
        let space_root = BlockPtr::decode(&mut r)?;
        let free_log = BlockPtr::decode(&mut r)?;
        let body = Body {
            generation,
            fs_root,
            meta_root,
            space_root,
            free_log,
            next_ino: r.u64()?,
            next_snap_id: r.u64()?,
            live_deadlist: r.u64()?,
            next_deadlist: r.u64()?,
            latest_snap_gen: r.u64()?,
            commit_time_ns: r.i64()?,
        };
        let rest = r.bytes(r.remaining())?;
        if rest.iter().any(|&x| x != 0) {
            return Err(Error::Corrupt("superblock body padding", at));
        }
        Ok((hh, body))
    }
}

/// Serializes and seals a superblock copy for `slot`.
pub fn encode(header: &Header, body: &Body, slot: u32, sealer: &mut Sealer) -> crate::io::Block {
    let mut h = header.clone();
    h.generation = body.generation;
    h.slot = slot;
    let mut b = zero_block();
    {
        let mut w = Writer::new(&mut b[..HEADER_LEN]);
        h.encode(&mut w);
    }
    let hh = *blake3::hash(&b[..HEADER_LEN]).as_bytes();
    {
        let mut w = Writer::new(&mut b[BODY_OFF..SUM_OFF]);
        body.encode(&hh, &mut w);
    }
    let (nonce, mac) = sealer.seal(
        BlockKind::Superblock,
        slot as u64,
        body.generation,
        &mut b[BODY_OFF..SUM_OFF],
    );
    b[HEADER_LEN..HEADER_LEN + NONCE_LEN].copy_from_slice(&nonce);
    b[HEADER_LEN + NONCE_LEN..BODY_OFF].copy_from_slice(&mac);
    let sum = *blake3::hash(&b[..SUM_OFF]).as_bytes();
    b[SUM_OFF..].copy_from_slice(&sum);
    b
}

/// A superblock copy whose outer checksum and header parsed.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub header: Header,
    pub raw: crate::io::Block,
    pub addr: u64,
}

/// Reads all four slots and returns the copies that pass the unkeyed checks,
/// newest generation first.
pub fn candidates<D: BlockDevice>(dev: &mut D) -> Result<Vec<Candidate>> {
    let total = dev.num_blocks();
    if total < 16 {
        return Err(Error::NoFilesystem);
    }
    let mut out: Vec<Candidate> = Vec::new();
    let mut any_io_ok = false;
    for slot in 0..4u32 {
        let addr = slot_addr(slot, total);
        let mut b = zero_block();
        if dev.read(addr, &mut b[..]).is_err() {
            continue;
        }
        any_io_ok = true;
        let sum = blake3::hash(&b[..SUM_OFF]);
        if !crypto::ct_eq(sum.as_bytes(), &b[SUM_OFF..]) {
            continue;
        }
        let Ok(h) = Header::decode(&b[..HEADER_LEN], addr) else {
            continue;
        };
        // A copy found in the wrong place, or for a different-sized device, is not trusted.
        if h.slot != slot || h.total_blocks != total {
            continue;
        }
        out.push(Candidate {
            header: h,
            raw: b,
            addr,
        });
    }
    if !any_io_ok {
        return Err(Error::Io(crate::device::IoError::Failed));
    }
    out.sort_by_key(|c| core::cmp::Reverse(c.header.generation));
    Ok(out)
}

/// Verifies (and decrypts) a candidate's body with `sealer`.
pub fn open_body(c: &Candidate, sealer: &Sealer) -> Result<Body> {
    let mut b = c.raw.clone();
    let nonce: [u8; NONCE_LEN] = b[HEADER_LEN..HEADER_LEN + NONCE_LEN].try_into().unwrap();
    let mac: [u8; MAC_LEN] = b[HEADER_LEN + NONCE_LEN..BODY_OFF].try_into().unwrap();
    sealer.open(
        BlockKind::Superblock,
        c.header.slot as u64,
        c.header.generation,
        &nonce,
        &mac,
        &mut b[BODY_OFF..SUM_OFF],
    )?;
    let (hh, body) = Body::decode(&b[BODY_OFF..SUM_OFF], c.addr)?;
    if !crypto::ct_eq(&hh, blake3::hash(&c.raw[..HEADER_LEN]).as_bytes())
        || body.generation != c.header.generation
    {
        return Err(Error::Corrupt("superblock header/body mismatch", c.addr));
    }
    Ok(body)
}

/// Writes the superblock copies for `body.generation`, with a flush before (so
/// everything it points to is durable first) and after (so the commit is
/// durable when this returns).
pub fn commit<D: BlockDevice>(io: &mut Io<D>, header: &Header, body: &Body) -> Result<()> {
    io.flush()?;
    let total = io.num_blocks();
    for slot in slots_for_gen(body.generation) {
        let b = encode(header, body, slot, &mut io.sealer);
        io.write_now(slot_addr(slot, total), &b[..])?;
    }
    io.flush()
}

const _: () = assert!(BODY_LEN >= 32 + 8 + 4 * 64 + 6 * 8);
