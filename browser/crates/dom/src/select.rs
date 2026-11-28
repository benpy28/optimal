//! Element behaviour that changes the tree: the `select` element model.
//!
//! HTML 4.10.7 (`select`), 4.10.10 (`option`) and the `selectedcontent`
//! element. An `option` has a selectedness that the "selectedness setting
//! algorithm" keeps consistent per `select`, and every enabled
//! `selectedcontent` inside a `select` holds a clone of the selected
//! option's children. All of it runs from the DOM's insertion,
//! post-connection and removing steps, the parser's "children parsed" steps
//! and attribute changes, so the parser and (later) scripts see the same
//! behaviour.
//!
//! # Exactness and cost
//!
//! The standard's words cost O(depth) per ancestor walk, O(subtree) per
//! insert or remove, and O(options) per selectedness run, which is
//! quadratic for a `select` with many options. This module computes the
//! same results faster, and [`Document::new_reference`] runs the standard's
//! words literally; tests require the two to agree after every operation.
//! The speedups (each logged: chosen, why, rejected):
//!
//! - **Ancestor context memo.** Every walk the standard does upwards
//!   (nearest ancestor `select`, option disabledness, `selectedcontent`
//!   disabledness, connectedness) is a fold over the ancestors, so the fold
//!   result for "a child of N" is memoised on N and computed from N's
//!   parent's memo. A memo depends only on N's ancestors, which change only
//!   when a subtree containing N moves, so moves clear memos inside the
//!   moved subtree and nothing else. Rejected: a global epoch (one move
//!   would invalidate every memo, so alternating moves and inserts would be
//!   quadratic); dynamic-tree structures (an O(log n) cost on every insert
//!   of every page, for one element type).
//! - **Flags for relevant subtrees.** Only `option` and `selectedcontent`
//!   elements have steps, so a node is flagged once a relevant element (or a
//!   memo) is below it, and flags are upward closed and never cleared.
//!   Insert and remove traverse flagged nodes only, so moving subtrees that
//!   never held an option costs nothing (the adoption agency moves many).
//!   Rejected: exact descendant counts (O(depth) per insert to maintain).
//! - **Settled selects.** After a selectedness run, a `select` records the
//!   one option left selected. Inserting or removing a subtree with exactly
//!   one option in it (a new `<option>`, or the adoption agency moving one)
//!   then updates that record in O(1) instead of rerunning the algorithm,
//!   which is what makes `<select>` with many `<option>`s linear. Subtrees
//!   with several options rerun the full algorithm once per operation.
//! - **Registries.** Each `select` keeps its enabled `selectedcontent`
//!   elements and a count of all `selectedcontent` descendants, so "update
//!   the descendant selectedcontent elements" and "has no descendant
//!   selectedcontent elements" need no traversal.
//!
//! Remaining super-linear cases, all from crafted input: moving one subtree
//! many times when an option or `selectedcontent` sits deep inside it (each
//! move visits the flagged path to it, as the standard's steps visit every
//! descendant), and the standard's own amplification (every
//! `selectedcontent` gets its own clone of the option, and each selection
//! change re-clones into all of them).
//!
//! # A deviation the standard forces
//!
//! Cloning an option's children into a `selectedcontent` can insert option
//! elements (an `<option>` nested inside the selected option) whose nearest
//! `select` is the same `select`. Their insertion steps queue another update
//! and their post-connection steps run it, which clones again: the standard
//! never terminates there. While a `select`'s update runs, mutations it
//! makes do not queue or run another update of that `select`. Every other
//! case is unaffected, because only such clones can re-trigger an update.
//!
//! Moving steps (`moveBefore`) are not implemented yet; nothing calls them.

use crate::{Document, LocalName, Namespace, NodeData, NodeId, local};
use optimal_wtf8::Wtf8Buf;
use std::collections::{HashMap, VecDeque};

/// Per-element behaviour state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) enum Behaviour {
    /// No state.
    #[default]
    None,
    /// A `select` element.
    Select(SelectState),
    /// An `option` element.
    Option(OptionState),
    /// A `selectedcontent` element.
    SelectedContent(ContentState),
}

impl Behaviour {
    pub(crate) fn for_new_element(
        ns: Namespace,
        name: LocalName,
        attributes: &[crate::Attribute],
    ) -> Behaviour {
        if ns != Namespace::Html {
            return Behaviour::None;
        }
        match name {
            local::SELECT => Behaviour::Select(SelectState::default()),
            local::OPTION => Behaviour::Option(OptionState {
                // "when the element is created, its selectedness must be set
                // to true if the element has a selected attribute."
                selectedness: attributes
                    .iter()
                    .any(|a| a.namespace == Namespace::None && a.local == local::SELECTED),
                dirtiness: false,
                cached_select: None,
            }),
            local::SELECTEDCONTENT => Behaviour::SelectedContent(ContentState::default()),
            _ => Behaviour::None,
        }
    }

    /// Does the element have insertion or removing steps here?
    pub(crate) fn is_relevant(&self) -> bool {
        matches!(self, Behaviour::Option(_) | Behaviour::SelectedContent(_))
    }
}

