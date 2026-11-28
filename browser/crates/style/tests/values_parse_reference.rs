//! Parse trees (`src/values/parse.rs`) checked against a reference that
//! writes out every derivation of a value on random grammars and picks
//! the preferred ones by the rules in that module's documentation.
//!
//! The reference doesn't run states. A derivation of a node is the list of
//! symbols it emits (as in `values_matcher_reference.rs`: keywords,
//! `<custom-ident>`, delimiters, functions, grammar commas `G` and `#`
//! separators `S`) with a tree saying which part of the grammar emitted
//! which symbols. Juxtaposition concatenates, `|` picks one alternative,
//! `&&` takes every order, `||` every order of every non-empty subset,
//! multipliers every count, `!` keeps derivations with a component value.
//! Commas written in the grammar are then dropped as css-values-4 2.1
//! says (next to another comma, or at the start or end of the list), and
//! the rest must equal the value.
//!
//! Each valid derivation gets a key, read in the tree's own order (left to
//! right, outside in), and the greatest key wins:
//!
//! - an item of a juxtaposition, a step of `&&`/`||`, or a repetition
//!   adds how far into the value the derivation is after it (rule 1: take
//!   as much as possible), where a grammar comma's token counts with the
//!   component after it and a separator with its repetition;
//! - a `|` adds its choice's rank, and so does each `&&`/`||` step (rules
//!   2 and 3: a child that can't reach a `<custom-ident>` first, then
//!   grammar order);
//! - a group or repetition that stops adds "stopped" at how far it is,
//!   which beats going on with a child that takes nothing (rule 4).
//!
//! Grammars with a repetition whose child can emit only grammar commas
//! (`[ a | ,? ]{2,4}`) are skipped: a repetition there can step past a
//! comma without taking anything, any number of times, and the parse
//! takes the fewest such steps (rule 4), which this key doesn't model;
//! `values_parse.rs` has a hand-worked case. No spec grammar has one. A
//! case whose winner uses the most repetitions written out is skipped,
//! as is one with too many derivations.

#[path = "common/values_random.rs"]
mod values_random;

use optimal_css::parse_component_values;
use optimal_style::values::parse::{Tree, TreeKind, parse};
use optimal_style::values::syntax::{Node, NodeId};
use optimal_style::values::types::{Builtin, NOT_CUSTOM_IDENT};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;
use std::collections::BTreeSet;
use values_random::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sym {
    Kw(&'static str),
    Custom,
    Delim(char),
    Func(NodeId),
    G,
    S,
}

/// What a part of a derivation is, with the range of symbols it emitted.
#[derive(Clone, Debug)]
struct D {
    a: usize,
    b: usize,
    kind: K,
}

#[derive(Clone, Debug)]
enum K {
    /// A keyword, type, delimiter or function: one symbol.
    Leaf,
    Comma,
    Inner(Box<D>),
    Alt(usize, Box<D>),
    /// The children in order, with their indices; for a repetition, the
    /// flag says the derivation used the most repetitions written out.
    List(Vec<(usize, D)>, bool),
}

impl D {
    fn shift(&mut self, by: usize) {
        self.a += by;
        self.b += by;
        match &mut self.kind {
            K::Leaf | K::Comma => {}
            K::Inner(d) | K::Alt(_, d) => d.shift(by),
            K::List(items, _) => items.iter_mut().for_each(|(_, d)| d.shift(by)),
        }
    }
}

/// A derivation: the symbols and the tree.
type Derivation = (Vec<Sym>, D);

/// A partial list: the symbols so far and each child's tree.
type Partial = (Vec<Sym>, Vec<(usize, D)>);

/// A matching derivation: its symbols, their value indices, the tree.
type Matched = (Vec<Sym>, Vec<Option<usize>>, D);

/// The most derivations written out for one node.
const LIMIT: usize = 30_000;

struct Reference<'n> {
    nodes: &'n [Node],
    /// The most symbols other than `G` a derivation may emit.
    cap: usize,
    too_big: bool,
}

