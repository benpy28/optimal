//! The value grammar matcher (`src/values/matcher.rs`) checked against a
//! literal reading of css-values-4 2 on random grammars and random values.
//!
//! The reference doesn't run states. It writes out the set of component
//! value sequences a grammar describes, as the spec defines a grammar
//! (2.1), up to the length of the value being checked, and asks whether
//! the value is one of them:
//!
//! - every combinator and multiplier is expanded into its sentences:
//!   juxtaposition concatenates, `|` unions, `&&` takes every order of all
//!   children, `||` every order of every non-empty subset, `{A,B}`, `*`,
//!   `+`, `?` and `#` repeat (with a comma token between repetitions for
//!   `#`), and `!` keeps the sentences with at least one component value;
//! - commas written in the grammar are kept as marks in the sentence and
//!   only then dropped by the sentences of 2.1 "Commas": "all items
//!   preceding the comma have been omitted", "all items following the comma
//!   have been omitted", or "multiple commas would be adjacent"; the `#`
//!   separator is a comma token, never omitted;
//! - a function's arguments are a list of their own: its contents are
//!   checked against the argument grammar's sentences.
//!
//! Each case prints the grammar and the value on failure.

use optimal_css::parse_component_values;
use optimal_style::values::Compiled;
use optimal_style::values::matcher::matches_list;
use optimal_style::values::math::Context;
use optimal_style::values::syntax::{Node, NodeId};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap};

/// A small deterministic generator (xorshift), so a failing case is
/// replayed from its seed.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

// --- Random grammars -------------------------------------------------------

/// Builds a random grammar node of at most `depth` levels into `nodes`.
/// No `!` inside `!` (the compiler rejects it), at most 3 children per
/// group.
fn grammar(g: &mut Gen, nodes: &mut Vec<Node>, depth: u32, in_bang: bool) -> NodeId {
    let node = if depth == 0 || g.chance(30) {
        match g.below(9) {
            0..=2 => Node::Keyword("a"),
            3 | 4 => Node::Keyword("b"),
            5 | 6 => Node::Comma,
            7 => Node::Delim('/'),
            _ if depth > 0 => {
                let inner = grammar(g, nodes, depth - 1, false);
                Node::Function("f", inner)
            }
            _ => Node::Keyword("a"),
        }
    } else {
        let kids = |g: &mut Gen, nodes: &mut Vec<Node>| -> Box<[NodeId]> {
            let k = 2 + g.below(2);
            (0..k)
                .map(|_| grammar(g, nodes, depth - 1, in_bang))
                .collect()
        };
        match g.below(if in_bang { 6 } else { 7 }) {
            0 | 1 => Node::Seq(kids(g, nodes)),
            2 => Node::All(kids(g, nodes)),
            3 => Node::Any(kids(g, nodes)),
            4 => Node::One(kids(g, nodes)),
            5 => {
                let node = grammar(g, nodes, depth - 1, in_bang);
                let min = g.below(3) as u32;
                let max = if g.chance(50) {
                    None
                } else {
                    Some((min + g.below(3) as u32).max(1))
                };
                Node::Repeat {
                    node,
                    min,
                    max,
                    hash: g.chance(50),
                }
            }
            _ => Node::Required(grammar(g, nodes, depth - 1, true)),
        }
    };
    nodes.push(node);
    (nodes.len() - 1) as NodeId
}

/// The grammar as text, for failure messages.
fn show(nodes: &[Node], n: NodeId) -> String {
    let join = |c: &[NodeId], sep: &str| {
        c.iter()
            .map(|&i| show(nodes, i))
            .collect::<Vec<_>>()
            .join(sep)
    };
    match &nodes[n as usize] {
        Node::Keyword(k) => k.to_string(),
        Node::Delim(c) => format!("'{c}'"),
        Node::Comma => ",".into(),
        Node::Function(name, a) => format!("{name}( {} )", show(nodes, *a)),
        Node::Seq(c) => format!("[ {} ]", join(c, " ")),
        Node::All(c) => format!("[ {} ]", join(c, " && ")),
        Node::Any(c) => format!("[ {} ]", join(c, " || ")),
        Node::One(c) => format!("[ {} ]", join(c, " | ")),
        Node::Repeat {
            node,
            min,
            max,
            hash,
        } => format!(
            "{}{}{{{min},{}}}",
            show(nodes, *node),
            if *hash { "#" } else { "" },
            max.map_or(String::new(), |m| m.to_string())
        ),
        Node::Required(c) => format!("[ {} ]!", show(nodes, *c)),
        other => format!("{other:?}"),
    }
}

