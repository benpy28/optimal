//! Small-object slabs for the kernel heap.
//!
//! A slab is one 4 KiB page cut into equal objects of one size class
//! (16 to 2048 bytes). Its bookkeeping is a 64-byte header at the start of
//! the page: a magic word, the class, a use count, list links and an
//! allocation bitmap. Freed objects are never written to, so a
//! use-after-free write can't corrupt the allocator (there are no free-list
//! pointers inside free memory to overwrite), and every free is checked:
//!
//! - the header's magic must be `secret ^ page address`, so a pointer into
//!   a page that isn't a slab (or a forged header without the boot-time
//!   secret) is refused;
//! - the pointer must be exactly at the start of an object of this class;
//! - its bit must be set, so a double free is refused.
//!
//! This module is the header logic only; the kernel puts the header at
//! the start of each slab page.

/// Size classes, smallest first.
pub const CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
/// Bytes of a slab: one page.
pub const SLAB_BYTES: usize = 4096;
/// Bytes of the header at the start of each slab.
pub const HEADER_BYTES: usize = 64;

/// Size class index for an allocation, or None if it goes straight to the
/// frame allocator (bigger than 2048 bytes, or aligned to more).
pub fn class_for(size: usize, align: usize) -> Option<usize> {
    let need = size.max(align).max(1);
    CLASSES.iter().position(|&c| c >= need)
}

/// Buddy order for an allocation too big for the slabs: the smallest block
/// of 2^order pages that holds `size` bytes. Buddy blocks are aligned to
/// their size, so any alignment up to that size is met too.
pub fn large_order(size: usize, align: usize) -> usize {
    let need = size.max(align).max(1).div_ceil(SLAB_BYTES);
    need.next_power_of_two().trailing_zeros() as usize
}

/// Offset of the first object of class `c` in the slab: objects are
/// aligned to their size, so the first one starts at the first multiple of
/// the size past the header.
pub fn first_object(class: usize) -> usize {
    HEADER_BYTES.next_multiple_of(CLASSES[class])
}

/// Number of objects of class `c` in one slab.
pub fn capacity(class: usize) -> usize {
    (SLAB_BYTES - first_object(class)) / CLASSES[class]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlabError {
    /// The page's header doesn't carry this kernel's magic for that page:
    /// not a slab, or corrupted.
    BadMagic,
    /// Not the start of an object of this slab's class.
    Misaligned,
    /// The object isn't allocated: a double free.
    NotAllocated,
}

/// The header at the start of every slab page. 64 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SlabHeader {
    magic: u64,
    class: u16,
    /// Set while the slab is on its class's list of slabs with free room.
    pub on_list: bool,
    _pad: u8,
    used: u32,
    /// List links (addresses of other slab pages, 0 for none), owned by the
    /// kernel's slab lists.
    pub next: u64,
    pub prev: u64,
    bitmap: [u64; 4],
}

const _: () = assert!(core::mem::size_of::<SlabHeader>() == HEADER_BYTES);
const _: () = assert!(CLASSES.len() == 8);

impl SlabHeader {
    /// A fresh, empty slab of class `class` for the page at `page`.
    pub fn new(page: u64, class: usize, secret: u64) -> SlabHeader {
        assert!(class < CLASSES.len());
        assert!(page.is_multiple_of(SLAB_BYTES as u64));
        SlabHeader {
            magic: secret ^ page,
            class: class as u16,
            on_list: false,
            _pad: 0,
            used: 0,
            next: 0,
            prev: 0,
            bitmap: [0; 4],
        }
    }

    /// Is this the header of a slab for `page`, made with `secret`?
    pub fn valid(&self, page: u64, secret: u64) -> bool {
        self.magic == secret ^ page && (self.class as usize) < CLASSES.len()
    }

    pub fn class(&self) -> usize {
        self.class as usize
    }

    pub fn used(&self) -> usize {
        self.used as usize
    }

