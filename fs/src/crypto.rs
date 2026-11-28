//! Block sealing: checksums (plain volumes) or authenticated encryption
//! (encrypted volumes), plus key derivation and key slots.
//!
//! Every block the filesystem writes is sealed with *associated data* that
//! binds it to its kind, its address, the generation it was written in and
//! the volume UUID. The 16-byte result (checksum or AEAD tag) is stored in the
//! *parent's* pointer, never next to the data, so the whole volume forms a
//! hash tree rooted in the superblock: a block that is corrupted, misplaced,
//! stale (an old version of the right block) or from another volume fails
//! verification.
//!
//! Plain volumes use BLAKE3 (keyed with a per-volume key derived from the
//! UUID, which is domain separation, not secrecy). Encrypted volumes use
//! XChaCha20-Poly1305 with a 192-bit nonce made of a 128-bit random
//! per-mount prefix and a 64-bit counter, so nonces never repeat even if the
//! machine crashes and replays the same generation numbers.

use crate::error::{Error, Result};
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

pub const MAC_LEN: usize = 16;
pub const NONCE_LEN: usize = 24;
pub const KEY_LEN: usize = 32;

/// Source of randomness (keys, salts, nonce prefixes). The kernel backs this
/// with RDSEED/RDRAND mixed into its CSPRNG; host tools use the OS.
pub trait Entropy {
    fn fill(&mut self, buf: &mut [u8]);
}

#[cfg(feature = "std")]
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEntropy;

#[cfg(feature = "std")]
impl Entropy for OsEntropy {
    fn fill(&mut self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS random number generator failed");
    }
}

/// What a sealed block is, mixed into its associated data so a block of one
/// kind can never be accepted as another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockKind {
    Node = 1,
    Data = 2,
    Bitmap = 3,
    SpaceTable = 4,
    FreeLog = 5,
    Superblock = 6,
    /// The commit's root block (tree and space-map roots, counters).
    Root = 7,
    /// Chunk-table leaf (logical metadata chunk -> two physical copies).
    ChunkLeaf = 8,
    /// Chunk-table interior table.
    ChunkTable = 9,
    /// Intent-log ring block (sealed on its own: nonce and MAC inside it).
    Log = 10,
}

/// Argon2id parameters for turning a passphrase into a key-encryption key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub mem_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl KdfParams {
    /// Default for real volumes on machines with at least 4 GiB of RAM:
    /// 1 GiB, 4 passes (about 2 s on a mid-range CPU). Use
    /// [`KdfParams::for_ram`] to size it for the machine.
    pub const DEFAULT: KdfParams = KdfParams {
        mem_kib: 1024 * 1024,
        iterations: 4,
        lanes: 1,
    };
    /// The storage design's floor: at least 64 MiB and 4 passes.
    pub const FLOOR_MEM_KIB: u32 = 64 * 1024;
    pub const FLOOR_ITERATIONS: u32 = 4;

    /// Parameters for a machine with `ram_bytes` of memory: memory
    /// min(1 GiB, RAM / 4) but at least 64 MiB, and passes raised so the
    /// memory-time product (and so the time, about 2 s) stays that of
    /// 1 GiB x 4 passes, capped at [`KdfParams::MAX_ITERATIONS`].
    pub fn for_ram(ram_bytes: u64) -> KdfParams {
        let mem_kib = (ram_bytes / 4 / 1024).clamp(Self::FLOOR_MEM_KIB as u64, 1024 * 1024) as u32;
        let work = 1024 * 1024 * 4u64;
        let iterations = work
            .div_ceil(mem_kib as u64)
            .clamp(Self::FLOOR_ITERATIONS as u64, Self::MAX_ITERATIONS as u64)
            as u32;
        KdfParams {
            mem_kib,
            iterations,
            lanes: 1,
        }
    }

    /// Whether these meet the floor ([`KdfParams::INSECURE_FAST_FOR_TESTS`]
    /// is the only weaker setting format accepts).
    pub fn meets_floor(&self) -> bool {
        self.mem_kib >= Self::FLOOR_MEM_KIB && self.iterations >= Self::FLOOR_ITERATIONS
    }

    /// Cheap parameters for tests only. Never use for real data.
    pub const INSECURE_FAST_FOR_TESTS: KdfParams = KdfParams {
        mem_kib: 64,
        iterations: 1,
        lanes: 1,
    };
    /// Upper bounds accepted from disk, so a malicious image can't make
    /// mount allocate unbounded memory or spin forever.
    pub const MAX_MEM_KIB: u32 = 4 * 1024 * 1024;
    pub const MAX_ITERATIONS: u32 = 64;
}