/// A `select` element's state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct SelectState {
    /// The standard's "pending selectedcontent update".
    pending: bool,
    /// Enabled `selectedcontent` elements whose cached select is this one.
    contents: Vec<NodeId>,
    /// How many `selectedcontent` elements are descendants (enabled or not).
    descendant_contents: usize,
    /// True when the options in the list of options with selectedness true
    /// are exactly `selected` (one or none) and the selectedness setting
    /// algorithm would change nothing. Never true with `multiple`.
    settled: bool,
    /// See `settled`.
    selected: Option<NodeId>,
    /// The operation in which the select last settled.
    settled_op: u64,
    /// An update of the descendant `selectedcontent` elements is running.
    updating: bool,
}

/// An `option` element's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OptionState {
    selectedness: bool,
    dirtiness: bool,
    /// The standard's "cached nearest ancestor select element".
    cached_select: Option<NodeId>,
}

/// A `selectedcontent` element's state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct ContentState {
    disabled: bool,
    cached_select: Option<NodeId>,
}

/// Document-wide bookkeeping.
#[derive(Clone, Debug, Default)]
pub(crate) struct Model {
    /// Run the standard's words literally (see [`Document::new_reference`]).
    pub(crate) reference: bool,
    /// Per node: an option, a `selectedcontent` or a memo is (or was) at or
    /// below it. Upward closed.
    pub(crate) flagged: Vec<bool>,
    /// Ancestor context for a child of the key node.
    pub(crate) memo: HashMap<NodeId, Ctx>,
    /// The microtask queue: selects whose update was queued.
    pub(crate) microtasks: VecDeque<NodeId>,
    /// Counts insert and remove operations, for `SelectState::settled_op`.
    pub(crate) op: u64,
    /// Template contents fragment to its template (the fragment's host).
    pub(crate) hosts: HashMap<NodeId, NodeId>,
    /// Work done, so tests can check the fast paths are taken.
    pub(crate) stats: Stats,
}

/// Counts of the model's work. The fast paths change only these, never a
/// result, so tests check them to know the fast paths are taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Runs of the full selectedness setting algorithm.
    pub full_runs: u64,
    /// Ancestor context folds computed (memo misses, per node).
    pub ctx_steps: u64,
    /// Nodes visited looking for options and selectedcontent elements in
    /// inserted or removed subtrees.
    pub visited: u64,
    /// Runs of "update a select's descendant selectedcontent elements".
    pub content_updates: u64,
}

/// The fold of every upward walk the model makes, for a child of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ctx {
    /// "Nearest ancestor select" for an option child, having seen no
    /// optgroup yet, and having seen one.
    opt0: Option<NodeId>,
    opt1: Option<NodeId>,
    /// An option child is disabled by an optgroup ancestor.
    opt_disabled: bool,
    /// "Recalculate a selectedcontent element's disabledness" for a
    /// selectedcontent child: its nearest select, and whether it is disabled.
    sc_select: Option<NodeId>,
    sc_disabled: bool,
    /// A select, option or selectedcontent is at or above the node.
    blocked: bool,
    /// The nearest select at or above the node, with no other conditions.
    any_select: Option<NodeId>,
    /// The node is connected.
    connected: bool,
}

impl Ctx {
    const DETACHED: Ctx = Ctx {
        opt0: None,
        opt1: None,
        opt_disabled: false,
        sc_select: None,
        sc_disabled: false,
        blocked: false,
        any_select: None,
        connected: false,
    };
}

/// What changed in a select's list of options before a selectedness run.
#[derive(Clone, Copy, Debug)]
enum Change {
    /// Unknown: run the algorithm.
    Full,
    /// One option joined (true) or left (false), and nothing else in any
    /// list of options changed.
    Leaf(NodeId, bool),
    /// Part of the operation with this number.
    Batch,
}

/// The context of the operation whose steps are running.
#[derive(Clone, Copy, Debug)]
struct Op {
    /// The only option in the subtree inserted or removed, if there is
    /// exactly one: then it is the only change to any list of options.
    single: Option<NodeId>,
}

fn is_html(doc: &Document, n: NodeId, name: LocalName) -> bool {
    doc.element(n).is_some_and(|e| e.is_html(name))
}

impl Document {
    // ---- State accessors ---------------------------------------------------

    fn behaviour(&self, n: NodeId) -> &Behaviour {
        match self.data(n) {
            NodeData::Element(e) => &e.behaviour,
            _ => &Behaviour::None,
        }
    }

    fn select_state(&mut self, s: NodeId) -> &mut SelectState {
        match &mut self.element_mut(s).expect("select element").behaviour {
            Behaviour::Select(st) => st,
            _ => unreachable!("not a select element"),
        }
    }

    fn option_state(&mut self, o: NodeId) -> &mut OptionState {
        match &mut self.element_mut(o).expect("option element").behaviour {
            Behaviour::Option(st) => st,
            _ => unreachable!("not an option element"),
        }
    }

    fn content_state(&mut self, c: NodeId) -> &mut ContentState {
        match &mut self
            .element_mut(c)
            .expect("selectedcontent element")
            .behaviour
        {
            Behaviour::SelectedContent(st) => st,
            _ => unreachable!("not a selectedcontent element"),
        }
    }

    fn select_ref(&self, s: NodeId) -> &SelectState {
        match self.behaviour(s) {
            Behaviour::Select(st) => st,
            _ => unreachable!("not a select element"),
        }
    }

    /// An option's selectedness, or `None` if `o` is not an HTML option.
    pub fn option_selectedness(&self, o: NodeId) -> Option<bool> {
        match self.behaviour(o) {
            Behaviour::Option(st) => Some(st.selectedness),
            _ => None,
        }
    }

