//! The stack of open elements, indexed so the tree builder's questions about
//! it cost O(log n) at any depth.
//!
//! The standard answers "has a p element in button scope", "which element
//! does this end tag close" and "where is the furthest block" by walking the
//! stack from the top. Done literally, a page with deep nesting pays
//! O(depth) per tag, which makes parsing quadratic: 20,000 nested `<div>`s
//! took seconds, and 25,000 open `<b>`s followed by stray `</i>`s took 2.6 s
//! (51 s at four times the size). Pages can be hostile, so the parser stays
//! linear instead.
//!
//! Every entry gets an order key, increasing up the stack. Next to the
//! stack, sorted key lists record where each element name is open and where
//! each group that the walks stop at (scope boundaries, special elements)
//! is. A walk's answer is then a comparison of two keys: "X is in scope" is
//! "the topmost X is at or above the topmost boundary". The adoption agency
//! inserts in the middle of the stack, so a new entry takes a key halfway
//! between its neighbours' keys, and all keys are renumbered on the rare
//! occasion that no key is left between two neighbours.
//!
//! Design choice (logged): order keys and sorted per-name key lists.
//! Rejected: capping tree depth as some engines do (it changes the tree, so
//! it is not the standard's algorithm), and plain stack indexes (a middle
//! insertion would shift every index above it).

use optimal_dom::{LocalName, Namespace, NodeId, local};
use std::collections::HashMap;
use std::ops::Deref;

/// The spacing of keys given to pushed entries: room for 64 halvings.
const STEP: u128 = 1 << 64;

/// The groups of elements that walks stop at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Group {
    /// Boundaries of the default scope (and part of every scope but table).
    Default = 0,
    /// `ol`, `ul`: also boundaries of list item scope.
    List = 1,
    /// `button`: also a boundary of button scope.
    Button = 2,
    /// Boundaries of table scope: `html`, `table`, `template`.
    Table = 3,
    /// The special category.
    Special = 4,
}

const GROUPS: usize = 5;

/// Which scope a question is asked in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// "in scope".
    Default,
    /// "in list item scope".
    ListItem,
    /// "in button scope".
    Button,
    /// "in table scope".
    Table,
}

impl Scope {
    fn groups(self) -> &'static [Group] {
        match self {
            Scope::Default => &[Group::Default],
            Scope::ListItem => &[Group::Default, Group::List],
            Scope::Button => &[Group::Default, Group::Button],
            Scope::Table => &[Group::Table],
        }
    }
}

/// Is the element in the standard's default scope boundary list?
pub(crate) fn is_default_boundary((ns, name): (Namespace, LocalName)) -> bool {
    match ns {
        Namespace::Html => matches!(
            name,
            local::APPLET
                | local::CAPTION
                | local::HTML
                | local::TABLE
                | local::TD
                | local::TH
                | local::MARQUEE
                | local::OBJECT
                | local::SELECT
                | local::TEMPLATE
        ),
        Namespace::MathMl => matches!(
            name,
            local::MI | local::MO | local::MN | local::MS | local::MTEXT | local::ANNOTATION_XML
        ),
        Namespace::Svg => matches!(name, local::FOREIGN_OBJECT | local::DESC | local::TITLE),
        _ => false,
    }
}

fn in_group(g: Group, nn: (Namespace, LocalName)) -> bool {
    let html = |names: &[LocalName]| nn.0 == Namespace::Html && names.contains(&nn.1);
    match g {
        Group::Default => is_default_boundary(nn),
        Group::List => html(&[local::OL, local::UL]),
        Group::Button => html(&[local::BUTTON]),
        Group::Table => html(&[local::HTML, local::TABLE, local::TEMPLATE]),
        Group::Special => is_special(nn),
    }
}

