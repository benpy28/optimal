//! Matching component values against a compiled grammar.
//!
//! A grammar describes a set of component value sequences, and a value is
//! valid when it is in that set (css-values-4 2). The matcher decides that
//! exactly: it runs the grammar over sets of states, where a state is a
//! position in the value plus what the implicit-comma rule and `!` need to
//! know, so every way of matching is followed at once and nothing is
//! guessed. Rejected: backtracking (exponential on ambiguous grammars
//! such as `<bg-position>`) and greedy matching (rejects values a later
//! component needed, which the spec doesn't allow).
//!
//! - A **state** is (position, comma flag, `!` flag). The comma flag
//!   carries the rule for commas written in a grammar (2.1): a comma is
//!   omitted when nothing before it in the list was matched, when nothing
//!   after it is, or when it would be next to another comma. So passing a
//!   grammar comma doesn't consume anything: it marks a comma as pending
//!   if something came before, and the next component matched in the same
//!   list must first consume a comma token. A pending comma at the end of
//!   the list is dropped. A `#` separator is a comma too, so a grammar
//!   comma next to one is omitted. The rule is scoped to a top-level value or a
//!   function's arguments, which are matched as their own lists.
//! - Every combinator is a function from a set of states to a set of
//!   states, and distributes over union, so `*`, `+`, `#` and `{A,}`
//!   iterate on the states not seen before (each state is expanded once,
//!   keeping long values linear), and `&&` and `||` run a subset walk over
//!   their children (2^k sets for k children; the compiler caps k).
//! - Functions and `()`-blocks are single component values whose contents
//!   are matched as a new list, once per (value, grammar node): memoised.

use optimal_css::{BlockKind, ComponentValue, Kind, Token, Values};
use std::collections::HashMap;

use super::syntax::{Node, NodeId};
use super::{Compiled, MAX_NESTING, types};

const NOTHING: u32 = 0;
const EMITTED: u32 = 1;
const PENDING: u32 = 2;
/// A `#` separator comma was just consumed: a grammar comma right after
/// it is adjacent to it, so omitted.
const AFTER_SEPARATOR: u32 = 3;
const COMMA_MASK: u32 = 3;
const BANG: u32 = 4;
const SHIFT: u32 = 3;

fn pos(s: u32) -> usize {
    (s >> SHIFT) as usize
}

fn state(pos: usize, comma: u32, bang: u32) -> u32 {
    ((pos as u32) << SHIFT) | comma | bang
}

fn normalize(v: &mut Vec<u32>) {
    v.sort_unstable();
    v.dedup();
}

struct Run<'g, 't, 'a> {
    g: &'g Compiled,
    items: Vec<ComponentValue<'t, 'a>>,
    depth: u32,
    memo: HashMap<(usize, NodeId), bool>,
}