    pub fn is_full(&self) -> bool {
        self.used() == capacity(self.class())
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Take a free object: returns its offset in the page (lowest first).
    pub fn alloc(&mut self) -> Option<usize> {
        let cap = capacity(self.class());
        for (w, word) in self.bitmap.iter_mut().enumerate() {
            let free = !*word;
            if free == 0 {
                continue;
            }
            let bit = free.trailing_zeros() as usize;
            let i = w * 64 + bit;
            if i >= cap {
                return None;
            }
            *word |= 1 << bit;
            self.used += 1;
            return Some(first_object(self.class()) + i * CLASSES[self.class()]);
        }
        None
    }

    /// Free the object at `offset` in the page, after checking the header
    /// (`page`, `secret`), the offset and the bitmap. A refused free changes
    /// nothing.
    pub fn free(&mut self, page: u64, offset: usize, secret: u64) -> Result<(), SlabError> {
        if !self.valid(page, secret) {
            return Err(SlabError::BadMagic);
        }
        let c = self.class();
        let first = first_object(c);
        // `first` is itself a multiple of the class size (see first_object),
        // so an object start is just an aligned offset at or after it.
        if offset < first || !offset.is_multiple_of(CLASSES[c]) {
            return Err(SlabError::Misaligned);
        }
        let i = (offset - first) / CLASSES[c];
        if i >= capacity(c) {
            return Err(SlabError::Misaligned);
        }
        let (w, bit) = (i / 64, i % 64);
        if self.bitmap[w] & (1 << bit) == 0 {
            return Err(SlabError::NotAllocated);
        }
        self.bitmap[w] &= !(1 << bit);
        self.used -= 1;
        Ok(())
    }

    /// Check the header's own consistency: the use count matches the
    /// bitmap, and no bit past the capacity is set.
    #[allow(clippy::needless_range_loop)]
    pub fn check(&self, page: u64, secret: u64) -> Result<(), &'static str> {
        if !self.valid(page, secret) {
            return Err("slab header magic wrong");
        }
        let cap = capacity(self.class());
        let mut n = 0;
        for i in 0..256 {
            if self.bitmap[i / 64] & (1 << (i % 64)) != 0 {
                if i >= cap {
                    return Err("slab bitmap bit past capacity");
                }
                n += 1;
            }
        }
        if n != self.used {
            return Err("slab use count doesn't match bitmap");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    const SECRET: u64 = 0x1234_5678_9abc_def0;
    const PAGE: u64 = 0x7000;

    #[test]
    fn classes_and_capacities_by_hand() {
        // first object = 64 rounded up to the class size:
        //   16, 32, 64 -> 64; 128 -> 128; 256 -> 256; ... 2048 -> 2048.
        // capacity = (4096 - first) / size:
        //   16: 4032/16 = 252   32: 4032/32 = 126   64: 4032/64 = 63
        //   128: 3968/128 = 31  256: 3840/256 = 15  512: 3584/512 = 7
        //   1024: 3072/1024 = 3 2048: 2048/2048 = 1
        let firsts: Vec<usize> = (0..8).map(first_object).collect();
        assert_eq!(firsts, [64, 64, 64, 128, 256, 512, 1024, 2048]);
        let caps: Vec<usize> = (0..8).map(capacity).collect();
        assert_eq!(caps, [252, 126, 63, 31, 15, 7, 3, 1]);
        // The bitmap (256 bits) covers the biggest capacity.
        assert!(caps.iter().all(|&c| c <= 256));
    }

    #[test]
    fn full_and_empty_by_hand() {
        // Class 7 (2048 bytes) holds one object; class 6 (1024) holds three.
        let mut h = SlabHeader::new(PAGE, 7, SECRET);
        assert!(h.is_empty() && !h.is_full());
        let off = h.alloc().unwrap();
        assert!(!h.is_empty() && h.is_full());
        h.free(PAGE, off, SECRET).unwrap();
        assert!(h.is_empty() && !h.is_full());
        let mut h = SlabHeader::new(PAGE, 6, SECRET);
        h.alloc().unwrap();
        h.alloc().unwrap();
        assert!(!h.is_empty() && !h.is_full());
        h.alloc().unwrap();
        assert!(h.is_full());
    }

    #[test]
    fn header_validity_by_hand() {
        let mut h = SlabHeader::new(PAGE, 7, SECRET);
        assert!(h.valid(PAGE, SECRET));
        assert!(!h.valid(PAGE + 4096, SECRET));
        assert!(!h.valid(PAGE, SECRET ^ 1));
        // A class number one past the last class is refused, even with the
        // right magic (it would index past CLASSES).
        h.class = CLASSES.len() as u16;
        assert!(!h.valid(PAGE, SECRET));
        h.class = CLASSES.len() as u16 - 1;
        assert!(h.valid(PAGE, SECRET));
    }

    #[test]
    fn first_object_is_a_multiple_of_its_class() {
        // free() relies on this to check alignment with the offset alone.
        for (c, size) in CLASSES.iter().enumerate() {
            assert_eq!(first_object(c) % size, 0);
        }
    }

    #[test]
    fn choosing_a_class() {
        assert_eq!(class_for(0, 1), Some(0));
        assert_eq!(class_for(1, 1), Some(0));
        assert_eq!(class_for(16, 8), Some(0));
        assert_eq!(class_for(17, 8), Some(1));
        assert_eq!(class_for(8, 64), Some(2)); // alignment wins
        assert_eq!(class_for(2048, 8), Some(7));
        assert_eq!(class_for(2049, 8), None);
        assert_eq!(class_for(8, 4096), None);
        // Large orders: pages needed, rounded up to a power of two.
        assert_eq!(large_order(2049, 8), 0);
        assert_eq!(large_order(4096, 8), 0);
        assert_eq!(large_order(4097, 8), 1);
        assert_eq!(large_order(3 * 4096, 8), 2);
        assert_eq!(large_order(8, 16384), 2);
        assert_eq!(large_order(1 << 30, 8), 18);
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn objects_are_aligned_distinct_and_inside_the_page() {
        for c in 0..8 {
            let mut h = SlabHeader::new(PAGE, c, SECRET);
            let mut seen = BTreeSet::new();
            while let Some(off) = h.alloc() {
                assert_eq!(off % CLASSES[c], 0);
                assert!(off >= HEADER_BYTES && off + CLASSES[c] <= SLAB_BYTES);
                assert!(seen.insert(off));
            }
            assert_eq!(seen.len(), capacity(c));
            assert!(h.is_full());
            h.check(PAGE, SECRET).unwrap();
            for off in &seen {
                h.free(PAGE, *off, SECRET).unwrap();
            }
            assert!(h.is_empty());
            h.check(PAGE, SECRET).unwrap();
        }
    }

    #[test]
    fn bad_frees_are_refused_and_change_nothing() {
        let mut h = SlabHeader::new(PAGE, 2, SECRET); // 64-byte objects
        let a = h.alloc().unwrap();
        assert_eq!(a, 64);
        let before = h;
        assert_eq!(h.free(PAGE, a + 8, SECRET), Err(SlabError::Misaligned));
        assert_eq!(h.free(PAGE, 0, SECRET), Err(SlabError::Misaligned));
        assert_eq!(h.free(PAGE, 32, SECRET), Err(SlabError::Misaligned));
        assert_eq!(h.free(PAGE, 4096, SECRET), Err(SlabError::Misaligned));
        assert_eq!(h.free(PAGE, a + 64, SECRET), Err(SlabError::NotAllocated));
        // Wrong page or wrong secret: the magic doesn't match.
        assert_eq!(h.free(PAGE + 4096, a, SECRET), Err(SlabError::BadMagic));
        assert_eq!(h.free(PAGE, a, SECRET ^ 1), Err(SlabError::BadMagic));
        assert_eq!(h, before);
        h.free(PAGE, a, SECRET).unwrap();
        assert_eq!(h.free(PAGE, a, SECRET), Err(SlabError::NotAllocated));
        // The last object of a class: capacity - 1 is fine, capacity is not.
        let mut h = SlabHeader::new(PAGE, 3, SECRET); // 128: first 128, cap 31
        let offs: Vec<usize> = std::iter::from_fn(|| h.alloc()).collect();
        assert_eq!(*offs.last().unwrap(), 128 + 30 * 128);
        assert_eq!(
            h.free(PAGE, 128 + 31 * 128, SECRET),
            Err(SlabError::Misaligned)
        );
    }

    #[test]
    fn check_finds_corruption() {
        let mut h = SlabHeader::new(PAGE, 0, SECRET);
        h.alloc().unwrap();
        h.check(PAGE, SECRET).unwrap();
        let mut bad = h;
        bad.used = 2;
        assert_eq!(
            bad.check(PAGE, SECRET),
            Err("slab use count doesn't match bitmap")
        );
        let mut bad = h;
        bad.bitmap[3] |= 1 << 63; // object 255, past capacity 252
        assert_eq!(
            bad.check(PAGE, SECRET),
            Err("slab bitmap bit past capacity")
        );
        let mut bad = h;
        bad.class = 9;
        assert_eq!(bad.check(PAGE, SECRET), Err("slab header magic wrong"));
        assert_eq!(h.check(PAGE ^ 4096, SECRET), Err("slab header magic wrong"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::test_cases(3000)))]
        /// Random alloc/free sequences against a set model.
        #[test]
        fn random_ops_match_model(class in 0usize..8, ops in prop::collection::vec((0u8..3, 0usize..4096), 1..600)) {
            let mut h = SlabHeader::new(PAGE, class, SECRET);
            let mut live = BTreeSet::new();
            for (kind, x) in ops {
                // Kind 2 frees a live object (when there is one); kind 1 an
                // arbitrary offset.
                let x = match (kind, live.iter().nth(x % live.len().max(1))) {
                    (2, Some(&o)) => o,
                    _ => x,
                };
                if kind == 0 {
                    match h.alloc() {
                        Some(off) => {
                            prop_assert!(live.insert(off));
                            prop_assert_eq!(off % CLASSES[class], 0);
                        }
                        None => prop_assert_eq!(live.len(), capacity(class)),
                    }
                } else {
                    // Free an arbitrary offset: must succeed exactly when it
                    // is a live object.
                    let r = h.free(PAGE, x, SECRET);
                    if live.remove(&x) {
                        prop_assert_eq!(r, Ok(()));
                    } else {
                        prop_assert!(r.is_err());
                    }
                }
                prop_assert_eq!(h.used(), live.len());
                prop_assert_eq!(h.check(PAGE, SECRET), Ok(()));
            }
        }
    }
}
