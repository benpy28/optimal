//! A small element tree for the cascade tests: any namespace, any
//! attribute, no element states (no hover, focus, links or form state),
//! no language or direction.

#![allow(dead_code)]

use optimal_selectors::{Direction, Element, PseudoClassKeyword};

pub const HTML: &str = "http://www.w3.org/1999/xhtml";
pub const SVG: &str = "http://www.w3.org/2000/svg";

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Index in the parent's children.
    pub index: usize,
    pub ns: String,
    pub local: String,
    pub attrs: Vec<(String, String)>,
}

/// Elements only; node 0 is the root element (its parent is the document).
#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// An HTML document (not XML).
    pub html: bool,
}

impl Tree {
    pub fn new(html: bool, root: &str) -> Self {
        let mut t = Tree {
            nodes: Vec::new(),
            html,
        };
        t.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            index: 0,
            ns: HTML.into(),
            local: root.into(),
            attrs: Vec::new(),
        });
        t
    }

    /// Appends an element under `parent`; returns it.
    pub fn add(&mut self, parent: usize, ns: &str, local: &str, attrs: &[(&str, &str)]) -> usize {
        let i = self.nodes.len();
        let index = self.nodes[parent].children.len();
        self.nodes.push(Node {
            parent: Some(parent),
            children: Vec::new(),
            index,
            ns: ns.into(),
            local: local.into(),
            attrs: attrs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        });
        self.nodes[parent].children.push(i);
        i
    }

    pub fn attr(&self, i: usize, name: &str) -> Option<&str> {
        self.nodes[i]
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn el(&self, i: usize) -> El<'_> {
        El { t: self, i }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct El<'t> {
    pub t: &'t Tree,
    pub i: usize,
}

impl PartialEq for El<'_> {
    fn eq(&self, o: &Self) -> bool {
        std::ptr::eq(self.t, o.t) && self.i == o.i
    }
}

impl Eq for El<'_> {}

impl std::hash::Hash for El<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        std::ptr::hash(self.t, h);
        self.i.hash(h);
    }
}

impl<'t> El<'t> {
    fn node(&self) -> &'t Node {
        &self.t.nodes[self.i]
    }
}

impl Element for El<'_> {
    fn parent_element(&self) -> Option<Self> {
        self.node().parent.map(|i| El { t: self.t, i })
    }
    fn is_root(&self) -> bool {
        self.node().parent.is_none()
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        let p = self.node().parent?;
        let idx = self.node().index;
        (idx > 0).then(|| El {
            t: self.t,
            i: self.t.nodes[p].children[idx - 1],
        })
    }
    fn next_sibling_element(&self) -> Option<Self> {
        let p = self.node().parent?;
        self.t.nodes[p]
            .children
            .get(self.node().index + 1)
            .map(|&i| El { t: self.t, i })
    }
    fn first_element_child(&self) -> Option<Self> {
        self.node().children.first().map(|&i| El { t: self.t, i })
    }
    fn same_parent(&self, other: &Self) -> bool {
        // The root's parent is the document, which no other element has.
        self.node().parent == other.node().parent
    }
    fn any_non_element_child(&self, _: impl FnMut(Option<&[u8]>) -> bool) -> bool {
        false
    }
    fn is_html_element_in_html_document(&self) -> bool {
        self.t.html && self.node().ns == HTML
    }
    fn local_name(&self) -> &[u8] {
        self.node().local.as_bytes()
    }
    fn namespace(&self) -> &[u8] {
        self.node().ns.as_bytes()
    }
    fn attribute(&self, ns: &[u8], local: &[u8]) -> Option<&[u8]> {
        if !ns.is_empty() {
            return None;
        }
        self.node()
            .attrs
            .iter()
            .find(|(k, _)| k.as_bytes() == local)
            .map(|(_, v)| v.as_bytes())
    }
    fn any_attribute(&self, mut f: impl FnMut(&[u8], &[u8], &[u8]) -> bool) -> bool {
        self.node()
            .attrs
            .iter()
            .any(|(k, v)| f(b"", k.as_bytes(), v.as_bytes()))
    }
    fn state(&self, _: PseudoClassKeyword) -> bool {
        false
    }
    fn custom_state(&self, _: &str) -> bool {
        false
    }
    fn with_language<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(b"")
    }
    fn directionality(&self) -> Direction {
        Direction::Ltr
    }
}
