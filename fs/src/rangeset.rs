//! A set of block numbers stored as disjoint, non-adjacent ranges.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// start -> end (exclusive). Ranges never overlap or touch.
    map: BTreeMap<u64, u64>,
    count: u64,
}

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of blocks in the set.
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn ranges(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.map.iter().map(|(&s, &e)| (s, e - s))
    }
    pub fn range_count(&self) -> usize {
        self.map.len()
    }

    /// Adds `[start, start+len)`. Returns false (and changes nothing) if any
    /// part was already present.
    pub fn insert(&mut self, start: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        let end = start + len;
        if self.overlaps(start, len) {
            return false;
        }
        let mut s = start;
        let mut e = end;
        if let Some((&ps, &pe)) = self.map.range(..=start).next_back()
            && pe == start
        {
            s = ps;
            self.map.remove(&ps);
        }
        if let Some(&ne) = self.map.get(&end) {
            e = ne;
            self.map.remove(&end);
        }
        self.map.insert(s, e);
        self.count += len;
        true
    }

    /// Removes `[start, start+len)`; every block must be present.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn remove(&mut self, start: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        let end = start + len;
        let Some((&ps, &pe)) = self.map.range(..=start).next_back() else {
            return false;
        };
        if pe < end {
            return false;
        }
        self.map.remove(&ps);
        if ps < start {
            self.map.insert(ps, start);
        }
        if end < pe {
            self.map.insert(end, pe);
        }
        self.count -= len;
        true
    }

    pub fn overlaps(&self, start: u64, len: u64) -> bool {
        let end = start + len;
        if let Some((_, &e)) = self.map.range(..=start).next_back()
            && e > start
        {
            return true;
        }
        self.map.range(start..end).next().is_some()
    }

    /// First block `>= b` that is in the set and the end of its range.
    pub fn next_range_from(&self, b: u64) -> Option<(u64, u64)> {
        if let Some((&s, &e)) = self.map.range(..=b).next_back()
            && e > b
        {
            return Some((s.max(b), e));
        }
        self.map.range(b..).next().map(|(&s, &e)| (s, e))
    }

    pub fn to_vec(&self) -> Vec<(u64, u64)> {
        self.ranges().collect()
    }
}
