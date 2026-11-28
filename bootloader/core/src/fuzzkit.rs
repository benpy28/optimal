//! Seeded structured mutation for the parser fuzz tests (DESIGN.md item 8).
//!
//! Stable Rust has no coverage-guided fuzzing without sanitizer runtimes, so
//! every parser gets a generator of valid inputs, the mutator below, and a
//! property. The case count comes from `OPTIMAL_FUZZ_CASES` (default 10^4,
//! CI's fuzz step runs 10^5, the nightly soak more) and the base seed from
//! `OPTIMAL_FUZZ_SEED` (default fixed, so a plain `cargo test` is
//! reproducible). A failing case prints its seed; rerun just that case with
//! `OPTIMAL_FUZZ_SEED=<seed> OPTIMAL_FUZZ_CASES=1`.

extern crate std;
use crate::testrng::Rng;
use std::vec::Vec;

pub fn cases() -> u64 {
    std::env::var("OPTIMAL_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

fn base_seed() -> u64 {
    std::env::var("OPTIMAL_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x0b7_f022)
}

/// Runs `case(rng)` for every case, each with its own seed, and names the
/// seed of a case that panics.
pub fn run(name: &str, salt: u64, case: impl FnMut(&mut Rng)) {
    run_n(name, salt, cases(), case)
}

/// As `run`, with an explicit case count (for parsers whose cases are slow).
pub fn run_n(name: &str, salt: u64, n: u64, mut case: impl FnMut(&mut Rng)) {
    let base = base_seed();
    for i in 0..n {
        let seed = base.wrapping_add(i).wrapping_mul(0x100_0000_01b3) ^ salt;
        // When a seed is given with one case, run exactly that seed.
        let seed = if n == 1 { base } else { seed };
        let mut rng = Rng::new(seed);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case(&mut rng)));
        if let Err(e) = r {
            std::eprintln!(
                "fuzz {name}: case {i} failed; rerun with OPTIMAL_FUZZ_SEED={seed} OPTIMAL_FUZZ_CASES=1"
            );
            std::panic::resume_unwind(e);
        }
    }
}

/// Values that tend to break length and offset arithmetic.
const LIES: [u64; 12] = [
    0,
    1,
    2,
    3,
    4,
    0x7f,
    0xff,
    0xffff,
    0x7fff_ffff,
    0xffff_ffff,
    0xffff_ffff_ffff_fff0,
    u64::MAX,
];

/// Applies 1 to 4 random mutations: byte flips, truncation, length-field
/// lies (an interesting u8/u16/u32/u64 written at a random offset),
/// duplicated chunks and deleted chunks.
pub fn mutate(rng: &mut Rng, b: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        if b.is_empty() {
            b.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(b.len() as u64) as usize;
        match rng.below(7) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b[at] = rng.next() as u8,
            2 => b.truncate(at),
            3 => {
                let w = [1usize, 2, 4, 8][rng.below(4) as usize];
                let v = if rng.below(2) == 0 {
                    LIES[rng.below(LIES.len() as u64) as usize]
                } else {
                    // Near the real length: the classic off-by-one lie.
                    (b.len() as u64).wrapping_add(rng.below(5)).wrapping_sub(2)
                };
                let bytes = v.to_le_bytes();
                for (k, &byte) in bytes.iter().enumerate().take(w) {
                    if let Some(x) = b.get_mut(at + k) {
                        *x = byte;
                    }
                }
            }
            4 => {
                let len = (1 + rng.below(32) as usize).min(b.len() - at);
                let chunk: Vec<u8> = b[at..at + len].to_vec();
                let to = rng.below(b.len() as u64 + 1) as usize;
                b.splice(to..to, chunk);
            }
            5 => {
                let len = (1 + rng.below(32) as usize).min(b.len() - at);
                b.drain(at..at + len);
            }
            _ => b.insert(at, rng.next() as u8),
        }
    }
}
