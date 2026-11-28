//! Property values: checking a declaration's value against its property's
//! grammar (CSS Values and Units Level 4, csswg-drafts commit 7027ab6,
//! with the value grammars of the specs the property table cites).
//!
//! - [`syntax`] reads the value definition syntax (css-values-4 2).
//! - [`table`] holds the productions the property grammars use, each
//!   copied from the spec that defines it; `tests/value_grammars.rs` checks
//!   every one, and every property grammar, against the spec text.
//! - [`types`] matches the basic data types (css-values-4 4 to 8, and the
//!   color keywords of css-color-4), [`math`] types math functions
//!   (css-values-4 10).
//! - [`matcher`] decides whether a value is in the set a grammar
//!   describes.
//!
//! [`compile`] builds every grammar once, on first use. [`value_matches`]
//! is what [`crate::property::classify`] asks; [`parse_value`] says how a
//! value matched ([`parse`]).
//!
//! Design choices (chosen, why, rejected):
//!
//! - **Grammars are read from the spec's own text** (copied into the
//!   tables and checked against the specs), not hand-translated into
//!   parsing code, so each grammar can be compared with its source and a
//!   spec change shows up as a test failure. Rejected: a hand-written
//!   parser per property, which can't be checked against the spec
//!   mechanically.
//! - **Exact matching over state sets** (see [`matcher`]): a value is valid
//!   exactly when the grammar allows it.
//! - **Unsupported features match nothing** ([`table::Def::Unsupported`]):
//!   a value that needs one is invalid, as in a browser without the
//!   feature, and the table says why.
//! - **One level per production.** Where two levels of a spec define a
//!   production, the table copies one and cites it: css-images-4 for
//!   `<image>` and the gradient syntaxes, with css-images-3 for the
//!   gradient functions themselves (css-images-4 extends their syntax
//!   productions without restating them), and the level the property
//!   table cites elsewhere.
//! - **Nesting is limited** to [`MAX_NESTING`] functions and blocks. For
//!   math functions the spec allows a limit of at least 32 (10.8); for
//!   the other functions (only `palette-mix()` and `image-set()` can nest
//!   in the grammars here) it is this engine's limit, so matching stays
//!   within a fixed stack.

pub mod matcher;
pub mod math;
pub mod parse;
pub mod syntax;
pub mod table;
pub mod types;

use optimal_css::{ComponentValue, Values};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::property::{LONGHANDS, SHORTHANDS, Target};
use math::Context;
use syntax::{Grammar, Node, NodeId, SyntaxError};
use table::{Def, PRODUCTIONS};
use types::{Base, Bounds, Builtin};

/// The deepest nesting of functions and blocks a value may have.
pub const MAX_NESTING: u32 = 64;

/// The most children an `&&` or `||` group may have (its subset walk has
/// 2^k steps).
pub const MAX_REORDERABLE: usize = 12;

/// Every grammar, compiled.
#[derive(Debug)]
pub struct Compiled {
    /// The nodes; no [`Node::Name`] is left.
    pub nodes: Vec<Node>,
    /// The calculation context of each [`Node::Type`] (unused elsewhere).
    pub ctx: Vec<Context>,
    /// The top node of each longhand's grammar, by [`LONGHANDS`] index.
    pub longhands: Vec<NodeId>,
    /// The top node of each shorthand's grammar, by [`SHORTHANDS`] index.
    pub shorthands: Vec<NodeId>,
    /// The top node of each production, by name.
    pub productions: HashMap<&'static str, NodeId>,
}