fn cost(s: &[Sym]) -> usize {
    s.iter().filter(|&&x| x != Sym::G).count()
}

fn claims_custom(nodes: &[Node], n: NodeId) -> bool {
    match &nodes[n as usize] {
        Node::Type(Builtin::CustomIdent(_), _) => true,
        Node::Required(c) | Node::Ref(c) => claims_custom(nodes, *c),
        Node::Repeat { node, .. } => claims_custom(nodes, *node),
        Node::Seq(c) | Node::All(c) | Node::Any(c) | Node::One(c) => {
            c.iter().any(|&c| claims_custom(nodes, c))
        }
        _ => false,
    }
}

/// The rank of each child: those that can't reach a `<custom-ident>`
/// first, then in grammar order.
fn ranks(nodes: &[Node], kids: &[NodeId]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..kids.len()).collect();
    order.sort_by_key(|&i| claims_custom(nodes, kids[i]));
    let mut rank = vec![0; kids.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    rank
}

fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first);
            out.push(p);
        }
    }
    out
}

impl Reference<'_> {
    /// Every way of emitting `kids` (by index into `all`) in order: the
    /// symbols and each child's tree.
    fn seq(&mut self, all: &[NodeId], order: &[usize]) -> Vec<Partial> {
        let mut cur: Vec<Partial> = vec![(Vec::new(), Vec::new())];
        for &i in order {
            let next = self.derive(all[i]);
            let mut out = Vec::new();
            for (syms, items) in &cur {
                for (s2, d2) in &next {
                    if cost(syms) + cost(s2) > self.cap {
                        continue;
                    }
                    let mut s = syms.clone();
                    let mut d = d2.clone();
                    d.shift(s.len());
                    s.extend_from_slice(s2);
                    let mut it = items.clone();
                    it.push((i, d));
                    out.push((s, it));
                }
            }
            if out.len() > LIMIT {
                self.too_big = true;
                out.truncate(LIMIT);
            }
            cur = out;
        }
        cur
    }

    fn derive(&mut self, n: NodeId) -> Vec<Derivation> {
        if self.too_big {
            return Vec::new();
        }
        let leaf = |s: Sym| {
            vec![(
                vec![s],
                D {
                    a: 0,
                    b: 1,
                    kind: K::Leaf,
                },
            )]
        };
        let list = |(syms, items): Partial, most: bool| -> Derivation {
            let b = syms.len();
            (
                syms,
                D {
                    a: 0,
                    b,
                    kind: K::List(items, most),
                },
            )
        };
        match &self.nodes[n as usize] {
            Node::Keyword(k) => leaf(Sym::Kw(k)),
            Node::Type(Builtin::CustomIdent(_), _) => leaf(Sym::Custom),
            Node::Delim(c) => leaf(Sym::Delim(*c)),
            Node::Function(..) => leaf(Sym::Func(n)),
            Node::Comma => vec![(
                vec![Sym::G],
                D {
                    a: 0,
                    b: 1,
                    kind: K::Comma,
                },
            )],
            Node::Seq(c) => {
                let c = c.clone();
                let order: Vec<usize> = (0..c.len()).collect();
                self.seq(&c, &order)
                    .into_iter()
                    .map(|x| list(x, false))
                    .collect()
            }
            Node::One(c) => {
                let c = c.clone();
                let mut out = Vec::new();
                for (i, &alt) in c.iter().enumerate() {
                    for (s, d) in self.derive(alt) {
                        let b = s.len();
                        out.push((
                            s,
                            D {
                                a: 0,
                                b,
                                kind: K::Alt(i, Box::new(d)),
                            },
                        ));
                    }
                }
                out
            }
            Node::All(c) | Node::Any(c) => {
                let all = matches!(self.nodes[n as usize], Node::All(_));
                let c = c.clone();
                let mut out = Vec::new();
                for mask in 1..(1usize << c.len()) {
                    if all && mask != (1 << c.len()) - 1 {
                        continue;
                    }
                    let subset: Vec<usize> =
                        (0..c.len()).filter(|i| mask & (1 << i) != 0).collect();
                    for p in permutations(&subset) {
                        out.extend(self.seq(&c, &p).into_iter().map(|x| list(x, false)));
                    }
                }
                out
            }
            &Node::Repeat {
                node,
                min,
                max,
                hash,
            } => {
                // Enough repetitions for every consuming one and a few
                // that take nothing.
                let most = max.unwrap_or(min.max(1) + self.cap as u32 + 2);
                let child = self.derive(node);
                let mut out = Vec::new();
                let mut cur: Vec<Partial> = vec![(Vec::new(), Vec::new())];
                for k in 0..=most {
                    if k >= min {
                        out.extend(
                            cur.iter()
                                .cloned()
                                .map(|x| list(x, k == most && max.is_none())),
                        );
                    }
                    if k == most {
                        break;
                    }
                    let mut next = Vec::new();
                    for (syms, items) in &cur {
                        for (s2, d2) in &child {
                            let mut s = syms.clone();
                            if hash && k > 0 {
                                s.push(Sym::S);
                            }
                            if cost(&s) + cost(s2) > self.cap {
                                continue;
                            }
                            let mut d = d2.clone();
                            d.shift(s.len());
                            s.extend_from_slice(s2);
                            let mut it = items.clone();
                            it.push((0, d));
                            next.push((s, it));
                        }
                    }
                    if next.len() > LIMIT {
                        self.too_big = true;
                        break;
                    }
                    cur = next;
                    if cur.is_empty() {
                        break;
                    }
                }
                out
            }
            Node::Required(c) => self
                .derive(*c)
                .into_iter()
                .filter(|(s, _)| s.iter().any(|x| !matches!(x, Sym::G | Sym::S)))
                .map(|(s, d)| {
                    let b = s.len();
                    (
                        s,
                        D {
                            a: 0,
                            b,
                            kind: K::Inner(Box::new(d)),
                        },
                    )
                })
                .collect(),
            Node::Never => Vec::new(),
            other => panic!("not generated: {other:?}"),
        }
    }
}

