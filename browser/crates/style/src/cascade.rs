//! Filtering and the cascade sort (css-cascade-5 sections 5 and 6.1).
//!
//! For an element, the declarations that apply are those of the style rules
//! whose selectors match it, plus its element-attached declarations (a
//! `style` attribute). The cascade sorts each property's declarations by,
//! from the first criterion to the last ([`Priority`]):
//!
//! 1. origin and importance (6.1: important user agent, important user,
//!    important author, normal author, normal user, normal user agent;
//!    there are no animation or transition declarations yet);
//! 2. context (there are no shadow trees yet, so every declaration is in
//!    the document's context);
//! 3. element-attached declarations before those mapped by selectors, for
//!    each importance;
//! 4. cascade layers (6.4): for normal declarations a later layer wins, for
//!    important ones an earlier layer wins; unlayered declarations are in
//!    the implicit outer layer, after every layer of their origin;
//! 5. specificity: of the rule's most specific selector that matches;
//! 6. order of appearance.
//!
//! The output of the cascade is each property's declarations in that
//! order, the first being the cascaded value ([`Cascaded`]). Defaulting
//! (`initial`, `inherit`, `revert` and the rest) works on that list in the
//! next step.
//!
//! Design choices (chosen, why, rejected):
//!
//! - **Rules are bucketed by their rightmost compound selector.** Each
//!   selector goes in one bucket: by an ID it requires, else a class, else
//!   a type, else the universal bucket. An element only tries the
//!   selectors in the buckets of its ID, each of its distinct classes, its
//!   local name and the universal bucket, so a sheet of many rules costs
//!   each element the rules that could match it. Keys are ASCII-lowercased
//!   on both sides, so quirks mode and the HTML case rules only make a
//!   bucket hold more candidates, which the matcher then checks. Rejected:
//!   trying every rule on every element (the literal model does that).
//! - **One [`MatchSession`] for the whole pass** (see
//!   [`Cascade::with_session`]), so the matcher's caches are shared
//!   between elements and rules; a nested rule passes its chain of
//!   ancestor rules to it, which is fixed per rule as the session needs.
//! - **The sort key is a derived total order** ([`Priority`]); the order of
//!   appearance is unique, so no two declarations compare equal.
//!
//! Selectors whose subject is a pseudo-element never match an element, so
//! they are not put in any bucket; styling pseudo-elements is a later step.

use optimal_selectors::{
    Element, MatchContext, MatchSession, Scope, SelectorList, Simple, Specificity,
};
use std::collections::HashMap;

use crate::collect::{Origin, StyleAttribute, StyleSet};

/// Origin and importance, in increasing precedence (6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Level {
    /// Normal user agent declarations.
    UserAgent,
    /// Normal user declarations.
    User,
    /// Normal author declarations.
    Author,
    /// Important author declarations.
    ImportantAuthor,
    /// Important user declarations.
    ImportantUser,
    /// Important user agent declarations.
    ImportantUserAgent,
}

impl Level {
    /// The level of a declaration of `origin`.
    pub fn of(origin: Origin, important: bool) -> Level {
        match (origin, important) {
            (Origin::UserAgent, false) => Level::UserAgent,
            (Origin::User, false) => Level::User,
            (Origin::Author, false) => Level::Author,
            (Origin::Author, true) => Level::ImportantAuthor,
            (Origin::User, true) => Level::ImportantUser,
            (Origin::UserAgent, true) => Level::ImportantUserAgent,
        }
    }
}

/// A declaration's precedence: a greater one wins. Fields compare in
/// order, as the criteria of 6.1 do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Priority {
    /// Origin and importance.
    pub level: Level,
    /// Whether it is element-attached.
    pub attached: bool,
    /// The layer, as a key that grows with precedence: the layer's rank for
    /// normal declarations, its rank counted from the end for important
    /// ones; zero for element-attached declarations.
    pub layer: u32,
    /// The specificity (zero for element-attached declarations).
    pub specificity: Specificity,
    /// The order of appearance: style sheets' declarations first, then the
    /// element-attached ones.
    pub order: u32,
}

/// Where a declaration comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// A style rule's: an index into [`StyleSet::decls`].
    Rule(u32),
    /// The element's style attribute: an index into
    /// [`StyleAttribute::decls`].
    Attribute(u32),
}