#[derive(Clone)]
pub struct SecretKey(pub [u8; KEY_LEN]);

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl core::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

pub fn derive_kek(pass: &[u8], salt: &[u8; 32], p: KdfParams) -> Result<SecretKey> {
    if p.mem_kib > KdfParams::MAX_MEM_KIB
        || p.iterations > KdfParams::MAX_ITERATIONS
        || p.iterations == 0
        || p.lanes == 0
        || p.lanes > 16
    {
        return Err(Error::Unsupported("key derivation parameters out of range"));
    }
    let params = argon2::Params::new(p.mem_kib, p.iterations, p.lanes, Some(KEY_LEN))
        .map_err(|_| Error::Unsupported("key derivation parameters"))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = SecretKey([0; KEY_LEN]);
    a.hash_password_into(pass, salt, &mut out.0)
        .map_err(|_| Error::Unsupported("key derivation"))?;
    Ok(out)
}

fn xnonce(n: &[u8; NONCE_LEN]) -> XNonce {
    XNonce::from(*n)
}

/// Wraps (encrypts) the master key under a key-encryption key.
pub fn wrap_key(
    kek: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    ad: &[u8],
    master: &SecretKey,
) -> ([u8; KEY_LEN], [u8; MAC_LEN]) {
    let c = XChaCha20Poly1305::new(&kek.0.into());
    let mut buf = master.0;
    let tag = c
        .encrypt_inout_detached(&xnonce(nonce), ad, (&mut buf[..]).into())
        .expect("wrap");
    (buf, tag.into())
}

pub fn unwrap_key(
    kek: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    ad: &[u8],
    wrapped: &[u8; KEY_LEN],
    tag: &[u8; MAC_LEN],
) -> Option<SecretKey> {
    let c = XChaCha20Poly1305::new(&kek.0.into());
    let mut out = SecretKey(*wrapped);
    c.decrypt_inout_detached(&xnonce(nonce), ad, (&mut out.0[..]).into(), &(*tag).into())
        .ok()?;
    Some(out)
}

/// The per-volume sealing engine.
///
/// Metadata is stored twice (two physical copies of each logical block).
/// The copies must not be byte-identical, or a drive that deduplicates or
/// compresses writes could keep only one. Copy 0 is the normal sealed
/// block. Copy 1 is copy 0 XOR a *copy mask*: on encrypted volumes the
/// XOR of the XChaCha20 keystreams under the block key and under a
/// separate copy-1 subkey with the same nonce (so copy 1 is exactly the
/// encryption of the plaintext under the subkey), on plain volumes a
/// BLAKE3 XOF of the block's associated data under a copy-1 key. A reader
/// turns copy 1 back into copy 0 with the same XOR and checks the MAC in
/// the parent pointer, so both copies are bound by one Merkle MAC. No
/// (key, nonce) pair ever repeats: copy 0 uses the block key, copy 1 the
/// subkey, and the re-derived copy 0 is only compared, never written.
pub struct Sealer {
    aead: Option<XChaCha20Poly1305>,
    /// Copy-1 subkey cipher (encrypted volumes).
    aead1: Option<XChaCha20Poly1305>,
    /// Copy-1 mask key (plain volumes).
    copy1_key: [u8; 32],
    checksum_key: [u8; 32],
    /// Key for hashing directory entry names (keeps the on-disk name hash
    /// order from leaking anything and stops hash-flooding).
    pub(crate) name_key: [u8; 32],
    uuid: [u8; 16],
    nonce_prefix: [u8; 16],
    counter: u64,
}

impl core::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Sealer(encrypted: {})", self.aead.is_some())
    }
}

