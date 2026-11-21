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
}
