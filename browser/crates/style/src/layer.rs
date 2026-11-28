//! Cascade layers (css-cascade-5 6.4): layer names, the layer tree of each
//! origin, and the layer order.
//!
//! Design choices (chosen, why, rejected):
//!
//! - **A tree per origin, ranked by a post-order walk.** Layers are sorted
//!   by first declaration, nested layers grouped inside their parent, and
//!   a parent's own (unlayered) rules come after all its nested layers
//!   (6.4.3). That is exactly the post-order of a tree whose children are
//!   kept in first-declaration order, with the root standing for the
//!   origin's unlayered rules. The walk is iterative, because `@layer`
//!   blocks can nest as deep as the input allows. Rejected: comparing full
//!   names segment by segment for every pair of declarations (what the
//!   literal model in the tests does), which costs the name length per
//!   comparison.
//! - **Named children are found through one hash map** keyed by (parent,
//!   name), so declaring many sibling layers stays linear. Anonymous
//!   layers are never looked up: each occurrence is a new node.

use optimal_css::{ComponentValue, Token};
use std::collections::HashMap;

use crate::Origin;

/// A cascade layer: an index into [`Layers`].
pub type LayerId = u32;

/// The layer trees of the three origins.
#[derive(Clone, Debug)]
pub struct Layers {
    nodes: Vec<Node>,
    named: HashMap<(LayerId, Box<str>), LayerId>,
}

#[derive(Clone, Debug)]
struct Node {
    /// `None` for an origin's root.
    parent: Option<LayerId>,
    /// `None` for a root or an anonymous layer.
    name: Option<Box<str>>,
    children: Vec<LayerId>,
}

impl Default for Layers {
    fn default() -> Self {
        Self::new()
    }
}

impl Layers {
    /// Three roots (user agent, user, author: [`Layers::root`]) and no layers.
    pub fn new() -> Self {
        let root = || Node {
            parent: None,
            name: None,
            children: Vec::new(),
        };
        Layers {
            nodes: vec![root(), root(), root()],
            named: HashMap::new(),
        }
    }

    /// The root of an origin's tree: the implicit outer layer that holds
    /// its unlayered rules.
    pub fn root(origin: Origin) -> LayerId {
        match origin {
            Origin::UserAgent => 0,
            Origin::User => 1,
            Origin::Author => 2,
        }
    }

    /// The layer named `name` inside `parent`, declared now if it wasn't.
    pub fn named(&mut self, parent: LayerId, name: &str) -> LayerId {
        if let Some(&id) = self.named.get(&(parent, name.into())) {
            return id;
        }
        let id = self.push(parent, Some(name.into()));
        self.named.insert((parent, name.into()), id);
        id
    }

    /// The layer `path` (one segment per nesting level) inside `parent`,
    /// declaring each level that wasn't declared yet, outermost first.
    pub fn path(&mut self, parent: LayerId, path: &[Box<str>]) -> LayerId {
        path.iter().fold(parent, |p, seg| self.named(p, seg))
    }

    /// A new anonymous layer inside `parent` (each occurrence is distinct).
    pub fn anonymous(&mut self, parent: LayerId) -> LayerId {
        self.push(parent, None)
    }

    fn push(&mut self, parent: LayerId, name: Option<Box<str>>) -> LayerId {
        let id = self.nodes.len() as LayerId;
        self.nodes.push(Node {
            parent: Some(parent),
            name,
            children: Vec::new(),
        });
        self.nodes[parent as usize].children.push(id);
        id
    }

    /// The number of layers, roots included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether there are no layers besides the roots.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 3
    }

    /// The parent layer (`None` for a root).
    pub fn parent(&self, id: LayerId) -> Option<LayerId> {
        self.nodes[id as usize].parent
    }

    /// The layer's own name segment (`None` for a root or anonymous layer).
    pub fn segment(&self, id: LayerId) -> Option<&str> {
        self.nodes[id as usize].name.as_deref()
    }

    /// Each layer's place in the layer order of its origin: a layer with a
    /// higher rank comes later (6.4.3). Ranks of layers of different origins
    /// are not comparable.
    pub fn ranks(&self) -> Vec<u32> {
        let mut rank = vec![0; self.nodes.len()];
        let mut next = 0;
        for root in 0..3 {
            // (node, index of the next child to visit)
            let mut stack: Vec<(LayerId, usize)> = vec![(root, 0)];
            while let Some(top) = stack.last_mut() {
                let (id, i) = *top;
                match self.nodes[id as usize].children.get(i) {
                    Some(&c) => {
                        top.1 += 1;
                        stack.push((c, 0));
                    }
                    None => {
                        rank[id as usize] = next;
                        next += 1;
                        stack.pop();
                    }
                }
            }
        }
        rank
    }
}