impl Drop for Sealer {
    fn drop(&mut self) {
        self.checksum_key.zeroize();
        self.copy1_key.zeroize();
        self.name_key.zeroize();
    }
}

impl Sealer {
    /// `master` is `None` for plain volumes.
    pub fn new(uuid: [u8; 16], master: Option<&SecretKey>, rng: &mut dyn Entropy) -> Self {
        let mut nonce_prefix = [0u8; 16];
        rng.fill(&mut nonce_prefix);
        match master {
            None => {
                let mut ctx = [0u8; 48];
                ctx[..32].copy_from_slice(b"OptimalFS v1 plain volume key ..");
                ctx[32..].copy_from_slice(&uuid);
                let base = blake3::derive_key("OptimalFS v1 plain checksum key", &ctx);
                Sealer {
                    aead: None,
                    aead1: None,
                    copy1_key: blake3::derive_key("OptimalFS v1 plain copy 1 mask key", &base),
                    checksum_key: base,
                    name_key: blake3::derive_key("OptimalFS v1 plain name hash key", &base),
                    uuid,
                    nonce_prefix,
                    counter: 0,
                }
            }
            Some(m) => {
                let mut bk = blake3::derive_key("OptimalFS v1 block encryption key", &m.0);
                let mut k1 = blake3::derive_key("OptimalFS v1 copy 1 block encryption key", &bk);
                let aead = XChaCha20Poly1305::new(&bk.into());
                let aead1 = XChaCha20Poly1305::new(&k1.into());
                bk.zeroize();
                k1.zeroize();
                Sealer {
                    aead: Some(aead),
                    aead1: Some(aead1),
                    copy1_key: [0; 32],
                    checksum_key: blake3::derive_key("OptimalFS v1 encrypted checksum key", &m.0),
                    name_key: blake3::derive_key("OptimalFS v1 encrypted name hash key", &m.0),
                    uuid,
                    nonce_prefix,
                    counter: 0,
                }
            }
        }
    }

    pub fn encrypted(&self) -> bool {
        self.aead.is_some()
    }

    fn ad(&self, kind: BlockKind, addr: u64, birth: u64) -> [u8; 36] {
        let mut ad = [0u8; 36];
        ad[0..3].copy_from_slice(b"OFS");
        ad[3] = kind as u8;
        ad[4..12].copy_from_slice(&addr.to_le_bytes());
        ad[12..20].copy_from_slice(&birth.to_le_bytes());
        ad[20..36].copy_from_slice(&self.uuid);
        ad
    }

