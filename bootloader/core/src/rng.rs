//! Random numbers for KASLR and the entropy response.
//!
//! All sources (firmware RNG protocol, RDSEED, RDRAND, timer jitter) are
//! hashed together into a 512-bit pool with SHA-512; output blocks are
//! SHA-512(pool || counter), a hash DRBG. If any one source is good, the
//! output is good; a weak or backdoored source can't make it worse.

use sha2::{Digest, Sha512};

pub struct Drbg {
    pool: [u8; 64],
    counter: u64,
    buf: [u8; 64],
    used: usize,
    /// Bits of entropy credited from sources the caller trusts.
    pub credited_bits: u32,
}

impl Default for Drbg {
    fn default() -> Self {
        Self::new()
    }
}

impl Drbg {
    pub fn new() -> Self {
        Drbg {
            pool: [0; 64],
            counter: 0,
            buf: [0; 64],
            used: 64,
            credited_bits: 0,
        }
    }

    /// Mixes `data` into the pool. `bits` is how much entropy to credit.
    pub fn mix(&mut self, label: &[u8], data: &[u8], bits: u32) {
        let mut h = Sha512::new();
        h.update(self.pool);
        h.update((label.len() as u64).to_le_bytes());
        h.update(label);
        h.update((data.len() as u64).to_le_bytes());
        h.update(data);
        self.pool = h.finalize().into();
        self.used = 64;
        self.credited_bits = self.credited_bits.saturating_add(bits);
    }

    fn refill(&mut self) {
        let mut h = Sha512::new();
        h.update(b"out");
        h.update(self.pool);
        h.update(self.counter.to_le_bytes());
        self.buf = h.finalize().into();
        self.counter += 1;
        // Ratchet the pool forward so earlier outputs can't be recomputed
        // from a later pool state.
        let mut r = Sha512::new();
        r.update(b"ratchet");
        r.update(self.pool);
        self.pool = r.finalize().into();
        self.used = 0;
    }

    pub fn next_u64(&mut self) -> u64 {
        if self.used + 8 > 64 {
            self.refill();
        }
        let v = u64::from_le_bytes(self.buf[self.used..self.used + 8].try_into().unwrap());
        self.used += 8;
        v
    }

    /// Uniform value in 0..n (n > 0), without modulo bias.
    pub fn below(&mut self, n: u64) -> u64 {
        if n <= 1 {
            return 0;
        }
        let zone = u64::MAX - (u64::MAX % n) - 1;
        loop {
            let v = self.next_u64();
            if v <= zone {
                return v % n;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_inputs_give_different_streams() {
        let mut a = Drbg::new();
        a.mix(b"x", b"1", 8);
        let mut b = Drbg::new();
        b.mix(b"x", b"2", 8);
        assert_ne!(a.next_u64(), b.next_u64());
        let first = a.next_u64();
        assert_ne!(first, a.next_u64());
    }

    #[test]
    fn below_is_in_range_and_spread() {
        let mut d = Drbg::new();
        d.mix(b"seed", b"t", 0);
        let mut hits = [0u32; 7];
        for _ in 0..70_000 {
            hits[d.below(7) as usize] += 1;
        }
        for h in hits {
            assert!((9_000..11_000).contains(&h), "{hits:?}");
        }
        assert_eq!(d.below(1), 0);
    }
}
