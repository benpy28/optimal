//! Signatures over the boot config.
//!
//! Signature file (`<config>.sig`), 104 bytes:
//!   0..8    magic "OPTBSIG1"
//!   8..40   key id = SHA-256 of the 32-byte Ed25519 public key
//!   40..104 Ed25519 signature over DOMAIN || SHA-512(config)
//!
//! The domain prefix means a signature made for anything else (another file
//! type, a future format) can never be replayed as a config signature.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

pub const MAGIC: &[u8; 8] = b"OPTBSIG1";
pub const DOMAIN: &[u8] = b"Optimal Boot config signature v1\0";
pub const SIG_FILE_LEN: usize = 104;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SigError {
    BadLength,
    BadMagic,
    UnknownKey,
    BadKey,
    Invalid,
}

impl SigError {
    pub fn describe(&self) -> &'static str {
        match self {
            SigError::BadLength => "signature file has the wrong size",
            SigError::BadMagic => "signature file is not an Optimal Boot signature",
            SigError::UnknownKey => "config is signed by a key this loader does not trust",
            SigError::BadKey => "trusted key is malformed",
            SigError::Invalid => "config signature is invalid (the config was changed or damaged)",
        }
    }
}

pub fn key_id(public_key: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(public_key).into()
}

pub fn sha512(data: &[u8]) -> [u8; 64] {
    Sha512::digest(data).into()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Constant-time comparison, so timing never leaks how much of a hash matched.
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

/// Verifies `sig_file` over `config` against the trusted keys. Returns the
/// index of the key that signed it.
pub fn verify(config: &[u8], sig_file: &[u8], trusted: &[[u8; 32]]) -> Result<usize, SigError> {
    if sig_file.len() != SIG_FILE_LEN {
        return Err(SigError::BadLength);
    }
    if &sig_file[0..8] != MAGIC {
        return Err(SigError::BadMagic);
    }
    let id: [u8; 32] = sig_file[8..40].try_into().unwrap();
    let index = trusted
        .iter()
        .position(|k| key_id(k) == id)
        .ok_or(SigError::UnknownKey)?;
    let key = VerifyingKey::from_bytes(&trusted[index]).map_err(|_| SigError::BadKey)?;
    let sig = Signature::from_bytes(sig_file[40..104].try_into().unwrap());
    // verify_strict also rejects small-order keys and non-canonical signatures.
    key.verify_strict(&signed_message(config), &sig)
        .map_err(|_| SigError::Invalid)?;
    Ok(index)
}

/// The exact bytes that are signed: DOMAIN || SHA-512(config). Signing the
/// digest keeps the message small and fixed-size (the loader runs on a small
/// firmware stack), with SHA-512's 256-bit collision resistance.
pub fn signed_message(config: &[u8]) -> [u8; DOMAIN.len() + 64] {
    let mut msg = [0u8; DOMAIN.len() + 64];
    msg[..DOMAIN.len()].copy_from_slice(DOMAIN);
    msg[DOMAIN.len()..].copy_from_slice(&sha512(config));
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    extern crate std;
    use std::vec::Vec;

    fn sign(key: &SigningKey, config: &[u8]) -> Vec<u8> {
        let sig = key.sign(&signed_message(config));
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&key_id(&key.verifying_key().to_bytes()));
        out.extend_from_slice(&sig.to_bytes());
        out
    }

    #[test]
    fn good_signature_verifies_and_tampering_fails() {
        let k1 = SigningKey::from_bytes(&[1u8; 32]);
        let k2 = SigningKey::from_bytes(&[2u8; 32]);
        let trusted = [k2.verifying_key().to_bytes(), k1.verifying_key().to_bytes()];
        let cfg = b"optimal-boot-config 1\n";
        let s = sign(&k1, cfg);
        assert_eq!(verify(cfg, &s, &trusted), Ok(1));
        // Any changed byte in the config fails.
        let mut bad = cfg.to_vec();
        bad[3] ^= 1;
        assert_eq!(verify(&bad, &s, &trusted), Err(SigError::Invalid));
        // Any changed byte in the signature fails.
        for i in 40..104 {
            let mut s2 = s.clone();
            s2[i] ^= 0x40;
            assert_eq!(
                verify(cfg, &s2, &trusted),
                Err(SigError::Invalid),
                "byte {i}"
            );
        }
        // Untrusted key.
        let k3 = SigningKey::from_bytes(&[3u8; 32]);
        assert_eq!(
            verify(cfg, &sign(&k3, cfg), &trusted),
            Err(SigError::UnknownKey)
        );
        // A key id swapped to a trusted key but signed by another key fails.
        let mut forged = sign(&k3, cfg);
        forged[8..40].copy_from_slice(&key_id(&k1.verifying_key().to_bytes()));
        assert_eq!(verify(cfg, &forged, &trusted), Err(SigError::Invalid));
        assert_eq!(verify(cfg, &s[..100], &trusted), Err(SigError::BadLength));
        let mut m = s.clone();
        m[0] = b'X';
        assert_eq!(verify(cfg, &m, &trusted), Err(SigError::BadMagic));
        // No domain prefix: a raw signature over the config is not accepted.
        let raw = k1.sign(cfg);
        let mut r = MAGIC.to_vec();
        r.extend_from_slice(&key_id(&k1.verifying_key().to_bytes()));
        r.extend_from_slice(&raw.to_bytes());
        assert_eq!(verify(cfg, &r, &trusted), Err(SigError::Invalid));
    }

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    /// Signature malleability: adding the group order L to s gives a second
    /// encoding of the same signature. verify_strict must refuse it, or one
    /// signed config would have two valid .sig files.
    #[test]
    fn non_canonical_s_is_refused() {
        // L = 2^252 + 27742317777372353535851937790883648493, little endian.
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ];
        let k = SigningKey::from_bytes(&[7u8; 32]);
        let trusted = [k.verifying_key().to_bytes()];
        let cfg = b"optimal-boot-config 1\n";
        let mut tried = 0;
        // s + L only fits in 32 bytes when s < 2^256 - L; try a few configs.
        for extra in 0u8..16 {
            let mut c = cfg.to_vec();
            c.push(extra);
            let s = sign(&k, &c);
            assert_eq!(verify(&c, &s, &trusted), Ok(0));
            let mut m = s.clone();
            let mut carry = 0u16;
            for i in 0..32 {
                let v = m[72 + i] as u16 + L[i] as u16 + carry;
                m[72 + i] = v as u8;
                carry = v >> 8;
            }
            if carry == 0 {
                tried += 1;
                assert_eq!(verify(&c, &m, &trusted), Err(SigError::Invalid));
            }
        }
        assert!(tried > 0);
    }

    /// A trusted key that is not a valid curve point, or is of small order,
    /// never verifies anything.
    #[test]
    fn malformed_and_weak_keys_are_refused() {
        let k = SigningKey::from_bytes(&[9u8; 32]);
        let cfg = b"x";
        // The identity point (small order): y = 1.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        let mut s = sign(&k, cfg);
        s[8..40].copy_from_slice(&key_id(&weak));
        assert_eq!(verify(cfg, &s, &[weak]), Err(SigError::Invalid));
        // y = 2 is not on the curve.
        let mut bad = [0u8; 32];
        bad[0] = 2;
        s[8..40].copy_from_slice(&key_id(&bad));
        assert_eq!(verify(cfg, &s, &[bad]), Err(SigError::BadKey));
    }

    /// Seeded fuzzing: random configs signed by one of several trusted keys
    /// verify, and any mutation of the config or of the signature file that
    /// changes a byte is refused.
    #[test]
    fn fuzz_any_change_is_refused() {
        let keys: Vec<SigningKey> = (1..=4u8)
            .map(|i| SigningKey::from_bytes(&[i; 32]))
            .collect();
        let trusted: Vec<[u8; 32]> = keys.iter().map(|k| k.verifying_key().to_bytes()).collect();
        // Each case signs and verifies up to three times (about 100x the cost
        // of a case of the other parsers), so it runs a tenth of the cases.
        let n = crate::fuzzkit::cases().div_ceil(10);
        crate::fuzzkit::run_n("sig", 5, n, |rng| {
            let cfg: Vec<u8> = (0..rng.below(512)).map(|_| rng.next() as u8).collect();
            let who = rng.below(4) as usize;
            let s = sign(&keys[who], &cfg);
            let k = 1 + rng.below(4) as usize;
            let pool = &trusted[4 - k..];
            let want = if who >= 4 - k {
                Ok(who - (4 - k))
            } else {
                Err(SigError::UnknownKey)
            };
            assert_eq!(verify(&cfg, &s, pool), want);
            let mut s2 = s.clone();
            crate::fuzzkit::mutate(rng, &mut s2);
            if s2 != s {
                assert!(verify(&cfg, &s2, pool).is_err(), "mutated sig accepted");
            }
            let mut c2 = cfg.clone();
            crate::fuzzkit::mutate(rng, &mut c2);
            if c2 != cfg {
                assert!(verify(&c2, &s, pool).is_err(), "mutated config accepted");
            }
        });
    }
}
