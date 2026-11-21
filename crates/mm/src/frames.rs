//! Physical frame allocator: a binary buddy allocator.
//!
//! Memory is handed out in naturally aligned blocks of 2^order frames
//! (order 0 = 4 KiB, order 9 = 2 MiB, order 18 = 1 GiB), so large aligned
//! blocks for huge pages come straight out of the allocator. Freeing a block
//! merges it with its "buddy" (the neighbouring block of the same size) when
//! that is free too, which keeps fragmentation down.
//!
//! Bookkeeping lives in a separate metadata array, one small entry per frame,
//! rather than inside the free memory itself. A stray write into freed memory
//! therefore can't corrupt the allocator, and every free is checked: freeing
//! a block that isn't allocated, or with the wrong size, is reported as an
//! error instead of silently corrupting the free lists.
//!
//! One allocator covers one contiguous zone of physical memory; the kernel
//! keeps one per zone (and later per NUMA node).

/// Largest block the kernel uses: 2^18 frames = 1 GiB.
pub const MAX_ORDER: usize = 18;

const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum State {
    /// Not managed: a hole in physical memory, firmware data, the kernel.
    Reserved = 0,
    /// First frame of a free block of `order`.
    FreeHead = 1,
    /// First frame of an allocated block of `order`.
    UsedHead = 2,
    /// Any other frame inside a free or allocated block.
    Tail = 3,
}

/// Per-frame bookkeeping, 12 bytes per 4 KiB frame (0.3% of memory).
#[derive(Clone, Copy, Debug)]
pub struct FrameMeta {
    next: u32,
    prev: u32,
    order: u8,
    state: State,
}

impl FrameMeta {
    pub const RESERVED: FrameMeta = FrameMeta {
        next: NIL,
        prev: NIL,
        order: 0,
        state: State::Reserved,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    /// The frame isn't inside this allocator's zone.
    OutOfZone,
    /// The frame isn't the start of an allocated block: a double free, or a
    /// pointer into the middle of a block, or memory never allocated.
    NotAllocated,
    /// The block was allocated with a different order.
    WrongOrder { allocated: u8 },
}

/// A buddy allocator over frames `base .. base + meta.len()`.
///
/// `MAX` is the largest order (blocks of 2^MAX frames). The kernel uses
/// [`MAX_ORDER`]; proofs use a small value so the model checker can explore
/// every case.
pub struct Buddy<'m, const MAX: usize = MAX_ORDER> {
    meta: &'m mut [FrameMeta],
    /// Frame number of meta[0]. A multiple of 2^MAX so buddy arithmetic on
    /// indices matches physical alignment.
    base: u64,
    heads: [u32; 32],
    free_frames: u64,
}

impl<'m, const MAX: usize> Buddy<'m, MAX> {
    /// An allocator with every frame reserved. `base` (a frame number) must be
    /// a multiple of 2^MAX, and `meta` at most 2^32 - 1 entries.
    pub fn new(meta: &'m mut [FrameMeta], base: u64) -> Self {
        assert!(MAX < 32);
        assert!(base.is_multiple_of(1 << MAX), "zone base not aligned");
        assert!(meta.len() < NIL as usize, "zone too large");
        meta.fill(FrameMeta::RESERVED);
        Buddy {
            meta,
            base,
            heads: [NIL; 32],
            free_frames: 0,
        }
    }

    /// First frame number this allocator could manage.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// One past the last frame number this allocator could manage.
    pub fn end(&self) -> u64 {
        self.base + self.meta.len() as u64
    }

    pub fn free_frames(&self) -> u64 {
        self.free_frames
    }

    /// Number of free blocks of exactly `order` (for tests and statistics).
    pub fn free_blocks(&self, order: usize) -> usize {
        let mut n = 0;
        let mut i = self.heads[order];
        while i != NIL {
            n += 1;
            i = self.meta[i as usize].next;
        }
        n
    }

    /// Hand frames `start .. start + count` to the allocator. They must lie in
    /// the zone and be currently reserved (each frame can only be added once).
    /// Returns how many frames were added.
    pub fn add_range(&mut self, start: u64, count: u64) -> u64 {
        let lo = start.max(self.base);
        let hi = (start.saturating_add(count)).min(self.end());
        let mut f = lo;
        let mut added = 0;
        while f < hi {
            let idx = (f - self.base) as usize;
            if self.meta[idx].state != State::Reserved {
                f += 1;
                continue;
            }
            // Largest aligned block starting at f that fits and is all reserved.
            let mut order = 0;
            while order < MAX {
                let size = 1u64 << (order + 1);
                if !(f - self.base).is_multiple_of(size) || f + size > hi {
                    break;
                }
                if !self.all_reserved(idx, size as usize) {
                    break;
                }
                order += 1;
            }
            let size = 1u64 << order;
            self.mark_block(idx, order, State::UsedHead);
            self.release(idx, order);
            added += size;
            f += size;
        }
        added
    }

