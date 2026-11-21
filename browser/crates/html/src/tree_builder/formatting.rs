//! The list of active formatting elements, with the tree builder's questions
//! about it answered without walking it when the answer is "none".
//!
//! The standard asks three things of the list that, done literally, walk it
//! from the end to the last marker: the Noah's Ark clause on every push ("are
//! there already three matching entries?"), the `<a>` start tag ("is there an
//! `a` after the last marker?"), and the adoption agency ("which is the last
//! entry with this name?"). A page with many distinct open formatting
//! elements (`<b id=1><b id=2>...`) made each of those O(list length), so
//! parsing was quadratic: 20,000 such tags took about two seconds.
//!
//! Each segment of the list (the part after a marker) keeps two counts:
//! entries per tag name, and entries per Noah's Ark key, a keyed hash of the
//! name and the attribute set. A question whose count is zero (or, for Noah's
//! Ark, under three) is answered without a walk. When a walk is needed,
//! entries are compared exactly, so a hash collision can only cost a walk,
//! never change a result. The hash is keyed per parser ([`RandomState`]), so
//! a page cannot pick colliding attributes.
//!
//! Design choice (logged): counts per segment over the plain list. Rejected:
//! a linked list with per-name position indexes, which would make every
//! operation O(1) even for contrived interleavings, at the price of much more
//! intricate adoption agency bookmark handling. The walks left are bounded by
//! the distance to the entry they find and use.

use super::{Afe, TagTok};
use optimal_dom::{LocalName, NodeId};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::ops::Deref;

#[derive(Debug, Default)]
struct Segment {
    names: HashMap<LocalName, u32>,
    keys: HashMap<u64, u32>,
}

impl Segment {
    fn add(&mut self, name: LocalName, key: u64) {
        *self.names.entry(name).or_default() += 1;
        *self.keys.entry(key).or_default() += 1;
    }

    fn sub(&mut self, name: LocalName, key: u64) {
        dec(&mut self.names, name);
        dec(&mut self.keys, key);
    }
}

fn dec<K: std::hash::Hash + Eq>(map: &mut HashMap<K, u32>, k: K) {
    let c = map.get_mut(&k).expect("entry was counted");
    *c -= 1;
    if *c == 0 {
        map.remove(&k);
    }
}

/// One entry's bookkeeping: its key and the segment it was counted in.
#[derive(Clone, Copy, Debug)]
struct Meta {
    key: u64,
    segment: u32,
}

/// The list of active formatting elements.
#[derive(Debug)]
///
/// `S` is the Noah's Ark key hasher. The parser uses a per-parser
/// [`RandomState`]; tests also use one that makes every key collide, to check
/// that a collision never changes a result.
pub(super) struct FormattingList<S = RandomState> {
    entries: Vec<Afe>,
    meta: Vec<Meta>,
    /// Counts for the part of the list after each marker; `segments[0]` is
    /// the part before the first marker.
    segments: Vec<Segment>,
    /// The nodes of the element entries.
    nodes: HashSet<NodeId>,
    hasher: S,
    /// Entries looked at by Noah's Ark walks, for the tests that check a walk
    /// stops as soon as it has seen every entry with the key.
    #[cfg(test)]
    walked: usize,
}

impl Default for FormattingList {
    fn default() -> Self {
        Self::with_hasher(RandomState::new())
    }
}

impl<S> Deref for FormattingList<S> {
    type Target = [Afe];
    fn deref(&self) -> &[Afe] {
        &self.entries
    }
}

/// Do two tags have the same attributes, as sets?
pub(super) fn same_attributes(a: &TagTok, b: &TagTok) -> bool {
    a.attrs.len() == b.attrs.len() && sorted_attrs(a) == sorted_attrs(b)
}

type AttrKey<'a> = (optimal_dom::Namespace, LocalName, &'a [u8]);

fn sorted_attrs(t: &TagTok) -> Vec<AttrKey<'_>> {
    let mut v: Vec<AttrKey<'_>> = t
        .attrs
        .iter()
        .map(|a| (a.namespace, a.local, a.value.as_wtf8().as_bytes()))
        .collect();
    v.sort_unstable();
    v
}

