//! Parse trees: which way a value matched its grammar.
//!
//! The matcher ([`super::matcher`]) only decides whether a value is in
//! the set a grammar describes. Computing a value needs more: which
//! alternative of a `|` matched, which child of a `||` took which
//! component values, how many repetitions a `#` made. When a value can be
//! matched in more than one way, the parse picks one by these rules,
//! applied left to right and from the outside in:
//!
//! 1. **Greedy.** Each component takes as many component values as it can
//!    while the whole value still matches. css-values-4 4.5 says so for
//!    `<position>` ("greedily parsed; it consumes as many components as
//!    possible"), and the property specs read their grammars the same way
//!    (`<length>{1,2} <length>?` gives `1px 2px` to the first).
//! 2. **Keywords before `<custom-ident>`.** Among the choices that take the
//!    same component values, one that can't reach a `<custom-ident>` comes
//!    first: "a `<custom-ident>` production can only claim the keyword if
//!    no other unfulfilled production can claim it" (css-values-4 4.2,
//!    with the `animation: ease-in ease-out` example).
//! 3. **Grammar order** otherwise: the first alternative of a `|`, the
//!    first child of a `||` or `&&`.
//! 4. A group or repetition **stops** rather than go on with a child that
//!    takes nothing, unless it must (an `&&` child, a repetition below its
//!    minimum, or a step that only moves past a grammar comma). Such a
//!    step in a repetition must bring it closer to its end.
//!
//! How (chosen, why, rejected): the parse reuses the matcher's state sets.
//! For a node, a start state and a set of acceptable end states, it lays
//! out the configurations the node can pass through (item index for
//! juxtaposition, used-children mask for `&&` and `||`, repetition count
//! for multipliers), keeps those from which an acceptable end is
//! reachable (one backward pass), and walks forward choosing by the rules
//! above, recursing into the chosen child with the end states that choice
//! allows. Each (node, state) is run once (memoised), so a parse costs
//! about what matching did. Rejected: backtracking (exponential, as for
//! matching), and recording parse pointers in the matcher itself (it
//! would make every match pay for trees only valid values need).

use optimal_css::{BlockKind, ComponentValue, Kind, Values};
use std::collections::{HashMap, HashSet, VecDeque};

use super::matcher::{BANG, COMMA_MASK, EMITTED, NOTHING, PENDING, Run, pos, state};
use super::syntax::{Node, NodeId};
use super::types::Builtin;
use super::{Compiled, MAX_NESTING};

/// How one grammar node matched.
#[derive(Clone, Debug)]
pub struct Tree<'t, 'a> {
    /// The grammar node.
    pub node: NodeId,
    /// What it matched.
    pub kind: TreeKind<'t, 'a>,
}

/// What a grammar node matched (see [`Tree`]).
#[derive(Clone, Debug)]
pub enum TreeKind<'t, 'a> {
    /// A keyword, delimiter or basic type: the component value.
    Value(ComponentValue<'t, 'a>),
    /// A function or `()`-block: the component value, and how its
    /// contents matched the argument grammar.
    Nested(ComponentValue<'t, 'a>, Box<Tree<'t, 'a>>),
    /// A comma written in the grammar (the comma token, if there is one,
    /// is matched with the component after it).
    Comma,
    /// A reference or a `!` group: how the node it holds matched.
    Inner(Box<Tree<'t, 'a>>),
    /// A `|`: the index of the alternative that matched, and how.
    Alternative(usize, Box<Tree<'t, 'a>>),
    /// Juxtaposition (every item, in order), `&&` and `||` (the children
    /// that matched, in the order of the value), or a multiplier (one per
    /// repetition): each with its index among the node's children (0 for
    /// a repetition).
    List(Vec<(usize, Tree<'t, 'a>)>),
}

/// The parse of `values` against grammar node `top`, or `None` if it
/// doesn't match. `depth` counts the functions and blocks around the
/// list.
pub fn parse<'t, 'a: 't>(
    g: &Compiled,
    top: NodeId,
    values: impl IntoIterator<Item = ComponentValue<'t, 'a>>,
    depth: u32,
) -> Option<Tree<'t, 'a>> {
    let mut p = Parser {
        run: Run::new(g, values, depth),
        out: HashMap::new(),
        custom: HashMap::new(),
    };
    let n = p.run.items.len();
    let start = state(0, NOTHING, 0);
    let ends: Vec<u32> = p
        .out(top, start)
        .iter()
        .copied()
        .filter(|&s| pos(s) == n)
        .collect();
    if ends.is_empty() {
        return None;
    }
    Some(p.extract(top, start, &ends).0)
}

