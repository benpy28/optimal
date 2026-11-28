//! Matching selectors against elements (selectors-4 section 17, "Matching
//! Selectors"), with the case-sensitivity rules of WHATWG HTML 4.16.2
//! ("Case-sensitivity of selectors").
//!
//! The tree is reached through the [`Element`] trait, so this crate does not
//! depend on a particular DOM. What the host language defines (whether a
//! link is visited, whether a control is disabled, an element's content
//! language and directionality) comes from the same trait; everything that
//! Selectors defines (structure, attributes, `:is()`, `:has()`, `:nth-*()`,
//! `:scope`, `:lang()` range matching) is done here.
//!
//! Complex selectors are matched right to left without recursion on the
//! number of compound selectors (a selector can hold any number of them), and
//! a failure says how far back the search must restart, so that a run of
//! descendant combinators costs about the depth of the tree for each one,
//! not the depth to the power of their number: when a compound selector
//! can't be found among an element's ancestors, it can't be found among the
//! ancestors of that element's descendants either.
//!
//! Within one call to [`matches`] or [`query_all`], or one
//! [`MatchSession`], results that don't
//! depend on the subject are cached, so that matching every element of a
//! tree costs about its size, not the size times the depth or the length
//! of the sibling runs:
//! - the outcome of searching an element's ancestors (descendant
//!   combinator) or earlier siblings (subsequent-sibling combinator) for
//!   the rest of a selector, for each part of each selector;
//! - each element's index among its siblings, overall and among those of
//!   its type, and the length of both runs (`:nth-*()`, `:*-of-type`);
//! - for each `of S` argument, each sibling's index among the siblings
//!   that match `S`.
//!
//! Matching `:has()` is not cached yet: an element whose subtree is
//! searched for each of its ancestors costs the depth of the tree each
//! time.
//!
//! Not matched yet, because the DOM has no shadow trees: `:host`,
//! `:host()`, `:host-context()`, `:has-slotted`, `::slotted()` and
//! `::part()` never match. A selector whose subject is a pseudo-element
//! never matches an element ("match a selector against an element");
//! matching pseudo-elements comes with the cascade.

use crate::ast::*;
use crate::lang::lang_matches;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;

/// An element of the tree being matched: a cheap handle (an index, or a
/// reference), compared with `==` and hashed for identity. The tree must
/// not change during a call to [`matches`] or [`query_all`], or a
/// [`MatchSession`].
///
/// Names and values are bytes (UTF-8, or WTF-8 for a DOM that can hold lone
/// surrogates): every comparison Selectors makes is on code points, and
/// these encodings keep code point boundaries, so comparing bytes gives the
/// same answers.
pub trait Element: Clone + Eq + Hash {
    /// The parent, if it is an element.
    fn parent_element(&self) -> Option<Self>;
    /// Whether the parent is a document (for `:root`).
    fn is_root(&self) -> bool;
    /// The previous sibling that is an element.
    fn prev_sibling_element(&self) -> Option<Self>;
    /// The next sibling that is an element.
    fn next_sibling_element(&self) -> Option<Self>;
    /// The first child that is an element.
    fn first_element_child(&self) -> Option<Self>;
    /// Whether both have a parent node, and it is the same node.
    fn same_parent(&self, other: &Self) -> bool;
    /// Calls `f` with each child that is not an element and could affect
    /// `:empty`: `Some(data)` for a text node (or another content node, such
    /// as an entity reference), `None` for any other node except comments
    /// and processing instructions, which are left out. Stops when `f`
    /// returns `true`, and returns whether it did.
    fn any_non_element_child(&self, f: impl FnMut(Option<&[u8]>) -> bool) -> bool;