/// The standard's special category.
pub(crate) fn is_special((ns, name): (Namespace, LocalName)) -> bool {
    match ns {
        Namespace::Html => matches!(
            name,
            local::ADDRESS
                | local::APPLET
                | local::AREA
                | local::ARTICLE
                | local::ASIDE
                | local::BASE
                | local::BASEFONT
                | local::BGSOUND
                | local::BLOCKQUOTE
                | local::BODY
                | local::BR
                | local::BUTTON
                | local::CAPTION
                | local::CENTER
                | local::COL
                | local::COLGROUP
                | local::DD
                | local::DETAILS
                | local::DIR
                | local::DIV
                | local::DL
                | local::DT
                | local::EMBED
                | local::FIELDSET
                | local::FIGCAPTION
                | local::FIGURE
                | local::FOOTER
                | local::FORM
                | local::FRAME
                | local::FRAMESET
                | local::H1
                | local::H2
                | local::H3
                | local::H4
                | local::H5
                | local::H6
                | local::HEAD
                | local::HEADER
                | local::HGROUP
                | local::HR
                | local::HTML
                | local::IFRAME
                | local::IMG
                | local::INPUT
                | local::KEYGEN
                | local::LI
                | local::LINK
                | local::LISTING
                | local::MAIN
                | local::MARQUEE
                | local::MENU
                | local::META
                | local::NAV
                | local::NOEMBED
                | local::NOFRAMES
                | local::NOSCRIPT
                | local::OBJECT
                | local::OL
                | local::P
                | local::PARAM
                | local::PLAINTEXT
                | local::PRE
                | local::SCRIPT
                | local::SEARCH
                | local::SECTION
                | local::SELECT
                | local::SOURCE
                | local::STYLE
                | local::SUMMARY
                | local::TABLE
                | local::TBODY
                | local::TD
                | local::TEMPLATE
                | local::TEXTAREA
                | local::TFOOT
                | local::TH
                | local::THEAD
                | local::TITLE
                | local::TR
                | local::TRACK
                | local::UL
                | local::WBR
                | local::XMP
        ),
        Namespace::MathMl => matches!(
            name,
            local::MI | local::MO | local::MN | local::MS | local::MTEXT | local::ANNOTATION_XML
        ),
        Namespace::Svg => matches!(name, local::FOREIGN_OBJECT | local::DESC | local::TITLE),
        _ => false,
    }
}

fn insert_sorted(v: &mut Vec<u128>, k: u128) {
    // Keys are unique, so the search always ends between two entries.
    let i = v.binary_search(&k).expect_err("key is not indexed yet");
    v.insert(i, k);
}

fn remove_sorted(v: &mut Vec<u128>, k: u128) {
    let i = v.binary_search(&k).expect("key is indexed");
    v.remove(i);
}

/// The stack of open elements.
#[derive(Debug, Default)]
pub(crate) struct OpenStack {
    nodes: Vec<NodeId>,
    names: Vec<(Namespace, LocalName)>,
    /// Strictly increasing up the stack.
    keys: Vec<u128>,
    /// For each name, the keys of the entries with it, ascending.
    by_name: HashMap<(Namespace, LocalName), Vec<u128>>,
    /// For each group, the keys of its entries, ascending.
    groups: [Vec<u128>; GROUPS],
    /// How many times each node (by arena index) is on the stack: 0 or 1,
    /// except that the head element is briefly pushed a second time.
    on_stack: Vec<u8>,
}

impl Deref for OpenStack {
    type Target = [NodeId];
    fn deref(&self) -> &[NodeId] {
        &self.nodes
    }
}

impl OpenStack {
    fn index_entry(&mut self, node: NodeId, nn: (Namespace, LocalName), key: u128) {
        insert_sorted(self.by_name.entry(nn).or_default(), key);
        for (g, list) in GROUP_LIST.iter().zip(&mut self.groups) {
            if in_group(*g, nn) {
                insert_sorted(list, key);
            }
        }
        let i = node.index();
        if i >= self.on_stack.len() {
            self.on_stack.resize(i + 1, 0);
        }
        self.on_stack[i] += 1;
    }

    fn unindex_entry(&mut self, node: NodeId, nn: (Namespace, LocalName), key: u128) {
        let list = self.by_name.get_mut(&nn).expect("name is indexed");
        remove_sorted(list, key);
        if list.is_empty() {
            self.by_name.remove(&nn);
        }
        for (g, list) in GROUP_LIST.iter().zip(&mut self.groups) {
            if in_group(*g, nn) {
                remove_sorted(list, key);
            }
        }
        self.on_stack[node.index()] -= 1;
    }

