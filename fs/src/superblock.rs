//! The superblock: the root of trust for a commit.
//!
//! There are four slots: blocks 0 and 1 at the front of the device and the
//! last two blocks at the back. Commit `g` writes slots `g % 2` and
//! `2 + g % 2`, so the two newest commits are always each stored twice, at
//! opposite ends of the device. Mount picks the newest copy that verifies.
//!
//! Layout (4096 bytes):
//! - `0..512` header, in the clear: identity, geometry, cipher suite,
//!   feature bits, KDF parameters and key slots (needed before any key is
//!   known). Identical in every slot and every commit; it changes only when
//!   a key slot does. Fields end at 424; 424..512 is reserved, zero.
//! - `512..528` trailer, in the clear: slot number (u32), zero (u32) and
//!   generation (u64). Authenticated as the body's associated data.
//! - `528..552` body nonce, `552..568` body MAC.
//! - `568..4064` body: the commit's root pointers, counters and history
//!   ring. Sealed as a block of kind `Superblock` (encrypted on encrypted
//!   volumes), with the header's BLAKE3 hash inside it, so the header is
//!   authenticated too.
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
use alloc::vec;
use alloc::vec::Vec;

pub const MAGIC: [u8; 8] = *b"OPTMLFS\x01";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 512;
/// Per-slot cleartext trailer: slot number and generation (authenticated
/// as the body's associated data). Kept out of the header so the header
/// bytes are identical in every slot and every commit.
const TRAILER_OFF: usize = HEADER_LEN;
const NONCE_OFF: usize = TRAILER_OFF + 16;
const MAC_OFF: usize = NONCE_OFF + NONCE_LEN;
const BODY_OFF: usize = MAC_OFF + MAC_LEN;
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
    /// Feature bits: unknown compatible ones are ignored, unknown
    /// read-only-compatible ones allow only read-only mounts, and unknown
    /// incompatible ones refuse the mount. Version 1 defines none.
    pub compat: u32,
    pub ro_compat: u32,
    pub incompat: u32,
    /// log2 of the metadata chunk size in blocks (0..=6), fixed at format.
    pub chunk_shift: u32,
    /// Not part of the header bytes: from the slot's trailer.
    pub generation: u64,
    pub slot: u32,
    pub kdf: KdfParams,
    pub salt: [u8; 32],
    pub keyslots: [KeySlot; KEYSLOTS],
}

/// The commit state a superblock records.
///
/// The superblock itself stores each commit's *core*: generation, time,
/// replay value, flags, window, the chunk table root and depth, and a
/// pointer to the commit's root block. The root block (a duplicated
/// metadata block, read through that commit's chunk table) holds the tree
/// and space-map roots and the counters, which fill the remaining fields
/// once it is read ([`Body::apply_root`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    pub generation: u64,
    /// Chunk table root: copy 0 at `addr`, copy 1 at `aux`.
    pub chunk_root: BlockPtr,
    pub chunk_depth: u32,
    /// The root block (logical address).
    pub root_block: BlockPtr,
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
    /// Newest commit that moved a metadata run holding live blocks (a
    /// chunk relocation or a copy repair), 0 if none. Moved blocks keep
    /// their old birth, so a recovery check whose window reaches this
    /// commit must verify all metadata, not only recent births.
    pub last_move_gen: u64,
    pub commit_time_ns: i64,
    /// Anti-rollback counter, mirrored into a TPM monotonic counter once
    /// the kernel has one (stage 2). Carried forward unchanged by commits.
    pub replay_value: u64,
    /// [`FLAG_CLEAN`] and future flags; unknown bits are rejected.
    pub flags: u64,
    /// Oldest generation inside the quarantine window when this commit was
    /// made: every block born at or after it may be one a lying drive lost,
    /// and every block older is protected. Recovery verifies from here.
    pub window_gen: u64,
    /// Reserved for the per-file fsync intent log. Always null in v1.
    pub log_root: BlockPtr,
    /// The previous [`HISTORY`] commits, newest first (fewer on a young
    /// volume). Their entries have no history of their own. Mount falls
    /// back through these when the newest commit's blocks did not all
    /// reach the media, which quarantine keeps possible for K commits.
    pub history: Vec<Body>,
}

