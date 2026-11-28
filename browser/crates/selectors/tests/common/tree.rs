//! A small tree for the matching tests: any namespace, any attribute, built
//! from the HTML parser's output or by hand, with the HTML states the WPT
//! fixture uses (links, the target element, `:enabled`, `:disabled`,
//! `:checked`, content language).

use optimal_dom::{Document, NodeData, NodeId};
use optimal_selectors::{Direction, Element, PseudoClassKeyword};

pub const HTML: &str = "http://www.w3.org/1999/xhtml";
pub const SVG: &str = "http://www.w3.org/2000/svg";
pub const XML: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Clone, Debug)]
pub struct Attr {
    pub ns: String,
    pub local: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Document,
    Fragment,
    Element {
        ns: String,
        local: String,
        attrs: Vec<Attr>,
    },
    Text(String),
    /// Comments and processing instructions (never affect `:empty`).
    Comment,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Index in the parent's children.
    pub index: usize,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// An HTML document (not XML).
    pub html: bool,
    /// The document's target element (`:target`).
    pub target: Option<usize>,
}

impl Tree {
    /// A tree holding only a document node (node 0).
    pub fn new(html: bool) -> Self {
        let mut t = Tree {
            nodes: Vec::new(),
            html,
            target: None,
        };
        t.create(Kind::Document);
        t
    }

    pub fn create(&mut self, kind: Kind) -> usize {
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            index: 0,
            kind,
        });
        self.nodes.len() - 1
    }

    pub fn element(&mut self, ns: &str, local: &str, attrs: &[(&str, &str)]) -> usize {
        self.create(Kind::Element {
            ns: ns.into(),
            local: local.into(),
            attrs: attrs
                .iter()
                .map(|(k, v)| Attr {
                    ns: String::new(),
                    local: (*k).into(),
                    value: (*v).into(),
                })
                .collect(),
        })
    }

    pub fn append(&mut self, parent: usize, child: usize) {
        assert!(self.nodes[child].parent.is_none());
        self.nodes[child].parent = Some(parent);
        self.nodes[child].index = self.nodes[parent].children.len();
        self.nodes[parent].children.push(child);
    }

    /// `setAttributeNS`.
    pub fn set_attribute(&mut self, el: usize, ns: &str, local: &str, value: &str) {
        let Kind::Element { attrs, .. } = &mut self.nodes[el].kind else {
            panic!("not an element")
        };
        match attrs.iter_mut().find(|a| a.ns == ns && a.local == local) {
            Some(a) => a.value = value.into(),
            None => attrs.push(Attr {
                ns: ns.into(),
                local: local.into(),
                value: value.into(),
            }),
        }
    }

    /// `cloneNode(true)`: a detached deep copy.
    pub fn clone_subtree(&mut self, n: usize) -> usize {
        let copy = self.create(self.nodes[n].kind.clone());
        for c in self.nodes[n].children.clone() {
            let cc = self.clone_subtree(c);
            self.append(copy, cc);
        }
        copy
    }

    /// The node and its descendants, in tree order.
    pub fn subtree(&self, n: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            out.push(x);
            stack.extend(self.nodes[x].children.iter().rev());
        }
        out
    }

    pub fn attr(&self, el: usize, ns: &str, local: &str) -> Option<&str> {
        match &self.nodes[el].kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|a| a.ns == ns && a.local == local)
                .map(|a| a.value.as_str()),
            _ => None,
        }
    }

    pub fn is_element(&self, n: usize) -> bool {
        matches!(self.nodes[n].kind, Kind::Element { .. })
    }

    /// The first element in `root`'s subtree, in tree order, with this ID.
    pub fn by_id(&self, root: usize, id: &str) -> Option<usize> {
        self.subtree(root)
            .into_iter()
            .find(|&n| self.attr(n, "", "id") == Some(id))
    }

    pub fn first_element_child(&self, n: usize) -> Option<usize> {
        self.nodes[n]
            .children
            .iter()
            .copied()
            .find(|&c| self.is_element(c))
    }

    /// Copy an `optimal-dom` document.
    pub fn from_dom(doc: &Document) -> Self {
        let mut t = Tree::new(true);
        t.copy_children(doc, Document::ROOT, 0);
        t
    }

    fn copy_children(&mut self, doc: &Document, from: NodeId, to: usize) {
        for c in doc.children(from) {
            let kind = match doc.data(c) {
                NodeData::Element(e) => Kind::Element {
                    ns: e.namespace.url().into(),
                    local: doc.name_text(e.local).to_string_lossy(),
                    attrs: e
                        .attributes
                        .iter()
                        .map(|a| Attr {
                            ns: a.namespace.url().into(),
                            local: doc.name_text(a.local).to_string_lossy(),
                            value: a.value.as_wtf8().to_string_lossy(),
                        })
                        .collect(),
                },
                NodeData::Text(s) => Kind::Text(s.as_wtf8().to_string_lossy()),
                NodeData::Comment(_) | NodeData::ProcessingInstruction { .. } => Kind::Comment,
                // The doctype is the document's child, never an element's.
                NodeData::Doctype { .. } => continue,
                other => panic!("unexpected child {other:?}"),
            };
            let n = self.create(kind);
            self.append(to, n);
            self.copy_children(doc, c, n);
        }
    }
}