    fn all_reserved(&self, idx: usize, len: usize) -> bool {
        self.meta[idx..idx + len]
            .iter()
            .all(|m| m.state == State::Reserved)
    }

    /// Allocate a block of 2^order frames. Returns its first frame number,
    /// aligned to 2^order frames.
    pub fn alloc(&mut self, order: usize) -> Option<u64> {
        if order > MAX {
            return None;
        }
        let mut have = order;
        while have <= MAX && self.heads[have] == NIL {
            have += 1;
        }
        if have > MAX {
            return None;
        }
        let idx = self.heads[have] as usize;
        self.unlink(idx, have);
        // Split down to the requested size, freeing the upper halves.
        while have > order {
            have -= 1;
            let upper = idx + (1 << have);
            self.mark_block(upper, have, State::FreeHead);
            self.push(upper, have);
        }
        self.mark_block(idx, order, State::UsedHead);
        self.free_frames -= 1 << order;
        Some(self.base + idx as u64)
    }

    /// Free a block previously returned by `alloc(order)`.
    pub fn free(&mut self, frame: u64, order: usize) -> Result<(), FreeError> {
        if frame < self.base || frame >= self.end() || order > MAX {
            return Err(FreeError::OutOfZone);
        }
        let idx = (frame - self.base) as usize;
        let m = self.meta[idx];
        if m.state != State::UsedHead {
            return Err(FreeError::NotAllocated);
        }
        if m.order as usize != order {
            return Err(FreeError::WrongOrder { allocated: m.order });
        }
        self.release(idx, order);
        Ok(())
    }

    /// Is `frame` the start of an allocated block, and of what order?
    pub fn allocated_order(&self, frame: u64) -> Option<usize> {
        if frame < self.base || frame >= self.end() {
            return None;
        }
        let m = self.meta[(frame - self.base) as usize];
        (m.state == State::UsedHead).then_some(m.order as usize)
    }

    /// Turn an allocated block (all frames marked) into free memory, merging
    /// with free buddies as far as possible.
    fn release(&mut self, mut idx: usize, mut order: usize) {
        self.free_frames += 1 << order;
        while order < MAX {
            let buddy = idx ^ (1 << order);
            if buddy + (1 << order) > self.meta.len() {
                break;
            }
            let b = self.meta[buddy];
            if b.state != State::FreeHead || b.order as usize != order {
                break;
            }
            self.unlink(buddy, order);
            idx = idx.min(buddy);
            order += 1;
        }
        self.mark_block(idx, order, State::FreeHead);
        self.push(idx, order);
    }

    fn mark_block(&mut self, idx: usize, order: usize, head: State) {
        let len = 1usize << order;
        self.meta[idx] = FrameMeta {
            next: NIL,
            prev: NIL,
            order: order as u8,
            state: head,
        };
        for m in &mut self.meta[idx + 1..idx + len] {
            *m = FrameMeta {
                next: NIL,
                prev: NIL,
                order: 0,
                state: State::Tail,
            };
        }
    }

    fn push(&mut self, idx: usize, order: usize) {
        let head = self.heads[order];
        self.meta[idx].next = head;
        self.meta[idx].prev = NIL;
        if head != NIL {
            self.meta[head as usize].prev = idx as u32;
        }
        self.heads[order] = idx as u32;
    }

    fn unlink(&mut self, idx: usize, order: usize) {
        let FrameMeta { next, prev, .. } = self.meta[idx];
        if prev == NIL {
            self.heads[order] = next;
        } else {
            self.meta[prev as usize].next = next;
        }
        if next != NIL {
            self.meta[next as usize].prev = prev;
        }
        self.meta[idx].next = NIL;
        self.meta[idx].prev = NIL;
    }