/// A declared value, placed in the cascade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    /// Its declaration.
    pub source: Source,
    /// Its precedence.
    pub priority: Priority,
}

/// The output of the cascade for one element: for each property that has
/// declarations, the declarations in decreasing precedence. Properties are
/// sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cascaded<'n> {
    /// (property name, its declarations from the winner down).
    pub properties: Vec<(&'n str, Vec<Entry>)>,
}

impl<'n> Cascaded<'n> {
    /// A property's declarations, from the winner down.
    pub fn get(&self, name: &str) -> Option<&[Entry]> {
        self.properties
            .binary_search_by(|(n, _)| (*n).cmp(name))
            .ok()
            .map(|i| &*self.properties[i].1)
    }

    /// A property's cascaded value: the winning declaration.
    pub fn winner(&self, name: &str) -> Option<Entry> {
        self.get(name).map(|e| e[0])
    }
}

/// A candidate: a rule and one of its selectors.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    rule: u32,
    selector: u32,
}

/// A [`StyleSet`] made ready for cascading: the layer order and the rule
/// buckets. Adding sheets afterwards needs a new one.
#[derive(Debug)]
pub struct Cascade<'s, 'a> {
    set: &'s StyleSet<'a>,
    ranks: Vec<u32>,
    ids: HashMap<Box<[u8]>, Vec<Candidate>>,
    classes: HashMap<Box<[u8]>, Vec<Candidate>>,
    types: HashMap<Box<[u8]>, Vec<Candidate>>,
    universal: Vec<Candidate>,
}

/// What a selector is bucketed by.
enum Key<'k> {
    Id(&'k str),
    Class(&'k str),
    Type(&'k str),
    Universal,
    /// The subject is a pseudo-element: it never matches an element.
    Never,
}

fn key(sel: &optimal_selectors::Selector) -> Key<'_> {
    let unit = sel.rest.last().map_or(&sel.first, |(_, u)| u);
    if !unit.pseudos.is_empty() {
        return Key::Never;
    }
    let mut best = Key::Universal;
    for s in &unit.compound {
        match s {
            Simple::Id(id) => return Key::Id(id),
            Simple::Class(c) if !matches!(best, Key::Class(_)) => best = Key::Class(c),
            // A type selector comes first in a compound, before any class.
            Simple::Type { local, .. } => best = Key::Type(local),
            _ => {}
        }
    }
    best
}

fn lower(b: &[u8]) -> Box<[u8]> {
    b.to_ascii_lowercase().into()
}

impl<'a> StyleSet<'a> {
    /// Gets the set ready for cascading.
    pub fn prepare(&self) -> Cascade<'_, 'a> {
        let mut c = Cascade {
            set: self,
            ranks: self.layers().ranks(),
            ids: HashMap::new(),
            classes: HashMap::new(),
            types: HashMap::new(),
            universal: Vec::new(),
        };
        for (r, rule) in self.rules().iter().enumerate() {
            if rule.blocks.is_empty() {
                continue;
            }
            for (i, sel) in rule.selectors.0.iter().enumerate() {
                let cand = Candidate {
                    rule: r as u32,
                    selector: i as u32,
                };
                match key(sel) {
                    Key::Id(k) => c.ids.entry(lower(k.as_bytes())).or_default().push(cand),
                    Key::Class(k) => c.classes.entry(lower(k.as_bytes())).or_default().push(cand),
                    Key::Type(k) => c.types.entry(lower(k.as_bytes())).or_default().push(cand),
                    Key::Universal => c.universal.push(cand),
                    Key::Never => {}
                }
            }
        }
        c
    }
}

/// A pass over a document's elements: see [`Cascade::with_session`].
pub struct Session<'c, 's, 'a, E> {
    cascade: &'c Cascade<'s, 'a>,
    matcher: MatchSession<'c, E>,
    /// Scratch space for a nested rule's chain of ancestor rules.
    chain: Vec<&'c SelectorList>,
    tried: usize,
}