/// An element of a [`Tree`].
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

impl<'t> El<'t> {
    pub fn new(t: &'t Tree, i: usize) -> Self {
        assert!(t.is_element(i));
        El { t, i }
    }

    fn node(&self) -> &'t Node {
        &self.t.nodes[self.i]
    }

    fn data(&self) -> (&'t str, &'t str, &'t [Attr]) {
        match &self.node().kind {
            Kind::Element { ns, local, attrs } => (ns, local, attrs),
            _ => unreachable!(),
        }
    }

    fn sibling(&self, forward: bool) -> Option<Self> {
        let p = self.node().parent?;
        let cs = &self.t.nodes[p].children;
        let idx = self.node().index;
        let found = if forward {
            cs[idx + 1..]
                .iter()
                .copied()
                .find(|&c| self.t.is_element(c))
        } else {
            cs[..idx]
                .iter()
                .rev()
                .copied()
                .find(|&c| self.t.is_element(c))
        };
        found.map(|i| El { t: self.t, i })
    }

    fn is_html(&self, name: &str) -> bool {
        let (ns, local, _) = self.data();
        ns == HTML && local == name
    }

    fn has_attr(&self, name: &str) -> bool {
        self.t.attr(self.i, "", name).is_some()
    }

    fn ancestors(&self) -> impl Iterator<Item = El<'t>> {
        std::iter::successors(self.parent_element(), |e| e.parent_element())
    }

    /// HTML: the element is a disabled form control (4.10.18.5), or an
    /// optgroup or option that is disabled, or a disabled fieldset.
    fn actually_disabled(&self) -> bool {
        let (ns, local, _) = self.data();
        if ns != HTML {
            return false;
        }
        match local {
            "button" | "input" | "select" | "textarea" => {
                self.has_attr("disabled") || self.in_disabled_fieldset()
            }
            "optgroup" => self.has_attr("disabled"),
            "option" => {
                self.has_attr("disabled")
                    || self
                        .parent_element()
                        .is_some_and(|p| p.is_html("optgroup") && p.has_attr("disabled"))
            }
            "fieldset" => self.has_attr("disabled") || self.in_disabled_fieldset(),
            _ => false,
        }
    }

    /// A descendant of a fieldset with a disabled attribute, outside that
    /// fieldset's first legend child.
    fn in_disabled_fieldset(&self) -> bool {
        let mut child = *self;
        for a in self.ancestors() {
            if a.is_html("fieldset") && a.has_attr("disabled") {
                let legend = a.t.nodes[a.i]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| a.t.is_element(c) && El { t: a.t, i: c }.is_html("legend"));
                if legend != Some(child.i) {
                    return true;
                }
            }
            child = a;
        }
        false
    }

    fn input_type(&self) -> String {
        self.t
            .attr(self.i, "", "type")
            .unwrap_or("text")
            .to_ascii_lowercase()
    }

    /// An option's selectedness, from its select's "selectedness setting
    /// algorithm" with no user interaction.
    fn selectedness(&self) -> bool {
        let select = self.ancestors().take(2).find(|a| a.is_html("select"));
        let Some(select) = select else {
            return self.has_attr("selected");
        };
        let options: Vec<El<'t>> = self
            .t
            .subtree(select.i)
            .into_iter()
            .filter(|&n| self.t.is_element(n) && El { t: self.t, i: n }.is_html("option"))
            .map(|n| El { t: self.t, i: n })
            .filter(|o| {
                o.parent_element() == Some(select)
                    || o.parent_element().is_some_and(|p| {
                        p.is_html("optgroup") && p.parent_element() == Some(select)
                    })
            })
            .collect();
        if select.has_attr("multiple") {
            return self.has_attr("selected");
        }
        let size = select
            .t
            .attr(select.i, "", "size")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        match options.iter().rposition(|o| o.has_attr("selected")) {
            Some(last) => options[last] == *self,
            None if size <= 1 => options.iter().find(|o| !o.actually_disabled()) == Some(self),
            None => false,
        }
    }
}