impl<'t, 'a> Run<'_, 't, 'a> {
    fn is_comma(&self, p: usize) -> bool {
        matches!(
            self.items.get(p).map(|v| v.kind()),
            Some(Kind::Token(Token::Comma))
        )
    }

    /// Whether the single component value at `p` matches a terminal node.
    fn terminal(&mut self, node: NodeId, p: usize) -> bool {
        let v = self.items[p];
        match &self.g.nodes[node as usize] {
            Node::Keyword(k) => {
                matches!(v.kind(), Kind::Token(Token::Ident(id)) if id.eq_ignore_ascii_case(k))
            }
            Node::Delim(c) => match (c, v.kind()) {
                (':', Kind::Token(Token::Colon)) | (';', Kind::Token(Token::Semicolon)) => true,
                (c, Kind::Token(Token::Delim(d))) => c == d,
                _ => false,
            },
            Node::Type(b, bounds) => {
                types::matches(*b, *bounds, v, self.g.ctx[node as usize], self.depth)
            }
            Node::Function(name, args) => {
                let Kind::Function(fname, inner) = v.kind() else {
                    return false;
                };
                if !fname.eq_ignore_ascii_case(name) {
                    return false;
                }
                let args = *args;
                self.nested(node, p, args, inner)
            }
            Node::Block(inner_node) => {
                let Kind::Block(BlockKind::Paren, inner) = v.kind() else {
                    return false;
                };
                let n = *inner_node;
                self.nested(node, p, n, inner)
            }
            _ => unreachable!("not a terminal"),
        }
    }

    fn nested(&mut self, node: NodeId, p: usize, top: NodeId, inner: Values<'t, 'a>) -> bool {
        if let Some(&r) = self.memo.get(&(p, node)) {
            return r;
        }
        let r = self.depth < MAX_NESTING && matches_list(self.g, top, inner, self.depth + 1);
        self.memo.insert((p, node), r);
        r
    }

    /// A terminal step: each state consumes its pending comma, if any,
    /// then one component value matching `node`.
    fn step(&mut self, node: NodeId, states: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        for &s in states {
            let mut p = pos(s);
            if s & COMMA_MASK == PENDING {
                if !self.is_comma(p) {
                    continue;
                }
                p += 1;
            }
            if p < self.items.len() && self.terminal(node, p) {
                out.push(state(p + 1, EMITTED, BANG));
            }
        }
        normalize(&mut out);
        out
    }

    /// The comma between repetitions of `#`: a comma token. A grammar
    /// comma right before or right after it is adjacent to it, so the same
    /// comma (2.1).
    fn separator(&self, states: &[u32]) -> Vec<u32> {
        let mut out: Vec<u32> = states
            .iter()
            .filter(|&&s| self.is_comma(pos(s)))
            .map(|&s| state(pos(s) + 1, AFTER_SEPARATOR, s & BANG))
            .collect();
        normalize(&mut out);
        out
    }

    fn run(&mut self, node: NodeId, states: &[u32]) -> Vec<u32> {
        if states.is_empty() {
            return Vec::new();
        }
        match &self.g.nodes[node as usize] {
            Node::Keyword(_)
            | Node::Delim(_)
            | Node::Type(..)
            | Node::Function(..)
            | Node::Block(_) => self.step(node, states),
            Node::Comma => {
                let mut out: Vec<u32> = states
                    .iter()
                    .map(|&s| match s & COMMA_MASK {
                        NOTHING | AFTER_SEPARATOR => s,
                        _ => (s & !COMMA_MASK) | PENDING,
                    })
                    .collect();
                normalize(&mut out);
                out
            }
            Node::Never => Vec::new(),
            Node::Ref(n) => {
                let n = *n;
                self.run(n, states)
            }
            Node::Name(..) => unreachable!("names are resolved by the compiler"),
            Node::Seq(items) => {
                let items = items.clone();
                let mut cur = states.to_vec();
                for &i in items.iter() {
                    cur = self.run(i, &cur);
                    if cur.is_empty() {
                        break;
                    }
                }
                cur
            }
            Node::One(items) => {
                let items = items.clone();
                let mut out = Vec::new();
                for &i in items.iter() {
                    out.extend(self.run(i, states));
                }
                normalize(&mut out);
                out
            }
            Node::All(items) | Node::Any(items) => {
                let all = matches!(self.g.nodes[node as usize], Node::All(_));
                let items = items.clone();
                let k = items.len();
                let full = (1usize << k) - 1;
                let mut t: Vec<Vec<u32>> = vec![Vec::new(); full + 1];
                t[0] = states.to_vec();
                let mut out = Vec::new();
                for mask in 0..=full {
                    let mut cur = std::mem::take(&mut t[mask]);
                    normalize(&mut cur);
                    if cur.is_empty() {
                        continue;
                    }
                    for (c, &child) in items.iter().enumerate() {
                        if mask & (1 << c) == 0 {
                            let next = self.run(child, &cur);
                            t[mask | (1 << c)].extend(next);
                        }
                    }
                    if mask == full || (!all && mask != 0) {
                        out.extend_from_slice(&cur);
                    }
                }
                normalize(&mut out);
                out
            }
            &Node::Repeat {
                node: child,
                min,
                max,
                hash,
            } => self.repeat(child, min, max, hash, states),
            Node::Required(inner) => {
                let inner = *inner;
                let cleared: Vec<u32> = states.iter().map(|&s| s & !BANG).collect();
                let mut cleared = cleared;
                normalize(&mut cleared);
                let mut out: Vec<u32> = self
                    .run(inner, &cleared)
                    .into_iter()
                    .filter(|&s| s & BANG != 0)
                    .map(|s| s & !BANG)
                    .collect();
                normalize(&mut out);
                out
            }
        }
    }

    fn once(&mut self, child: NodeId, sep: bool, states: &[u32]) -> Vec<u32> {
        if sep {
            let s = self.separator(states);
            self.run(child, &s)
        } else {
            self.run(child, states)
        }
    }

    fn repeat(
        &mut self,
        child: NodeId,
        min: u32,
        max: Option<u32>,
        hash: bool,
        states: &[u32],
    ) -> Vec<u32> {
        let mut cur = states.to_vec();
        for i in 0..min {
            cur = self.once(child, hash && i > 0, &cur);
            if cur.is_empty() {
                return cur;
            }
        }
        let mut out = cur.clone();
        let mut count = min;
        match max {
            Some(max) => {
                while count < max && !cur.is_empty() {
                    cur = self.once(child, hash && count > 0, &cur);
                    out.extend_from_slice(&cur);
                    count += 1;
                }
                normalize(&mut out);
            }
            None => {
                // The states after one or more repetitions: a state there
                // goes on with a separator (for `#`), while the same state
                // after none goes on without, so the two are kept apart.
                let mut seen = if count == 0 {
                    self.once(child, false, &cur)
                } else {
                    cur
                };
                normalize(&mut seen);
                // Expand each state once: `seen` holds every state reached.
                let mut frontier = seen.clone();
                while !frontier.is_empty() {
                    let next = self.once(child, hash, &frontier);
                    frontier = next
                        .into_iter()
                        .filter(|s| seen.binary_search(s).is_err())
                        .collect();
                    seen.extend_from_slice(&frontier);
                    normalize(&mut seen);
                }
                out.extend(seen);
                normalize(&mut out);
            }
        }
        out
    }
}

/// Whether `values` (one list: a whole value, or a function's arguments)
/// matches grammar node `top`. `depth` counts the functions and blocks
/// around the list.
pub fn matches_list(g: &Compiled, top: NodeId, values: Values<'_, '_>, depth: u32) -> bool {
    matches_items(g, top, values, depth)
}

/// [`matches_list`] on any sequence of component values.
pub fn matches_items<'t, 'a: 't>(
    g: &Compiled,
    top: NodeId,
    values: impl IntoIterator<Item = ComponentValue<'t, 'a>>,
    depth: u32,
) -> bool {
    let items: Vec<ComponentValue<'t, 'a>> = values
        .into_iter()
        .filter(|v| !matches!(v.kind(), Kind::Token(Token::Whitespace)))
        .collect();
    let n = items.len();
    let mut run = Run {
        g,
        items,
        depth,
        memo: HashMap::new(),
    };
    let end = run.run(top, &[state(0, NOTHING, 0)]);
    end.iter().any(|&s| pos(s) == n)
}