    pub fn next_nonce(&mut self) -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        if self.aead.is_some() {
            n[..16].copy_from_slice(&self.nonce_prefix);
            n[16..].copy_from_slice(&self.counter.to_le_bytes());
            self.counter = self
                .counter
                .checked_add(1)
                .expect("nonce counter exhausted");
        }
        n
    }

    /// Reserves `n` consecutive nonces and returns the first. Nonce `i` of
    /// the run is [`nonce_add`]`(base, i)`. Used for data extents, which store
    /// one base nonce for all their blocks.
    pub fn reserve_nonces(&mut self, n: u64) -> [u8; NONCE_LEN] {
        let base = self.next_nonce();
        if self.aead.is_some() {
            self.counter = self
                .counter
                .checked_add(n.saturating_sub(1))
                .expect("nonce counter exhausted");
        }
        base
    }

    /// Seals with a caller-chosen nonce (from [`Sealer::reserve_nonces`]).
    pub fn seal_with(
        &self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        nonce: &[u8; NONCE_LEN],
        buf: &mut [u8],
    ) -> [u8; MAC_LEN] {
        let ad = self.ad(kind, addr, birth);
        match &self.aead {
            Some(c) => c
                .encrypt_inout_detached(&xnonce(nonce), &ad, buf.into())
                .expect("seal")
                .into(),
            None => self.checksum(&ad, buf),
        }
    }

    /// Seals `buf` in place (encrypting it on encrypted volumes) and returns
    /// the nonce and MAC that the parent pointer must record.
    pub fn seal(
        &mut self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        buf: &mut [u8],
    ) -> ([u8; NONCE_LEN], [u8; MAC_LEN]) {
        let nonce = self.next_nonce();
        let mac = self.seal_with(kind, addr, birth, &nonce, buf);
        (nonce, mac)
    }

    /// Turns a plaintext block into its copy-1 bytes (the copy-0 bytes are
    /// [`Sealer::seal_with`]'s output for the same arguments).
    pub fn seal_copy1(
        &self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        nonce: &[u8; NONCE_LEN],
        buf: &mut [u8],
    ) {
        let ad = self.ad(kind, addr, birth);
        match &self.aead1 {
            // Encryption is a keystream XOR; the tag is not needed (the
            // parent's MAC covers copy 0, which copy 1 maps back to).
            Some(c) => {
                let _ = c
                    .encrypt_inout_detached(&xnonce(nonce), &ad, buf.into())
                    .expect("seal copy 1");
            }
            None => self.xor_plain_mask(&ad, buf),
        }
    }

    /// Converts copy-0 bytes to copy-1 bytes or back (an involution: it is
    /// an XOR with a mask that depends only on the keys, nonce and
    /// associated data). Nothing is verified; check the result with
    /// [`Sealer::open`] as copy 0.
    pub fn swap_copy(
        &self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        nonce: &[u8; NONCE_LEN],
        buf: &mut [u8],
    ) {
        let ad = self.ad(kind, addr, birth);
        match (&self.aead, &self.aead1) {
            (Some(c0), Some(c1)) => {
                for c in [c0, c1] {
                    let _ = c
                        .encrypt_inout_detached(&xnonce(nonce), &ad, (&mut *buf).into())
                        .expect("copy mask");
                }
            }
            _ => self.xor_plain_mask(&ad, buf),
        }
    }

    fn xor_plain_mask(&self, ad: &[u8], buf: &mut [u8]) {
        let mut h = blake3::Hasher::new_keyed(&self.copy1_key);
        h.update(ad);
        let mut xof = h.finalize_xof();
        let mut m = [0u8; 64];
        for c in buf.chunks_mut(64) {
            xof.fill(&mut m[..c.len()]);
            for (x, y) in c.iter_mut().zip(m.iter()) {
                *x ^= y;
            }
        }
    }

    /// Verifies (and decrypts) `buf` in place.
    pub fn open(
        &self,
        kind: BlockKind,
        addr: u64,
        birth: u64,
        nonce: &[u8; NONCE_LEN],
        mac: &[u8; MAC_LEN],
        buf: &mut [u8],
    ) -> Result<()> {
        let ad = self.ad(kind, addr, birth);
        let ok = match &self.aead {
            Some(c) => c
                .decrypt_inout_detached(&xnonce(nonce), &ad, buf.into(), &(*mac).into())
                .is_ok(),
            None => ct_eq(&self.checksum(&ad, buf), mac),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::Corrupt("block failed verification", addr))
        }
    }

    fn checksum(&self, ad: &[u8], data: &[u8]) -> [u8; MAC_LEN] {
        let mut h = blake3::Hasher::new_keyed(&self.checksum_key);
        h.update(ad);
        h.update(data);
        let mut out = [0u8; MAC_LEN];
        out.copy_from_slice(&h.finalize().as_bytes()[..MAC_LEN]);
        out
    }

    /// Keyed 64-bit hash of a directory entry name.
    pub fn name_hash(&self, name: &[u8]) -> u64 {
        let h = blake3::keyed_hash(&self.name_key, name);
        u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
    }
}

/// Nonce `i` of a run reserved by [`Sealer::reserve_nonces`]. The counter
/// lives in the last 8 bytes; plain volumes use all-zero nonces.
pub fn nonce_add(base: &[u8; NONCE_LEN], i: u64) -> [u8; NONCE_LEN] {
    let mut n = *base;
    if n == [0u8; NONCE_LEN] {
        return n;
    }
    let c = u64::from_le_bytes(n[16..].try_into().unwrap()).wrapping_add(i);
    n[16..].copy_from_slice(&c.to_le_bytes());
    n
}

/// Constant-time comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}