/// The value index each symbol of a derivation takes, after the comma
/// rule of css-values-4 2.1 drops grammar commas: one next to another
/// comma, or at the start or the end.
fn literal(syms: &[Sym]) -> Vec<Option<usize>> {
    let mut kept: Vec<usize> = Vec::new();
    for (i, &x) in syms.iter().enumerate() {
        let last = kept.last().map(|&j| syms[j]);
        match (last, x) {
            (Some(Sym::G), Sym::G) | (Some(Sym::S), Sym::G) => {}
            (Some(Sym::G), Sym::S) => {
                kept.pop();
                kept.push(i);
            }
            _ => kept.push(i),
        }
    }
    if kept.first().is_some_and(|&j| syms[j] == Sym::G) {
        kept.remove(0);
    }
    if kept.last().is_some_and(|&j| syms[j] == Sym::G) {
        kept.pop();
    }
    let mut at = vec![None; syms.len()];
    for (k, &j) in kept.iter().enumerate() {
        at[j] = Some(k);
    }
    at
}

/// How far into the value the symbols before `b` reach: one past the last
/// value index a component value or separator took.
fn reach(syms: &[Sym], at: &[Option<usize>], b: usize) -> i64 {
    (0..b)
        .filter(|&i| syms[i] != Sym::G)
        .filter_map(|i| at[i])
        .map(|k| k as i64 + 1)
        .max()
        .unwrap_or(0)
}

struct Keyed<'r> {
    nodes: &'r [Node],
    syms: &'r [Sym],
    at: &'r [Option<usize>],
}

