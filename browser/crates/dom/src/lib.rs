//! The DOM tree for the Optimal browser engine (WHATWG DOM).
//!
//! Nodes live in one arena per document and are named by [`NodeId`], a plain
//! index. Tree links (parent, children, siblings) are indices too, so the tree
//! has no reference cycles, no reference counting and no `unsafe`, and moving
//! a node is a few index writes.
//!
//! Design choices (logged):
//! - Arena with indices. Rejected: `Rc<RefCell<Node>>` (slow, cycles leak,
//!   runtime borrow panics) and raw pointers (`unsafe`).
//! - Nodes are never freed while their document lives. Detached nodes stay in
//!   the arena; reclaiming them is the garbage collector's job once a script
//!   engine can hold references. When that lands, `NodeId` gains a generation
//!   so a stale handle is a checked error, never a use of the wrong node.
//!
//! Status: the subset the HTML parser needs, with the invariants the DOM
//! standard requires for it, and the element behaviour that changes the tree
//! during parsing (the `select` element model, see [`select`]). The full DOM
//! API comes with the script engine.
//!
//! Mutation follows the DOM standard's insert and remove algorithms: they
//! run the HTML element insertion, post-connection and removing steps, so
//! every caller (the parser now, scripts later) gets the same behaviour.

#![forbid(unsafe_code)]

pub mod names;
pub mod select;

pub use names::{Interner, LocalName, Namespace, local, static_name_count};
use optimal_wtf8::{Wtf8, Wtf8Buf};

/// A node in a [`Document`]'s arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// The index of the node in its document's arena.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The document's mode, set by the parser from the DOCTYPE.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuirksMode {
    /// Standards mode.
    #[default]
    NoQuirks,
    /// Limited-quirks mode.
    LimitedQuirks,
    /// Quirks mode.
    Quirks,
}

/// An attribute on an element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// Namespace (usually none).
    pub namespace: Namespace,
    /// Prefix, for namespaced attributes from the parser (`xlink`, `xml`,
    /// `xmlns`).
    pub prefix: Option<LocalName>,
    /// Local name.
    pub local: LocalName,
    /// Value.
    pub value: Wtf8Buf,
}

/// The parts of a node that differ by kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeData {
    /// The document itself (always node 0).
    Document,
    /// A `DocumentType`.
    Doctype {
        /// Name.
        name: Wtf8Buf,
        /// Public identifier.
        public_id: Wtf8Buf,
        /// System identifier.
        system_id: Wtf8Buf,
    },
    /// An element.
    Element(Element),
    /// A text node.
    Text(Wtf8Buf),
    /// A comment.
    Comment(Wtf8Buf),
    /// A processing instruction.
    ProcessingInstruction {
        /// Target.
        target: Wtf8Buf,
        /// Data.
        data: Wtf8Buf,
    },
    /// A document fragment (for example a template's contents).
    DocumentFragment,
}

/// Element-specific data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// Namespace.
    pub namespace: Namespace,
    /// Local name.
    pub local: LocalName,
    /// Attributes in order.
    pub attributes: Vec<Attribute>,
    /// For `template` elements: the contents fragment.
    pub template_contents: Option<NodeId>,
    /// State the element's behaviour keeps (selectedness, cached select...).
    pub(crate) behaviour: select::Behaviour,
}

impl Element {
    /// Is this an HTML element with local name `name`?
    #[inline]
    pub fn is_html(&self, name: LocalName) -> bool {
        self.namespace == Namespace::Html && self.local == name
    }

    /// The value of the attribute with no namespace and local name `name`.
    pub fn attribute(&self, name: LocalName) -> Option<Wtf8<'_>> {
        self.attributes
            .iter()
            .find(|a| a.namespace == Namespace::None && a.local == name)
            .map(|a| a.value.as_wtf8())
    }
}

/// One node: its links and its data.
#[derive(Clone, Debug)]
pub struct Node {
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
    /// What kind of node this is, and its data.
    pub data: NodeData,
}

