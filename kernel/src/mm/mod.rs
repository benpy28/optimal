//! Kernel memory management: physical frames now; page tables, the heap
//! and kernel stacks build on this.

pub mod frames;
pub mod paging;
#[cfg(feature = "ktest")]
mod paging_tests;
#[cfg(feature = "ktest")]
mod tests;

/// Below this physical address nothing goes to the frame allocator: the
/// first MiB holds firmware structures, and the SMP startup trampoline
/// (Phase 2) needs a page there that nothing else may own.
pub const LOW_MEMORY_END: u64 = 1 << 20;