impl Keyed<'_> {
    fn key(&self, n: NodeId, d: &D, out: &mut Vec<i64>) {
        let reach = |b| reach(self.syms, self.at, b);
        match (&self.nodes[n as usize], &d.kind) {
            (_, K::Leaf | K::Comma) => {}
            (Node::Required(c), K::Inner(t)) | (Node::Ref(c), K::Inner(t)) => self.key(*c, t, out),
            (Node::One(c), K::Alt(i, t)) => {
                out.push(-(ranks(self.nodes, c)[*i] as i64));
                self.key(c[*i], t, out);
            }
            (Node::Seq(c), K::List(items, _)) => {
                for (i, t) in items {
                    out.push(reach(t.b));
                    self.key(c[*i], t, out);
                }
            }
            (Node::All(c) | Node::Any(c), K::List(items, _)) => {
                let r = ranks(self.nodes, c);
                for (i, t) in items {
                    out.extend([reach(t.b), 0, -(r[*i] as i64)]);
                    self.key(c[*i], t, out);
                }
                out.extend([reach(d.b), 1, 0]);
            }
            (Node::Repeat { node, .. }, K::List(items, _)) => {
                for (_, t) in items {
                    // The separator, emitted before the child, is part of
                    // the step.
                    out.extend([reach(t.b), 0, 0]);
                    self.key(*node, t, out);
                }
                out.extend([reach(d.b), 1, 0]);
            }
            (node, kind) => panic!("{node:?} with {kind:?}"),
        }
    }
}

fn uses_most(d: &D) -> bool {
    match &d.kind {
        K::Leaf | K::Comma => false,
        K::Inner(t) | K::Alt(_, t) => uses_most(t),
        K::List(items, most) => *most || items.iter().any(|(_, t)| uses_most(t)),
    }
}

/// The trees (as text, see [`show`]) the rules prefer for `value` against
/// `top`: `Some(empty)` if it doesn't match, `None` if the case is skipped.
fn preferred(nodes: &[Node], top: NodeId, value: &[Item]) -> Option<BTreeSet<String>> {
    counted(nodes, top, value).map(|x| x.0)
}

/// [`preferred`], and how many derivations matched.
fn counted(nodes: &[Node], top: NodeId, value: &[Item]) -> Option<(BTreeSet<String>, usize)> {
    let mut matched = 0;
    let mut r = Reference {
        nodes,
        cap: value.len(),
        too_big: false,
    };
    let all = r.derive(top);
    if r.too_big {
        return None;
    }
    let mut best: Option<(Vec<i64>, Vec<Matched>)> = None;
    for (syms, d) in all {
        let at = literal(&syms);
        if at.iter().flatten().count() != value.len() {
            continue;
        }
        // Each kept symbol against its component value.
        let mut ok = true;
        for (i, &x) in syms.iter().enumerate() {
            let Some(k) = at[i] else { continue };
            ok &= match (x, &value[k]) {
                (Sym::Kw(w), Item::Ident(v)) => w.eq_ignore_ascii_case(v),
                (Sym::Custom, Item::Ident(v)) => {
                    !NOT_CUSTOM_IDENT.iter().any(|w| w.eq_ignore_ascii_case(v))
                }
                (Sym::Delim(c), Item::Delim(e)) => c == *e,
                (Sym::G | Sym::S, Item::Comma) => true,
                (Sym::Func(f), Item::Func(name, args)) => {
                    let Node::Function(fname, inner) = nodes[f as usize] else {
                        unreachable!()
                    };
                    fname.eq_ignore_ascii_case(name) && !preferred(nodes, inner, args)?.is_empty()
                }
                _ => false,
            };
        }
        if !ok {
            continue;
        }
        matched += 1;
        let mut key = Vec::new();
        Keyed {
            nodes,
            syms: &syms,
            at: &at,
        }
        .key(top, &d, &mut key);
        match &mut best {
            Some((k, v)) if *k == key => v.push((syms, at, d)),
            Some((k, _)) if *k > key => {}
            _ => best = Some((key, vec![(syms, at, d)])),
        }
    }
    let Some((_, winners)) = best else {
        return Some((BTreeSet::new(), 0));
    };
    let mut out = BTreeSet::new();
    for (_, at, d) in &winners {
        if uses_most(d) {
            return None;
        }
        for s in shows(nodes, top, d, at, value)? {
            out.insert(s);
        }
    }
    Some((out, matched))
}