    /// Give every entry a fresh, evenly spaced key.
    fn renumber(&mut self) {
        for (i, k) in self.keys.iter_mut().enumerate() {
            *k = (i as u128 + 1) * STEP;
        }
        self.by_name.clear();
        self.groups = Default::default();
        self.on_stack.clear();
        for i in 0..self.nodes.len() {
            let (n, nn, k) = (self.nodes[i], self.names[i], self.keys[i]);
            self.index_entry(n, nn, k);
        }
    }

    /// Push an element with its namespace and local name.
    pub(crate) fn push(&mut self, node: NodeId, ns: Namespace, name: LocalName) {
        let key = self.keys.last().map_or(STEP, |&k| k + STEP);
        self.index_entry(node, (ns, name), key);
        self.nodes.push(node);
        self.names.push((ns, name));
        self.keys.push(key);
    }

    /// Pop the current node.
    pub(crate) fn pop(&mut self) -> Option<NodeId> {
        let n = self.nodes.pop()?;
        let nn = self.names.pop().expect("parallel stacks");
        let k = self.keys.pop().expect("parallel stacks");
        self.unindex_entry(n, nn, k);
        Some(n)
    }

    /// Remove the entry at `i`.
    pub(crate) fn remove(&mut self, i: usize) -> NodeId {
        let n = self.nodes.remove(i);
        let nn = self.names.remove(i);
        let k = self.keys.remove(i);
        self.unindex_entry(n, nn, k);
        n
    }

    /// Insert an entry at `i`.
    pub(crate) fn insert(&mut self, i: usize, node: NodeId, ns: Namespace, name: LocalName) {
        let below = |s: &Self| if i == 0 { 0 } else { s.keys[i - 1] };
        let above = |s: &Self| s.keys.get(i).copied().unwrap_or(below(s) + 2 * STEP);
        if above(self) - below(self) < 2 {
            self.renumber();
        }
        let key = below(self) + (above(self) - below(self)) / 2;
        self.index_entry(node, (ns, name), key);
        self.nodes.insert(i, node);
        self.names.insert(i, (ns, name));
        self.keys.insert(i, key);
    }

    /// Replace the entry at `i`.
    pub(crate) fn replace(&mut self, i: usize, node: NodeId, ns: Namespace, name: LocalName) {
        let k = self.keys[i];
        self.unindex_entry(self.nodes[i], self.names[i], k);
        self.index_entry(node, (ns, name), k);
        self.nodes[i] = node;
        self.names[i] = (ns, name);
    }

    /// Pop entries until `len` remain.
    #[cfg(test)]
    pub(crate) fn truncate(&mut self, len: usize) {
        while self.nodes.len() > len {
            self.pop();
        }
    }