    /// Whether this is an HTML element in an HTML document (the HTML
    /// case-sensitivity rules apply to it).
    fn is_html_element_in_html_document(&self) -> bool;
    /// The local name.
    fn local_name(&self) -> &[u8];
    /// The namespace URL, empty for none.
    fn namespace(&self) -> &[u8];
    /// The value of the attribute in namespace `ns` (empty for none) with
    /// local name `local`, compared exactly.
    fn attribute(&self, ns: &[u8], local: &[u8]) -> Option<&[u8]>;
    /// Calls `f` with each attribute's namespace (empty for none), local
    /// name and value, until it returns `true`; returns whether it did.
    fn any_attribute(&self, f: impl FnMut(&[u8], &[u8], &[u8]) -> bool) -> bool;
    /// The element's ID (DOM: the `id` attribute in no namespace).
    fn id(&self) -> Option<&[u8]> {
        self.attribute(b"", b"id")
    }
    /// The element's class attribute (DOM: `class` in no namespace).
    fn class_attribute(&self) -> Option<&[u8]> {
        self.attribute(b"", b"class")
    }

    /// Whether the element matches a pseudo-class that the host language
    /// defines: the dynamic, link, target, input, media, display state and
    /// `:defined` pseudo-classes. Not asked for the ones Selectors defines
    /// (`:root`, `:empty`, `:scope`, the child-indexed and typed
    /// child-indexed ones) or the shadow tree ones.
    fn state(&self, pc: PseudoClassKeyword) -> bool;
    /// Whether the element's custom state set holds `name` (`:state()`).
    fn custom_state(&self, name: &str) -> bool;
    /// Calls `f` with the element's content language as a BCP 47 tag, empty
    /// when it isn't tagged (HTML: unknown).
    fn with_language<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R;
    /// The element's directionality.
    fn directionality(&self) -> Direction;
}

/// An element's directionality (`:dir()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Left to right.
    Ltr,
    /// Right to left.
    Rtl,
}

/// What `:scope` represents (8.4).
#[derive(Clone, Debug, PartialEq)]
pub enum Scope<E> {
    /// No scoping root: `:scope` is `:root`.
    None,
    /// This element is the scoping root.
    Element(E),
    /// A virtual scoping root: the document or document fragment at the
    /// root of a tree, which acts as the parent of its element children
    /// (those that share `first_child`'s parent). It is featureless: only `:scope` (and selectors made
    /// of it) matches it, and it can't be the subject. `first_child` is its
    /// first element child.
    Root {
        /// The first element child of the document or fragment.
        first_child: Option<E>,
    },
}

/// The inputs to matching besides the selector and the element.
#[derive(Clone, Debug)]
pub struct MatchContext<'c, E> {
    /// The scoping root.
    pub scope: Scope<E>,
    /// Whether the document is in quirks mode (IDs and classes then match
    /// ASCII case-insensitively).
    pub quirks: bool,
    /// For a nested style rule: the selectors of the rules it is nested in,
    /// outermost first, which `&` stands for. Empty outside nesting, where
    /// `&` is `:scope` (css-nesting-1).
    pub nesting: &'c [&'c SelectorList],
}

impl<E> MatchContext<'_, E> {
    /// A context with no scoping root, in no-quirks mode, outside nesting.
    pub fn new() -> Self {
        MatchContext {
            scope: Scope::None,
            quirks: false,
            nesting: &[],
        }
    }
}

impl<E> Default for MatchContext<'_, E> {
    fn default() -> Self {
        Self::new()
    }
}

/// "Match a selector against an element" (17.2): whether any selector in
/// `list` matches `element`. To match many elements, a [`MatchSession`]
/// shares the work between them.
pub fn matches<E: Element>(list: &SelectorList, element: &E, cx: &MatchContext<'_, E>) -> bool {
    MatchSession::new(cx).matches(list, element)
}

/// "Match a selector against a tree" (17.4) for a scoped selector, as the
/// DOM's `querySelectorAll()` does: the elements that are descendants of the
/// scoping root and match `list`, in tree order. The scope must be an element
/// or a virtual root.
pub fn query_all<E: Element>(list: &SelectorList, cx: &MatchContext<'_, E>) -> Vec<E> {
    let session = MatchSession::new(cx);
    let (first, stop) = match &cx.scope {
        Scope::Element(e) => (e.first_element_child(), Some(e.clone())),
        Scope::Root { first_child } => (first_child.clone(), None),
        Scope::None => panic!("query_all needs a scoping root"),
    };
    let mut out = Vec::new();
    let mut next = first;
    while let Some(e) = next {
        if session.matches(list, &e) {
            out.push(e.clone());
        }
        next = following_in_subtree(&e, stop.as_ref());
    }
    out
}