// --- Values ----------------------------------------------------------------

/// A component value, as the reference sees it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Item {
    Ident(String),
    Delim(char),
    Comma,
    Func(String, Vec<Item>),
}

fn render(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| match i {
            Item::Ident(s) => s.clone(),
            Item::Delim(c) => c.to_string(),
            Item::Comma => ",".into(),
            Item::Func(n, a) => format!("{n}({})", render(a)),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn random_value(g: &mut Gen, len: usize, depth: u32) -> Vec<Item> {
    (0..len)
        .map(|_| match g.below(10) {
            0..=2 => Item::Ident(if g.chance(15) { "A".into() } else { "a".into() }),
            3 | 4 => Item::Ident("b".into()),
            5 => Item::Ident("c".into()),
            6 | 7 => Item::Comma,
            8 => Item::Delim('/'),
            _ if depth > 0 => {
                let n = g.below(4);
                Item::Func(
                    if g.chance(10) { "g".into() } else { "f".into() },
                    random_value(g, n, depth - 1),
                )
            }
            _ => Item::Comma,
        })
        .collect()
}

// --- The reference ---------------------------------------------------------

/// One symbol of a sentence: a component value the grammar names, a comma
/// written in the grammar (`G`), or a `#` separator (`S`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Sym {
    Kw(&'static str),
    Delim(char),
    Func(NodeId),
    G,
    S,
}

type Sentence = Vec<Sym>;

/// The sentences of a grammar, up to `cap` symbols that aren't grammar
/// commas. A run of grammar commas is kept as one `G`, and a `G` next to an
/// `S` is dropped as it is appended: both come straight from "multiple
/// commas would be adjacent", and doing it as the sentence is built keeps
/// the sets finite.
struct Reference<'n> {
    nodes: &'n [Node],
    cap: usize,
    memo: HashMap<NodeId, BTreeSet<Sentence>>,
}

fn cost(s: &Sentence) -> usize {
    s.iter().filter(|&&x| x != Sym::G).count()
}

fn push(s: &mut Sentence, x: Sym) {
    match (s.last().copied(), x) {
        // Adjacent grammar commas: one of them is omitted.
        (Some(Sym::G), Sym::G) => {}
        // A grammar comma next to a separator comma is omitted.
        (Some(Sym::G), Sym::S) => {
            s.pop();
            s.push(Sym::S);
        }
        (Some(Sym::S), Sym::G) => {}
        _ => s.push(x),
    }
}

fn concat(a: &Sentence, b: &Sentence) -> Sentence {
    let mut s = a.clone();
    for &x in b {
        push(&mut s, x);
    }
    s
}

fn permutations(items: &[NodeId]) -> Vec<Vec<NodeId>> {
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
    fn product(&mut self, a: &BTreeSet<Sentence>, b: &BTreeSet<Sentence>) -> BTreeSet<Sentence> {
        let mut out = BTreeSet::new();
        for x in a {
            for y in b {
                let s = concat(x, y);
                if cost(&s) <= self.cap {
                    out.insert(s);
                }
            }
        }
        out
    }

    fn seq(&mut self, order: &[NodeId]) -> BTreeSet<Sentence> {
        let mut cur: BTreeSet<Sentence> = [Vec::new()].into();
        for &c in order {
            let next = self.sentences(c);
            cur = self.product(&cur, &next);
        }
        cur
    }

    fn sentences(&mut self, n: NodeId) -> BTreeSet<Sentence> {
        if let Some(s) = self.memo.get(&n) {
            return s.clone();
        }
        let one = |x: Sym| -> BTreeSet<Sentence> { [vec![x]].into() };
        let out: BTreeSet<Sentence> = match &self.nodes[n as usize] {
            Node::Keyword(k) => one(Sym::Kw(k)),
            Node::Delim(c) => one(Sym::Delim(*c)),
            Node::Comma => one(Sym::G),
            Node::Function(_, _) => one(Sym::Func(n)),
            Node::Seq(c) => {
                let c = c.clone();
                self.seq(&c)
            }
            Node::One(c) => {
                let c = c.clone();
                c.iter().flat_map(|&i| self.sentences(i)).collect()
            }
            Node::All(c) => {
                let c = c.clone();
                let mut out = BTreeSet::new();
                for p in permutations(&c) {
                    out.extend(self.seq(&p));
                }
                out
            }
            Node::Any(c) => {
                let c = c.clone();
                let mut out = BTreeSet::new();
                for mask in 1..(1usize << c.len()) {
                    let subset: Vec<NodeId> = (0..c.len())
                        .filter(|i| mask & (1 << i) != 0)
                        .map(|i| c[i])
                        .collect();
                    for p in permutations(&subset) {
                        out.extend(self.seq(&p));
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
                let child = self.sentences(node);
                let sep: BTreeSet<Sentence> = if hash {
                    [vec![Sym::S]].into()
                } else {
                    [Vec::new()].into()
                };
                let sep_child = self.product(&sep, &child);
                // Exactly k repetitions, for k up to `min` (or to 1).
                let mut reps: BTreeSet<Sentence> = [Vec::new()].into();
                let mut k = 0;
                let mut out = BTreeSet::new();
                while k < min.max(1) {
                    if k >= min {
                        out.extend(reps.iter().cloned());
                    }
                    let step = if k == 0 { &child } else { &sep_child };
                    reps = self.product(&reps, step);
                    k += 1;
                }
                // Now k = max(min, 1) and `reps` holds exactly k.
                match max {
                    Some(m) => {
                        while k <= m {
                            out.extend(reps.iter().cloned());
                            reps = self.product(&reps, &sep_child);
                            k += 1;
                        }
                    }
                    None => {
                        // k or more: close under one more repetition.
                        let mut frontier: Vec<Sentence> = reps.into_iter().collect();
                        let mut seen: BTreeSet<Sentence> = frontier.iter().cloned().collect();
                        while let Some(x) = frontier.pop() {
                            for y in &sep_child {
                                let s = concat(&x, y);
                                if cost(&s) <= self.cap && seen.insert(s.clone()) {
                                    frontier.push(s);
                                }
                            }
                        }
                        out.extend(seen);
                    }
                }
                out
            }
            Node::Required(c) => self
                .sentences(*c)
                .into_iter()
                .filter(|s| s.iter().any(|x| !matches!(x, Sym::G | Sym::S)))
                .collect(),
            Node::Never => BTreeSet::new(),
            other => panic!("not generated: {other:?}"),
        };
        self.memo.insert(n, out.clone());
        out
    }
}

/// The literal sentences of a list: grammar commas at the start or the
/// end are omitted ("all items preceding/following the comma have been
/// omitted"); the rest are comma tokens.
fn literal(mut s: Sentence) -> Sentence {
    if s.first() == Some(&Sym::G) {
        s.remove(0);
    }
    if s.last() == Some(&Sym::G) {
        s.pop();
    }
    s
}

/// Whether `value` (one list) is a sentence of `top`.
fn reference(nodes: &[Node], top: NodeId, value: &[Item]) -> bool {
    let mut r = Reference {
        nodes,
        cap: value.len(),
        memo: HashMap::new(),
    };
    r.sentences(top)
        .into_iter()
        .map(literal)
        .any(|s| fits(nodes, &s, value))
}

fn fits(nodes: &[Node], s: &Sentence, value: &[Item]) -> bool {
    s.len() == value.len()
        && s.iter().zip(value).all(|(x, v)| match (x, v) {
            (Sym::Kw(k), Item::Ident(i)) => k.eq_ignore_ascii_case(i),
            (Sym::Delim(c), Item::Delim(d)) => c == d,
            (Sym::G | Sym::S, Item::Comma) => true,
            (Sym::Func(f), Item::Func(name, args)) => {
                let Node::Function(fname, inner) = nodes[*f as usize] else {
                    unreachable!()
                };
                fname.eq_ignore_ascii_case(name) && reference(nodes, inner, args)
            }
            _ => false,
        })
}

/// A random sentence of `top`, as a value, if it has one within `cap`.
fn sample(g: &mut Gen, nodes: &[Node], top: NodeId, cap: usize, depth: u32) -> Option<Vec<Item>> {
    let mut r = Reference {
        nodes,
        cap,
        memo: HashMap::new(),
    };
    let all: Vec<Sentence> = r.sentences(top).into_iter().map(literal).collect();
    if all.is_empty() {
        return None;
    }
    let s = &all[g.below(all.len())];
    s.iter()
        .map(|x| {
            Some(match x {
                Sym::Kw(k) => Item::Ident(k.to_string()),
                Sym::Delim(c) => Item::Delim(*c),
                Sym::G | Sym::S => Item::Comma,
                Sym::Func(f) => {
                    let Node::Function(name, inner) = nodes[*f as usize] else {
                        unreachable!()
                    };
                    let args = if depth == 0 {
                        Vec::new()
                    } else {
                        sample(g, nodes, inner, 3, depth - 1).unwrap_or_default()
                    };
                    Item::Func(name.to_string(), args)
                }
            })
        })
        .collect()
}

// --- The test ----------------------------------------------------------------

fn compiled(nodes: Vec<Node>) -> Compiled {
    let n = nodes.len();
    Compiled {
        nodes,
        ctx: vec![Context::NoPercent; n],
        longhands: Vec::new(),
        shorthands: Vec::new(),
        productions: HashMap::new(),
    }
}

fn run(seed: u64) -> (usize, usize) {
    let mut g = Gen(seed | 1);
    let mut nodes = Vec::new();
    let depth = 1 + g.below(3) as u32;
    let top = grammar(&mut g, &mut nodes, depth, false);
    let text = show(&nodes, top);
    let c = compiled(nodes.clone());
    let (mut yes, mut total) = (0, 0);
    for _ in 0..8 {
        let value = if g.chance(50) {
            let cap = 1 + g.below(5);
            match sample(&mut g, &nodes, top, cap, 2) {
                Some(v) => v,
                None => continue,
            }
        } else {
            let len = g.below(6);
            random_value(&mut g, len, 2)
        };
        let want = reference(&nodes, top, &value);
        let source = render(&value);
        let (parsed, range) = parse_component_values(Wtf8::from_str(&source));
        let got = matches_list(&c, top, parsed.values(range), 0);
        assert_eq!(
            got, want,
            "seed {seed}\ngrammar: {text}\nvalue: {source}\nmatcher says {got}, reference {want}"
        );
        total += 1;
        yes += want as usize;
    }
    (yes, total)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(3000)
    ))]
    #[test]
    fn matcher_matches_the_literal_reading(seed in any::<u64>()) {
        run(seed);
    }
}