    fn selectedness(&self, o: NodeId) -> bool {
        self.option_selectedness(o).expect("option element")
    }

    fn set_selectedness(&mut self, o: NodeId, v: bool) {
        self.option_state(o).selectedness = v;
    }

    fn has_multiple(&self, s: NodeId) -> bool {
        self.element(s)
            .is_some_and(|e| e.attribute(local::MULTIPLE).is_some())
    }

    /// The select's display size: the `size` attribute parsed by the rules
    /// for parsing non-negative integers, else 4 with `multiple`, else 1.
    pub fn display_size(&self, s: NodeId) -> u64 {
        let e = self.element(s).expect("select element");
        if let Some(v) = e.attribute(local::SIZE)
            && let Some(n) = parse_non_negative_integer(v.as_bytes())
        {
            return n;
        }
        if self.has_multiple(s) { 4 } else { 1 }
    }

    // ---- Ancestor context ------------------------------------------------

    /// The context for a child of `n` given the context for a child of
    /// `n`'s parent (or `None` if `n` has no parent).
    fn ctx_step(&self, n: NodeId, parent: Option<Ctx>) -> Ctx {
        let p = parent.unwrap_or(Ctx {
            connected: n == Document::ROOT,
            ..Ctx::DETACHED
        });
        let Some(e) = self.element(n).filter(|e| e.namespace == Namespace::Html) else {
            return p;
        };
        match e.local {
            local::SELECT => Ctx {
                opt0: Some(n),
                opt1: Some(n),
                opt_disabled: false,
                sc_select: Some(n),
                sc_disabled: p.blocked,
                blocked: true,
                any_select: Some(n),
                connected: p.connected,
            },
            local::OPTION => Ctx {
                opt0: None,
                opt1: None,
                opt_disabled: false,
                sc_select: None,
                sc_disabled: true,
                blocked: true,
                ..p
            },
            local::SELECTEDCONTENT => Ctx {
                sc_select: None,
                sc_disabled: true,
                blocked: true,
                ..p
            },
            local::DATALIST | local::HR => Ctx {
                opt0: None,
                opt1: None,
                opt_disabled: false,
                ..p
            },
            local::OPTGROUP => Ctx {
                opt0: p.opt1,
                opt1: None,
                opt_disabled: e.attribute(local::DISABLED).is_some(),
                ..p
            },
            _ => p,
        }
    }

    /// The context for a child of `n`, by walking to the root.
    fn ctx_walk(&self, n: NodeId) -> Ctx {
        let mut path = vec![n];
        while let Some(p) = self.parent(*path.last().unwrap()) {
            path.push(p);
        }
        let mut ctx = None;
        for &x in path.iter().rev() {
            ctx = Some(self.ctx_step(x, ctx));
        }
        ctx.unwrap()
    }

    /// The context for a child of `n`, memoised.
    fn ctx(&mut self, n: NodeId) -> Ctx {
        if self.model.reference {
            return self.ctx_walk(n);
        }
        if let Some(c) = self.model.memo.get(&n) {
            return *c;
        }
        let mut path = vec![n];
        let mut base = None;
        while let Some(p) = self.parent(*path.last().unwrap()) {
            if let Some(c) = self.model.memo.get(&p) {
                base = Some(*c);
                break;
            }
            path.push(p);
        }
        let mut ctx = base;
        self.model.stats.ctx_steps += path.len() as u64;
        for &x in path.iter().rev() {
            let c = self.ctx_step(x, ctx);
            self.model.memo.insert(x, c);
            // Memos live only on flagged nodes, so clearing memos in a moved
            // subtree needs to visit flagged nodes only. The path ends at a
            // memoised (so flagged) node or at a root, so flags stay upward
            // closed.
            self.model.flagged[x.index()] = true;
            ctx = Some(c);
        }
        ctx.unwrap()
    }

    /// The context for `n` itself: for a child of its parent.
    fn ctx_of(&mut self, n: NodeId) -> Ctx {
        match self.parent(n) {
            Some(p) => self.ctx(p),
            // Only the root has no parent and is connected, and nothing
            // asks for the root's own context.
            None => Ctx::DETACHED,
        }
    }

    /// Is the option disabled (HTML 4.10.10)?
    pub fn option_disabled(&self, o: NodeId) -> bool {
        if self
            .element(o)
            .is_some_and(|e| e.attribute(local::DISABLED).is_some())
        {
            return true;
        }
        self.parent(o)
            .is_some_and(|p| self.ctx_walk(p).opt_disabled)
    }

    fn option_disabled_mut(&mut self, o: NodeId) -> bool {
        if self
            .element(o)
            .is_some_and(|e| e.attribute(local::DISABLED).is_some())
        {
            return true;
        }
        self.ctx_of(o).opt_disabled
    }

    /// The selects at or above `n`, nearest first.
    fn select_chain(&mut self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut s = self.ctx(n).any_select;
        while let Some(x) = s {
            out.push(x);
            s = self.ctx_of(x).any_select;
        }
        out
    }

    // ---- Traversals ------------------------------------------------------

    /// The option and selectedcontent elements among `root`'s inclusive
    /// descendants, in tree order. Clears the memos it passes, because the
    /// caller is moving `root`.
    fn relevant_in(&mut self, root: NodeId) -> Vec<NodeId> {
        self.relevant_walk(root, true)
    }