    /// Check every internal invariant; used by tests and debug builds.
    /// Returns a description of the first violation found.
    pub fn check(&self) -> Result<(), &'static str> {
        let mut free = 0u64;
        for order in 0..=MAX {
            let mut prev = NIL;
            let mut i = self.heads[order];
            let mut steps = 0;
            while i != NIL {
                steps += 1;
                if steps > self.meta.len() {
                    return Err("free list cycle");
                }
                let m = self.meta[i as usize];
                if m.state != State::FreeHead || m.order as usize != order {
                    return Err("free list entry is not a free head of its order");
                }
                if m.prev != prev {
                    return Err("free list back link broken");
                }
                if !(i as usize).is_multiple_of(1 << order) {
                    return Err("free block misaligned");
                }
                free += 1 << order;
                prev = i;
                i = m.next;
            }
        }
        if free != self.free_frames {
            return Err("free frame count wrong");
        }
        // Walk the zone block by block.
        let mut i = 0;
        let mut counted_free = 0u64;
        while i < self.meta.len() {
            let m = self.meta[i];
            match m.state {
                State::Reserved => i += 1,
                State::Tail => return Err("tail frame outside any block"),
                State::FreeHead | State::UsedHead => {
                    let len = 1usize << m.order;
                    if i + len > self.meta.len() || !i.is_multiple_of(len) {
                        return Err("block out of range or misaligned");
                    }
                    if self.meta[i + 1..i + len]
                        .iter()
                        .any(|t| t.state != State::Tail)
                    {
                        return Err("block body not all tail frames");
                    }
                    if m.state == State::FreeHead {
                        counted_free += len as u64;
                        // A free block whose buddy is also free should have merged.
                        if (m.order as usize) < MAX {
                            let buddy = i ^ len;
                            if buddy + len <= self.meta.len() {
                                let b = self.meta[buddy];
                                if b.state == State::FreeHead && b.order == m.order {
                                    return Err("two free buddies not merged");
                                }
                            }
                        }
                    }
                    i += len;
                }
            }
        }
        if counted_free != self.free_frames {
            return Err("blocks marked free don't match the free count");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    fn zone(n: usize) -> Vec<FrameMeta> {
        vec![FrameMeta::RESERVED; n]
    }

    #[test]
    fn whole_zone_becomes_one_block() {
        let mut m = zone(1 << 10);
        let mut b: Buddy<10> = Buddy::new(&mut m, 0);
        assert_eq!(b.add_range(0, 1 << 10), 1 << 10);
        assert_eq!(b.free_blocks(10), 1);
        b.check().unwrap();
    }

    #[test]
    fn odd_range_splits_into_aligned_blocks() {
        let mut m = zone(64);
        let mut b: Buddy<6> = Buddy::new(&mut m, 0);
        assert_eq!(b.add_range(3, 40), 40);
        b.check().unwrap();
        assert_eq!(b.free_frames(), 40);
        // 3, 4-7, 8-15, 16-31, 32-39, 40-41, 42
        assert_eq!(b.free_blocks(0), 2);
        assert_eq!(b.free_blocks(1), 1);
        assert_eq!(b.free_blocks(2), 1);
        assert_eq!(b.free_blocks(3), 2);
        assert_eq!(b.free_blocks(4), 1);
    }

    #[test]
    fn alloc_is_aligned_and_merges_back() {
        let mut m = zone(256);
        let mut b: Buddy<8> = Buddy::new(&mut m, 512);
        b.add_range(512, 256);
        let a = b.alloc(0).unwrap();
        let c = b.alloc(3).unwrap();
        assert_eq!(c % 8, 0);
        b.check().unwrap();
        b.free(a, 0).unwrap();
        b.free(c, 3).unwrap();
        b.check().unwrap();
        assert_eq!(b.free_blocks(8), 1);
    }

    #[test]
    fn bad_frees_are_rejected() {
        let mut m = zone(16);
        let mut b: Buddy<4> = Buddy::new(&mut m, 0);
        b.add_range(0, 16);
        let a = b.alloc(1).unwrap();
        assert_eq!(b.free(a, 0), Err(FreeError::WrongOrder { allocated: 1 }));
        assert_eq!(b.free(a + 1, 0), Err(FreeError::NotAllocated));
        b.free(a, 1).unwrap();
        assert_eq!(b.free(a, 1), Err(FreeError::NotAllocated));
        assert_eq!(b.free(99, 0), Err(FreeError::OutOfZone));
        b.check().unwrap();
    }

    #[test]
    fn exhaustion_returns_none() {
        let mut m = zone(8);
        let mut b: Buddy<3> = Buddy::new(&mut m, 0);
        b.add_range(0, 8);
        for _ in 0..8 {
            b.alloc(0).unwrap();
        }
        assert_eq!(b.alloc(0), None);
        assert_eq!(b.alloc(9), None);
        b.check().unwrap();
    }

    #[test]
    fn adding_twice_adds_nothing() {
        let mut m = zone(32);
        let mut b: Buddy<5> = Buddy::new(&mut m, 0);
        assert_eq!(b.add_range(0, 32), 32);
        assert_eq!(b.add_range(0, 32), 0);
        b.check().unwrap();
    }

    #[derive(Debug, Clone)]
    enum Op {
        Alloc(usize),
        Free(usize),
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::test_cases(2000)))]
        /// Random alloc/free sequences against a reference model: blocks never
        /// overlap, are aligned, stay inside the added ranges, and every
        /// internal invariant holds after every step.
        #[test]
        fn random_ops_match_model(
            ranges in prop::collection::vec((0u64..512, 1u64..200), 1..4),
            ops in prop::collection::vec(
                prop_oneof![(0usize..7).prop_map(Op::Alloc), (0usize..64).prop_map(Op::Free)],
                1..300,
            ),
        ) {
            let mut m = zone(512);
            let mut b: Buddy<9> = Buddy::new(&mut m, 0);
            let mut usable = vec![false; 512];
            for (s, c) in &ranges {
                b.add_range(*s, *c);
                for f in *s..(*s + *c).min(512) { usable[f as usize] = true; }
            }
            b.check().unwrap();
            let mut owned: BTreeMap<u64, usize> = BTreeMap::new();
            let mut taken = vec![false; 512];
            for op in ops {
                match op {
                    Op::Alloc(order) => {
                        if let Some(f) = b.alloc(order) {
                            prop_assert_eq!(f % (1 << order), 0);
                            for x in f..f + (1 << order) {
                                prop_assert!(usable[x as usize], "allocated unusable frame");
                                prop_assert!(!taken[x as usize], "overlapping allocation");
                                taken[x as usize] = true;
                            }
                            owned.insert(f, order);
                        }
                    }
                    Op::Free(k) => {
                        if owned.is_empty() { continue; }
                        let (&f, &order) = owned.iter().nth(k % owned.len()).unwrap();
                        owned.remove(&f);
                        prop_assert_eq!(b.free(f, order), Ok(()));
                        for x in f..f + (1 << order) { taken[x as usize] = false; }
                        prop_assert_eq!(b.free(f, order), Err(FreeError::NotAllocated));
                    }
                }
                prop_assert_eq!(b.check(), Ok(()));
                let used: u64 = owned.values().map(|o| 1u64 << o).sum();
                let total = usable.iter().filter(|u| **u).count() as u64;
                prop_assert_eq!(b.free_frames() + used, total);
            }
            for (f, o) in owned { b.free(f, o).unwrap(); }
            b.check().unwrap();
            prop_assert_eq!(b.free_frames(), usable.iter().filter(|u| **u).count() as u64);
        }
    }
}

