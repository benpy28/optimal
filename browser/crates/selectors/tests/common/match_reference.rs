//! A literal model of selector matching, for the differential tests: the
//! recursive definitions of selectors-4 section 17 and the pseudo-class
//! sections, read as directly as possible, on [`Tree`] node indices. Slow
//! (it tries every candidate and every element of the tree for `:has()`),
//! and only for small trees.
//!
//! It shares the parsed form with the crate (parsing is checked against its
//! own model), and `:lang()` range matching (checked by hand-worked tests),
//! and nothing else.

use super::tree::{HTML, Kind, Tree};
use optimal_selectors::{
    AttrCase, AttrMatcher, Combinator, Forgiving, ForgivingItem, Namespace, PseudoClass, PseudoClassKeyword as K,
    RelativeSelector, Selector, SelectorList, Simple, Unit, lang_matches,
};

/// The scoping root: none, an element, or the tree's root node (virtual).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RefScope {
    None,
    Element(usize),
    Root(usize),
}

pub struct Model<'t> {
    pub t: &'t Tree,
    pub scope: RefScope,
    pub quirks: bool,
    pub nesting: Vec<SelectorList>,
}

const VIRTUAL: usize = usize::MAX;

impl Model<'_> {
    /// The parent for matching: the parent element, or the virtual root
    /// when it is the scoping root and the parent is not an element.
    fn parent(&self, n: usize) -> Option<usize> {
        if n == VIRTUAL {
            return None;
        }
        let p = self.t.nodes[n].parent?;
        if self.t.is_element(p) {
            return Some(p);
        }
        match self.scope {
            RefScope::Root(r) if r == p => Some(VIRTUAL),
            _ => None,
        }
    }

    fn element_siblings(&self, n: usize) -> Vec<usize> {
        match self.t.nodes[n].parent {
            Some(p) => self.t.nodes[p].children.iter().copied().filter(|&c| self.t.is_element(c)).collect(),
            None => vec![n],
        }
    }

    /// Elements before `n` among its siblings, nearest first.
    fn preceding(&self, n: usize) -> Vec<usize> {
        if n == VIRTUAL {
            return vec![];
        }
        let s = self.element_siblings(n);
        let i = s.iter().position(|&x| x == n).unwrap();
        s[..i].iter().rev().copied().collect()
    }

    fn following(&self, n: usize) -> Vec<usize> {
        if n == VIRTUAL {
            return vec![];
        }
        let s = self.element_siblings(n);
        let i = s.iter().position(|&x| x == n).unwrap();
        s[i + 1..].to_vec()
    }

    fn ancestors(&self, n: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut c = self.parent(n);
        while let Some(p) = c {
            out.push(p);
            c = self.parent(p);
        }
        out
    }

    /// The elements `x` with `a c x`.
    fn related(&self, a: usize, c: Combinator, x: usize) -> bool {
        match c {
            Combinator::Descendant => self.ancestors(x).contains(&a),
            Combinator::Child => self.parent(x) == Some(a),
            Combinator::NextSibling => self.preceding(x).first() == Some(&a),
            Combinator::SubsequentSibling => self.preceding(x).contains(&a),
        }
    }

    pub fn list(&self, l: &SelectorList, n: usize) -> bool {
        l.0.iter().any(|s| self.complex(s, n, None))
    }

    /// The units of `s` up to and including `upto`, with their
    /// combinators: matched recursively, the rightmost first.
    fn complex(&self, s: &Selector, n: usize, anchor: Option<(usize, Combinator)>) -> bool {
        self.upto(s, s.rest.len(), n, anchor)
    }

    fn upto(&self, s: &Selector, i: usize, n: usize, anchor: Option<(usize, Combinator)>) -> bool {
        let unit = if i == 0 { &s.first } else { &s.rest[i - 1].1 };
        if !self.unit(unit, n) {
            return false;
        }
        if i == 0 {
            return anchor.is_none_or(|(a, c)| self.related(a, c, n));
        }
        let c = s.rest[i - 1].0;
        let candidates: Vec<usize> = match c {
            Combinator::Descendant => self.ancestors(n),
            Combinator::Child => self.parent(n).into_iter().collect(),
            Combinator::NextSibling => self.preceding(n).into_iter().take(1).collect(),
            Combinator::SubsequentSibling => self.preceding(n),
        };
        candidates.into_iter().any(|x| self.upto(s, i - 1, x, anchor))
    }

    fn unit(&self, u: &Unit, n: usize) -> bool {
        if !u.pseudos.is_empty() {
            return false;
        }
        if n == VIRTUAL {
            // Featureless: every simple selector must be allowed to match,
            // and :has() needs another one beside it.
            return u.compound.iter().all(|s| self.on_virtual(s))
                && u.compound.iter().any(|s| !matches!(s, Simple::PseudoClass(PseudoClass::Has(_))));
        }
        u.compound.iter().all(|s| self.simple(s, n))
    }

    fn on_virtual(&self, s: &Simple) -> bool {
        match s {
            Simple::PseudoClass(PseudoClass::Keyword(K::Scope)) => true,
            Simple::Nesting => match self.nesting.split_last() {
                None => true,
                Some((last, outer)) => self.outer(outer).list(last, VIRTUAL),
            },
            Simple::PseudoClass(PseudoClass::Is(f) | PseudoClass::Where(f)) => self.forgiving(f, VIRTUAL),
            Simple::PseudoClass(PseudoClass::Has(r)) => self.has(r, VIRTUAL),
            _ => false,
        }
    }

    fn outer(&self, outer: &[SelectorList]) -> Model<'_> {
        Model {
            t: self.t,
            scope: self.scope,
            quirks: self.quirks,
            nesting: outer.to_vec(),
        }
    }

    fn forgiving(&self, f: &Forgiving, n: usize) -> bool {
        f.0.iter().any(|i| matches!(i, ForgivingItem::Valid(s) if self.complex(s, n, None)))
    }

    fn data(&self, n: usize) -> (&str, &str, &[super::tree::Attr]) {
        match &self.t.nodes[n].kind {
            Kind::Element { ns, local, attrs } => (ns, local, attrs),
            _ => unreachable!(),
        }
    }

    fn html(&self, n: usize) -> bool {
        self.t.html && self.data(n).0 == HTML
    }

    fn ns_ok(ns: &Namespace, url: &str) -> bool {
        match ns {
            Namespace::Any => true,
            Namespace::None => url.is_empty(),
            Namespace::Url(u) => &**u == url,
        }
    }

    fn simple(&self, s: &Simple, n: usize) -> bool {
        let (ns, local, attrs) = self.data(n);
        let same = |a: &str, b: &str| if self.quirks { a.to_ascii_lowercase() == b.to_ascii_lowercase() } else { a == b };
        match s {
            Simple::Type { ns: want, local: name, .. } => {
                let name = if self.html(n) { name.to_ascii_lowercase() } else { name.to_string() };
                Self::ns_ok(want, ns) && name == local
            }
            Simple::Universal { ns: want, .. } => Self::ns_ok(want, ns),
            Simple::Id(id) => self.t.attr(n, "", "id").is_some_and(|v| same(v, id)),
            Simple::Class(c) => self
                .t
                .attr(n, "", "class")
                .is_some_and(|v| v.split_ascii_whitespace().any(|w| same(w, c))),
            Simple::Attribute(a) => {
                let name = if self.html(n) { a.local.to_ascii_lowercase() } else { a.local.to_string() };
                attrs.iter().any(|at| {
                    Self::ns_ok(&a.ns, &at.ns)
                        && at.local == name
                        && match &a.test {
                            None => true,
                            Some((m, v)) => {
                                let listed = self.html(n)
                                    && at.ns.is_empty()
                                    && [
                                        "accept", "accept-charset", "align", "alink", "axis", "bgcolor", "charset",
                                        "checked", "clear", "codetype", "color", "compact", "declare", "defer", "dir",
                                        "direction", "disabled", "enctype", "face", "frame", "hreflang", "http-equiv",
                                        "lang", "language", "link", "media", "method", "multiple", "nohref",
                                        "noresize", "noshade", "nowrap", "readonly", "rel", "rev", "rules", "scope",
                                        "scrolling", "selected", "shape", "target", "text", "type", "valign",
                                        "valuetype", "vlink",
                                    ]
                                    .contains(&at.local.as_str());
                                let ci = match a.case {
                                    Some(AttrCase::Insensitive) => true,
                                    Some(AttrCase::Sensitive) => false,
                                    None => listed,
                                };
                                let (value, v) = if ci {
                                    (at.value.to_ascii_lowercase(), v.to_ascii_lowercase())
                                } else {
                                    (at.value.clone(), v.to_string())
                                };
                                match m {
                                    AttrMatcher::Equals => value == v,
                                    AttrMatcher::Includes => value.split_ascii_whitespace().any(|w| w == v) && !v.is_empty() && !v.contains(|c: char| c.is_ascii_whitespace()),
                                    AttrMatcher::DashMatch => value == v || value.starts_with(&format!("{v}-")),
                                    AttrMatcher::Prefix => !v.is_empty() && value.starts_with(&v),
                                    AttrMatcher::Suffix => !v.is_empty() && value.ends_with(&v),
                                    AttrMatcher::Substring => !v.is_empty() && value.contains(&v),
                                }
                            }
                        }
                })
            }
            Simple::Nesting => match self.nesting.split_last() {
                None => self.scope_matches(n),
                Some((last, outer)) => self.outer(outer).list(last, n),
            },
            Simple::PseudoClass(pc) => self.pc(pc, n),
        }
    }

    fn scope_matches(&self, n: usize) -> bool {
        match self.scope {
            RefScope::None => self.is_root(n),
            RefScope::Element(e) => e == n,
            RefScope::Root(_) => false,
        }
    }

    fn is_root(&self, n: usize) -> bool {
        self.t.nodes[n].parent.is_some_and(|p| matches!(self.t.nodes[p].kind, Kind::Document))
    }

    fn same_type(&self, a: usize, b: usize) -> bool {
        let (an, al, _) = self.data(a);
        let (bn, bl, _) = self.data(b);
        an == bn && al == bl
    }

    fn pc(&self, pc: &PseudoClass, n: usize) -> bool {
        match pc {
            PseudoClass::Keyword(k) => match k {
                K::Root => self.is_root(n),
                K::Scope => self.scope_matches(n),
                K::Empty => self.t.nodes[n].children.iter().all(|&c| match &self.t.nodes[c].kind {
                    Kind::Comment => true,
                    Kind::Text(s) => s.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')),
                    _ => false,
                }),
                K::FirstChild => self.preceding(n).is_empty(),
                K::LastChild => self.following(n).is_empty(),
                K::OnlyChild => self.preceding(n).is_empty() && self.following(n).is_empty(),
                K::FirstOfType => !self.preceding(n).iter().any(|&x| self.same_type(x, n)),
                K::LastOfType => !self.following(n).iter().any(|&x| self.same_type(x, n)),
                K::OnlyOfType => !self.preceding(n).iter().chain(&self.following(n)).any(|&x| self.same_type(x, n)),
                K::Host | K::HasSlotted => false,
                // The tree's own states.
                other => optimal_selectors::Element::state(&super::tree::El::new(self.t, n), *other),
            },
            PseudoClass::Is(f) | PseudoClass::Where(f) => self.forgiving(f, n),
            PseudoClass::Not(l) => !self.list(l, n),
            PseudoClass::Has(r) => self.has(r, n),
            PseudoClass::NthChild(anb, of) => {
                let ok = |x: usize| of.as_ref().is_none_or(|l| self.list(l, x));
                ok(n) && anb.matches(1 + self.preceding(n).into_iter().filter(|&x| ok(x)).count() as u32)
            }
            PseudoClass::NthLastChild(anb, of) => {
                let ok = |x: usize| of.as_ref().is_none_or(|l| self.list(l, x));
                ok(n) && anb.matches(1 + self.following(n).into_iter().filter(|&x| ok(x)).count() as u32)
            }
            PseudoClass::NthOfType(anb) => {
                anb.matches(1 + self.preceding(n).into_iter().filter(|&x| self.same_type(x, n)).count() as u32)
            }
            PseudoClass::NthLastOfType(anb) => {
                anb.matches(1 + self.following(n).into_iter().filter(|&x| self.same_type(x, n)).count() as u32)
            }
            PseudoClass::Lang(ranges) => optimal_selectors::Element::with_language(&super::tree::El::new(self.t, n), |tag| {
                ranges.iter().any(|r| lang_matches(tag, r.as_bytes()))
            }),
            PseudoClass::Dir(d) => {
                let dir = optimal_selectors::Element::directionality(&super::tree::El::new(self.t, n));
                (d.eq_ignore_ascii_case("ltr") && dir == optimal_selectors::Direction::Ltr)
                    || (d.eq_ignore_ascii_case("rtl") && dir == optimal_selectors::Direction::Rtl)
            }
            PseudoClass::State(_) | PseudoClass::Host(_) | PseudoClass::HostContext(_) => false,
        }
    }

    /// :has(): some element of the whole tree matches a relative selector
    /// absolutized with this element as its anchor.
    fn has(&self, rel: &[RelativeSelector], anchor: usize) -> bool {
        let all: Vec<usize> = (0..self.t.nodes.len()).filter(|&x| self.t.is_element(x)).collect();
        rel.iter().any(|r| all.iter().any(|&x| self.complex(&r.selector, x, Some((anchor, r.combinator)))))
    }
}