/// The cases aren't nearly all one answer: a reference that said "no" to
/// everything would pass the property test above.
#[test]
fn both_answers_are_common() {
    let (mut yes, mut total) = (0, 0);
    for seed in 0..2000u64 {
        let (y, t) = run(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        yes += y;
        total += t;
    }
    assert!(total > 10_000, "{total} cases");
    assert!(
        yes * 5 > total && yes * 5 < total * 4,
        "{yes} of {total} valid"
    );
}

/// Hand-worked cases of the reference itself, so a mistake in it doesn't
/// hide one in the matcher.
#[test]
fn reference_hand_worked() {
    let mut nodes = Vec::new();
    let mut add = |n: Node| {
        nodes.push(n);
        (nodes.len() - 1) as NodeId
    };
    let a = add(Node::Keyword("a"));
    let b = add(Node::Keyword("b"));
    let comma = add(Node::Comma);
    let opt_a = add(Node::Repeat {
        node: a,
        min: 0,
        max: Some(1),
        hash: false,
    });
    let opt_b = add(Node::Repeat {
        node: b,
        min: 0,
        max: Some(1),
        hash: false,
    });
    // a? , b?
    let seq = add(Node::Seq([opt_a, comma, opt_b].into()));
    // a#
    let list = add(Node::Repeat {
        node: a,
        min: 1,
        max: None,
        hash: true,
    });
    // [ a? , b? ]#
    let seq_list = add(Node::Repeat {
        node: seq,
        min: 1,
        max: None,
        hash: true,
    });
    let v = |s: &str| -> Vec<Item> {
        s.split(' ')
            .filter(|x| !x.is_empty())
            .map(|x| {
                if x == "," {
                    Item::Comma
                } else {
                    Item::Ident(x.into())
                }
            })
            .collect()
    };
    let check = |top, s: &str, want: bool| {
        assert_eq!(
            reference(&nodes, top, &v(s)),
            want,
            "{} on {s:?}",
            show(&nodes, top)
        );
        let c = compiled(nodes.clone());
        let (parsed, range) = parse_component_values(Wtf8::from_str(s));
        assert_eq!(
            matches_list(&c, top, parsed.values(range), 0),
            want,
            "matcher: {s:?}"
        );
    };
    check(seq, "a , b", true);
    check(seq, "a", true);
    check(seq, "b", true);
    check(seq, "", true);
    check(seq, "a b", false);
    check(seq, ", b", false);
    check(seq, "a ,", false);
    check(list, "a , a , a", true);
    check(list, "a a", false);
    check(list, "a ,", false);
    // The separator absorbs the grammar comma next to it.
    check(seq_list, "a , b , a , b", true);
    // An empty repetition still has its separators: the comma rule omits
    // only commas written in the grammar.
    check(seq_list, "a , b , , a", true);
    check(seq_list, "a , b , a b", false);
    check(seq_list, "a , a", true);
    check(seq_list, "b , b", true);
}