/// A document and all of its nodes.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    /// Names used by this document's elements and attributes.
    pub names: Interner,
    /// Quirks mode.
    pub quirks_mode: QuirksMode,
    /// Element behaviour bookkeeping: microtasks, ancestor-context memos.
    pub(crate) model: select::Model,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// The document node.
    pub const ROOT: NodeId = NodeId(0);

    /// An empty document.
    pub fn new() -> Self {
        Document {
            nodes: vec![Node {
                parent: None,
                first_child: None,
                last_child: None,
                prev_sibling: None,
                next_sibling: None,
                data: NodeData::Document,
            }],
            names: Interner::new(),
            quirks_mode: QuirksMode::NoQuirks,
            model: select::Model {
                flagged: vec![false],
                ..select::Model::default()
            },
        }
    }

    /// An empty document that runs element behaviour exactly as the
    /// standard words it: full ancestor walks, full descendant traversals
    /// and the full selectedness setting algorithm every time, with no
    /// memos or fast paths. Tests run the same operations on this and on
    /// [`Document::new`] and require identical results.
    pub fn new_reference() -> Self {
        let mut d = Self::new();
        d.model.reference = true;
        d
    }

    /// Number of nodes ever created (attached or not).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Create a detached node.
    pub fn create(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).expect("more than 4 billion nodes"));
        self.nodes.push(Node {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        });
        self.model.flagged.push(false);
        id
    }

    /// Create a detached element. A `template` in the HTML namespace gets its
    /// contents fragment.
    pub fn create_element(
        &mut self,
        namespace: Namespace,
        local: LocalName,
        attributes: Vec<Attribute>,
    ) -> NodeId {
        let template_contents = (namespace == Namespace::Html && local == local::TEMPLATE)
            .then(|| self.create(NodeData::DocumentFragment));
        let host = NodeId(u32::try_from(self.nodes.len()).expect("more than 4 billion nodes"));
        if let Some(c) = template_contents {
            self.model.hosts.insert(c, host);
        }
        let behaviour = select::Behaviour::for_new_element(namespace, local, &attributes);
        let relevant = behaviour.is_relevant();
        let id = self.create(NodeData::Element(Element {
            namespace,
            local,
            attributes,
            template_contents,
            behaviour,
        }));
        if relevant {
            self.model.flagged[id.index()] = true;
        }
        id
    }

    /// The node's data.
    #[inline]
    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.nodes[id.index()].data
    }

    /// The node's data, mutably.
    #[inline]
    pub fn data_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id.index()].data
    }

    /// The element data, if the node is an element.
    #[inline]
    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes[id.index()].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// The element data, mutably, if the node is an element.
    #[inline]
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes[id.index()].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Parent.
    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent
    }

    /// First child.
    #[inline]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].first_child
    }

    /// Last child.
    #[inline]
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].last_child
    }

    /// Previous sibling.
    #[inline]
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].prev_sibling
    }

    /// Next sibling.
    #[inline]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].next_sibling
    }

    /// Iterate over the children of `id`, in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.first_child(id),
        }
    }

    /// Is `a` an inclusive ancestor of `b` (`a == b`, or `a` is above `b`)?
    pub fn is_inclusive_ancestor(&self, a: NodeId, b: NodeId) -> bool {
        let mut n = Some(b);
        while let Some(x) = n {
            if x == a {
                return true;
            }
            n = self.parent(x);
        }
        false
    }

    /// Would inserting `child` into `parent` make a node its own ancestor?
    /// This is the DOM's "host-including inclusive ancestor" check: a
    /// template's contents count as being inside the template, so a
    /// template cannot go into its own contents. A node with no children
    /// and no template contents can only cycle by being `parent`, which
    /// keeps the parser's inserts (always of new, empty nodes) O(1) at any
    /// depth.
    pub fn would_cycle(&self, child: NodeId, parent: NodeId) -> bool {
        let contents = self.element(child).and_then(|e| e.template_contents);
        let has_contents = contents.is_some_and(|c| self.nodes[c.index()].first_child.is_some());
        if self.nodes[child.index()].first_child.is_none() && !has_contents {
            // Below an empty node there is only an empty contents fragment.
            return child == parent || contents == Some(parent);
        }
        let mut n = Some(parent);
        while let Some(x) = n {
            if x == child {
                return true;
            }
            n = self.parent(x).or_else(|| self.model.hosts.get(&x).copied());
        }
        false
    }

    /// Does the node have an element child?
    pub fn has_element_child(&self, id: NodeId) -> bool {
        self.children(id).any(|c| self.element(c).is_some())
    }

    /// The DOM's "remove": detach `child` from its parent, if it has one,
    /// then run the removing steps for it and its descendants.
    pub fn remove(&mut self, child: NodeId) {
        let Some(parent) = self.nodes[child.index()].parent else {
            return;
        };
        self.unlink(child, parent);
        self.after_removal(child, parent);
    }

    /// Detach `child` from `parent`, touching links only.
    fn unlink(&mut self, child: NodeId, parent: NodeId) {
        let prev = self.nodes[child.index()].prev_sibling;
        let next = self.nodes[child.index()].next_sibling;
        match prev {
            Some(p) => self.nodes[p.index()].next_sibling = next,
            None => self.nodes[parent.index()].first_child = next,
        }
        match next {
            Some(n) => self.nodes[n.index()].prev_sibling = prev,
            None => self.nodes[parent.index()].last_child = prev,
        }
        let c = &mut self.nodes[child.index()];
        c.parent = None;
        c.prev_sibling = None;
        c.next_sibling = None;
    }

    /// The DOM's "pre-insert" and "insert": insert `child` into `parent`
    /// before `reference` (or at the end when `reference` is `None`). A
    /// document fragment inserts its children instead. `child` is first
    /// removed from wherever it is (with its removing steps), then the
    /// insertion and post-connection steps run.
    ///
    /// # Panics
    /// If `reference` is not a child of `parent`, or if the insertion would
    /// make a node its own ancestor. Callers (the parser) check both first, as
    /// the DOM's pre-insertion validity rules require.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        assert!(
            !self.would_cycle(child, parent),
            "insertion would create a cycle"
        );
        let mut reference = reference;
        if let Some(r) = reference {
            assert_eq!(
                self.parent(r),
                Some(parent),
                "reference is not a child of parent"
            );
            // Pre-insert: inserting a node before itself inserts it before
            // its next sibling (it is removed and put back in place).
            if r == child {
                reference = self.next_sibling(child);
            }
        }
        let nodes: Vec<NodeId> = if matches!(self.data(child), NodeData::DocumentFragment) {
            let nodes: Vec<NodeId> = self.children(child).collect();
            for &n in &nodes {
                self.remove(n);
            }
            nodes
        } else {
            vec![child]
        };
        for &n in &nodes {
            self.remove(n);
            self.link(parent, n, reference);
            self.after_link(n, parent);
        }
        self.post_connection(&nodes);
    }

    /// Link a detached `child` into `parent` before `reference`, touching
    /// links only.
    fn link(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        let prev = match reference {
            Some(r) => self.nodes[r.index()].prev_sibling,
            None => self.nodes[parent.index()].last_child,
        };
        {
            let c = &mut self.nodes[child.index()];
            c.parent = Some(parent);
            c.prev_sibling = prev;
            c.next_sibling = reference;
        }
        match prev {
            Some(p) => self.nodes[p.index()].next_sibling = Some(child),
            None => self.nodes[parent.index()].first_child = Some(child),
        }
        match reference {
            Some(r) => self.nodes[r.index()].prev_sibling = Some(child),
            None => self.nodes[parent.index()].last_child = Some(child),
        }
    }

    /// Append `child` to `parent`.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, None);
    }

    /// Move every child of `from` to the end of `to`, in order.
    pub fn move_children(&mut self, from: NodeId, to: NodeId) {
        if from == to {
            // Appending each child to its own parent in order changes nothing.
            return;
        }
        while let Some(c) = self.first_child(from) {
            self.append(to, c);
        }
    }

    /// The text of a static or interned name.
    pub fn name_text(&self, name: LocalName) -> Wtf8<'_> {
        self.names.text(name)
    }

    /// Check every link invariant: parents and children agree, sibling lists
    /// are consistent in both directions, and there are no cycles. For tests.
    pub fn check_invariants(&self) {
        for (i, n) in self.nodes.iter().enumerate() {
            let id = NodeId(i as u32);
            // The prev-link check also rules out a sibling cycle, so this
            // walk always ends: if the walk first came back to a node, that
            // node's prev link would have to name two different nodes (or
            // be None, for the first child).
            let mut prev = None;
            let mut c = n.first_child;
            while let Some(x) = c {
                let xn = &self.nodes[x.index()];
                assert_eq!(xn.parent, Some(id), "child's parent link");
                assert_eq!(xn.prev_sibling, prev, "prev link");
                prev = Some(x);
                c = xn.next_sibling;
            }
            assert_eq!(n.last_child, prev, "last child link");
            if n.parent.is_none() {
                assert!(n.prev_sibling.is_none() && n.next_sibling.is_none());
            }
            assert!(
                !self
                    .parent(id)
                    .is_some_and(|p| self.is_inclusive_ancestor(id, p)),
                "cycle"
            );
        }
        self.check_behaviour();
    }
}

/// Iterator over a node's children. See [`Document::children`].
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let n = self.next?;
        self.next = self.doc.next_sibling(n);
        Some(n)
    }
}

#[cfg(test)]
mod tests;