/// Matching many elements in one context, with the caches described in
/// the module documentation shared between them, so that matching a whole
/// tree costs about its size. The selector lists are borrowed for the
/// session's life (the caches know them by address), and the tree must not
/// change during it.
pub struct MatchSession<'s, E> {
    cx: &'s MatchContext<'s, E>,
    caches: Caches<E>,
}

impl<'s, E: Element> MatchSession<'s, E> {
    /// A session with empty caches.
    pub fn new(cx: &'s MatchContext<'s, E>) -> Self {
        MatchSession {
            cx,
            caches: Caches::default(),
        }
    }

    /// Whether any selector in `list` matches `element`, as [`matches`].
    pub fn matches(&self, list: &'s SelectorList, element: &E) -> bool {
        Matcher {
            cx: self.cx,
            nesting: self.cx.nesting,
            caches: &self.caches,
        }
        .list(list, &Node::El(element.clone()))
    }
}

/// The element after `e` in tree order, staying inside `root`'s subtree
/// (`None` for the whole tree).
fn following_in_subtree<E: Element>(e: &E, root: Option<&E>) -> Option<E> {
    if let Some(c) = e.first_element_child() {
        return Some(c);
    }
    let mut cur = e.clone();
    loop {
        if root == Some(&cur) {
            return None;
        }
        if let Some(s) = cur.next_sibling_element() {
            return Some(s);
        }
        cur = cur.parent_element()?;
        if root == Some(&cur) {
            return None;
        }
    }
}

/// An element, or the virtual scoping root.
#[derive(Clone, Debug, PartialEq)]
enum Node<E> {
    El(E),
    Root,
}

/// The outcome of matching part of a complex selector. A failure says
/// where the search may continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Res {
    Matched,
    /// The compound selector failed at this element only: an earlier
    /// sibling or further ancestor may still do.
    Local,
    /// No later sibling candidate can match either; only trying another
    /// element for the nearest descendant combinator to the right can help.
    RestartDescendant,
    /// Nothing can match for this subject.
    Global,
}

/// A has-anchor: the element a relative selector is matched from, and the
/// combinator that joins it to the first compound selector.
type Anchor<'a, E> = Option<(&'a Node<E>, Combinator)>;

struct Matcher<'c, E> {
    cx: &'c MatchContext<'c, E>,
    /// The rules `&` stands for at this point (a suffix of cx.nesting is
    /// dropped as `&` is followed outwards).
    nesting: &'c [&'c SelectorList],
    caches: &'c Caches<E>,
}

/// Where a selector sits during one call: its address (the selectors are
/// borrowed for the whole call, so no two share one) and how many nesting
/// rules `&` can still stand for, which is all its matching depends on
/// besides the element.
type SelKey = (usize, usize);

/// An element's 1-based index among the siblings that match an `of S`
/// argument, and how many match; `None` if it doesn't match.
type OfIndex = Option<(u32, u32)>;

/// An element's place among its siblings: 1-based indices from the start,
/// and the run lengths, overall and among the siblings of its type.
#[derive(Clone, Copy, Debug)]
struct Position {
    index: u32,
    count: u32,
    type_index: u32,
    type_count: u32,
}

/// The caches of one call (see the module documentation). Each holds
/// results that depend only on the tree and the selector, never on the
/// subject being matched.
struct Caches<E> {
    /// (selector, unit i, element) -> the result of the search that starts
    /// at the element where unit i matched and looks through its ancestors
    /// or earlier siblings (the combinator left of unit i) for the rest.
    search: RefCell<HashMap<(SelKey, usize, E), Res>>,
    position: RefCell<HashMap<E, Position>>,
    /// (`of S` list, element) -> its index from the start and the count
    /// among the siblings matching `S`, or `None` if it doesn't match.
    of: RefCell<HashMap<(SelKey, E), OfIndex>>,
}

impl<E> Default for Caches<E> {
    fn default() -> Self {
        Caches {
            search: RefCell::default(),
            position: RefCell::default(),
            of: RefCell::default(),
        }
    }
}