/// Why the grammars couldn't be compiled. Always a mistake in the tables;
/// the tests compile them.
#[derive(Clone, Debug, PartialEq)]
pub enum CompileError {
    /// A grammar's text couldn't be read.
    Syntax(SyntaxError),
    /// A name no production, property or basic type defines.
    Undefined(&'static str),
    /// A range on a reference that isn't a basic numeric type, or a range
    /// on a relative length that bounds more than its sign.
    BadRange(&'static str),
    /// References that loop without passing through a function or block,
    /// so a grammar would be infinite.
    Cycle(&'static str),
    /// An `&&` or `||` group with more than [`MAX_REORDERABLE`] children.
    TooManyReorderable,
    /// A `!` inside another `!`.
    NestedRequired,
    /// A basic type whose `|` alternatives give it two calculation
    /// contexts.
    ConflictingContext(NodeId),
}

/// The basic data types, by the name a grammar writes them with.
const BASIC: &[(&str, Builtin)] = &[
    (
        "_length-percentage-with-unit",
        Builtin::LengthPercentageWithUnit,
    ),
    ("angle", Builtin::Angle),
    ("angle-percentage", Builtin::AnglePercentage),
    ("custom-ident", Builtin::CustomIdent(&[])),
    ("dashed-ident", Builtin::DashedIdent),
    ("frequency", Builtin::Frequency),
    ("frequency-percentage", Builtin::FrequencyPercentage),
    ("ident", Builtin::Ident),
    ("integer", Builtin::Integer),
    ("length", Builtin::Length),
    ("length-percentage", Builtin::LengthPercentage),
    ("number", Builtin::Number),
    ("percentage", Builtin::Percentage),
    ("resolution", Builtin::Resolution),
    ("string", Builtin::String),
    ("time", Builtin::Time),
    ("time-percentage", Builtin::TimePercentage),
    ("url-token", Builtin::UrlToken),
    ("zero", Builtin::Zero),
];

fn builtin(name: &str) -> Option<Builtin> {
    BASIC.iter().find(|(n, _)| *n == name).map(|&(_, b)| b)
}

/// A property's name and value grammar, for [`compile_with`].
pub type PropertyGrammar = (&'static str, &'static str);

/// Builds every grammar: the productions, then each property's.
pub fn compile() -> Result<Compiled, CompileError> {
    let longhands: Vec<PropertyGrammar> = LONGHANDS.iter().map(|l| (l.name, l.value)).collect();
    let shorthands: Vec<PropertyGrammar> = SHORTHANDS.iter().map(|s| (s.name, s.value)).collect();
    compile_with(PRODUCTIONS, &longhands, &shorthands)
}

/// Builds the grammars of the given productions and properties, as
/// [`compile`] does for the real tables. The top node of each property's
/// grammar is in [`Compiled::longhands`] or [`Compiled::shorthands`], in
/// the order given. The tests use it to check each [`CompileError`] and
/// grammar features the tables don't use yet.
pub fn compile_with(
    table: &[table::Production],
    longhand_grammars: &[PropertyGrammar],
    shorthand_grammars: &[PropertyGrammar],
) -> Result<Compiled, CompileError> {
    let mut g = Grammar::default();
    let mut productions = HashMap::new();
    for p in table {
        let top = match p.def {
            Def::Prod(text) | Def::Prose(text, _) | Def::Rule { grammar: text, .. } => {
                g.read(text).map_err(CompileError::Syntax)?
            }
            Def::Builtin(b, _) => {
                g.nodes.push(Node::Type(b, Bounds::ANY));
                (g.nodes.len() - 1) as NodeId
            }
            Def::Unsupported(_) => {
                g.nodes.push(Node::Never);
                (g.nodes.len() - 1) as NodeId
            }
        };
        productions.insert(p.name, top);
    }
    let read = |g: &mut Grammar, v| g.read(v).map_err(CompileError::Syntax);
    let longhands = longhand_grammars
        .iter()
        .map(|&(_, v)| read(&mut g, v))
        .collect::<Result<Vec<_>, _>>()?;
    let shorthands = shorthand_grammars
        .iter()
        .map(|&(_, v)| read(&mut g, v))
        .collect::<Result<Vec<_>, _>>()?;
    let property = |name: &str| -> Option<NodeId> {
        if let Some(i) = longhand_grammars.iter().position(|&(n, _)| n == name) {
            return Some(longhands[i]);
        }
        shorthand_grammars
            .iter()
            .position(|&(n, _)| n == name)
            .map(|i| shorthands[i])
    };
    // Resolve names.
    for i in 0..g.nodes.len() {
        let Node::Name(name, bounds) = g.nodes[i] else {
            continue;
        };
        let resolved =
            if let Some(prop) = name.strip_prefix('\'').and_then(|n| n.strip_suffix('\'')) {
                // A property's grammar, without its top-level `#` (2.1 item 3).
                let top = property(prop).ok_or(CompileError::Undefined(name))?;
                let top = match g.nodes[top as usize] {
                    Node::Repeat {
                        node,
                        min: 1,
                        max: None,
                        hash: true,
                    } => node,
                    _ => top,
                };
                if bounds != Bounds::ANY {
                    return Err(CompileError::BadRange(name));
                }
                Node::Ref(top)
            } else if let Some(&top) = productions.get(name) {
                if bounds != Bounds::ANY {
                    return Err(CompileError::BadRange(name));
                }
                Node::Ref(top)
            } else if let Some(b) = builtin(name) {
                if bounds != Bounds::ANY && types::percent_base(b).is_none() && !numeric_scalar(b) {
                    return Err(CompileError::BadRange(name));
                }
                if matches!(
                    b,
                    Builtin::Length | Builtin::LengthPercentage | Builtin::LengthPercentageWithUnit
                ) && !(bounds.max == f64::INFINITY
                    && (bounds.min == 0.0 || bounds.min == f64::NEG_INFINITY))
                {
                    return Err(CompileError::BadRange(name));
                }
                Node::Type(b, bounds)
            } else {
                return Err(CompileError::Undefined(name));
            };
        g.nodes[i] = resolved;
    }
    check(&g)?;
    let ctx = contexts(&g)?;
    Ok(Compiled {
        nodes: g.nodes,
        ctx,
        longhands,
        shorthands,
        productions,
    })
}

fn numeric_scalar(b: Builtin) -> bool {
    matches!(
        b,
        Builtin::Integer
            | Builtin::Number
            | Builtin::Length
            | Builtin::Angle
            | Builtin::Time
            | Builtin::Frequency
            | Builtin::Resolution
    )
}

/// The children of a node, in the grammar (references followed).
fn children(n: &Node) -> Vec<NodeId> {
    match n {
        Node::Ref(c) | Node::Function(_, c) | Node::Block(c) | Node::Required(c) => vec![*c],
        Node::Repeat { node, .. } => vec![*node],
        Node::Seq(c) | Node::All(c) | Node::Any(c) | Node::One(c) => c.to_vec(),
        _ => Vec::new(),
    }
}

/// Rejects reference cycles that don't pass through a function or block,
/// oversized `&&`/`||` groups, and nested `!`.
fn check(g: &Grammar) -> Result<(), CompileError> {
    for n in &g.nodes {
        if let Node::All(c) | Node::Any(c) = n
            && c.len() > MAX_REORDERABLE
        {
            return Err(CompileError::TooManyReorderable);
        }
    }
    // Depth-first walk over edges that stay within one list (functions
    // and blocks start a new one), iteratively, with colours.
    let len = g.nodes.len();
    let in_list = |n: &Node| match n {
        Node::Function(..) | Node::Block(_) => Vec::new(),
        n => children(n),
    };
    let mut colour = vec![0u8; len]; // 0 new, 1 on the path, 2 done
    for root in 0..len {
        if colour[root] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        colour[root] = 1;
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            let kids = in_list(&g.nodes[node]);
            if *next < kids.len() {
                let child = kids[*next] as usize;
                *next += 1;
                match colour[child] {
                    1 => return Err(CompileError::Cycle("a reference loop")),
                    0 => {
                        colour[child] = 1;
                        stack.push((child, 0));
                    }
                    _ => {}
                }
            } else {
                colour[node] = 2;
                stack.pop();
            }
        }
    }
    // A `!` reaching another in its list, however deep: one walk per `!`
    // (a walk sharing the colours above would miss a `!` reached first
    // from elsewhere), each node visited once.
    for n in &g.nodes {
        let Node::Required(inner) = n else { continue };
        let mut seen = vec![false; len];
        let mut todo = vec![*inner];
        while let Some(m) = todo.pop() {
            if std::mem::replace(&mut seen[m as usize], true) {
                continue;
            }
            let m = &g.nodes[m as usize];
            if matches!(m, Node::Required(_)) {
                return Err(CompileError::NestedRequired);
            }
            todo.extend(in_list(m));
        }
    }
    // Function and block contents are new roots, checked by the loop.
    Ok(())
}

/// The calculation context of every basic type node: from the numeric
/// types among its `|` alternatives (references to alternations
/// followed), see [`math`].
fn contexts(g: &Grammar) -> Result<Vec<Context>, CompileError> {
    let len = g.nodes.len();
    let own = |b: Builtin| match types::percent_base(b) {
        Some(Base::Percent) => Context::Percent,
        Some(base) => Context::Against(base),
        None => Context::NoPercent,
    };
    let mut ctx: Vec<Option<Context>> = vec![None; len];
    for i in 0..len {
        if !matches!(g.nodes[i], Node::One(_)) {
            continue;
        }
        // The basic types this alternation offers directly.
        let mut members = Vec::new();
        let mut todo = vec![i];
        let mut seen = vec![i];
        while let Some(n) = todo.pop() {
            match &g.nodes[n] {
                Node::One(c) => {
                    for &c in c.iter() {
                        if !seen.contains(&(c as usize)) {
                            seen.push(c as usize);
                            todo.push(c as usize);
                        }
                    }
                }
                Node::Ref(c) => {
                    if !seen.contains(&(*c as usize)) {
                        seen.push(*c as usize);
                        todo.push(*c as usize);
                    }
                }
                Node::Type(b, _) => members.push((n, *b)),
                _ => {}
            }
        }
        let mut against = None;
        let mut percent = false;
        for &(_, b) in &members {
            match types::percent_base(b) {
                Some(Base::Percent) => percent = true,
                Some(base) => {
                    if against.is_some_and(|a| a != base) {
                        return Err(CompileError::ConflictingContext(i as NodeId));
                    }
                    against = Some(base);
                }
                None => {}
            }
        }
        let group = match (against, percent) {
            (Some(b), _) => Context::Against(b),
            (None, true) => Context::Percent,
            (None, false) => Context::NoPercent,
        };
        for &(n, b) in &members {
            // A type alone in its slot keeps its own context; one with
            // percentage-taking alternatives takes the group's.
            let c = if own(b) == Context::NoPercent || own(b) == Context::Percent {
                group
            } else {
                own(b)
            };
            match ctx[n] {
                Some(old) if old != c => return Err(CompileError::ConflictingContext(n as NodeId)),
                _ => ctx[n] = Some(c),
            }
        }
    }
    Ok((0..len)
        .map(|i| match (&g.nodes[i], ctx[i]) {
            (_, Some(c)) => c,
            (Node::Type(b, _), None) => own(*b),
            _ => Context::NoPercent,
        })
        .collect())
}

/// Every grammar, compiled once.
pub fn compiled() -> &'static Compiled {
    static COMPILED: OnceLock<Compiled> = OnceLock::new();
    COMPILED.get_or_init(|| compile().expect("the grammar tables compile (see the tests)"))
}

/// Whether `values` matches the grammar of the property `target` names.
/// Custom properties take any value, and `all` only the CSS-wide
/// keywords, which [`crate::property::classify`] handles before asking.
pub fn value_matches(target: Target, values: Values<'_, '_>) -> bool {
    value_matches_items(target, values)
}

/// [`value_matches`] on any sequence of component values (white space
/// included or not), such as a collected slice.
pub fn value_matches_items<'t, 'a: 't>(
    target: Target,
    values: impl IntoIterator<Item = ComponentValue<'t, 'a>>,
) -> bool {
    let g = compiled();
    let top = match target {
        Target::Longhand(l) => g.longhands[l as usize],
        Target::Shorthand(s) => g.shorthands[s as usize],
        Target::All => return false,
        Target::Custom => return true,
    };
    matcher::matches_items(g, top, values, 0)
}

/// How `values` matched the grammar of the property `target` names (see
/// [`parse`]), or `None` if it doesn't match, or for custom properties and
/// `all`, which have no grammar here.
pub fn parse_value<'t, 'a: 't>(
    target: Target,
    values: impl IntoIterator<Item = ComponentValue<'t, 'a>>,
) -> Option<parse::Tree<'t, 'a>> {
    let g = compiled();
    let top = match target {
        Target::Longhand(l) => g.longhands[l as usize],
        Target::Shorthand(s) => g.shorthands[s as usize],
        Target::All | Target::Custom => return None,
    };
    parse::parse(g, top, values, 0)
}