/// The CSS-wide keywords (css-values-4 2.5 and css-cascade-5 7.3), which a
/// `<layer-name>` may not use as a segment (6.4.2).
pub(crate) const CSS_WIDE_KEYWORDS: &[&str] = &[
    "initial",
    "inherit",
    "unset",
    "revert",
    "revert-layer",
    "revert-rule",
];

/// Parses `<layer-name>` = `<ident> [ '.' <ident> ]*` (no white space
/// between) from all of `values`. `None` if they aren't exactly one.
pub(crate) fn layer_name(values: &[ComponentValue<'_, '_>]) -> Option<Vec<Box<str>>> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        match values.get(i).map(|v| v.token()) {
            Some(Token::Ident(s))
                if !CSS_WIDE_KEYWORDS.iter().any(|k| s.eq_ignore_ascii_case(k)) =>
            {
                out.push(Box::from(&**s));
            }
            _ => return None,
        }
        i += 1;
        match values.get(i).map(|v| v.token()) {
            None => return Some(out),
            Some(Token::Delim('.')) => i += 1,
            Some(_) => return None,
        }
    }
}

/// Splits `values` at top-level commas, trimming white space from each
/// piece.
pub(crate) fn split_commas<'v, 't, 'a>(
    values: &'v [ComponentValue<'t, 'a>],
) -> Vec<&'v [ComponentValue<'t, 'a>]> {
    values
        .split(|v| matches!(v.token(), Token::Comma))
        .map(trim)
        .collect()
}

/// `values` without white space at either end.
pub(crate) fn trim<'v, 't, 'a>(
    mut values: &'v [ComponentValue<'t, 'a>],
) -> &'v [ComponentValue<'t, 'a>] {
    while let [first, rest @ ..] = values
        && matches!(first.token(), Token::Whitespace)
    {
        values = rest;
    }
    while let [rest @ .., last] = values
        && matches!(last.token(), Token::Whitespace)
    {
        values = rest;
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use optimal_css::parse_component_values;
    use optimal_wtf8::Wtf8;

    fn name(src: &str) -> Option<Vec<String>> {
        let (p, r) = parse_component_values(Wtf8::from_str(src));
        let values: Vec<_> = p.values(r).collect();
        layer_name(trim(&values)).map(|v| v.into_iter().map(String::from).collect())
    }

    #[test]
    fn layer_names() {
        assert_eq!(name("a"), Some(vec!["a".into()]));
        assert_eq!(name(" a.b.c "), Some(vec!["a".into(), "b".into(), "c".into()]));
        // Case is kept: names are identifiers, compared exactly.
        assert_eq!(name("A.b"), Some(vec!["A".into(), "b".into()]));
        assert_eq!(name("a . b"), None);
        assert_eq!(name("a."), None);
        assert_eq!(name(".a"), None);
        assert_eq!(name("a.1"), None);
        assert_eq!(name("a b"), None);
        assert_eq!(name(""), None);
        assert_eq!(name("\"a\""), None);
        // CSS-wide keywords, in any case, in any segment (6.4.2).
        for k in CSS_WIDE_KEYWORDS {
            assert_eq!(name(k), None, "{k}");
            assert_eq!(name(&format!("a.{}", k.to_uppercase())), None, "{k}");
        }
        // Not CSS-wide keywords.
        assert_eq!(name("default"), Some(vec!["default".into()]));
        assert_eq!(name("revert-layers"), Some(vec!["revert-layers".into()]));
    }

    #[test]
    fn order_is_post_order_per_origin() {
        // The example of 6.4.3: reset.type, reset, framework.theme,
        // framework, then the outer layer.
        let mut l = Layers::new();
        let root = Layers::root(Origin::Author);
        let reset_type = l.path(root, &["reset".into(), "type".into()]);
        let framework = l.named(root, "framework");
        let theme = l.named(framework, "theme");
        let reset = l.named(root, "reset");
        let r = l.ranks();
        let order = [reset_type, reset, theme, framework, root];
        for w in order.windows(2) {
            assert!(r[w[0] as usize] < r[w[1] as usize], "{w:?} {r:?}");
        }
        // Declaring again finds the same layer; anonymous ones are new.
        assert_eq!(l.named(root, "reset"), reset);
        let a1 = l.anonymous(root);
        let a2 = l.anonymous(root);
        assert_ne!(a1, a2);
        assert_eq!(l.segment(a1), None);
        assert_eq!(l.segment(theme), Some("theme"));
        assert_eq!(l.parent(theme), Some(framework));
    }

    #[test]
    fn deep_nesting_is_iterative() {
        let mut l = Layers::new();
        let mut p = Layers::root(Origin::User);
        for _ in 0..200_000 {
            p = l.anonymous(p);
        }
        let r = l.ranks();
        // The deepest layer comes first, the user root last of its origin.
        assert!(r[p as usize] < r[Layers::root(Origin::User) as usize]);
    }
}
