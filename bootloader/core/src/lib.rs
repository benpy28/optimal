//! Optimal Boot core: the parts of the bootloader that are pure logic, kept
//! free of firmware calls so they can be unit tested and fuzzed on the host.

#![no_std]

pub mod acpi;
pub mod config;
pub mod elf;
pub mod kaslr;
pub mod memmap;
pub mod paging;
pub mod protocol;
pub mod rng;
pub mod scan;
pub mod sig;
#[cfg(test)]
pub mod testrng;
pub mod time;