/// Every text of a reference tree (a function's contents can have more
/// than one preferred tree).
fn shows(
    nodes: &[Node],
    n: NodeId,
    d: &D,
    at: &[Option<usize>],
    value: &[Item],
) -> Option<Vec<String>> {
    Some(match (&nodes[n as usize], &d.kind) {
        (Node::Comma, _) => vec![",".into()],
        (Node::Function(_, inner), _) => {
            let Item::Func(name, args) = &value[at[d.a].expect("kept")] else {
                unreachable!()
            };
            preferred(nodes, *inner, args)?
                .into_iter()
                .map(|t| format!("{name}({t})"))
                .collect()
        }
        (_, K::Leaf) => vec![render(&[value[at[d.a].expect("kept")].clone()])],
        (Node::Required(c), K::Inner(t)) => shows(nodes, *c, t, at, value)?,
        (Node::One(c), K::Alt(i, t)) => shows(nodes, c[*i], t, at, value)?
            .into_iter()
            .map(|s| format!("{i}:{s}"))
            .collect(),
        (node, K::List(items, _)) => {
            let child = |i: usize| match node {
                Node::Repeat { node, .. } => *node,
                Node::Seq(c) | Node::All(c) | Node::Any(c) => c[i],
                _ => unreachable!(),
            };
            let mut acc = vec![String::new()];
            for (k, (i, t)) in items.iter().enumerate() {
                let opts = shows(nodes, child(*i), t, at, value)?;
                let mut next = Vec::new();
                for a in &acc {
                    for o in &opts {
                        let sep = if k == 0 { "" } else { " " };
                        next.push(format!("{a}{sep}{i}={o}"));
                    }
                }
                acc = next;
            }
            acc.into_iter().map(|s| format!("[{s}]")).collect()
        }
        (node, kind) => panic!("{node:?} with {kind:?}"),
    })
}