impl Element for El<'_> {
    fn parent_element(&self) -> Option<Self> {
        let p = self.node().parent?;
        self.t.is_element(p).then_some(El { t: self.t, i: p })
    }
    fn is_root(&self) -> bool {
        self.node()
            .parent
            .is_some_and(|p| matches!(self.t.nodes[p].kind, Kind::Document))
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        self.sibling(false)
    }
    fn next_sibling_element(&self) -> Option<Self> {
        self.sibling(true)
    }
    fn first_element_child(&self) -> Option<Self> {
        self.t
            .first_element_child(self.i)
            .map(|i| El { t: self.t, i })
    }
    fn same_parent(&self, other: &Self) -> bool {
        self.node().parent.is_some() && self.node().parent == other.node().parent
    }
    fn any_non_element_child(&self, mut f: impl FnMut(Option<&[u8]>) -> bool) -> bool {
        self.node()
            .children
            .iter()
            .any(|&c| match &self.t.nodes[c].kind {
                Kind::Text(s) => f(Some(s.as_bytes())),
                Kind::Comment | Kind::Element { .. } => false,
                Kind::Document | Kind::Fragment => f(None),
            })
    }
    fn is_html_element_in_html_document(&self) -> bool {
        self.t.html && self.data().0 == HTML
    }
    fn local_name(&self) -> &[u8] {
        self.data().1.as_bytes()
    }
    fn namespace(&self) -> &[u8] {
        self.data().0.as_bytes()
    }
    fn attribute(&self, ns: &[u8], local: &[u8]) -> Option<&[u8]> {
        self.data()
            .2
            .iter()
            .find(|a| a.ns.as_bytes() == ns && a.local.as_bytes() == local)
            .map(|a| a.value.as_bytes())
    }
    fn any_attribute(&self, mut f: impl FnMut(&[u8], &[u8], &[u8]) -> bool) -> bool {
        self.data()
            .2
            .iter()
            .any(|a| f(a.ns.as_bytes(), a.local.as_bytes(), a.value.as_bytes()))
    }
    fn state(&self, pc: PseudoClassKeyword) -> bool {
        use PseudoClassKeyword as K;
        let link = (self.is_html("a") || self.is_html("area")) && self.has_attr("href");
        let form = [
            "button", "input", "select", "textarea", "optgroup", "option", "fieldset",
        ]
        .iter()
        .any(|n| self.is_html(n));
        match pc {
            // No history: every link is unvisited.
            K::Link | K::AnyLink => link,
            K::Visited => false,
            K::Target => self.t.target == Some(self.i),
            K::Disabled => self.actually_disabled(),
            K::Enabled => form && !self.actually_disabled(),
            K::Checked => {
                (self.is_html("input")
                    && matches!(self.input_type().as_str(), "checkbox" | "radio")
                    && self.has_attr("checked"))
                    || (self.is_html("option") && self.selectedness())
            }
            // No user interaction in the tests.
            K::Hover | K::Active | K::Focus | K::FocusVisible | K::FocusWithin => false,
            other => panic!("the test tree has no state for :{}", other.name()),
        }
    }
    fn custom_state(&self, _: &str) -> bool {
        false
    }
    /// HTML 3.2.6.2: the nearest inclusive ancestor with `xml:lang`, or an
    /// HTML or SVG element with `lang` in no namespace. The fixture sets no
    /// default language.
    fn with_language<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let mut cur = Some(*self);
        while let Some(e) = cur {
            if let Some(v) = e.t.attr(e.i, XML, "lang") {
                return f(v.as_bytes());
            }
            let ns = e.data().0;
            if let Some(v) =
                e.t.attr(e.i, "", "lang")
                    .filter(|_| ns == HTML || ns == SVG)
            {
                return f(v.as_bytes());
            }
            cur = e.parent_element();
        }
        f(b"")
    }
    fn directionality(&self) -> Direction {
        let mut cur = Some(*self);
        while let Some(e) = cur {
            match e
                .t
                .attr(e.i, "", "dir")
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("rtl") => return Direction::Rtl,
                Some("ltr") => return Direction::Ltr,
                Some("auto") => panic!("the test tree has no dir=auto"),
                _ => cur = e.parent_element(),
            }
        }
        Direction::Ltr
    }
}