impl<'s, 'a> Cascade<'s, 'a> {
    /// Runs `f` with a session for cascading the elements of one document
    /// (in quirks mode or not), whose matching caches are shared between
    /// the elements. The tree must not change during the session.
    pub fn with_session<E: Element, R>(
        &self,
        quirks: bool,
        f: impl FnOnce(&mut Session<'_, 's, 'a, E>) -> R,
    ) -> R {
        let cx = MatchContext {
            scope: Scope::None,
            quirks,
            nesting: &[],
        };
        let mut session = Session {
            cascade: self,
            matcher: MatchSession::new(&cx),
            chain: Vec::new(),
            tried: 0,
        };
        f(&mut session)
    }

    /// The layer key of a declaration in `layer` (see [`Priority::layer`]).
    fn layer_key(&self, layer: u32, important: bool) -> u32 {
        let rank = self.ranks[layer as usize];
        if important { u32::MAX - rank } else { rank }
    }
}

impl<'c, 's: 'c, 'a, E: Element> Session<'c, 's, 'a, E> {
    /// How many selectors the session has tried against elements: the
    /// candidates from the buckets (for tests of the bucketing).
    pub fn selectors_tried(&self) -> usize {
        self.tried
    }

    /// The cascade for `element`, with the declarations of its `style`
    /// attribute if it has one.
    pub fn cascade<'n>(
        &mut self,
        element: &E,
        style: Option<&'n StyleAttribute<'_>>,
    ) -> Cascaded<'n>
    where
        'c: 'n,
    {
        let c = self.cascade;
        let set = c.set;
        // Candidates from the element's buckets.
        let mut cands: Vec<Candidate> = Vec::new();
        if let Some(id) = element.id()
            && let Some(b) = c.ids.get(&*lower(id))
        {
            cands.extend_from_slice(b);
        }
        if let Some(class) = element.class_attribute() {
            let mut seen: Vec<Box<[u8]>> = class
                .split(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c'))
                .filter(|w| !w.is_empty())
                .map(lower)
                .collect();
            seen.sort_unstable();
            seen.dedup();
            for w in &seen {
                if let Some(b) = c.classes.get(w) {
                    cands.extend_from_slice(b);
                }
            }
        }
        if let Some(b) = c.types.get(&*lower(element.local_name())) {
            cands.extend_from_slice(b);
        }
        cands.extend_from_slice(&c.universal);
        self.tried += cands.len();

        // The rules that match, each with its most specific matching
        // selector's specificity.
        let mut matched: Vec<(u32, Specificity)> = Vec::new();
        for cand in cands {
            let rule = &set.rules()[cand.rule as usize];
            let list = &rule.singles[cand.selector as usize];
            self.chain.clear();
            let mut p = rule.parent;
            while let Some(i) = p {
                let r = &set.rules()[i as usize];
                self.chain.push(&r.selectors);
                p = r.parent;
            }
            self.chain.reverse();
            if self.matcher.matches_nested(list, &self.chain, element) {
                matched.push((cand.rule, rule.specificity[cand.selector as usize]));
            }
        }
        matched.sort_unstable();

        let mut entries: Vec<(&'n str, Entry)> = Vec::new();
        let mut i = 0;
        while i < matched.len() {
            let (r, mut spec) = matched[i];
            while i < matched.len() && matched[i].0 == r {
                spec = spec.max(matched[i].1);
                i += 1;
            }
            let rule = &set.rules()[r as usize];
            for block in &rule.blocks {
                for d in block.decls.clone() {
                    let decl = &set.decls()[d as usize];
                    let important = decl.decl.important;
                    entries.push((
                        &decl.name,
                        Entry {
                            source: Source::Rule(d),
                            priority: Priority {
                                level: Level::of(rule.origin, important),
                                attached: false,
                                layer: c.layer_key(block.layer, important),
                                specificity: spec,
                                order: decl.order,
                            },
                        },
                    ));
                }
            }
        }
        if let Some(style) = style {
            for (i, (name, d)) in style.decls.iter().enumerate() {
                entries.push((
                    name,
                    Entry {
                        source: Source::Attribute(i as u32),
                        priority: Priority {
                            level: Level::of(Origin::Author, d.important),
                            attached: true,
                            layer: 0,
                            specificity: Specificity::ZERO,
                            order: set.end_order() + i as u32,
                        },
                    },
                ));
            }
        }
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0).then(b.1.priority.cmp(&a.1.priority)));
        let mut out = Cascaded::default();
        for (name, e) in entries {
            match out.properties.last_mut() {
                Some((n, list)) if *n == name => list.push(e),
                _ => out.properties.push((name, vec![e])),
            }
        }
        out
    }
}