/// The elements in `e`'s run of siblings, in tree order.
fn sibling_run<E: Element>(e: &E) -> Vec<E> {
    let mut first = e.clone();
    while let Some(p) = first.prev_sibling_element() {
        first = p;
    }
    let mut run = vec![first];
    while let Some(n) = run.last().and_then(E::next_sibling_element) {
        run.push(n);
    }
    run
}

/// Selectors white space (selectors-4 section 3, "Characters and case
/// sensitivity"): also the DOM's ASCII whitespace.
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

/// Attributes whose values HTML compares ASCII case-insensitively on HTML
/// elements in HTML documents (HTML 4.16.2). Applied to attributes in no
/// namespace: the list names HTML's own attributes, which have none.
const CASE_INSENSITIVE_VALUES: &[&str] = &[
    "accept",
    "accept-charset",
    "align",
    "alink",
    "axis",
    "bgcolor",
    "charset",
    "checked",
    "clear",
    "codetype",
    "color",
    "compact",
    "declare",
    "defer",
    "dir",
    "direction",
    "disabled",
    "enctype",
    "face",
    "frame",
    "hreflang",
    "http-equiv",
    "lang",
    "language",
    "link",
    "media",
    "method",
    "multiple",
    "nohref",
    "noresize",
    "noshade",
    "nowrap",
    "readonly",
    "rel",
    "rev",
    "rules",
    "scope",
    "scrolling",
    "selected",
    "shape",
    "target",
    "text",
    "type",
    "valign",
    "valuetype",
    "vlink",
];

/// `sel`, ASCII-lowercased, is identical to `name`.
fn eq_lowered(sel: &[u8], name: &[u8]) -> bool {
    sel.len() == name.len()
        && sel
            .iter()
            .zip(name)
            .all(|(s, n)| s.to_ascii_lowercase() == *n)
}

