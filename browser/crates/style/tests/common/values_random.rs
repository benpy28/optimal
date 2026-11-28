//! Random grammars and values for the value grammar reference tests
//! (`values_matcher_reference.rs`, `values_parse_reference.rs`).

#![allow(dead_code)]

use optimal_style::values::Compiled;
use optimal_style::values::math::Context;
use optimal_style::values::syntax::{Node, NodeId};
use optimal_style::values::types::{Bounds, Builtin};
use std::collections::HashMap;

/// A small deterministic generator (xorshift), so a failing case is
/// replayed from its seed.
pub struct Gen(pub u64);

impl Gen {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

// --- Random grammars -------------------------------------------------------

/// Builds a random grammar node of at most `depth` levels into `nodes`.
/// No `!` inside `!` (the compiler rejects it), at most 3 children per
/// group.
pub fn grammar(g: &mut Gen, nodes: &mut Vec<Node>, depth: u32, in_bang: bool) -> NodeId {
    let node = if depth == 0 || g.chance(30) {
        match g.below(9) {
            0 | 1 => Node::Keyword("a"),
            2 => Node::Type(Builtin::CustomIdent(&[]), Bounds::ANY),
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
pub fn show(nodes: &[Node], n: NodeId) -> String {
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
        Node::Type(..) => "<custom-ident>".into(),
        other => format!("{other:?}"),
    }
}

// --- Values ----------------------------------------------------------------

/// A component value, as the reference sees it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Ident(String),
    Delim(char),
    Comma,
    Func(String, Vec<Item>),
}

pub fn render(items: &[Item]) -> String {
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

pub fn random_value(g: &mut Gen, len: usize, depth: u32) -> Vec<Item> {
    (0..len)
        .map(|_| match g.below(10) {
            0..=2 => Item::Ident(if g.chance(15) { "A".into() } else { "a".into() }),
            3 | 4 => Item::Ident("b".into()),
            5 => Item::Ident(if g.chance(20) { "inherit" } else { "c" }.into()),
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

pub fn compiled(nodes: Vec<Node>) -> Compiled {
    let n = nodes.len();
    Compiled {
        nodes,
        ctx: vec![Context::NoPercent; n],
        longhands: Vec::new(),
        shorthands: Vec::new(),
        productions: HashMap::new(),
        rules: HashMap::new(),
    }
}