impl<S: BuildHasher> FormattingList<S> {
    /// An empty list whose Noah's Ark keys come from `hasher`.
    pub(super) fn with_hasher(hasher: S) -> Self {
        FormattingList {
            entries: Vec::new(),
            meta: Vec::new(),
            segments: vec![Segment::default()],
            nodes: HashSet::new(),
            hasher,
            #[cfg(test)]
            walked: 0,
        }
    }

    /// The Noah's Ark key of a tag: a keyed hash of its name and attribute set.
    fn key(&self, tok: &TagTok) -> u64 {
        self.hasher.hash_one((tok.name, sorted_attrs(tok)))
    }

    fn top_index(&self) -> u32 {
        (self.segments.len() - 1) as u32
    }

    fn uncount(&mut self, i: usize) {
        if let Afe::El(n, t) = &self.entries[i] {
            self.nodes.remove(n);
            let m = self.meta[i];
            self.segments[m.segment as usize].sub(t.name, m.key);
        }
    }

    /// Push a marker.
    pub(super) fn push_marker(&mut self) {
        self.entries.push(Afe::Marker);
        self.meta.push(Meta {
            key: 0,
            segment: self.top_index(),
        });
        self.segments.push(Segment::default());
    }

    /// Push an element, applying the Noah's Ark clause first: if three
    /// entries after the last marker already have the same name, namespace
    /// (always HTML here) and attributes, remove the earliest of them.
    pub(super) fn push(&mut self, node: NodeId, tok: TagTok) {
        let key = self.key(&tok);
        let top = self.top_index();
        let with_key = self.segments[top as usize]
            .keys
            .get(&key)
            .copied()
            .unwrap_or(0);
        if with_key >= 3 {
            // Walk back only until every entry with this key has been seen:
            // the walk ends at the earliest of them, not at the marker.
            let mut same = Vec::with_capacity(3);
            let mut seen = 0;
            for (i, e) in self.entries.iter().enumerate().rev() {
                if seen == with_key {
                    break;
                }
                #[cfg(test)]
                {
                    self.walked += 1;
                }
                match e {
                    Afe::Marker => break,
                    Afe::El(_, t) => {
                        if self.meta[i].key == key {
                            seen += 1;
                            if t.name == tok.name && same_attributes(t, &tok) {
                                same.push(i);
                            }
                        }
                    }
                }
            }
            if same.len() >= 3 {
                self.remove(*same.last().expect("three entries"));
            }
        }
        self.segments[top as usize].add(tok.name, key);
        self.nodes.insert(node);
        self.entries.push(Afe::El(node, tok));
        self.meta.push(Meta { key, segment: top });
    }

    /// Insert an element entry at `i` (the adoption agency's bookmark). The
    /// position is always after the last marker.
    pub(super) fn insert(&mut self, i: usize, node: NodeId, tok: TagTok) {
        let key = self.key(&tok);
        let top = self.top_index();
        self.segments[top as usize].add(tok.name, key);
        self.nodes.insert(node);
        self.entries.insert(i, Afe::El(node, tok));
        self.meta.insert(i, Meta { key, segment: top });
    }

    /// Remove the entry at `i`, which must be an element.
    pub(super) fn remove(&mut self, i: usize) {
        assert!(
            matches!(self.entries[i], Afe::El(..)),
            "only elements are removed singly"
        );
        self.uncount(i);
        self.entries.remove(i);
        self.meta.remove(i);
    }

    /// Point the element entry at `i` at a new node (same token).
    pub(super) fn set_node(&mut self, i: usize, node: NodeId) {
        match &mut self.entries[i] {
            Afe::El(n, _) => {
                self.nodes.remove(n);
                self.nodes.insert(node);
                *n = node;
            }
            Afe::Marker => panic!("set_node on a marker"),
        }
    }

    /// "Clear the list of active formatting elements up to the last marker".
    pub(super) fn clear_to_marker(&mut self) {
        while let Some(e) = self.entries.pop() {
            self.meta.pop();
            match e {
                Afe::Marker => {
                    self.segments.pop();
                    return;
                }
                Afe::El(n, _) => {
                    self.nodes.remove(&n);
                }
            }
        }
        // No marker: the whole list is gone.
        self.segments.truncate(1);
        self.segments[0] = Segment::default();
    }

    /// Is `node` in the list?
    #[inline]
    pub(super) fn contains_node(&self, node: NodeId) -> bool {
        self.nodes.contains(&node)
    }

