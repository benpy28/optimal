//! Typed specified values, and their serialization (cssom-1
//! #serializing-css-values, "serialize a CSS value").
//!
//! [`build`] turns a parse tree ([`super::parse`]) into a [`Specified`]:
//! the same shape, owning its data, with each leaf typed (keywords,
//! numbers, dimensions, math functions as simplified calculation trees,
//! colors, strings, identifiers, URLs). [`serialize`] writes one back:
//!
//! - component values as cssom-1 "serialize a CSS component value" serializes each kind (keywords in
//!   lowercase, `<length>` zeros with their unit, strings and URLs quoted,
//!   identifiers escaped), math functions by css-values-4 #calc-serialize, colors
//!   by css-color-4 #serializing-color-values;
//! - children of `&&` and `||` in the canonical order, which every
//!   property here defines as "per grammar" (cssom-1: "reorder the
//!   component values to use the canonical order");
//! - functions as css-values-4 #functional-serialization ("serialize a
//!   functional notation"):
//!   the name in lowercase, the arguments joined by single spaces;
//! - one space between component values, none before a comma (cssom-1
//!   step 4). A comma the grammar writes is left out when the components
//!   on one side of it are all omitted, as the value was parsed
//!   (css-values-4 #comb-comma, "Commas").
//!
//! The "shorter representation" rules (omitting what can be omitted
//! without changing the meaning) and the serialization rules specs give
//! for their own values are in [`super::shortest`], each attached to a
//! production or a property's grammar.

use optimal_css::{BlockKind, ComponentValue, Kind, Token};

use super::Compiled;
use super::calc::{self, MathValue};
use super::color::{self, Color};
use super::parse::{Tree, TreeKind};
use super::serialize::{identifier, number, string, url};
use super::syntax::{Node, NodeId};
use super::types::{Builtin, Unit, unit_of};

/// A typed specified value: how a value matched its grammar, with typed
/// leaves.
#[derive(Clone, Debug, PartialEq)]
pub struct Specified {
    /// The grammar node.
    pub node: NodeId,
    /// What it matched.
    pub kind: SpecKind,
}

