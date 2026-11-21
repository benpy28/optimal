//! Memory-management logic for the Optimal kernel.
//!
//! Everything here is plain safe Rust with no hardware access: the kernel
//! supplies the memory these structures work on. That split is deliberate:
//!
//! - host unit tests and property tests (`cargo test`) check it against
//!   simple reference models,
//! - Kani proofs (`cargo kani`) check key properties for every input within
//!   their bounds (see docs/verification.md, proof targets 1 and 2),
//! - the kernel's own test suite checks it again on real (virtual) hardware.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod frames;
pub mod paging;
pub mod slab;
pub mod zones;

/// Size of a base page / physical frame.
pub const PAGE_SIZE: u64 = 4096;

/// Property-test case count: `PROPTEST_CASES` if set (CI runs far more),
/// otherwise the given default.
#[cfg(test)]
pub(crate) fn test_cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