    fn relevant_walk(&mut self, root: NodeId, clear: bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        let reference = self.model.reference;
        if !reference && !self.model.flagged[root.index()] {
            return out;
        }
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            self.model.stats.visited += 1;
            if clear && !reference {
                self.model.memo.remove(&n);
            }
            if self.behaviour(n).is_relevant() {
                out.push(n);
            }
            let mut c = self.last_child(n);
            while let Some(x) = c {
                if reference || self.model.flagged[x.index()] {
                    stack.push(x);
                }
                c = self.prev_sibling(x);
            }
        }
        out
    }

    /// All descendants of `root` in tree order, for the literal model.
    fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.children(root).collect();
        stack.reverse();
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut c = self.last_child(n);
            while let Some(x) = c {
                stack.push(x);
                c = self.prev_sibling(x);
            }
        }
        out
    }

    /// Does `a` come before `b` in tree order?
    pub fn precedes(&self, a: NodeId, b: NodeId) -> bool {
        if a == b {
            return false;
        }
        if self.parent(a).is_some() && self.parent(a) == self.parent(b) {
            return self.sibling_precedes(a, b);
        }
        let chain = |mut n: NodeId| {
            let mut v = vec![n];
            while let Some(p) = self.parent(n) {
                v.push(p);
                n = p;
            }
            v.reverse();
            v
        };
        let (ca, cb) = (chain(a), chain(b));
        let i = ca.iter().zip(&cb).take_while(|(x, y)| x == y).count();
        if i == ca.len() {
            return true; // a is an ancestor of b
        }
        if i == cb.len() {
            return false; // b is an ancestor of a
        }
        assert!(i > 0, "nodes in different trees have no tree order");
        self.sibling_precedes(ca[i], cb[i])
    }

    /// Does sibling `x` come before sibling `y`? Searches outwards from `x`
    /// in both directions, so the cost is the distance between them.
    fn sibling_precedes(&self, x: NodeId, y: NodeId) -> bool {
        let (mut back, mut fwd) = (self.prev_sibling(x), self.next_sibling(x));
        loop {
            if fwd == Some(y) {
                return true;
            }
            if back == Some(y) {
                return false;
            }
            assert!(back.is_some() || fwd.is_some(), "siblings share a parent");
            back = back.and_then(|n| self.prev_sibling(n));
            fwd = fwd.and_then(|n| self.next_sibling(n));
        }
    }

    /// The select's list of options (HTML 4.10.7), in tree order.
    pub fn list_of_options(&self, s: NodeId) -> Vec<NodeId> {
        self.list_with_groups(s)
            .into_iter()
            .map(|(o, _)| o)
            .collect()
    }

    /// The list of options, each with its optgroup (if any), so callers can
    /// tell disabledness without walking.
    fn list_with_groups(&self, s: NodeId) -> Vec<(NodeId, Option<NodeId>)> {
        let mut out = Vec::new();
        // (node, the optgroup between it and the select, if any)
        let mut stack: Vec<(NodeId, Option<NodeId>)> = Vec::new();
        let push_children = |doc: &Document, stack: &mut Vec<_>, n: NodeId, g: Option<NodeId>| {
            let mut c = doc.last_child(n);
            while let Some(x) = c {
                stack.push((x, g));
                c = doc.prev_sibling(x);
            }
        };
        push_children(self, &mut stack, s, None);
        while let Some((n, g)) = stack.pop() {
            if is_html(self, n, local::OPTION) {
                out.push((n, g));
            }
            let skip = is_html(self, n, local::SELECT)
                || is_html(self, n, local::HR)
                || is_html(self, n, local::OPTION)
                || is_html(self, n, local::DATALIST)
                || (is_html(self, n, local::OPTGROUP) && g.is_some());
            if !skip {
                let g = if is_html(self, n, local::OPTGROUP) {
                    Some(n)
                } else {
                    g
                };
                push_children(self, &mut stack, n, g);
            }
        }
        out
    }

    // ---- The DOM hooks ---------------------------------------------------

    fn next_op(&mut self, relevant: &[NodeId]) -> Op {
        self.model.op += 1;
        let mut options = relevant
            .iter()
            .filter(|&&d| matches!(self.behaviour(d), Behaviour::Option(_)));
        let single = match (options.next(), options.next()) {
            (Some(&o), None) => Some(o),
            _ => None,
        };
        Op { single }
    }

    /// After `n` was linked under `parent`: the insertion steps for `n`'s
    /// inclusive descendants.
    pub(crate) fn after_link(&mut self, n: NodeId, parent: NodeId) {
        if self.model.flagged[n.index()] {
            let mut a = Some(parent);
            while let Some(x) = a {
                if self.model.flagged[x.index()] {
                    break;
                }
                self.model.flagged[x.index()] = true;
                a = self.parent(x);
            }
        }
        let relevant = self.relevant_in(n);
        if relevant.is_empty() {
            return;
        }
        let op = self.next_op(&relevant);
        let contents = relevant
            .iter()
            .filter(|&&d| is_html(self, d, local::SELECTEDCONTENT))
            .count();
        for s in self.select_chain(parent) {
            self.select_state(s).descendant_contents += contents;
        }
        for d in relevant {
            self.insertion_steps(d, op);
        }
    }

    /// After `n` was detached from `parent`: the removing steps for `n`'s
    /// inclusive descendants.
    pub(crate) fn after_removal(&mut self, n: NodeId, parent: NodeId) {
        let relevant = self.relevant_in(n);
        if relevant.is_empty() {
            return;
        }
        let op = self.next_op(&relevant);
        let contents = relevant
            .iter()
            .filter(|&&d| is_html(self, d, local::SELECTEDCONTENT))
            .count();
        for s in self.select_chain(parent) {
            self.select_state(s).descendant_contents -= contents;
        }
        for d in relevant {
            self.removing_steps(d, op);
        }
    }

    /// The post-connection steps for the inserted nodes' connected
    /// inclusive descendants.
    pub(crate) fn post_connection(&mut self, nodes: &[NodeId]) {
        let mut list = Vec::new();
        for &n in nodes {
            // Memos were cleared when these were linked, and any made since
            // are for their new place.
            list.extend(self.relevant_walk(n, false));
        }
        for n in list {
            if !self.is_connected_mut(n) {
                continue;
            }
            let select = match self.behaviour(n) {
                Behaviour::Option(st) => st.cached_select,
                Behaviour::SelectedContent(st) => st.cached_select,
                _ => None,
            };
            // Both elements' steps: "Let select be the cached select. If
            // select is null or its pending selectedcontent update is false,
            // return. Update its descendant selectedcontent elements."
            if let Some(s) = select
                && self.select_ref(s).pending
                && !self.select_ref(s).updating
            {
                self.update_contents(s);
            }
        }
    }

    fn is_connected_mut(&mut self, n: NodeId) -> bool {
        self.ctx_of(n).connected
    }

    fn insertion_steps(&mut self, d: NodeId, op: Op) {
        match self.behaviour(d) {
            Behaviour::Option(_) => {
                let selects = self.update_option_select(d, op);
                if !self.is_connected_mut(d) {
                    return;
                }
                for s in selects {
                    self.queue_update(s);
                }
            }
            Behaviour::SelectedContent(_) => {
                let select = self.recalc_content(d);
                if !self.is_connected_mut(d) {
                    return;
                }
                let disabled = self.content_state(d).disabled;
                if let Some(s) = select
                    && !disabled
                    && !self.has_multiple(s)
                {
                    self.queue_update(s);
                }
            }
            _ => {}
        }
    }

    fn removing_steps(&mut self, d: NodeId, op: Op) {
        match self.behaviour(d) {
            Behaviour::Option(_) => {
                for s in self.update_option_select(d, op) {
                    self.queue_update(s);
                }
            }
            Behaviour::SelectedContent(_) => {
                self.recalc_content(d);
            }
            _ => {}
        }
    }

    /// The parser's "children parsed" steps for an element it popped.
    pub fn children_parsed(&mut self, el: NodeId) {
        let Behaviour::Option(st) = self.behaviour(el) else {
            return;
        };
        let (select, selected) = (st.cached_select, st.selectedness);
        // Not reachable during an update, which never pops an element.
        if let Some(s) = select
            && selected
        {
            self.update_contents(s);
        }
    }

    /// Run every queued microtask.
    pub fn perform_microtask_checkpoint(&mut self) {
        while let Some(s) = self.model.microtasks.pop_front() {
            // Not reachable during an update, which never runs microtasks.
            if self.select_ref(s).pending {
                self.update_contents(s);
            }
        }
    }

    /// Are microtasks queued?
    pub fn has_microtasks(&self) -> bool {
        !self.model.microtasks.is_empty()
    }

    // ---- option ----------------------------------------------------------

    /// "Update an option's nearest ancestor select".
    fn update_option_select(&mut self, o: NodeId, op: Op) -> Vec<NodeId> {
        let old = self.option_state(o).cached_select;
        let new = self.ctx_of(o).opt0;
        self.option_state(o).cached_select = new;
        if old == new {
            return Vec::new();
        }
        let mut result = Vec::new();
        for (s, joined) in [(old, false), (new, true)] {
            let Some(s) = s else { continue };
            let change = if op.single == Some(o) {
                Change::Leaf(o, joined)
            } else {
                Change::Batch
            };
            let changed = self.run_selectedness(s, change);
            if self.selectedness(o) || changed {
                result.push(s);
            }
        }
        result
    }

    /// The selectedness setting algorithm. Returns hasChanged.
    fn run_selectedness(&mut self, s: NodeId, change: Change) -> bool {
        if self.has_multiple(s) {
            self.select_state(s).settled = false;
            return false;
        }
        let st = self.select_ref(s);
        if !self.model.reference && st.settled {
            match change {
                Change::Batch if st.settled_op == self.model.op => return false,
                Change::Leaf(o, joined) => return self.run_leaf(s, o, joined),
                _ => {}
            }
        }
        self.run_full(s)
    }

    /// The algorithm as the standard words it.
    fn run_full(&mut self, s: NodeId) -> bool {
        self.model.stats.full_runs += 1;
        let mut has_changed = false;
        let mut first_enabled = None;
        let mut last_selected: Option<NodeId> = None;
        for (o, group) in self.list_with_groups(s) {
            if self.selectedness(o) {
                if let Some(l) = last_selected {
                    self.set_selectedness(l, false);
                    has_changed = true;
                }
                last_selected = Some(o);
            }
            if first_enabled.is_none() {
                let disabled = self
                    .element(o)
                    .unwrap()
                    .attribute(local::DISABLED)
                    .is_some()
                    || group.is_some_and(|g| {
                        self.element(g)
                            .unwrap()
                            .attribute(local::DISABLED)
                            .is_some()
                    });
                if !disabled {
                    first_enabled = Some(o);
                }
            }
        }
        let mut selected = last_selected;
        if last_selected.is_none()
            && let Some(f) = first_enabled
            && self.display_size(s) == 1
        {
            self.set_selectedness(f, true);
            has_changed = true;
            selected = Some(f);
        }
        let op = self.model.op;
        let st = self.select_state(s);
        st.settled = true;
        st.selected = selected;
        st.settled_op = op;
        has_changed
    }

    /// The algorithm when one option `o` joined or left a settled select
    /// and nothing else in its list changed. Same result as `run_full`:
    /// - joined, selected: it and the recorded option are the only selected
    ///   ones, and the later in tree order wins;
    /// - joined, not selected: nothing changes unless no option was
    ///   selected, which (settled, display size 1) means no option was
    ///   enabled, so `o` is the first enabled option if it is enabled;
    /// - left, not the recorded option: nothing changes;
    /// - left, the recorded option: none is selected now, so with display
    ///   size 1 the first enabled option must be found: run in full.
    ///
    /// "Only `o` changed" is what makes this exact: with two options moving
    /// together, the first one's run already sees the second in its new
    /// place.
    fn run_leaf(&mut self, s: NodeId, o: NodeId, joined: bool) -> bool {
        let recorded = self.select_ref(s).selected;
        if !joined {
            if recorded != Some(o) {
                return false;
            }
            self.select_state(s).selected = None;
            if self.display_size(s) == 1 {
                return self.run_full(s);
            }
            return false;
        }
        if self.selectedness(o) {
            match recorded {
                None => {
                    self.select_state(s).selected = Some(o);
                    false
                }
                Some(r) => {
                    if self.precedes(r, o) {
                        self.set_selectedness(r, false);
                        self.select_state(s).selected = Some(o);
                    } else {
                        self.set_selectedness(o, false);
                    }
                    true
                }
            }
        } else if recorded.is_none() && self.display_size(s) == 1 && !self.option_disabled_mut(o) {
            self.set_selectedness(o, true);
            self.select_state(s).selected = Some(o);
            true
        } else {
            false
        }
    }

    // ---- selectedcontent -------------------------------------------------

    /// "Recalculate a selectedcontent element's disabledness". Keeps the
    /// select's registry of enabled contents in step.
    fn recalc_content(&mut self, c: NodeId) -> Option<NodeId> {
        let ctx = self.ctx_of(c);
        let (select, disabled) = (ctx.sc_select, ctx.sc_disabled);
        let old = self.content_state(c).clone();
        if !old.disabled
            && let Some(s) = old.cached_select
        {
            let list = &mut self.select_state(s).contents;
            let i = list.iter().position(|&x| x == c).expect("registered");
            list.swap_remove(i);
        }
        if !disabled && let Some(s) = select {
            self.select_state(s).contents.push(c);
        }
        let st = self.content_state(c);
        st.disabled = disabled;
        st.cached_select = select;
        select
    }

    /// "Queue an update of a select's descendant selectedcontent elements".
    fn queue_update(&mut self, s: NodeId) {
        let st = self.select_ref(s);
        if st.updating || st.pending || !self.has_descendant_contents(s) {
            return;
        }
        self.select_state(s).pending = true;
        self.model.microtasks.push_back(s);
    }

    fn has_descendant_contents(&self, s: NodeId) -> bool {
        if self.model.reference {
            self.descendants(s)
                .into_iter()
                .any(|n| is_html(self, n, local::SELECTEDCONTENT))
        } else {
            self.select_ref(s).descendant_contents > 0
        }
    }

    /// "Update a select's descendant selectedcontent elements".
    fn update_contents(&mut self, s: NodeId) {
        self.model.stats.content_updates += 1;
        self.select_state(s).pending = false;
        if self.has_multiple(s) {
            return;
        }
        let mut list: Vec<NodeId> = if self.model.reference {
            self.descendants(s)
                .into_iter()
                .filter(|&n| {
                    matches!(self.behaviour(n), Behaviour::SelectedContent(st) if !st.disabled)
                })
                .collect()
        } else {
            let mut v = self.select_ref(s).contents.clone();
            v.sort_by(|&a, &b| {
                if self.precedes(a, b) {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
            v
        };
        self.select_state(s).updating = true;
        for c in list.drain(..) {
            self.update_content(s, c);
        }
        self.select_state(s).updating = false;
    }

    /// "Update a selectedcontent".
    fn update_content(&mut self, s: NodeId, c: NodeId) {
        if self.content_state(c).disabled {
            return;
        }
        match self.first_selected(s) {
            None => self.replace_all(c, None),
            Some(o) => self.clone_option_into(o, c),
        }
    }

    /// The first option in the list of options with selectedness true.
    pub fn first_selected(&self, s: NodeId) -> Option<NodeId> {
        let st = self.select_ref(s);
        if !self.model.reference && st.settled {
            return st.selected;
        }
        self.list_of_options(s)
            .into_iter()
            .find(|&o| self.selectedness(o))
    }

    /// "Clone an option into a selectedcontent".
    fn clone_option_into(&mut self, o: NodeId, c: NodeId) {
        let fragment = self.create(NodeData::DocumentFragment);
        let children: Vec<NodeId> = self.children(o).collect();
        for child in children {
            let copy = self.clone_node(child, true);
            self.append(fragment, copy);
        }
        let removed: Vec<NodeId> = self
            .descendants(fragment)
            .into_iter()
            .filter(|&n| {
                self.element(n).is_some_and(|e| {
                    matches!(
                        (e.namespace, e.local),
                        (Namespace::Html, local::SCRIPT)
                            | (Namespace::Svg, local::SCRIPT)
                            | (Namespace::Html, local::FRAME)
                            | (Namespace::Html, local::IFRAME)
                            | (Namespace::Html, local::EMBED)
                            | (Namespace::Html, local::OBJECT)
                    )
                })
            })
            .collect();
        for e in removed {
            self.remove(e);
        }
        self.replace_all(c, Some(fragment));
    }

    // ---- Tree operations the model needs ---------------------------------

    /// The DOM's "replace all": remove every child of `parent`, then insert
    /// `node` (a fragment inserts its children).
    pub fn replace_all(&mut self, parent: NodeId, node: Option<NodeId>) {
        let children: Vec<NodeId> = self.children(parent).collect();
        for c in children {
            self.remove(c);
        }
        if let Some(n) = node {
            self.insert_before(parent, n, None);
        }
    }

    /// The DOM's "clone a node": a copy of `node`, with its descendants if
    /// `subtree`. Template contents are cloned too (the template cloning
    /// steps). Iterative, so deep trees cannot overflow the stack; children
    /// are cloned in order and each is appended once it is complete, as the
    /// standard's recursion does.
    ///
    /// # Panics
    /// On the document node (cloning documents is not supported yet).
    pub fn clone_node(&mut self, node: NodeId, subtree: bool) -> NodeId {
        struct Frame {
            copy: NodeId,
            /// The children still to clone, and where their copies go.
            todo: Vec<(NodeId, NodeId)>,
        }
        let frame = |doc: &mut Document, n: NodeId| -> Frame {
            let copy = doc.clone_single(n);
            let mut todo = Vec::new();
            if subtree {
                if let (Some(from), Some(to)) = (
                    doc.element(n).and_then(|e| e.template_contents),
                    doc.element(copy).and_then(|e| e.template_contents),
                ) {
                    todo.extend(doc.children(from).map(|c| (c, to)));
                }
                todo.extend(doc.children(n).map(|c| (c, copy)));
            }
            todo.reverse();
            Frame { copy, todo }
        };
        let root = frame(self, node);
        let mut stack = vec![(root, None::<NodeId>)];
        loop {
            let top = stack.last_mut().unwrap();
            if let Some((child, target)) = top.0.todo.pop() {
                let f = frame(self, child);
                stack.push((f, Some(target)));
                continue;
            }
            let (done, target) = stack.pop().unwrap();
            match target {
                Some(t) => self.append(t, done.copy),
                None => return done.copy,
            }
        }
    }

    fn clone_single(&mut self, n: NodeId) -> NodeId {
        match self.data(n).clone() {
            NodeData::Document => panic!("cloning a document is not supported"),
            NodeData::Element(e) => self.create_element(e.namespace, e.local, e.attributes),
            other => self.create(other),
        }
    }

    // ---- Attributes ------------------------------------------------------

    /// Set an attribute with no namespace, then run the attribute change
    /// steps.
    pub fn set_attribute(&mut self, el: NodeId, name: LocalName, value: Wtf8Buf) {
        let e = self.element_mut(el).expect("element");
        let old = match e
            .attributes
            .iter_mut()
            .find(|a| a.namespace == Namespace::None && a.local == name)
        {
            Some(a) => Some(std::mem::replace(&mut a.value, value)),
            None => {
                e.attributes.push(crate::Attribute {
                    namespace: Namespace::None,
                    prefix: None,
                    local: name,
                    value,
                });
                None
            }
        };
        self.attribute_changed(el, name, old.is_some(), true);
    }

    /// Remove an attribute with no namespace, if present, then run the
    /// attribute change steps.
    pub fn remove_attribute(&mut self, el: NodeId, name: LocalName) {
        let e = self.element_mut(el).expect("element");
        let Some(i) = e
            .attributes
            .iter()
            .position(|a| a.namespace == Namespace::None && a.local == name)
        else {
            return;
        };
        e.attributes.remove(i);
        self.attribute_changed(el, name, true, false);
    }

    /// The attribute change steps, given whether the old and new values are
    /// non-null.
    fn attribute_changed(&mut self, el: NodeId, name: LocalName, had: bool, has: bool) {
        let Some(e) = self.element(el).filter(|e| e.namespace == Namespace::Html) else {
            return;
        };
        match e.local {
            local::OPTION => {
                if !matches!(name, local::SELECTED | local::DISABLED) || (had && has) {
                    return;
                }
                let mut has_changed = false;
                let st = self.option_state(el);
                if name == local::SELECTED && !st.dirtiness {
                    if !had && !st.selectedness {
                        st.selectedness = true;
                        has_changed = true;
                    }
                    if !has && st.selectedness {
                        st.selectedness = false;
                        has_changed = true;
                    }
                }
                let Some(s) = self.option_state(el).cached_select else {
                    return;
                };
                self.select_state(s).settled = false;
                self.reset_selectedness(s, has_changed);
            }
            local::SELECT => {
                if name == local::SIZE {
                    self.select_state(el).settled = false;
                    self.reset_selectedness(el, false);
                }
                if name != local::MULTIPLE || (had && has) {
                    return;
                }
                if !has {
                    // "set the selectedness of all but the first option
                    // element with its selectedness set to true in element's
                    // list of options to false."
                    let mut seen = false;
                    for o in self.list_of_options(el) {
                        if self.selectedness(o) {
                            if seen {
                                self.set_selectedness(o, false);
                            }
                            seen = true;
                        }
                    }
                }
                self.select_state(el).settled = false;
                self.reset_selectedness(el, true);
            }
            local::OPTGROUP if name == local::DISABLED && had != has => {
                // No change steps in the standard, but option disabledness
                // below changed: memos below are stale and the select's
                // record no longer proves the algorithm has nothing to do.
                self.relevant_in(el);
                if let Some(s) = self.ctx(el).opt0 {
                    self.select_state(s).settled = false;
                }
            }
            _ => {}
        }
    }

    /// "Reset a select's selectedness".
    fn reset_selectedness(&mut self, s: NodeId, has_changed: bool) {
        let changed = self.run_selectedness(s, Change::Full);
        if changed || has_changed {
            self.update_contents(s);
        }
    }
}

/// The rules for parsing non-negative integers (HTML 2.3.4.1). Values too
/// large for `u64` saturate, which no comparison here can tell apart.
fn parse_non_negative_integer(s: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') {
        i += 1;
    }
    let mut negative = false;
    if i < s.len() && s[i] == b'-' {
        negative = true;
        i += 1;
    } else if i < s.len() && s[i] == b'+' {
        i += 1;
    }
    let start = i;
    let mut v: u64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add(u64::from(s[i] - b'0'));
        i += 1;
    }
    if i == start || (negative && v != 0) {
        return None;
    }
    Some(v)
}