/// The production tree as text: the same form as [`shows`].
fn show(src: &str, t: &Tree<'_, '_>) -> String {
    match &t.kind {
        TreeKind::Value(v) => src[v.span()].to_string(),
        TreeKind::Nested(v, inner) => {
            let s = &src[v.span()];
            format!(
                "{}({})",
                &s[..s.find('(').expect("a function")],
                show(src, inner)
            )
        }
        TreeKind::Comma => ",".into(),
        TreeKind::Inner(t) => show(src, t),
        TreeKind::Alternative(i, t) => format!("{i}:{}", show(src, t)),
        TreeKind::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|(i, t)| format!("{i}={}", show(src, t)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

/// Whether `n` can emit nothing at all.
fn can_be_empty(nodes: &[Node], n: NodeId) -> bool {
    match &nodes[n as usize] {
        Node::Seq(c) | Node::All(c) => c.iter().all(|&c| can_be_empty(nodes, c)),
        Node::One(c) | Node::Any(c) => c.iter().any(|&c| can_be_empty(nodes, c)),
        &Node::Repeat { node, min, .. } => min == 0 || can_be_empty(nodes, node),
        _ => false,
    }
}

/// Whether `n` can emit grammar commas and nothing else.
fn only_commas(nodes: &[Node], n: NodeId) -> bool {
    let quiet = |c: NodeId| can_be_empty(nodes, c) || only_commas(nodes, c);
    match &nodes[n as usize] {
        Node::Comma => true,
        Node::Seq(c) | Node::All(c) => {
            c.iter().all(|&c| quiet(c)) && c.iter().any(|&c| only_commas(nodes, c))
        }
        Node::One(c) | Node::Any(c) => c.iter().any(|&c| only_commas(nodes, c)),
        &Node::Repeat { node, max, .. } => max != Some(0) && only_commas(nodes, node),
        _ => false,
    }
}

/// Runs the cases of one seed: (checked, with more than one matching
/// derivation).
fn run(seed: u64) -> (usize, usize) {
    let mut g = Gen(seed | 1);
    let mut nodes = Vec::new();
    let depth = 1 + g.below(3) as u32;
    let top = grammar(&mut g, &mut nodes, depth, false);
    let skip = (0..nodes.len())
        .any(|i| matches!(nodes[i], Node::Repeat { node, .. } if only_commas(&nodes, node)));
    if skip {
        return (0, 0);
    }
    let text = show_grammar(&nodes, top);
    let c = compiled(nodes.clone());
    let (mut checked, mut ambiguous) = (0, 0);
    for _ in 0..6 {
        let value = if g.chance(70) {
            let cap = 1 + g.below(5);
            match sample(&mut g, &nodes, top, cap) {
                Some(v) => v,
                None => continue,
            }
        } else {
            let len = g.below(5);
            random_value(&mut g, len, 1)
        };
        let Some((want, matched)) = counted(&nodes, top, &value) else {
            continue;
        };
        let source = render(&value);
        let (parsed, range) = parse_component_values(Wtf8::from_str(&source));
        let got = parse(&c, top, parsed.values(range), 0).map(|t| show(&source, &t));
        match &got {
            None => assert!(
                want.is_empty(),
                "seed {seed}\ngrammar: {text}\nvalue: {source}\nno parse, reference {want:?}"
            ),
            Some(t) => assert!(
                want.contains(t),
                "seed {seed}\ngrammar: {text}\nvalue: {source}\nparse {t}\nreference {want:?}"
            ),
        }
        checked += 1;
        ambiguous += (matched > 1) as usize;
    }
    (checked, ambiguous)
}

fn show_grammar(nodes: &[Node], top: NodeId) -> String {
    values_random::show(nodes, top)
}

/// A value the grammar matches, from the reference's own derivations.
fn sample(g: &mut Gen, nodes: &[Node], top: NodeId, cap: usize) -> Option<Vec<Item>> {
    let mut r = Reference {
        nodes,
        cap,
        too_big: false,
    };
    let all = r.derive(top);
    if all.is_empty() || r.too_big {
        return None;
    }
    let (syms, _) = &all[g.below(all.len())];
    let at = literal(syms);
    let mut out = Vec::new();
    for (i, &x) in syms.iter().enumerate() {
        if at[i].is_none() {
            continue;
        }
        out.push(match x {
            Sym::Kw(k) => Item::Ident(k.to_string()),
            Sym::Custom => Item::Ident(if g.chance(50) { "c" } else { "a" }.into()),
            Sym::Delim(c) => Item::Delim(c),
            Sym::G | Sym::S => Item::Comma,
            Sym::Func(f) => {
                let Node::Function(name, inner) = nodes[f as usize] else {
                    unreachable!()
                };
                let args = sample(g, nodes, inner, 2).unwrap_or_default();
                Item::Func(name.to_string(), args)
            }
        });
    }
    Some(out)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(500)
    ))]
    #[test]
    fn parse_picks_a_preferred_derivation(seed in any::<u64>()) {
        run(seed);
    }
}

/// Enough cases are checked, and many have more than one derivation, so
/// the rules are exercised and not only the single-way matches.
#[test]
fn ambiguous_cases_are_common() {
    let (mut checked, mut ambiguous) = (0, 0);
    for seed in 0..300u64 {
        let (c, a) = run(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        checked += c;
        ambiguous += a;
    }
    assert!(checked > 800, "{checked} cases");
    assert!(ambiguous > 200, "{ambiguous} of {checked} ambiguous");
}