/// [`parse`] for a whole value of the property whose grammar is `top`.
pub fn parse_list<'t, 'a: 't>(
    g: &Compiled,
    top: NodeId,
    values: Values<'t, 'a>,
) -> Option<Tree<'t, 'a>> {
    parse(g, top, values, 0)
}

struct Parser<'g, 't, 'a> {
    run: Run<'g, 't, 'a>,
    /// The end states of each (node, start state).
    out: HashMap<(NodeId, u32), Vec<u32>>,
    /// Whether a node can reach a `<custom-ident>` in its list.
    custom: HashMap<NodeId, bool>,
}

/// The states of `v` that are in the sorted `set`.
fn within(v: &[u32], set: &[u32]) -> Vec<u32> {
    v.iter()
        .copied()
        .filter(|s| set.binary_search(s).is_ok())
        .collect()
}

/// The states of `v` at its greatest position.
fn furthest(v: &[u32]) -> Vec<u32> {
    let Some(m) = v.iter().map(|&s| pos(s)).max() else {
        return Vec::new();
    };
    let mut c: Vec<u32> = v.iter().copied().filter(|&s| pos(s) == m).collect();
    c.sort_unstable();
    c.dedup();
    c
}

/// One configuration of a group or multiplier walk: a step count or mask
/// and a state.
type Config = (u32, u32);

/// The configurations of a walk from which an accepting one is
/// reachable, with the fewest steps it takes.
struct Layout {
    alive: HashMap<Config, u32>,
}