    /// Is there an element entry named `name` after the last marker?
    #[inline]
    pub(super) fn has_name_after_marker(&self, name: LocalName) -> bool {
        self.segments[self.top_index() as usize]
            .names
            .contains_key(&name)
    }

    /// The index of the last element entry named `name` after the last
    /// marker.
    pub(super) fn last_named_after_marker(&self, name: LocalName) -> Option<usize> {
        if !self.has_name_after_marker(name) {
            return None;
        }
        for (i, e) in self.entries.iter().enumerate().rev() {
            match e {
                Afe::Marker => return None,
                Afe::El(_, t) if t.name == name => return Some(i),
                Afe::El(..) => {}
            }
        }
        None
    }

    /// Check every count against the entries (tests and debug builds).
    pub(super) fn check(&self) {
        let mut want = vec![Segment::default()];
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                Afe::Marker => want.push(Segment::default()),
                Afe::El(_, t) => {
                    assert_eq!(
                        self.meta[i].segment as usize,
                        want.len() - 1,
                        "entry in wrong segment"
                    );
                    assert_eq!(self.meta[i].key, self.key(t), "stale key");
                    want.last_mut().unwrap().add(t.name, self.meta[i].key);
                }
            }
        }
        assert_eq!(want.len(), self.segments.len(), "segment count drifted");
        for (w, s) in want.iter().zip(&self.segments) {
            assert_eq!(w.names, s.names, "name counts drifted");
            assert_eq!(w.keys, s.keys, "key counts drifted");
        }
        assert_eq!(self.entries.len(), self.meta.len());
        let nodes: HashSet<NodeId> = self
            .entries
            .iter()
            .filter_map(|e| match e {
                Afe::El(n, _) => Some(*n),
                Afe::Marker => None,
            })
            .collect();
        assert_eq!(nodes, self.nodes, "node set drifted");
        assert_eq!(
            nodes.len(),
            self.entries
                .iter()
                .filter(|e| matches!(e, Afe::El(..)))
                .count(),
            "an element is listed twice"
        );
    }
}

#[cfg(test)]
mod tests {
    //! The reference model is the standard's algorithm done literally on a
    //! plain `Vec`: every question walks back to the last marker.

    use super::*;
    use optimal_dom::{Attribute, Document, Namespace, local};
    use optimal_wtf8::Wtf8Buf;
    use proptest::prelude::*;

    const NAMES: [LocalName; 3] = [local::B, local::I, local::A];