fn eq(a: &[u8], b: &[u8], ascii_ci: bool) -> bool {
    if ascii_ci {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

impl<'c, E: Element> Matcher<'c, E> {
    fn list(&self, list: &SelectorList, node: &Node<E>) -> bool {
        list.0
            .iter()
            .any(|s| self.complex(s, node, None) == Res::Matched)
    }

    fn parent(&self, n: &Node<E>) -> Option<Node<E>> {
        match n {
            Node::El(e) => match e.parent_element() {
                Some(p) => Some(Node::El(p)),
                // A child of the virtual root, if it is the scoping root.
                None => match &self.cx.scope {
                    Scope::Root {
                        first_child: Some(f),
                    } if f.same_parent(e) => Some(Node::Root),
                    _ => None,
                },
            },
            Node::Root => None,
        }
    }

    fn prev_sibling(n: &Node<E>) -> Option<Node<E>> {
        match n {
            Node::El(e) => e.prev_sibling_element().map(Node::El),
            Node::Root => None,
        }
    }

    /// Whether `x` is joined to `anchor` by `c`, reading `anchor c x`.
    fn related(&self, anchor: &Node<E>, c: Combinator, x: &Node<E>) -> bool {
        match c {
            Combinator::Child => self.parent(x).as_ref() == Some(anchor),
            Combinator::NextSibling => Self::prev_sibling(x).as_ref() == Some(anchor),
            Combinator::Descendant => {
                let mut cur = self.parent(x);
                while let Some(p) = cur {
                    if &p == anchor {
                        return true;
                    }
                    cur = self.parent(&p);
                }
                false
            }
            Combinator::SubsequentSibling => {
                let mut cur = Self::prev_sibling(x);
                while let Some(p) = cur {
                    if &p == anchor {
                        return true;
                    }
                    cur = Self::prev_sibling(&p);
                }
                false
            }
        }
    }

    /// Match the complex selector `sel` with its subject at `node` ("match
    /// a complex selector against an element", 17.2), right to left. With an
    /// anchor, the leftmost compound selector must also be joined to it.
    fn complex(&self, sel: &Selector, node: &Node<E>, anchor: Anchor<'_, E>) -> Res {
        let unit = |i: usize| {
            if i == 0 {
                &sel.first
            } else {
                &sel.rest[i - 1].1
            }
        };
        let comb = |i: usize| sel.rest[i - 1].0;
        // Matching unit i at n: the compound selector, then for the
        // leftmost one the anchor.
        let at = |i: usize, n: &Node<E>| -> Option<bool> {
            if !self.unit(unit(i), n) {
                return Some(false);
            }
            if i == 0 {
                return Some(anchor.is_none_or(|(a, c)| self.related(a, c, n)));
            }
            None
        };
        let top = sel.rest.len();
        match at(top, node) {
            Some(true) => return Res::Matched,
            Some(false) => return Res::Local,
            None => {}
        }
        // Searches that walk (descendant and subsequent-sibling
        // combinators) are cached by the element they start from, outside
        // :has(), where the leftmost unit also depends on the anchor.
        let key = (std::ptr::from_ref(sel) as usize, self.nesting.len());
        let cached = |i: usize| {
            anchor.is_none()
                && matches!(
                    comb(i),
                    Combinator::Descendant | Combinator::SubsequentSibling
                )
        };
        let lookup = |i: usize, n: &Node<E>| match n {
            Node::El(e) if cached(i) => self
                .caches
                .search
                .borrow()
                .get(&(key, i, e.clone()))
                .copied(),
            _ => None,
        };
        if let Some(r) = lookup(top, node) {
            return r;
        }
        // stack[k] = (index of a unit that matched, the element it is now
        // searching from for the unit to its left, the elements it has
        // searched from, all of which share its result).
        let mut stack: Vec<(usize, Node<E>, Vec<E>)> = vec![(top, node.clone(), Vec::new())];
        // The result handed back to the frame on top of the stack.
        let mut back: Option<Res> = None;
        while let Some((i, cur, from)) = stack.last_mut() {
            let i = *i;
            let c = comb(i);
            if let Some(r) = back.take() {
                let out = match (r, c) {
                    (Res::Matched | Res::Global, _) | (_, Combinator::NextSibling) => Some(r),
                    (_, Combinator::Child) => Some(Res::RestartDescendant),
                    (Res::RestartDescendant, Combinator::SubsequentSibling) => Some(r),
                    // The search goes on from `cur`, as a search started
                    // there would.
                    _ => lookup(i, cur),
                };
                if let Some(out) = out {
                    self.finish(key, i, from, cached(i), out);
                    stack.pop();
                    back = Some(out);
                    continue;
                }
            }
            if cached(i)
                && let Node::El(e) = &*cur
            {
                from.push(e.clone());
            }
            let next = match c {
                Combinator::Descendant | Combinator::Child => self.parent(cur),
                Combinator::NextSibling | Combinator::SubsequentSibling => Self::prev_sibling(cur),
            };
            let Some(next) = next else {
                let out = match c {
                    Combinator::Descendant | Combinator::Child => Res::Global,
                    _ => Res::RestartDescendant,
                };
                self.finish(key, i, from, cached(i), out);
                back = Some(out);
                stack.pop();
                continue;
            };
            *cur = next.clone();
            let j = i - 1;
            match at(j, &next) {
                Some(true) => back = Some(Res::Matched),
                Some(false) => back = Some(Res::Local),
                None => match lookup(j, &next) {
                    Some(r) => back = Some(r),
                    None => stack.push((j, next, Vec::new())),
                },
            }
        }
        back.expect("the stack empties only with a result")
    }

    /// Record a finished search's result for every element it searched
    /// from.
    fn finish(&self, key: SelKey, i: usize, from: &mut Vec<E>, cached: bool, r: Res) {
        if cached {
            let mut search = self.caches.search.borrow_mut();
            for e in from.drain(..) {
                search.insert((key, i, e), r);
            }
        }
    }

    /// A `<complex-selector-unit>`: the compound selector, and no
    /// pseudo-elements (an element is never a pseudo-element).
    fn unit(&self, unit: &Unit, node: &Node<E>) -> bool {
        if !unit.pseudos.is_empty() {
            return false;
        }
        match node {
            Node::El(e) => unit.compound.iter().all(|s| self.simple(s, e)),
            // The virtual root is featureless: each simple selector must be
            // allowed to match it, and :has() only counts beside another.
            Node::Root => {
                unit.compound.iter().all(|s| self.simple_on_root(s))
                    && unit
                        .compound
                        .iter()
                        .any(|s| !matches!(s, Simple::PseudoClass(PseudoClass::Has(_))))
            }
        }
    }

    /// A simple selector against the featureless virtual root.
    fn simple_on_root(&self, s: &Simple) -> bool {
        match s {
            Simple::PseudoClass(PseudoClass::Keyword(PseudoClassKeyword::Scope)) => true,
            Simple::Nesting if self.nesting.is_empty() => true,
            Simple::Nesting => {
                let (last, outer) = self.nesting.split_last().expect("not empty");
                Matcher {
                    cx: self.cx,
                    nesting: outer,
                    caches: self.caches,
                }
                .list(last, &Node::Root)
            }
            Simple::PseudoClass(PseudoClass::Is(f) | PseudoClass::Where(f)) => {
                self.forgiving(f, &Node::Root)
            }
            Simple::PseudoClass(PseudoClass::Has(rel)) => self.has(rel, &Node::Root),
            // :not() can only match when its argument is allowed to match
            // and doesn't; every selector allowed to match the virtual root
            // does match it.
            _ => false,
        }
    }

    fn forgiving(&self, f: &Forgiving, node: &Node<E>) -> bool {
        f.0.iter().any(|item| match item {
            ForgivingItem::Valid(s) => self.complex(s, node, None) == Res::Matched,
            ForgivingItem::Invalid(_) => false,
        })
    }

    fn simple(&self, s: &Simple, e: &E) -> bool {
        match s {
            Simple::Type { ns, local, .. } => {
                Self::namespace(ns, e.namespace())
                    && if e.is_html_element_in_html_document() {
                        eq_lowered(local.as_bytes(), e.local_name())
                    } else {
                        local.as_bytes() == e.local_name()
                    }
            }
            Simple::Universal { ns, .. } => Self::namespace(ns, e.namespace()),
            Simple::Id(id) => e.id().is_some_and(|v| eq(v, id.as_bytes(), self.cx.quirks)),
            Simple::Class(c) => e.class_attribute().is_some_and(|v| {
                v.split(|&b| is_ws(b))
                    .any(|w| eq(w, c.as_bytes(), self.cx.quirks))
            }),
            Simple::Attribute(a) => Self::attribute(a, e),
            Simple::PseudoClass(pc) => self.pseudo_class(pc, e),
            Simple::Nesting => match self.nesting.split_last() {
                None => self.scope(e),
                Some((last, outer)) => Matcher {
                    cx: self.cx,
                    nesting: outer,
                    caches: self.caches,
                }
                .list(last, &Node::El(e.clone())),
            },
        }
    }

    fn namespace(ns: &Namespace, url: &[u8]) -> bool {
        match ns {
            Namespace::Any => true,
            Namespace::None => url.is_empty(),
            Namespace::Url(u) => u.as_bytes() == url,
        }
    }

    /// An attribute selector (section 6), with HTML's case rules.
    fn attribute(a: &Attribute, e: &E) -> bool {
        let html = e.is_html_element_in_html_document();
        let sel = a.local.as_bytes();
        e.any_attribute(|ns, local, value| {
            if !Self::namespace(&a.ns, ns) {
                return false;
            }
            let named = if html {
                eq_lowered(sel, local)
            } else {
                sel == local
            };
            if !named {
                return false;
            }
            let Some((m, v)) = &a.test else {
                return true;
            };
            let ci = match a.case {
                Some(AttrCase::Insensitive) => true,
                Some(AttrCase::Sensitive) => false,
                None => {
                    html && ns.is_empty()
                        && CASE_INSENSITIVE_VALUES
                            .iter()
                            .any(|n| n.as_bytes() == local)
                }
            };
            Self::value(*m, value, v.as_bytes(), ci)
        })
    }

    fn value(m: AttrMatcher, value: &[u8], v: &[u8], ci: bool) -> bool {
        let starts = |s: &[u8]| s.len() >= v.len() && eq(&s[..v.len()], v, ci);
        match m {
            AttrMatcher::Equals => eq(value, v, ci),
            AttrMatcher::Includes => {
                !v.is_empty()
                    && !v.iter().any(|&b| is_ws(b))
                    && value.split(|&b| is_ws(b)).any(|w| eq(w, v, ci))
            }
            AttrMatcher::DashMatch => {
                eq(value, v, ci) || (starts(value) && value.get(v.len()) == Some(&b'-'))
            }
            AttrMatcher::Prefix => !v.is_empty() && starts(value),
            AttrMatcher::Suffix => {
                !v.is_empty()
                    && value.len() >= v.len()
                    && eq(&value[value.len() - v.len()..], v, ci)
            }
            AttrMatcher::Substring => !v.is_empty() && value.windows(v.len()).any(|w| eq(w, v, ci)),
        }
    }

    fn scope(&self, e: &E) -> bool {
        match &self.cx.scope {
            Scope::None => e.is_root(),
            Scope::Element(s) => s == e,
            Scope::Root { .. } => false,
        }
    }

    fn pseudo_class(&self, pc: &PseudoClass, e: &E) -> bool {
        use PseudoClassKeyword as K;
        match pc {
            PseudoClass::Keyword(k) => match k {
                K::Root => e.is_root(),
                K::Scope => self.scope(e),
                K::Empty => {
                    e.first_element_child().is_none()
                        && !e.any_non_element_child(|t| match t {
                            Some(data) => !data
                                .iter()
                                .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')),
                            None => true,
                        })
                }
                K::FirstChild => e.prev_sibling_element().is_none(),
                K::LastChild => e.next_sibling_element().is_none(),
                K::OnlyChild => {
                    e.prev_sibling_element().is_none() && e.next_sibling_element().is_none()
                }
                K::FirstOfType => self.position(e).type_index == 1,
                K::LastOfType => {
                    let p = self.position(e);
                    p.type_index == p.type_count
                }
                K::OnlyOfType => self.position(e).type_count == 1,
                K::Host | K::HasSlotted => false,
                _ => e.state(*k),
            },
            PseudoClass::Is(f) | PseudoClass::Where(f) => self.forgiving(f, &Node::El(e.clone())),
            PseudoClass::Not(list) => !self.list(list, &Node::El(e.clone())),
            PseudoClass::Has(rel) => self.has(rel, &Node::El(e.clone())),
            PseudoClass::NthChild(anb, of) => self
                .nth(of.as_ref(), e)
                .is_some_and(|(index, _)| anb.matches(index)),
            PseudoClass::NthLastChild(anb, of) => self
                .nth(of.as_ref(), e)
                .is_some_and(|(index, count)| anb.matches(count - index + 1)),
            PseudoClass::NthOfType(anb) => anb.matches(self.position(e).type_index),
            PseudoClass::NthLastOfType(anb) => {
                let p = self.position(e);
                anb.matches(p.type_count - p.type_index + 1)
            }
            PseudoClass::Lang(ranges) => {
                e.with_language(|tag| ranges.iter().any(|r| lang_matches(tag, r.as_bytes())))
            }
            PseudoClass::Dir(d) => {
                let want = if d.eq_ignore_ascii_case("ltr") {
                    Direction::Ltr
                } else if d.eq_ignore_ascii_case("rtl") {
                    Direction::Rtl
                } else {
                    // Other values are valid but match nothing (7.1).
                    return false;
                };
                e.directionality() == want
            }
            PseudoClass::State(name) => e.custom_state(name),
            PseudoClass::Host(_) | PseudoClass::HostContext(_) => false,
        }
    }

    /// `e`'s place among its siblings. The first query in a run of
    /// siblings fills in the whole run.
    fn position(&self, e: &E) -> Position {
        if let Some(p) = self.caches.position.borrow().get(e) {
            return *p;
        }
        let run = sibling_run(e);
        let count = u32::try_from(run.len()).unwrap_or(u32::MAX);
        let mut types: HashMap<(&[u8], &[u8]), u32> = HashMap::new();
        let mut type_index = Vec::with_capacity(run.len());
        for s in &run {
            let n = types.entry((s.namespace(), s.local_name())).or_insert(0);
            *n = n.saturating_add(1);
            type_index.push(*n);
        }
        let mut position = self.caches.position.borrow_mut();
        for (k, (s, &t)) in run.iter().zip(&type_index).enumerate() {
            let p = Position {
                index: u32::try_from(k + 1).unwrap_or(u32::MAX),
                count,
                type_index: t,
                type_count: types[&(s.namespace(), s.local_name())],
            };
            position.insert(s.clone(), p);
        }
        position[e]
    }

    /// For `:nth-child()` and `:nth-last-child()` (13.3.1, 13.3.2): `e`'s
    /// 1-based index from the start among its siblings that match `of`
    /// (all of them without `of`), and how many those are; `None` if `e`
    /// doesn't match `of`.
    fn nth(&self, of: Option<&SelectorList>, e: &E) -> OfIndex {
        let Some(of) = of else {
            let p = self.position(e);
            return Some((p.index, p.count));
        };
        let key = (std::ptr::from_ref(of) as usize, self.nesting.len());
        if let Some(r) = self.caches.of.borrow().get(&(key, e.clone())) {
            return *r;
        }
        let run = sibling_run(e);
        // Matching may fill other caches, so no borrow is held across it.
        let hits: Vec<bool> = run
            .iter()
            .map(|s| self.list(of, &Node::El(s.clone())))
            .collect();
        let count = u32::try_from(hits.iter().filter(|&&h| h).count()).unwrap_or(u32::MAX);
        let mut cache = self.caches.of.borrow_mut();
        let mut index = 0u32;
        for (s, h) in run.into_iter().zip(hits) {
            let r = h.then(|| {
                index = index.saturating_add(1);
                (index, count)
            });
            cache.insert((key, s), r);
        }
        cache[&(key, e.clone())]
    }

    /// `:has()` (4.5): whether any relative selector, anchored at `anchor`,
    /// matches some element.
    fn has(&self, rel: &[RelativeSelector], anchor: &Node<E>) -> bool {
        rel.iter().any(|r| {
            let siblings = matches!(
                r.combinator,
                Combinator::NextSibling | Combinator::SubsequentSibling
            );
            // Whether the subject can lie below the first compound
            // selector's element (any descendant or child combinator after
            // the leading one).
            let deeper = r
                .selector
                .rest
                .iter()
                .any(|(c, _)| matches!(c, Combinator::Descendant | Combinator::Child));
            let try_at = |x: &E| {
                self.complex(
                    &r.selector,
                    &Node::El(x.clone()),
                    Some((anchor, r.combinator)),
                ) == Res::Matched
            };
            if siblings {
                let Node::El(a) = anchor else { return false };
                let mut s = a.next_sibling_element();
                while let Some(x) = s {
                    if try_at(&x) || (deeper && Self::any_descendant(&x, &try_at)) {
                        return true;
                    }
                    // `+ X` reaches only the next sibling.
                    if r.combinator == Combinator::NextSibling && r.selector.rest.is_empty() {
                        return false;
                    }
                    s = x.next_sibling_element();
                }
                false
            } else {
                let first = match anchor {
                    Node::El(a) => a.first_element_child(),
                    Node::Root => match &self.cx.scope {
                        Scope::Root { first_child } => first_child.clone(),
                        _ => None,
                    },
                };
                // A child combinator with nothing after it only reaches
                // the children.
                let children_only = r.combinator == Combinator::Child && !deeper;
                let mut s = first;
                while let Some(x) = s {
                    if try_at(&x) || (!children_only && Self::any_descendant(&x, &try_at)) {
                        return true;
                    }
                    s = x.next_sibling_element();
                }
                false
            }
        })
    }

    /// Whether `f` holds for any descendant of `root`.
    fn any_descendant(root: &E, f: &impl Fn(&E) -> bool) -> bool {
        let mut next = root.first_element_child();
        while let Some(e) = next {
            if f(&e) {
                return true;
            }
            next = following_in_subtree(&e, Some(root));
        }
        false
    }
}