impl<'t, 'a> Parser<'_, 't, 'a> {
    fn out(&mut self, node: NodeId, s: u32) -> &[u32] {
        if !self.out.contains_key(&(node, s)) {
            let v = self.run.run(node, &[s]);
            self.out.insert((node, s), v);
        }
        &self.out[&(node, s)]
    }

    /// `node` once more in a repetition: after a separator if `sep`.
    fn once(&mut self, child: NodeId, sep: bool, s: u32) -> Vec<u32> {
        if !sep {
            return self.out(child, s).to_vec();
        }
        let mut v = Vec::new();
        for t in self.run.separator(&[s]) {
            v.extend_from_slice(self.out(child, t));
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    fn claims_custom_ident(&mut self, node: NodeId) -> bool {
        if let Some(&b) = self.custom.get(&node) {
            return b;
        }
        // The compiler refused cycles within a list, so this ends.
        let g = self.run.g;
        let b = match &g.nodes[node as usize] {
            Node::Type(Builtin::CustomIdent(_), _) => true,
            Node::Ref(c) | Node::Required(c) => self.claims_custom_ident(*c),
            Node::Repeat { node, .. } => self.claims_custom_ident(*node),
            Node::Seq(c) | Node::All(c) | Node::Any(c) | Node::One(c) => {
                c.iter().any(|&c| self.claims_custom_ident(c))
            }
            _ => false,
        };
        self.custom.insert(node, b);
        b
    }

    /// Children in preference order for rules 2 and 3.
    fn ranked(&mut self, kids: &[NodeId]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..kids.len()).collect();
        let claims: Vec<bool> = kids.iter().map(|&k| self.claims_custom_ident(k)).collect();
        order.sort_by_key(|&i| claims[i]);
        order
    }

    /// Lays out a walk from `start` with `next` giving each
    /// configuration's successors and `accept` the accepting ones.
    fn layout(
        &mut self,
        start: Config,
        mut next: impl FnMut(&mut Self, Config) -> Vec<Config>,
        accept: impl Fn(Config) -> bool,
    ) -> Layout {
        let mut edges: HashMap<Config, Vec<Config>> = HashMap::new();
        let mut todo = vec![start];
        let mut seen: HashSet<Config> = [start].into();
        while let Some(c) = todo.pop() {
            let succ = next(self, c);
            for &d in &succ {
                if seen.insert(d) {
                    todo.push(d);
                }
            }
            edges.insert(c, succ);
        }
        let mut preds: HashMap<Config, Vec<Config>> = HashMap::new();
        for (&c, succ) in &edges {
            for &d in succ {
                preds.entry(d).or_default().push(c);
            }
        }
        // Backward breadth-first from the accepting configurations.
        let mut alive: HashMap<Config, u32> = HashMap::new();
        let mut queue = VecDeque::new();
        for &c in edges.keys() {
            if accept(c) {
                alive.insert(c, 0);
                queue.push_back(c);
            }
        }
        while let Some(c) = queue.pop_front() {
            let d = alive[&c] + 1;
            for &p in preds.get(&c).map_or(&[][..], |v| v) {
                if let std::collections::hash_map::Entry::Vacant(e) = alive.entry(p) {
                    e.insert(d);
                    queue.push_back(p);
                }
            }
        }
        Layout { alive }
    }

    /// The parse of `node` from state `s` to one of the sorted states
    /// `ends` (all at one position, all reachable), and the end state.
    fn extract(&mut self, node: NodeId, s: u32, ends: &[u32]) -> (Tree<'t, 'a>, u32) {
        let g = self.run.g;
        let tree = |kind| Tree { node, kind };
        match &g.nodes[node as usize] {
            Node::Keyword(_)
            | Node::Delim(_)
            | Node::Type(..)
            | Node::Function(..)
            | Node::Block(_) => {
                let mut p = pos(s);
                if s & COMMA_MASK == PENDING {
                    p += 1;
                }
                let end = state(p + 1, EMITTED, BANG);
                debug_assert!(ends.contains(&end));
                let v = self.run.items[p];
                let kind = match (&g.nodes[node as usize], v.kind()) {
                    (Node::Function(_, args), Kind::Function(_, inner))
                    | (Node::Block(args), Kind::Block(BlockKind::Paren, inner)) => {
                        let depth = self.run.depth + 1;
                        debug_assert!(depth <= MAX_NESTING);
                        let inner = parse(g, *args, inner, depth).expect("the matcher matched it");
                        TreeKind::Nested(v, Box::new(inner))
                    }
                    _ => TreeKind::Value(v),
                };
                (tree(kind), end)
            }
            Node::Comma => {
                let end = self.out(node, s)[0];
                (tree(TreeKind::Comma), end)
            }
            Node::Ref(c) => {
                let (t, end) = self.extract(*c, s, ends);
                (tree(TreeKind::Inner(Box::new(t))), end)
            }
            Node::Required(c) => {
                let c = *c;
                let inner: Vec<u32> = self
                    .out(c, s & !BANG)
                    .iter()
                    .copied()
                    .filter(|&x| x & BANG != 0 && ends.binary_search(&(x & !BANG)).is_ok())
                    .collect();
                let (t, end) = self.extract(c, s & !BANG, &furthest(&inner));
                (tree(TreeKind::Inner(Box::new(t))), end & !BANG)
            }
            Node::One(c) => {
                let c = c.clone();
                for i in self.ranked(&c) {
                    let hits = within(self.out(c[i], s), ends);
                    if !hits.is_empty() {
                        let (t, end) = self.extract(c[i], s, &furthest(&hits));
                        return (tree(TreeKind::Alternative(i, Box::new(t))), end);
                    }
                }
                unreachable!("an alternative reaches the end")
            }
            Node::Seq(c) => {
                let c = c.clone();
                let k = c.len() as u32;
                let layout = self.layout(
                    (0, s),
                    |p, (i, u)| {
                        if i == k {
                            return Vec::new();
                        }
                        p.out(c[i as usize], u)
                            .iter()
                            .map(|&v| (i + 1, v))
                            .collect()
                    },
                    |(i, u)| i == k && ends.binary_search(&u).is_ok(),
                );
                let mut items = Vec::new();
                let mut u = s;
                for (i, &child) in c.iter().enumerate() {
                    let next: Vec<u32> = self
                        .out(child, u)
                        .iter()
                        .copied()
                        .filter(|&v| layout.alive.contains_key(&(i as u32 + 1, v)))
                        .collect();
                    let (t, end) = self.extract(child, u, &furthest(&next));
                    items.push((i, t));
                    u = end;
                }
                (tree(TreeKind::List(items)), u)
            }
            Node::All(c) | Node::Any(c) => {
                let all = matches!(g.nodes[node as usize], Node::All(_));
                let c = c.clone();
                let full = (1u32 << c.len()) - 1;
                let accept = |(mask, u): Config| {
                    (mask == full || (!all && mask != 0)) && ends.binary_search(&u).is_ok()
                };
                let layout = self.layout(
                    (0, s),
                    |p, (mask, u)| {
                        let mut v = Vec::new();
                        for (i, &child) in c.iter().enumerate() {
                            if mask & (1 << i) == 0 {
                                v.extend(p.out(child, u).iter().map(|&t| (mask | 1 << i, t)));
                            }
                        }
                        v
                    },
                    accept,
                );
                let order = self.ranked(&c);
                let mut items = Vec::new();
                let (mut mask, mut u) = (0u32, s);
                loop {
                    // The furthest each unused child can go and stay alive.
                    let mut best: Option<(usize, Vec<u32>)> = None;
                    for &i in &order {
                        if mask & (1 << i) != 0 {
                            continue;
                        }
                        let next: Vec<u32> = self
                            .out(c[i], u)
                            .iter()
                            .copied()
                            .filter(|&v| layout.alive.contains_key(&(mask | 1 << i, v)))
                            .collect();
                        let next = furthest(&next);
                        let Some(&first) = next.first() else { continue };
                        if best.as_ref().is_none_or(|(_, b)| pos(first) > pos(b[0])) {
                            best = Some((i, next));
                        }
                    }
                    // The ends are at one position, so once the walk may
                    // stop, a child could only take nothing: stop (rule 4).
                    let stop = accept((mask, u));
                    match best {
                        Some((i, next)) if !stop => {
                            let (t, end) = self.extract(c[i], u, &next);
                            items.push((i, t));
                            mask |= 1 << i;
                            u = end;
                        }
                        _ => {
                            debug_assert!(stop);
                            break;
                        }
                    }
                }
                (tree(TreeKind::List(items)), u)
            }
            &Node::Repeat {
                node: child,
                min,
                max,
                hash,
            } => {
                let cap = max.unwrap_or(min.max(1));
                let layout = self.layout(
                    (0, s),
                    |p, (k, u)| {
                        if max.is_some_and(|m| k >= m) {
                            return Vec::new();
                        }
                        let next = (k + 1).min(cap);
                        p.once(child, hash && k > 0, u)
                            .into_iter()
                            .map(|v| (next, v))
                            .collect()
                    },
                    |(k, u)| k >= min && ends.binary_search(&u).is_ok(),
                );
                let mut items = Vec::new();
                let (mut k, mut u) = (0u32, s);
                loop {
                    let stop = k >= min && ends.binary_search(&u).is_ok();
                    let can_go = !max.is_some_and(|m| k >= m);
                    let next_k = (k + 1).min(cap);
                    let next: Vec<u32> = if can_go {
                        self.once(child, hash && k > 0, u)
                            .into_iter()
                            .filter(|&v| layout.alive.contains_key(&(next_k, v)))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let mut next = furthest(&next);
                    // A step that takes nothing must get closer to an
                    // end, so the walk can't go round in circles (a child
                    // that only passes a grammar comma); the closest one
                    // always is.
                    if next.first().is_some_and(|&v| pos(v) == pos(u)) {
                        let here = layout.alive[&(k, u)];
                        next.retain(|&v| layout.alive[&(next_k, v)] < here);
                    }
                    // As for && and ||: a walk that may stop is at the end.
                    if stop {
                        break;
                    }
                    assert!(!next.is_empty(), "an alive configuration leads on");
                    // The separator, if any, is part of this repetition.
                    let from = if hash && k > 0 {
                        self.run.separator(&[u])[0]
                    } else {
                        u
                    };
                    let hits = within(self.out(child, from), &next);
                    let (t, end) = self.extract(child, from, &hits);
                    items.push((0, t));
                    k = next_k;
                    u = end;
                }
                (tree(TreeKind::List(items)), u)
            }
            Node::Never => unreachable!("matches nothing"),
            Node::Name(..) => unreachable!("names are resolved by the compiler"),
        }
    }
}
