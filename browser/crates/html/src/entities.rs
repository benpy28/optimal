//! Named character references (`&amp;`, `&notin;`, ...).
//!
//! The table is generated from the WHATWG `entities.json` (see
//! `tools/gen_entities.py`). Matching is longest-prefix over a sorted table:
//! each further character narrows a range of candidate names by binary
//! search, so a lookup costs O(name length x log 2231) with no allocation and
//! no extra memory beyond the table itself.

#[path = "entities_table.rs"]
mod table;

pub use table::{ENTITIES, LONGEST_NAME};

/// The code points a named reference expands to (one or two).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expansion {
    /// First code point.
    pub first: u32,
    /// Second code point, or 0 when there is only one.
    pub second: u32,
}

/// An incremental longest-prefix matcher. Feed it the characters after `&`
/// one at a time; it remembers the longest full name seen so far.
#[derive(Clone, Debug)]
pub struct Matcher {
    lo: usize,
    hi: usize,
    depth: usize,
    best: Option<(usize, usize)>,
}

impl Default for Matcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Matcher {
    /// A matcher that has seen nothing yet.
    pub const fn new() -> Self {
        Matcher {
            lo: 0,
            hi: ENTITIES.len(),
            depth: 0,
            best: None,
        }
    }

    /// Feed the next byte. Returns `false` once no name can match any longer,
    /// after which further bytes must not be fed.
    pub fn feed(&mut self, b: u8) -> bool {
        let d = self.depth;
        let slice = &ENTITIES[self.lo..self.hi];
        // Names in [lo, hi) all share the first `d` bytes, so they are sorted
        // by byte `d` (shorter names, which have no byte `d`, come first).
        let start = slice.partition_point(|(n, _, _)| n.len() <= d || n[d] < b);
        let end = slice.partition_point(|(n, _, _)| n.len() <= d || n[d] <= b);
        self.hi = self.lo + end;
        self.lo += start;
        self.depth += 1;
        if self.lo == self.hi {
            return false;
        }
        if ENTITIES[self.lo].0.len() == self.depth {
            self.best = Some((self.lo, self.depth));
        }
        true
    }

    /// The longest full name matched so far: its length in bytes and its
    /// expansion.
    pub fn best(&self) -> Option<(usize, Expansion)> {
        self.best.map(|(i, len)| {
            let (_, first, second) = ENTITIES[i];
            (len, Expansion { first, second })
        })
    }
}

/// Look up a whole name (without `&`). For tests and tools.
pub fn lookup(name: &[u8]) -> Option<Expansion> {
    ENTITIES
        .binary_search_by(|(n, _, _)| (*n).cmp(name))
        .ok()
        .map(|i| Expansion {
            first: ENTITIES[i].1,
            second: ENTITIES[i].2,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        assert!(ENTITIES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(
            LONGEST_NAME,
            ENTITIES.iter().map(|e| e.0.len()).max().unwrap()
        );
    }

    #[test]
    fn table_matches_whatwg_json() {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../data/entities.json")).unwrap();
        let map = json.as_object().unwrap();
        assert_eq!(map.len(), ENTITIES.len());
        for (name, v) in map {
            let cps: Vec<u32> = v["codepoints"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_u64().unwrap() as u32)
                .collect();
            let e = lookup(&name.as_bytes()[1..]).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(e.first, cps[0], "{name}");
            assert_eq!(e.second, cps.get(1).copied().unwrap_or(0), "{name}");
        }
    }

    fn longest(s: &str) -> Option<(usize, Expansion)> {
        let mut m = Matcher::new();
        for &b in s.as_bytes() {
            if !m.feed(b) {
                break;
            }
        }
        m.best()
    }

    /// Hand-worked: "notit;" matches "not" (3 bytes, U+00AC); "notin;" matches
    /// the whole thing (U+2209). These are the spec's own examples.
    #[test]
    fn longest_prefix_examples_from_the_spec() {
        assert_eq!(
            longest("notit;"),
            Some((
                3,
                Expansion {
                    first: 0xAC,
                    second: 0
                }
            ))
        );
        assert_eq!(
            longest("notin;"),
            Some((
                6,
                Expansion {
                    first: 0x2209,
                    second: 0
                }
            ))
        );
        assert_eq!(
            longest("amp"),
            Some((
                3,
                Expansion {
                    first: 0x26,
                    second: 0
                }
            ))
        );
        assert_eq!(
            longest("amp;x"),
            Some((
                4,
                Expansion {
                    first: 0x26,
                    second: 0
                }
            ))
        );
        assert_eq!(
            longest("ampx"),
            Some((
                3,
                Expansion {
                    first: 0x26,
                    second: 0
                }
            ))
        );
        assert_eq!(longest("xyzzy"), None);
        assert_eq!(
            longest("noti"),
            Some((
                3,
                Expansion {
                    first: 0xAC,
                    second: 0
                }
            ))
        );
        // A two-code-point reference: &NotEqualTilde; is U+2242 U+0338.
        assert_eq!(
            longest("NotEqualTilde;"),
            Some((
                14,
                Expansion {
                    first: 0x2242,
                    second: 0x338
                }
            ))
        );
    }

    #[test]
    fn every_name_matches_itself_exactly() {
        for &(name, first, second) in ENTITIES.iter() {
            let mut m = Matcher::new();
            for &b in name {
                assert!(m.feed(b));
            }
            assert_eq!(m.best(), Some((name.len(), Expansion { first, second })));
        }
    }

    /// Reference model: the longest table name that is a prefix of the input.
    #[test]
    fn matcher_agrees_with_brute_force_on_every_prefix_extension() {
        for &(name, _, _) in ENTITIES.iter() {
            for extra in [b"" as &[u8], b";", b"x", b"=", b"9;"] {
                let mut input = name.to_vec();
                input.extend_from_slice(extra);
                let brute = ENTITIES
                    .iter()
                    .filter(|(n, _, _)| input.starts_with(n))
                    .max_by_key(|(n, _, _)| n.len())
                    .map(|&(n, a, b)| {
                        (
                            n.len(),
                            Expansion {
                                first: a,
                                second: b,
                            },
                        )
                    });
                let mut m = Matcher::new();
                for &b in &input {
                    if !m.feed(b) {
                        break;
                    }
                }
                assert_eq!(m.best(), brute, "{}", String::from_utf8_lossy(&input));
            }
        }
    }
}