/// What a grammar node matched (see [`Specified`]).
#[derive(Clone, Debug, PartialEq)]
pub enum SpecKind {
    /// A keyword, as the grammar writes it.
    Keyword(&'static str),
    /// A delimiter (`/`, `+`).
    Delim(char),
    /// A `<number>`, `<integer>` or `<zero>`.
    Number(f64),
    /// A `<percentage>`.
    Percentage(f64),
    /// A dimension: the number and its unit (a unitless zero `<length>`
    /// is `0px`).
    Dimension(f64, Unit),
    /// A math function.
    Math(MathValue),
    /// An `<ident>`, `<custom-ident>` or `<dashed-ident>`, as written.
    Ident(String),
    /// A `<string>`.
    String(String),
    /// A `<url-token>`'s URL.
    Url(String),
    /// A `<color>`.
    Color(Color),
    /// A function (its name as the grammar writes it) or a `()`-block
    /// (`None`), and its contents.
    Nested(Option<&'static str>, Box<Specified>),
    /// A comma the grammar writes.
    Comma,
    /// A reference or `!` group: what it holds.
    Inner(Box<Specified>),
    /// A `|`: the alternative's index, and what it matched.
    Alternative(usize, Box<Specified>),
    /// Juxtaposition, `&&`, `||` or a multiplier: the children that
    /// matched, each with its index among the node's children.
    List(Vec<(usize, Specified)>),
}

/// The typed specified value of a parse tree.
pub fn build(g: &Compiled, t: &Tree<'_, '_>) -> Specified {
    let kind = match &t.kind {
        TreeKind::Value(v) => leaf(g, t.node, *v),
        TreeKind::Nested(_, inner) => {
            let name = match g.nodes[t.node as usize] {
                Node::Function(name, _) => Some(name),
                _ => None,
            };
            SpecKind::Nested(name, Box::new(build(g, inner)))
        }
        TreeKind::Comma => SpecKind::Comma,
        TreeKind::Inner(inner) => {
            if let Node::Ref(top) = g.nodes[t.node as usize]
                && Some(&top) == g.productions.get("color")
            {
                SpecKind::Color(color::parse(first_value(inner)))
            } else {
                SpecKind::Inner(Box::new(build(g, inner)))
            }
        }
        TreeKind::Alternative(i, inner) => {
            let mut b = build(g, inner);
            // A <zero> offered beside <angle> is the angle 0deg
            // (css-transforms-1 #transform-functions: "a <zero> behaves the
            // same as 0deg"), and serializes as an <angle> does (cssom-1
            // "serialize a CSS component value").
            if let SpecKind::Number(v) = b.kind
                && matches!(g.nodes[b.node as usize], Node::Type(Builtin::Zero, _))
                && let Node::One(alts) = &g.nodes[t.node as usize]
                && alts.iter().any(|&a| {
                    matches!(
                        g.nodes[a as usize],
                        Node::Type(Builtin::Angle | Builtin::AnglePercentage, _)
                    )
                })
            {
                b.kind = SpecKind::Dimension(v, unit_of("deg").expect("deg is a unit"));
            }
            SpecKind::Alternative(*i, Box::new(b))
        }
        TreeKind::List(items) => {
            SpecKind::List(items.iter().map(|(i, c)| (*i, build(g, c))).collect())
        }
    };
    Specified { node: t.node, kind }
}

/// The first component value a parse tree matched (a `<color>` matches
/// exactly one).
fn first_value<'t, 'a>(t: &Tree<'t, 'a>) -> ComponentValue<'t, 'a> {
    match &t.kind {
        TreeKind::Value(v) | TreeKind::Nested(v, _) => *v,
        TreeKind::Inner(t) | TreeKind::Alternative(_, t) => first_value(t),
        TreeKind::List(items) => items
            .iter()
            .find_map(|(_, t)| (!matches!(t.kind, TreeKind::Comma)).then(|| first_value(t)))
            .expect("a color has a component value"),
        TreeKind::Comma => unreachable!("a comma holds no value"),
    }
}

/// A leaf: a keyword, delimiter or basic type matching one component
/// value.
fn leaf(g: &Compiled, node: NodeId, v: ComponentValue<'_, '_>) -> SpecKind {
    match &g.nodes[node as usize] {
        Node::Keyword(k) => return SpecKind::Keyword(k),
        Node::Delim(c) => return SpecKind::Delim(*c),
        _ => {}
    }
    let b = match g.nodes[node as usize] {
        Node::Type(b, _) => b,
        _ => unreachable!("a leaf is a keyword, a delimiter or a basic type"),
    };
    match v.kind() {
        Kind::Function(name, args) => SpecKind::Math(calc::parse(name, args, g.ctx[node as usize])),
        Kind::Token(Token::Number(n)) => match b {
            // A unitless zero is a <length> (css-values-4 #lengths).
            Builtin::Length | Builtin::LengthPercentage => {
                SpecKind::Dimension(n.value, unit_of("px").expect("px is a unit"))
            }
            _ => SpecKind::Number(n.value),
        },
        Kind::Token(Token::Percentage { value, .. }) => SpecKind::Percentage(*value),
        Kind::Token(Token::Dimension(n, unit)) => {
            SpecKind::Dimension(n.value, unit_of(unit).expect("a matched unit is known"))
        }
        Kind::Token(Token::Hash(..)) => SpecKind::Color(color::parse(v)),
        Kind::Token(Token::Ident(id)) => match b {
            Builtin::NamedColor | Builtin::SystemColor => SpecKind::Color(color::parse(v)),
            _ => SpecKind::Ident(id.to_string()),
        },
        Kind::Token(Token::String(s)) => SpecKind::String(s.to_string()),
        Kind::Token(Token::Url(u)) => SpecKind::Url(u.to_string()),
        Kind::Block(BlockKind::Paren, _) | Kind::Block(..) | Kind::Token(_) => {
            unreachable!("no basic type matches this")
        }
    }
}

/// One serialized component value, or a comma: by the rule for its
/// grammar node if there is one ([`super::shortest`]).
fn item(s: &Specified, g: &Compiled, out: &mut Vec<String>) {
    match g.rules.get(&s.node) {
        Some(rule) => out.extend(rule(s, g)),
        None => walk(s, g, out),
    }
}

/// The serialized component values of `s` by the generic rules (its
/// descendants by their own rules).
pub(super) fn items(s: &Specified, g: &Compiled) -> Vec<String> {
    let mut out = Vec::new();
    walk(s, g, &mut out);
    out
}

/// The generic serialization of `s`.
fn walk(s: &Specified, g: &Compiled, out: &mut Vec<String>) {
    match &s.kind {
        SpecKind::Keyword(k) => out.push(k.to_ascii_lowercase()),
        SpecKind::Delim(c) => out.push(c.to_string()),
        SpecKind::Number(v) => out.push(number(*v)),
        SpecKind::Percentage(v) => out.push(format!("{}%", number(*v))),
        SpecKind::Dimension(v, u) => {
            out.push(format!("{}{}", number(*v), u.name.to_ascii_lowercase()))
        }
        SpecKind::Math(m) => out.push(calc::serialize(m)),
        SpecKind::Ident(id) => {
            let mut text = String::new();
            identifier(&mut text, id);
            out.push(text);
        }
        SpecKind::String(v) => {
            let mut text = String::new();
            string(&mut text, v);
            out.push(text);
        }
        SpecKind::Url(v) => {
            let mut text = String::new();
            url(&mut text, v);
            out.push(text);
        }
        SpecKind::Color(c) => {
            let mut text = String::new();
            color::serialize(c, &mut text);
            out.push(text);
        }
        SpecKind::Nested(name, inner) => {
            let mut text = match name {
                Some(n) => n.to_ascii_lowercase(),
                None => String::new(),
            };
            text.push('(');
            text.push_str(&join(inner, g));
            text.push(')');
            out.push(text);
        }
        SpecKind::Comma => out.push(",".to_string()),
        SpecKind::Inner(inner) | SpecKind::Alternative(_, inner) => item(inner, g, out),
        SpecKind::List(children) => {
            let hash = matches!(g.nodes[s.node as usize], Node::Repeat { hash: true, .. });
            let reorder = matches!(g.nodes[s.node as usize], Node::All(_) | Node::Any(_));
            let mut kids: Vec<&(usize, Specified)> = children.iter().collect();
            if reorder {
                kids.sort_by_key(|(i, _)| *i);
            }
            for (n, (_, c)) in kids.into_iter().enumerate() {
                if hash && n > 0 {
                    out.push(",".to_string());
                }
                item(c, g, out);
            }
        }
    }
}

/// The serialization of a list of component values: items joined by one
/// space, none before a comma, with the commas of omitted components
/// dropped.
fn join(s: &Specified, g: &Compiled) -> String {
    let mut items = Vec::new();
    item(s, g, &mut items);
    let mut kept: Vec<String> = Vec::new();
    for it in items {
        if it == "," && kept.last().is_none_or(|l| l == ",") {
            continue;
        }
        kept.push(it);
    }
    if kept.last().is_some_and(|l| l == ",") {
        kept.pop();
    }
    let mut text = String::new();
    for (i, it) in kept.iter().enumerate() {
        if i > 0 && it != "," {
            text.push(' ');
        }
        text.push_str(it);
    }
    text
}

/// The serialization of a specified value.
pub fn serialize(s: &Specified, g: &Compiled) -> String {
    join(s, g)
}