    /// Pop everything.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.truncate(0);
    }

    fn index_of(&self, key: u128) -> usize {
        self.keys
            .binary_search(&key)
            .expect("indexed key is on the stack")
    }

    fn topmost_key(&self, ns: Namespace, name: LocalName) -> Option<u128> {
        self.by_name
            .get(&(ns, name))
            .and_then(|v| v.last().copied())
    }

    fn group_top(&self, g: Group) -> Option<u128> {
        self.groups[g as usize].last().copied()
    }

    fn scope_top(&self, scope: Scope) -> Option<u128> {
        scope
            .groups()
            .iter()
            .filter_map(|&g| self.group_top(g))
            .max()
    }

    /// Is an HTML element with this name open?
    #[inline]
    pub(crate) fn has_html(&self, name: LocalName) -> bool {
        self.by_name.contains_key(&(Namespace::Html, name))
    }

    /// "Has an element in scope": is an HTML element named one of `names`
    /// in `scope`? (The topmost one is at or above the topmost boundary.)
    pub(crate) fn any_in_scope(&self, names: &[LocalName], scope: Scope) -> bool {
        let Some(x) = names
            .iter()
            .filter_map(|&n| self.topmost_key(Namespace::Html, n))
            .max()
        else {
            return false;
        };
        self.scope_top(scope).is_none_or(|b| x >= b)
    }

    /// Is `node` in the default scope?
    pub(crate) fn node_in_scope(&self, node: NodeId, nn: (Namespace, LocalName)) -> bool {
        if !self.contains(node) {
            return false;
        }
        // The topmost entry for the node (it is on the stack once, except
        // the head element pushed again for a moment).
        let keys = &self.by_name[&nn];
        let key = keys
            .iter()
            .rev()
            .copied()
            .find(|&k| self.nodes[self.index_of(k)] == node)
            .expect("node is on the stack");
        self.scope_top(Scope::Default).is_none_or(|b| key >= b)
    }

    /// The element an end tag named `name` closes under "any other end tag":
    /// the topmost HTML element with that name, if no special element is
    /// above it. `None`: the tag is ignored.
    pub(crate) fn end_tag_target(&self, name: LocalName) -> Option<usize> {
        let x = self.topmost_key(Namespace::Html, name)?;
        self.group_top(Group::Special)
            .is_none_or(|s| x >= s)
            .then(|| self.index_of(x))
    }

    /// The index of the lowest special element above index `i`.
    pub(crate) fn special_above(&self, i: usize) -> Option<usize> {
        let k = self.keys[i];
        let g = &self.groups[Group::Special as usize];
        let j = g.partition_point(|&x| x <= k);
        g.get(j).map(|&x| self.index_of(x))
    }

    /// Is `node` on the stack? O(1).
    #[inline]
    pub(crate) fn contains(&self, node: NodeId) -> bool {
        self.on_stack.get(node.index()).is_some_and(|&c| c > 0)
    }

    /// Check the indexes against the entries (tests and debug builds).
    pub(crate) fn check(&self) {
        assert_eq!(self.nodes.len(), self.names.len());
        assert_eq!(self.nodes.len(), self.keys.len());
        assert!(
            self.keys.windows(2).all(|w| w[0] < w[1]),
            "keys not increasing"
        );
        let mut by_name: HashMap<(Namespace, LocalName), Vec<u128>> = HashMap::new();
        let mut groups: [Vec<u128>; GROUPS] = Default::default();
        let mut on = vec![0u8; self.on_stack.len()];
        for i in 0..self.nodes.len() {
            by_name.entry(self.names[i]).or_default().push(self.keys[i]);
            for (g, list) in GROUP_LIST.iter().zip(&mut groups) {
                if in_group(*g, self.names[i]) {
                    list.push(self.keys[i]);
                }
            }
            on[self.nodes[i].index()] += 1;
        }
        assert_eq!(by_name, self.by_name, "name index drifted");
        assert_eq!(groups, self.groups, "group index drifted");
        assert_eq!(on, self.on_stack, "on-stack flags drifted");
    }
}

const GROUP_LIST: [Group; GROUPS] = [
    Group::Default,
    Group::List,
    Group::Button,
    Group::Table,
    Group::Special,
];

#[cfg(test)]
mod tests {
    //! The reference model is the standard's walks done literally on a plain
    //! `Vec`.

    use super::*;
    use optimal_dom::Document;
    use proptest::prelude::*;

    const NAMES: &[(Namespace, LocalName)] = &[
        (Namespace::Html, local::P),
        (Namespace::Html, local::B),
        (Namespace::Html, local::DIV),
        (Namespace::Html, local::TABLE),
        (Namespace::Html, local::TD),
        (Namespace::Html, local::BUTTON),
        (Namespace::Html, local::UL),
        (Namespace::Html, local::LI),
        (Namespace::Html, local::TEMPLATE),
        (Namespace::Html, local::HTML),
        (Namespace::Svg, local::P),
        (Namespace::Svg, local::DESC),
        (Namespace::MathMl, local::MI),
    ];

    fn walk_in_scope(
        m: &[(NodeId, (Namespace, LocalName))],
        target: impl Fn(NodeId, (Namespace, LocalName)) -> bool,
        scope: Scope,
    ) -> bool {
        for &(n, nn) in m.iter().rev() {
            if target(n, nn) {
                return true;
            }
            if scope.groups().iter().any(|&g| in_group(g, nn)) {
                return false;
            }
        }
        false
    }

    fn walk_end_tag(m: &[(NodeId, (Namespace, LocalName))], name: LocalName) -> Option<usize> {
        for (i, &(_, nn)) in m.iter().enumerate().rev() {
            if nn == (Namespace::Html, name) {
                return Some(i);
            }
            if is_special(nn) {
                return None;
            }
        }
        None
    }