/// The volume was unmounted cleanly at this commit.
pub const FLAG_CLEAN: u64 = 1;
/// Commits recorded in each superblock's history ring: K, so the newest
/// commit that landed always lists the last commit before a lying drive's
/// drop point, even if every later superblock write landed and nothing
/// else did. Each entry ends up in up to K bodies, so in all four slots.
pub const HISTORY: usize = 8;

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
        w.u32(self.incompat);
        w.bytes(&self.uuid);
        w.u64(self.total_blocks);
        w.u32(BLOCK_SIZE as u32);
        w.u8(self.suite);
        w.zeros(3);
        w.u32(self.compat);
        w.u32(self.ro_compat);
        w.u32(1 << self.chunk_shift);
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
        let incompat = r.u32()?;
        if incompat != 0 {
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
        let compat = r.u32()?;
        let ro_compat = r.u32()?;
        let chunk = r.u32()?;
        if !chunk.is_power_of_two() || chunk > 1 << crate::chunk::MAX_CHUNK_SHIFT {
            return Err(r.err());
        }
        let chunk_shift = chunk.trailing_zeros();
        let kdf = KdfParams {
            mem_kib: r.u32()?,
            iterations: r.u32()?,
            lanes: r.u32()?,
        };
        let salt = r.array()?;
        let mut keyslots = [KeySlot::default(); KEYSLOTS];
        for ks in &mut keyslots {
            // Slot kind: 0 empty, 1 passphrase (Argon2id). 2 is reserved for
            // TPM-sealed slots (stage 2); other values are unknown.
            let used = r.u8()?;
            if used > 1 {
                return Err(Error::Unsupported("key slot kind"));
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
            compat,
            ro_compat,
            incompat,
            chunk_shift,
            generation: 0,
            slot: 0,
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

/// Magic at the start of a root block: "ROOT".
const ROOT_MAGIC: u32 = 0x544f_4f52;

impl Body {
    /// A body with only the core fields known (the rest from the root block).
    #[allow(clippy::too_many_arguments)]
    fn from_core(
        generation: u64,
        commit_time_ns: i64,
        replay_value: u64,
        flags: u64,
        window_gen: u64,
        chunk_root: BlockPtr,
        chunk_depth: u32,
        root_block: BlockPtr,
    ) -> Body {
        Body {
            generation,
            chunk_root,
            chunk_depth,
            root_block,
            fs_root: BlockPtr::NULL,
            meta_root: BlockPtr::NULL,
            space_root: BlockPtr::NULL,
            free_log: BlockPtr::NULL,
            next_ino: 0,
            next_snap_id: 0,
            live_deadlist: 0,
            next_deadlist: 0,
            latest_snap_gen: 0,
            last_move_gen: 0,
            commit_time_ns,
            replay_value,
            flags,
            window_gen,
            log_root: BlockPtr::NULL,
            history: Vec::new(),
        }
    }

    /// The commit-state fields shared by a body and its history entries.
    fn encode_core(&self, w: &mut Writer) {
        w.u64(self.generation);
        w.i64(self.commit_time_ns);
        w.u64(self.replay_value);
        w.u64(self.flags);
        w.u64(self.window_gen);
        self.chunk_root.encode(w);
        w.u32(self.chunk_depth);
        w.u32(0);
        self.root_block.encode(w);
    }

    fn decode_core(r: &mut Reader) -> Result<Body> {
        let generation = r.u64()?;
        let commit_time_ns = r.i64()?;
        let replay_value = r.u64()?;
        let flags = r.u64()?;
        let window_gen = r.u64()?;
        let chunk_root = BlockPtr::decode(r)?;
        let chunk_depth = r.u32()?;
        if r.u32()? != 0 {
            return Err(r.err());
        }
        let root_block = BlockPtr::decode(r)?;
        if flags & !FLAG_CLEAN != 0 {
            return Err(Error::Unsupported("superblock flags"));
        }
        if window_gen > generation
            || chunk_depth > crate::chunk::MAX_DEPTH
            || chunk_root.is_null()
            || root_block.is_null()
            || root_block.birth != generation
            || chunk_root.birth > generation
        {
            return Err(Error::Corrupt("superblock core", generation));
        }
        Ok(Body::from_core(
            generation,
            commit_time_ns,
            replay_value,
            flags,
            window_gen,
            chunk_root,
            chunk_depth,
            root_block,
        ))
    }

    /// The root block's contents for this commit.
    pub fn encode_root(&self) -> crate::io::Block {
        let mut b = zero_block();
        let mut w = Writer::new(&mut b[..]);
        w.u32(ROOT_MAGIC);
        w.u32(0);
        w.u64(self.generation);
        for p in [
            &self.fs_root,
            &self.meta_root,
            &self.space_root,
            &self.free_log,
        ] {
            p.encode(&mut w);
        }
        w.u64(self.next_ino);
        w.u64(self.next_snap_id);
        w.u64(self.live_deadlist);
        w.u64(self.next_deadlist);
        w.u64(self.latest_snap_gen);
        w.u64(self.last_move_gen);
        b
    }

    /// Fills the fields stored in the root block (already verified).
    pub fn apply_root(&mut self, b: &[u8]) -> Result<()> {
        let at = self.root_block.addr;
        let mut r = Reader::new(b, "root block", at);
        if r.u32()? != ROOT_MAGIC || r.u32()? != 0 || r.u64()? != self.generation {
            return Err(Error::Corrupt("root block header", at));
        }
        self.fs_root = BlockPtr::decode(&mut r)?;
        self.meta_root = BlockPtr::decode(&mut r)?;
        self.space_root = BlockPtr::decode(&mut r)?;
        self.free_log = BlockPtr::decode(&mut r)?;
        self.next_ino = r.u64()?;
        self.next_snap_id = r.u64()?;
        self.live_deadlist = r.u64()?;
        self.next_deadlist = r.u64()?;
        self.latest_snap_gen = r.u64()?;
        self.last_move_gen = r.u64()?;
        if self.last_move_gen > self.generation {
            return Err(Error::Corrupt("root block move generation", at));
        }
        if r.bytes(r.remaining())?.iter().any(|&x| x != 0) {
            return Err(Error::Corrupt("root block padding", at));
        }
        Ok(())
    }

    fn encode(&self, header_hash: &[u8; 32], w: &mut Writer) {
        w.bytes(header_hash);
        self.encode_core(w);
        self.log_root.encode(w);
        debug_assert!(self.history.len() <= HISTORY);
        w.u32(self.history.len() as u32);
        w.u32(0);
        for h in &self.history {
            h.encode_core(w);
        }
    }

    fn decode(b: &[u8], at: u64) -> Result<([u8; 32], Body)> {
        let mut r = Reader::new(b, "superblock body", at);
        let hh = r.array()?;
        let mut body = Body::decode_core(&mut r)?;
        body.log_root = BlockPtr::decode(&mut r)?;
        if !body.log_root.is_null() {
            return Err(Error::Unsupported("fsync log (newer format)"));
        }
        let n = r.u32()? as usize;
        if n > HISTORY || r.u32()? != 0 {
            return Err(Error::Corrupt("superblock history", at));
        }
        let mut prev = body.generation;
        for _ in 0..n {
            let h = Body::decode_core(&mut r)?;
            // Strictly older, newest first.
            if h.generation >= prev {
                return Err(Error::Corrupt("superblock history order", at));
            }
            prev = h.generation;
            body.history.push(h);
        }
        let rest = r.bytes(r.remaining())?;
        if rest.iter().any(|&x| x != 0) {
            return Err(Error::Corrupt("superblock body padding", at));
        }
        Ok((hh, body))
    }

    /// This commit's state without its history (a history entry).
    pub fn core(&self) -> Body {
        Body {
            history: Vec::new(),
            log_root: BlockPtr::NULL,
            ..self.clone()
        }
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
    {
        let mut w = Writer::new(&mut b[TRAILER_OFF..NONCE_OFF]);
        w.u32(slot);
        w.u32(0);
        w.u64(body.generation);
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
    b[NONCE_OFF..MAC_OFF].copy_from_slice(&nonce);
    b[MAC_OFF..BODY_OFF].copy_from_slice(&mac);
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
    let mut unsupported = None;
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
        let mut h = match Header::decode(&b[..HEADER_LEN], addr) {
            Ok(h) => h,
            Err(e @ Error::Unsupported(_)) => {
                unsupported = Some(e);
                continue;
            }
            Err(_) => continue,
        };
        let mut r = Reader::new(&b[TRAILER_OFF..NONCE_OFF], "superblock trailer", addr);
        let (Ok(s), Ok(0), Ok(g)) = (r.u32(), r.u32(), r.u64()) else {
            continue;
        };
        h.slot = s;
        h.generation = g;
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
    // An intact copy from a newer format version: say so, don't guess.
    if out.is_empty()
        && let Some(e) = unsupported
    {
        return Err(e);
    }
    out.sort_by_key(|c| core::cmp::Reverse(c.header.generation));
    Ok(out)
}

/// Verifies (and decrypts) a candidate's body with `sealer`.
pub fn open_body(c: &Candidate, sealer: &Sealer) -> Result<Body> {
    let mut b = c.raw.clone();
    let nonce: [u8; NONCE_LEN] = b[NONCE_OFF..MAC_OFF].try_into().unwrap();
    let mac: [u8; MAC_LEN] = b[MAC_OFF..BODY_OFF].try_into().unwrap();
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
///
/// With `all_slots`, the other pair is then overwritten with this commit
/// too (after the first pair is durable), so no older or stray copy is
/// left anywhere: used after mount fell back past newer copies, and for
/// replay-counter bumps.
pub fn commit<D: BlockDevice>(
    io: &mut Io<D>,
    header: &Header,
    body: &Body,
    all_slots: bool,
    dead: &mut u8,
) -> Result<()> {
    io.flush()?;
    let total = io.num_blocks();
    let waves = plan_waves(body.generation, all_slots, *dead)?;
    for wave in &waves {
        for &slot in wave {
            let b = encode(header, body, slot, &mut io.sealer);
            if let Err(e) = io.write_now(slot_addr(slot, total), &b[..]) {
                // A slot cannot move: it is not written again on this
                // mount, and the retry writes around it.
                *dead |= 1 << slot;
                return Err(e);
            }
        }
        io.flush()?;
    }
    Ok(())
}

/// The superblock slots a commit writes, in waves: each wave is written,
/// then flushed, before the next starts.
///
/// With every slot working: a normal commit writes its own pair in one
/// wave (the other pair keeps the previous commit); with `all_slots`, the
/// front slots (0, 1) then the back slots (2, 3), so a power cut tearing
/// every write in flight still leaves a whole wave intact.
///
/// With slots in `dead` (bit per slot; a write to it failed on this
/// mount), each wave is one slot, so at most one copy is ever in flight:
/// the commit's own live slots first, then live slots of the other pair
/// until two copies are written (all live slots with `all_slots`). Every
/// commit thus still lands on two slots, and a cut part way leaves either
/// the new commit or the previous one whole on some slot. Fewer than two
/// live slots: refused with [`Error::DeviceFailing`] before any write, as
/// one copy overwritten in place could be torn with nothing to fall back
/// to.
pub fn plan_waves(generation: u64, all_slots: bool, dead: u8) -> Result<Vec<Vec<u32>>> {
    let live = |s: &u32| dead & (1 << s) == 0;
    if dead & 0xf == 0 {
        return Ok(if all_slots {
            vec![vec![0, 1], vec![2, 3]]
        } else {
            vec![slots_for_gen(generation).to_vec()]
        });
    }
    let own = slots_for_gen(generation);
    let other = [own[0] ^ 1, own[1] ^ 1];
    let mut order: Vec<u32> = own
        .iter()
        .chain(other.iter())
        .copied()
        .filter(live)
        .collect();
    if order.len() < 2 {
        return Err(Error::DeviceFailing);
    }
    if !all_slots {
        order.truncate(2);
    }
    Ok(order.into_iter().map(|s| vec![s]).collect())
}

/// Bytes of one history entry / the core of a body.
const CORE_LEN: usize = 5 * 8 + 64 + 8 + 64;
const _: () = assert!(CORE_LEN == 176);
const _: () = assert!(BODY_LEN >= 32 + CORE_LEN + 64 + 8 + HISTORY * CORE_LEN);