/// Kani proofs (proof target 2 in docs/verification.md). Bounded: a 16-frame
/// zone with blocks up to 2^4 frames, every possible order and frame.
#[cfg(kani)]
mod proofs {
    use super::*;

    const N: usize = 16;

    fn any_zone(meta: &mut [FrameMeta; N]) -> Buddy<'_, 4> {
        let mut b: Buddy<4> = Buddy::new(meta, 0);
        let start: u64 = kani::any();
        let count: u64 = kani::any();
        kani::assume(start < N as u64 && count <= N as u64);
        b.add_range(start, count);
        b
    }

    /// Two allocations never overlap, and each is aligned and inside the zone.
    #[kani::proof]
    #[kani::unwind(20)]
    fn allocations_never_overlap() {
        let mut meta = [FrameMeta::RESERVED; N];
        let mut b = any_zone(&mut meta);
        let o1: usize = kani::any();
        let o2: usize = kani::any();
        kani::assume(o1 <= 4 && o2 <= 4);
        if let (Some(a), Some(c)) = (b.alloc(o1), b.alloc(o2)) {
            let (ae, ce) = (a + (1 << o1), c + (1 << o2));
            assert!(ae <= c || ce <= a, "overlap");
            assert!(a % (1 << o1) == 0 && c % (1 << o2) == 0);
            assert!(ae <= N as u64 && ce <= N as u64);
        }
    }

    /// Freeing anything other than a live block of the right order is refused
    /// and changes nothing; a correct free succeeds exactly once.
    #[kani::proof]
    #[kani::unwind(20)]
    fn frees_are_checked() {
        let mut meta = [FrameMeta::RESERVED; N];
        let mut b = any_zone(&mut meta);
        let o: usize = kani::any();
        kani::assume(o <= 4);
        if let Some(a) = b.alloc(o) {
            let before = b.free_frames();
            let f: u64 = kani::any();
            let fo: usize = kani::any();
            kani::assume(fo <= 4);
            if f != a || fo != o {
                assert!(b.free(f, fo).is_err());
                assert!(b.free_frames() == before);
            }
            assert!(b.free(a, o).is_ok());
            assert!(b.free(a, o) == Err(FreeError::NotAllocated));
        }
    }

    /// After any add + alloc + free sequence of length 2, invariants hold.
    #[kani::proof]
    #[kani::unwind(20)]
    fn invariants_hold() {
        let mut meta = [FrameMeta::RESERVED; N];
        let mut b = any_zone(&mut meta);
        let o: usize = kani::any();
        kani::assume(o <= 4);
        let a = b.alloc(o);
        assert!(b.check().is_ok());
        if let Some(a) = a {
            b.free(a, o).unwrap();
            assert!(b.check().is_ok());
        }
    }
}