    #[derive(Debug, Clone)]
    enum Op {
        Push(usize),
        Pop,
        Remove(usize),
        Insert(usize, usize),
        Replace(usize, usize),
        Truncate(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        let n = NAMES.len();
        prop_oneof![
            6 => (0..n).prop_map(Op::Push),
            2 => Just(Op::Pop),
            1 => any::<usize>().prop_map(Op::Remove),
            2 => (any::<usize>(), 0..n).prop_map(|(i, k)| Op::Insert(i, k)),
            1 => (any::<usize>(), 0..n).prop_map(|(i, k)| Op::Replace(i, k)),
            1 => any::<usize>().prop_map(Op::Truncate),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(2048)
        ))]

        #[test]
        fn stack_answers_match_literal_walks(ops in prop::collection::vec(op(), 1..150)) {
            let mut doc = Document::new();
            let mut s = OpenStack::default();
            let mut m: Vec<(NodeId, (Namespace, LocalName))> = vec![];
            for o in ops {
                let mut fresh = |k: usize| {
                    let (ns, name) = NAMES[k];
                    (doc.create_element(ns, name, vec![]), (ns, name))
                };
                match o {
                    Op::Push(k) => {
                        let (n, nn) = fresh(k);
                        s.push(n, nn.0, nn.1);
                        m.push((n, nn));
                    }
                    Op::Pop => {
                        prop_assert_eq!(s.pop(), m.pop().map(|e| e.0));
                    }
                    Op::Remove(i) if !m.is_empty() => {
                        let i = i % m.len();
                        prop_assert_eq!(s.remove(i), m.remove(i).0);
                    }
                    Op::Insert(i, k) => {
                        let i = i % (m.len() + 1);
                        let (n, nn) = fresh(k);
                        s.insert(i, n, nn.0, nn.1);
                        m.insert(i, (n, nn));
                    }
                    Op::Replace(i, k) if !m.is_empty() => {
                        let i = i % m.len();
                        let (n, nn) = fresh(k);
                        s.replace(i, n, nn.0, nn.1);
                        m[i] = (n, nn);
                    }
                    Op::Truncate(i) => {
                        let i = i % (m.len() + 1);
                        s.truncate(i);
                        m.truncate(i);
                    }
                    _ => {}
                }
                s.check();
                prop_assert_eq!(&s[..], &m.iter().map(|e| e.0).collect::<Vec<_>>()[..]);
                for &(ns, name) in NAMES {
                    if ns != Namespace::Html {
                        continue;
                    }
                    prop_assert_eq!(s.has_html(name), m.iter().any(|e| e.1 == (ns, name)));
                    prop_assert_eq!(s.end_tag_target(name), walk_end_tag(&m, name));
                    for scope in [Scope::Default, Scope::ListItem, Scope::Button, Scope::Table] {
                        prop_assert_eq!(
                            s.any_in_scope(&[name], scope),
                            walk_in_scope(&m, |_, nn| nn == (ns, name), scope),
                            "{:?} in {:?}", name, scope
                        );
                    }
                }
                prop_assert_eq!(
                    s.any_in_scope(&[local::P, local::LI], Scope::Button),
                    walk_in_scope(&m, |_, nn| nn == (Namespace::Html, local::P) || nn == (Namespace::Html, local::LI), Scope::Button)
                );
                for (i, &(n, nn)) in m.iter().enumerate() {
                    prop_assert!(s.contains(n));
                    prop_assert_eq!(s.node_in_scope(n, nn), walk_in_scope(&m, |x, _| x == n, Scope::Default));
                    let want = (i + 1..m.len()).find(|&j| is_special(m[j].1));
                    prop_assert_eq!(s.special_above(i), want);
                }
            }
        }
    }

    #[test]
    fn inserting_into_one_gap_renumbers() {
        let mut doc = Document::new();
        let mut s = OpenStack::default();
        let a = doc.create_element(Namespace::Html, local::DIV, vec![]);
        let b = doc.create_element(Namespace::Html, local::P, vec![]);
        s.push(a, Namespace::Html, local::DIV);
        s.push(b, Namespace::Html, local::P);
        // Each insert halves the gap above `a`; 64 halvings use it up.
        let mut inserted = vec![];
        for _ in 0..200 {
            let n = doc.create_element(Namespace::Html, local::B, vec![]);
            s.insert(1, n, Namespace::Html, local::B);
            inserted.push(n);
            s.check();
        }
        assert_eq!(s.len(), 202);
        assert_eq!(s[0], a);
        assert_eq!(s[201], b);
        assert_eq!(s[1], *inserted.last().unwrap());
        assert!(s.any_in_scope(&[local::P], Scope::Default));
        assert_eq!(
            s.end_tag_target(local::B),
            None,
            "p is special and above every b"
        );
        s.pop();
        assert_eq!(s.end_tag_target(local::B), Some(200));
    }

    fn stack_of(doc: &mut Document, names: &[LocalName]) -> OpenStack {
        let mut s = OpenStack::default();
        for &n in names {
            let e = doc.create_element(Namespace::Html, n, vec![]);
            s.push(e, Namespace::Html, n);
        }
        s
    }

    /// Renumbering happens exactly when a gap is used up, never sooner: the
    /// gap above the bottom entry starts at 2^64 and each insert halves it,
    /// so the 65th insert is the first that finds no room.
    #[test]
    fn renumbers_only_when_the_gap_is_used_up() {
        let mut doc = Document::new();
        let mut s = stack_of(&mut doc, &[local::DIV, local::P]);
        let top_key = s.keys[1];
        for k in 1..=70 {
            let n = doc.create_element(Namespace::Html, local::B, vec![]);
            s.insert(1, n, Namespace::Html, local::B);
            let renumbered = *s.keys.last().unwrap() != top_key;
            assert_eq!(renumbered, k >= 65, "insert {k}");
            s.check();
        }
    }

    /// An insert at the top spaces its key just as a push would.
    #[test]
    fn insert_at_the_top_is_spaced_like_a_push() {
        let mut doc = Document::new();
        let mut s = stack_of(&mut doc, &[local::DIV]);
        let n = doc.create_element(Namespace::Html, local::P, vec![]);
        s.insert(1, n, Namespace::Html, local::P);
        assert_eq!(s.keys, [STEP, 2 * STEP]);
        let n = doc.create_element(Namespace::Html, local::B, vec![]);
        s.insert(0, n, Namespace::Html, local::B);
        assert_eq!(s.keys, [STEP / 2, STEP, 2 * STEP]);
    }

    /// Inserting at the bottom over and over renumbers from the bottom too;
    /// renumbered keys must leave room below the first entry.
    #[test]
    fn inserting_at_the_bottom_renumbers_with_room_below() {
        let mut doc = Document::new();
        let mut s = stack_of(&mut doc, &[local::DIV]);
        let mut want = vec![s[0]];
        for _ in 0..200 {
            let n = doc.create_element(Namespace::Html, local::B, vec![]);
            s.insert(0, n, Namespace::Html, local::B);
            want.insert(0, n);
            s.check();
        }
        assert_eq!(&s[..], &want[..]);
        assert!(s.keys[0] > 0);
    }

    #[test]
    fn clear_empties_every_index() {
        let mut doc = Document::new();
        let mut s = stack_of(
            &mut doc,
            &[local::HTML, local::BODY, local::TABLE, local::P],
        );
        let p = s[3];
        s.clear();
        assert!(s.is_empty());
        assert!(!s.contains(p));
        assert!(!s.has_html(local::P));
        assert!(!s.any_in_scope(&[local::P], Scope::Default));
        s.check();
    }

    #[test]
    fn special_foreign_elements() {
        for n in [
            local::MI,
            local::MO,
            local::MN,
            local::MS,
            local::MTEXT,
            local::ANNOTATION_XML,
        ] {
            assert!(is_special((Namespace::MathMl, n)));
            assert!(!is_special((Namespace::Svg, n)));
        }
        for n in [local::FOREIGN_OBJECT, local::DESC, local::TITLE] {
            assert!(is_special((Namespace::Svg, n)));
        }
        assert!(is_special((Namespace::Html, local::TITLE)));
        assert!(!is_special((Namespace::Svg, local::SVG)));
        assert!(!is_special((Namespace::MathMl, local::MATH)));
        assert!(!is_special((Namespace::None, local::DIV)));
    }

    /// check must notice an index that disagrees with the entries.
    #[test]
    #[should_panic]
    fn check_catches_a_stale_index() {
        let mut doc = Document::new();
        let mut s = stack_of(&mut doc, &[local::HTML, local::P]);
        s.by_name.clear();
        s.check();
    }
}