impl Document {
    /// The model's work counters.
    #[doc(hidden)]
    pub fn behaviour_stats(&self) -> Stats {
        self.model.stats
    }

    /// Every element's behaviour state and the microtask queue, one line per
    /// stateful element, for tests that compare two documents.
    #[doc(hidden)]
    pub fn behaviour_dump(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for i in 0..self.node_count() {
            let n = NodeId(i as u32);
            match self.behaviour(n) {
                Behaviour::Option(st) => {
                    let _ = writeln!(
                        out,
                        "{i} option selected={} dirty={} select={:?}",
                        st.selectedness, st.dirtiness, st.cached_select
                    );
                }
                Behaviour::SelectedContent(st) => {
                    let _ = writeln!(
                        out,
                        "{i} selectedcontent disabled={} select={:?}",
                        st.disabled, st.cached_select
                    );
                }
                Behaviour::Select(st) => {
                    let _ = writeln!(out, "{i} select pending={}", st.pending);
                }
                Behaviour::None => {}
            }
        }
        let _ = writeln!(out, "microtasks {:?}", self.model.microtasks);
        out
    }

    /// Check the model's invariants against the standard's definitions,
    /// computed from scratch. For tests.
    pub(crate) fn check_behaviour(&self) {
        let ids = (0..self.node_count()).map(|i| NodeId(i as u32));
        for n in ids {
            if let Some(p) = self.parent(n)
                && !self.model.reference
            {
                assert!(
                    !self.model.flagged[n.index()] || self.model.flagged[p.index()],
                    "flags upward closed"
                );
            }
            if let Some(m) = self.model.memo.get(&n) {
                assert!(self.model.flagged[n.index()], "memo on a flagged node");
                assert_eq!(*m, self.ctx_walk(n), "memo is current");
            }
            let walk = || {
                self.parent(n)
                    .map(|p| self.ctx_walk(p))
                    .unwrap_or(Ctx::DETACHED)
            };
            match self.behaviour(n) {
                Behaviour::Option(st) => {
                    assert!(self.model.reference || self.model.flagged[n.index()]);
                    assert_eq!(st.cached_select, walk().opt0, "option's cached select");
                }
                Behaviour::SelectedContent(st) => {
                    assert!(self.model.reference || self.model.flagged[n.index()]);
                    let w = walk();
                    assert_eq!(
                        (st.cached_select, st.disabled),
                        (w.sc_select, w.sc_disabled),
                        "selectedcontent's cached select and disabledness"
                    );
                }
                Behaviour::Select(st) => {
                    let desc = self.descendants(n);
                    let count = desc
                        .iter()
                        .filter(|&&d| is_html(self, d, local::SELECTEDCONTENT))
                        .count();
                    assert_eq!(st.descendant_contents, count, "selectedcontent count");
                    let mut enabled: Vec<NodeId> = desc
                        .into_iter()
                        .filter(|&d| {
                            matches!(self.behaviour(d),
                                Behaviour::SelectedContent(c) if !c.disabled && c.cached_select == Some(n))
                        })
                        .collect();
                    let mut reg = st.contents.clone();
                    enabled.sort();
                    reg.sort();
                    assert_eq!(reg, enabled, "registry of enabled selectedcontent");
                    if st.settled && !self.model.reference {
                        assert!(!self.has_multiple(n), "settled with multiple");
                        let list = self.list_of_options(n);
                        let selected: Vec<NodeId> = list
                            .iter()
                            .copied()
                            .filter(|&o| self.selectedness(o))
                            .collect();
                        assert_eq!(selected, st.selected.into_iter().collect::<Vec<_>>());
                        if st.selected.is_none() && self.display_size(n) == 1 {
                            assert!(
                                list.iter().all(|&o| self.option_disabled(o)),
                                "settled with nothing selected but an enabled option"
                            );
                        }
                    }
                }
                Behaviour::None => {}
            }
        }
        for &s in &self.model.microtasks {
            assert!(matches!(self.behaviour(s), Behaviour::Select(_)));
        }
    }
}

#[cfg(test)]
mod tests;