    fn tok(name: usize, attrs: &[(usize, usize)]) -> TagTok {
        let locals = [local::ID, local::CLASS, local::TITLE];
        // Tokens never carry two attributes with one name.
        let mut seen = [false; 3];
        let attrs: Vec<(usize, usize)> = attrs
            .iter()
            .copied()
            .filter(|&(n, _)| !std::mem::replace(&mut seen[n % 3], true))
            .collect();
        TagTok {
            name: NAMES[name % 3],
            self_closing: false,
            ack: false,
            attrs: attrs
                .iter()
                .map(|&(n, v)| Attribute {
                    namespace: Namespace::None,
                    prefix: None,
                    local: locals[n % 3],
                    value: Wtf8Buf::from(["x", "y"][v % 2]),
                })
                .collect(),
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        Push(usize, Vec<(usize, usize)>),
        Marker,
        Clear,
        Remove(usize),
        SetNode(usize),
        Insert(usize, usize, Vec<(usize, usize)>),
    }

    fn op() -> impl Strategy<Value = Op> {
        let attrs = || prop::collection::vec((0usize..3, 0usize..2), 0..3);
        prop_oneof![
            6 => (0usize..3, attrs()).prop_map(|(n, a)| Op::Push(n, a)),
            1 => Just(Op::Marker),
            1 => Just(Op::Clear),
            2 => any::<usize>().prop_map(Op::Remove),
            1 => any::<usize>().prop_map(Op::SetNode),
            1 => (any::<usize>(), 0usize..3, attrs()).prop_map(|(i, n, a)| Op::Insert(i, n, a)),
        ]
    }

    fn model_same(a: &TagTok, b: &TagTok) -> bool {
        a.name == b.name
            && a.attrs.len() == b.attrs.len()
            && a.attrs.iter().all(|x| {
                b.attrs
                    .iter()
                    .any(|y| y.namespace == x.namespace && y.local == x.local && y.value == x.value)
            })
    }

    fn model_push(m: &mut Vec<Afe>, node: NodeId, t: TagTok) {
        let mut same = vec![];
        for (i, e) in m.iter().enumerate().rev() {
            match e {
                Afe::Marker => break,
                Afe::El(_, u) if model_same(u, &t) => same.push(i),
                Afe::El(..) => {}
            }
        }
        if same.len() >= 3 {
            m.remove(*same.last().unwrap());
        }
        m.push(Afe::El(node, t));
    }

    fn model_last_named(m: &[Afe], name: LocalName) -> Option<usize> {
        for (i, e) in m.iter().enumerate().rev() {
            match e {
                Afe::Marker => return None,
                Afe::El(_, t) if t.name == name => return Some(i),
                Afe::El(..) => {}
            }
        }
        None
    }

    /// Element entries after the last marker, by index.
    fn after_marker(m: &[Afe]) -> Vec<usize> {
        let start = m
            .iter()
            .rposition(|e| matches!(e, Afe::Marker))
            .map_or(0, |i| i + 1);
        (start..m.len()).collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(2048)
        ))]

        #[test]
        fn list_matches_literal_algorithm(ops in prop::collection::vec(op(), 1..120)) {
            check_against_model(FormattingList::default(), ops)?;
        }

        #[test]
        fn colliding_keys_never_change_a_result(ops in prop::collection::vec(op(), 1..120)) {
            check_against_model(FormattingList::with_hasher(Colliding), ops)?;
        }
    }

    /// A hasher that sends every key to the same value: every entry then
    /// collides with every other, so only the exact comparisons decide.
    #[derive(Clone, Default)]
    struct Colliding;

    impl BuildHasher for Colliding {
        type Hasher = ConstHasher;
        fn build_hasher(&self) -> ConstHasher {
            ConstHasher
        }
    }

    struct ConstHasher;

    impl std::hash::Hasher for ConstHasher {
        fn finish(&self) -> u64 {
            7
        }
        fn write(&mut self, _: &[u8]) {}
    }

    fn check_against_model<S: BuildHasher>(
        mut list: FormattingList<S>,
        ops: Vec<Op>,
    ) -> Result<(), TestCaseError> {
        let mut doc = Document::new();
        let mut model: Vec<Afe> = vec![];
        for o in ops {
            match o {
                Op::Push(n, a) => {
                    let node = doc.create_element(Namespace::Html, NAMES[n], vec![]);
                    list.push(node, tok(n, &a));
                    model_push(&mut model, node, tok(n, &a));
                }
                Op::Marker => {
                    list.push_marker();
                    model.push(Afe::Marker);
                }
                Op::Clear => {
                    list.clear_to_marker();
                    while let Some(e) = model.pop() {
                        if matches!(e, Afe::Marker) {
                            break;
                        }
                    }
                }
                Op::Remove(i) => {
                    // The parser only removes entries after the last marker.
                    let c = after_marker(&model);
                    if !c.is_empty() {
                        let i = c[i % c.len()];
                        list.remove(i);
                        model.remove(i);
                    }
                }
                Op::SetNode(i) => {
                    let c = after_marker(&model);
                    if !c.is_empty() {
                        let i = c[i % c.len()];
                        let node = doc.create_element(Namespace::Html, local::B, vec![]);
                        list.set_node(i, node);
                        if let Afe::El(n, _) = &mut model[i] {
                            *n = node;
                        }
                    }
                }
                Op::Insert(i, n, a) => {
                    let start = after_marker(&model).first().copied().unwrap_or(model.len());
                    let i = start + i % (model.len() - start + 1);
                    let node = doc.create_element(Namespace::Html, NAMES[n], vec![]);
                    list.insert(i, node, tok(n, &a));
                    model.insert(i, Afe::El(node, tok(n, &a)));
                }
            }
            list.check();
            prop_assert_eq!(&list[..], &model[..]);
            for name in NAMES {
                prop_assert_eq!(
                    list.last_named_after_marker(name),
                    model_last_named(&model, name)
                );
                prop_assert_eq!(
                    list.has_name_after_marker(name),
                    model_last_named(&model, name).is_some()
                );
            }
            for e in &model {
                if let Afe::El(n, _) = e {
                    prop_assert!(list.contains_node(*n));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn noahs_ark_keeps_three_and_ignores_attribute_order() {
        let mut doc = Document::new();
        let mut list = FormattingList::default();
        let mut nodes = vec![];
        for k in 0..5 {
            let n = doc.create_element(Namespace::Html, local::B, vec![]);
            nodes.push(n);
            // Same set, different order every other time.
            let attrs: &[(usize, usize)] = if k % 2 == 0 {
                &[(0, 0), (1, 1)]
            } else {
                &[(1, 1), (0, 0)]
            };
            list.push(n, tok(0, attrs));
        }
        list.check();
        let listed: Vec<NodeId> = list
            .iter()
            .map(|e| match e {
                Afe::El(n, _) => *n,
                Afe::Marker => unreachable!(),
            })
            .collect();
        assert_eq!(listed, nodes[2..].to_vec(), "the two earliest were removed");
        // A marker starts a new count.
        list.push_marker();
        let m = doc.create_element(Namespace::Html, local::B, vec![]);
        list.push(m, tok(0, &[(0, 0), (1, 1)]));
        assert_eq!(list.len(), 5);
        assert!(!list.contains_node(nodes[0]));
        assert_eq!(list.last_named_after_marker(local::B), Some(4));
        assert_eq!(list.last_named_after_marker(local::I), None);
        list.clear_to_marker();
        assert_eq!(list.len(), 3);
        assert_eq!(list.last_named_after_marker(local::B), Some(2));
        list.check();
    }

    /// The Noah's Ark walk stops once it has seen every entry with the key,
    /// so a long run of other entries below them costs nothing.
    #[test]
    fn noahs_ark_walk_stops_at_the_earliest_match() {
        let mut doc = Document::new();
        let mut list = FormattingList::default();
        for k in 0..1000 {
            let n = doc.create_element(Namespace::Html, local::I, vec![]);
            list.push(n, tok(1, &[(0, k % 2), (1, k / 2 % 2), (2, k / 4 % 2)]));
        }
        for _ in 0..3 {
            let n = doc.create_element(Namespace::Html, local::B, vec![]);
            list.push(n, tok(0, &[]));
        }
        list.walked = 0;
        let n = doc.create_element(Namespace::Html, local::B, vec![]);
        list.push(n, tok(0, &[]));
        assert_eq!(list.walked, 3, "looked at the three matching entries only");
        // Noah's Ark keeps three of each of the eight attribute sets of i,
        // and removed the earliest b.
        assert_eq!(list.len(), 8 * 3 + 3);
        list.check();
    }

    #[test]
    fn keys_depend_on_name_and_attribute_set_only() {
        let list = FormattingList::default();
        let k = |t: &TagTok| list.key(t);
        assert_eq!(k(&tok(0, &[(0, 0), (1, 1)])), k(&tok(0, &[(1, 1), (0, 0)])));
        // A keyed 64-bit hash: distinct tags collide with odds of about 2^-64.
        assert_ne!(k(&tok(0, &[])), k(&tok(1, &[])));
        assert_ne!(k(&tok(0, &[(0, 0)])), k(&tok(0, &[(0, 1)])));
        assert_ne!(k(&tok(0, &[(0, 0)])), k(&tok(0, &[])));
    }

    #[test]
    fn same_attributes_compares_sets() {
        assert!(same_attributes(
            &tok(0, &[(0, 0), (1, 1)]),
            &tok(2, &[(1, 1), (0, 0)])
        ));
        assert!(same_attributes(&tok(0, &[]), &tok(0, &[])));
        assert!(!same_attributes(&tok(0, &[(0, 0)]), &tok(0, &[(0, 1)])));
        assert!(!same_attributes(&tok(0, &[(0, 0)]), &tok(0, &[(1, 0)])));
        assert!(!same_attributes(
            &tok(0, &[(0, 0)]),
            &tok(0, &[(0, 0), (1, 0)])
        ));
    }

    /// check must notice a count that disagrees with the entries.
    #[test]
    #[should_panic]
    fn check_catches_a_wrong_count() {
        let mut doc = Document::new();
        let mut list = FormattingList::default();
        let n = doc.create_element(Namespace::Html, local::B, vec![]);
        list.push(n, tok(0, &[]));
        list.segments[0] = Segment::default();
        list.check();
    }
}
