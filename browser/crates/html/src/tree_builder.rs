//! The HTML tree builder (WHATWG HTML, "13.2.6 Tree construction").
//!
//! Takes tokens from the [`Tokenizer`] and builds a [`Document`], following
//! the standard's insertion modes rule for rule. Rule comments name the
//! standard's sections and entries so each can be checked against the text.
//!
//! Not yet implemented (each needs another part of the engine first, and is
//! tracked in `browser/conformance/README.md`): script execution and
//! `document.write` (needs the JS engine), custom elements, declarative
//! shadow roots (needs shadow trees in the DOM), the Sanitizer API, template
//! content patching (`<template for>`), and form-owner association (needs
//! form DOM APIs). None of them change the tree for documents without script,
//! sanitizer or patching input.

use crate::open_stack::{OpenStack, Scope};
use crate::tokenizer::{self, InitialState, SinkResult, Token, TokenSink, Tokenizer};
use crate::{ParseError, RunResult};
use formatting::FormattingList;
use optimal_dom::{
    Attribute, Document, Element, LocalName, Namespace, NodeData, NodeId, QuirksMode, local,
};
use optimal_wtf8::{Wtf8, Wtf8Buf};
use std::collections::HashSet;

mod formatting;

/// Options for one parse.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Is scripting enabled? Changes how `<noscript>` parses.
    pub scripting: bool,
    /// Is this an `iframe srcdoc` document? (Never quirks mode.)
    pub iframe_srcdoc: bool,
    /// Collect parse errors (tokenizer errors with positions, tree builder
    /// errors with a description).
    pub collect_errors: bool,
}

/// A tree construction parse error. The standard gives these no codes, so
/// each carries a short description of the rule that reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeError {
    /// A tokenizer error.
    Tokenizer(ParseError),
    /// A tree builder error.
    Tree(&'static str),
}

/// The result of parsing.
#[derive(Debug)]
pub struct Parsed {
    /// The document. For fragment parsing, the nodes are the children of
    /// [`Parsed::fragment`].
    pub document: Document,
    /// For fragment parsing: the fragment holding the result.
    pub fragment: Option<NodeId>,
    /// Parse errors, if requested.
    pub errors: Vec<TreeError>,
}

/// Parse a whole document.
pub fn parse_document(input: &str, options: ParseOptions) -> Parsed {
    let mut p = Parser::new(options);
    p.feed(Wtf8::from_str(input));
    p.finish()
}

/// The context element for fragment parsing (`innerHTML` and friends).
#[derive(Clone, Debug)]
pub struct FragmentContext {
    /// Namespace of the context element.
    pub namespace: Namespace,
    /// Local name of the context element.
    pub local_name: String,
    /// Its attributes (name, value), all in no namespace.
    pub attributes: Vec<(String, String)>,
    /// The context document's quirks mode.
    pub quirks_mode: QuirksMode,
}

/// Parse a fragment in the context of an element.
pub fn parse_fragment(input: &str, context: &FragmentContext, options: ParseOptions) -> Parsed {
    let mut p = Parser::new_fragment(context, options);
    p.feed(Wtf8::from_str(input));
    p.finish()
}

/// A streaming parser: tokenizer plus tree builder.
pub struct Parser {
    tokenizer: Tokenizer,
    builder: TreeBuilder,
}

impl Parser {
    /// A parser for a new document.
    pub fn new(options: ParseOptions) -> Self {
        Parser {
            tokenizer: Tokenizer::new(),
            builder: TreeBuilder::new(options),
        }
    }

    /// A fragment parser ("13.4 Parsing HTML fragments").
    pub fn new_fragment(context: &FragmentContext, options: ParseOptions) -> Self {
        let mut builder = TreeBuilder::new(options);
        let mut tokenizer = Tokenizer::new();
        builder.doc.quirks_mode = context.quirks_mode;
        let ns = context.namespace;
        let name = builder.doc.names.intern_str(&context.local_name);
        let attrs: Vec<Attribute> = context
            .attributes
            .iter()
            .map(|(n, v)| Attribute {
                namespace: Namespace::None,
                prefix: None,
                local: builder.doc.names.intern_str(n),
                value: Wtf8Buf::from(v.as_str()),
            })
            .collect();
        // The context element belongs to another document; here it is a
        // detached element that is never inserted.
        let ctx = builder.doc.create_element(ns, name, attrs.clone());
        if ns == Namespace::Html {
            let state = match name {
                local::TITLE | local::TEXTAREA => Some(InitialState::Rcdata),
                local::STYLE | local::XMP | local::IFRAME | local::NOEMBED | local::NOFRAMES => {
                    Some(InitialState::Rawtext)
                }
                local::SCRIPT => Some(InitialState::ScriptData),
                local::NOSCRIPT if builder.scripting => Some(InitialState::Rawtext),
                local::PLAINTEXT => Some(InitialState::Plaintext),
                _ => None,
            };
            if let Some(s) = state {
                tokenizer.set_state(s);
            }
        }
        if ns == Namespace::MathMl && name == local::ANNOTATION_XML {
            let tok = TagTok {
                name,
                self_closing: false,
                ack: false,
                attrs,
            };
            if builder.is_annotation_xml_html_ip(&tok) {
                builder.html_ip.insert(ctx);
            }
        }
        let root = builder
            .doc
            .create_element(Namespace::Html, local::HTML, vec![]);
        builder.doc.append(Document::ROOT, root);
        builder.push_open(root);
        let fragment = builder.doc.create(NodeData::DocumentFragment);
        builder.fragment_redirect = Some((root, fragment));
        builder.context = Some(ctx);
        if ns == Namespace::Html && name == local::TEMPLATE {
            builder.template_modes.push(Mode::InTemplate);
        }
        builder.reset_insertion_mode();
        // The form element pointer: the context element has no ancestors
        // here, so it is the context itself if that is a form.
        if ns == Namespace::Html && name == local::FORM {
            builder.form = Some(ctx);
        }
        Parser { tokenizer, builder }
    }

    /// Push more input and parse as far as possible.
    pub fn feed(&mut self, input: Wtf8<'_>) {
        self.tokenizer.feed(input);
        self.pump();
    }

    fn pump(&mut self) {
        loop {
            match self.tokenizer.run(&mut self.builder) {
                RunResult::NeedInput | RunResult::Done => return,
                // Scripts would run here. Without a script engine there is
                // nothing to run, so parsing simply continues.
                RunResult::Paused => continue,
            }
        }
    }

    /// End the input and return the result.
    pub fn finish(mut self) -> Parsed {
        self.tokenizer.end();
        self.pump();
        if cfg!(debug_assertions) {
            self.builder.open.check();
            self.builder.afe.check();
        }
        let fragment = self.builder.fragment_redirect.map(|(_, f)| f);
        Parsed {
            document: self.builder.doc,
            fragment,
            errors: self.builder.errors,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

/// A start or end tag, with its name interned and attributes in DOM form.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TagTok {
    name: LocalName,
    self_closing: bool,
    /// The self-closing flag was acknowledged.
    ack: bool,
    attrs: Vec<Attribute>,
}

impl TagTok {
    fn attr(&self, name: LocalName) -> Option<Wtf8<'_>> {
        self.attrs
            .iter()
            .find(|a| a.namespace == Namespace::None && a.local == name)
            .map(|a| a.value.as_wtf8())
    }

    fn bare(name: LocalName) -> TagTok {
        TagTok {
            name,
            self_closing: false,
            ack: false,
            attrs: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    /// Tab, LF, FF, CR or space.
    Whitespace,
    /// U+0000.
    Null,
    /// Anything else.
    Other,
}

enum Tok<'a> {
    Doctype(&'a tokenizer::Doctype),
    Start(TagTok),
    End(TagTok),
    Comment(Wtf8<'a>),
    Pi(Wtf8<'a>, Wtf8<'a>),
    /// A run of characters that are all of one class.
    Chars(CharClass, Wtf8<'a>),
    Eof,
}

/// What a rule did with the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Done,
    /// Reprocess the token (the insertion mode has changed).
    Reprocess,
}

use Step::{Done, Reprocess};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Afe {
    Marker,
    El(NodeId, TagTok),
}

/// An insertion location: insert into `parent` before `before` (or append).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Loc {
    parent: NodeId,
    before: Option<NodeId>,
}

/// The tree builder: a [`TokenSink`] that builds a [`Document`].
pub struct TreeBuilder {
    /// The document being built.
    pub doc: Document,
    mode: Mode,
    original_mode: Mode,
    template_modes: Vec<Mode>,
    open: OpenStack,
    afe: FormattingList,
    head: Option<NodeId>,
    form: Option<NodeId>,
    frameset_ok: bool,
    scripting: bool,
    iframe_srcdoc: bool,
    foster_parenting: bool,
    context: Option<NodeId>,
    /// Fragment case: insertions into the root `html` element go to the
    /// fragment instead (the insertion target redirection map's one entry).
    fragment_redirect: Option<(NodeId, NodeId)>,
    /// annotation-xml elements that are HTML integration points.
    html_ip: HashSet<NodeId>,
    pending_table_text: Vec<(CharClass, Wtf8Buf)>,
    pending_text: Option<(Loc, Wtf8Buf)>,
    /// Ignore a leading LF in the next token (after `<pre>`, `<listing>`,
    /// `<textarea>`).
    skip_next_lf: bool,
    /// Tokenizer state the last token asked for.
    switch_to: Option<InitialState>,
    collect_errors: bool,
    errors: Vec<TreeError>,
    /// A piece of text is being processed (see [`TreeBuilder::error`]).
    in_char_piece: bool,
    /// Tree errors the previous and the current piece of text raised.
    char_errors_prev: Vec<&'static str>,
    char_errors_cur: Vec<&'static str>,
    stopped: bool,
}

const HTML_WS: &[u8] = b"\t\n\x0C\r ";

impl TreeBuilder {
    fn new(options: ParseOptions) -> Self {
        TreeBuilder {
            doc: Document::new(),
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            template_modes: vec![],
            open: OpenStack::default(),
            afe: FormattingList::default(),
            head: None,
            form: None,
            frameset_ok: true,
            scripting: options.scripting,
            iframe_srcdoc: options.iframe_srcdoc,
            foster_parenting: false,
            context: None,
            fragment_redirect: None,
            html_ip: HashSet::new(),
            pending_table_text: vec![],
            pending_text: None,
            skip_next_lf: false,
            switch_to: None,
            collect_errors: options.collect_errors,
            errors: vec![],
            in_char_piece: false,
            char_errors_prev: vec![],
            char_errors_cur: vec![],
            stopped: false,
        }
    }

    fn error(&mut self, what: &'static str) {
        if !self.collect_errors {
            return;
        }
        if self.in_char_piece {
            // The standard reports per character token. We get text in
            // pieces whose boundaries depend on how the input was chunked,
            // so a run of pieces that each raise the same error reports it
            // once: the count then never depends on chunking.
            if !self.char_errors_cur.contains(&what) {
                self.char_errors_cur.push(what);
            }
            if self.char_errors_prev.contains(&what) {
                return;
            }
        }
        self.errors.push(TreeError::Tree(what));
    }

    // ---- Element queries ---------------------------------------------------

    fn el(&self, n: NodeId) -> &Element {
        self.doc
            .element(n)
            .expect("node on the stack is an element")
    }

    fn is_html(&self, n: NodeId, name: LocalName) -> bool {
        self.doc.element(n).is_some_and(|e| e.is_html(name))
    }

    fn is_html_one_of(&self, n: NodeId, names: &[LocalName]) -> bool {
        self.doc
            .element(n)
            .is_some_and(|e| e.namespace == Namespace::Html && names.contains(&e.local))
    }

    fn ns_name(&self, n: NodeId) -> (Namespace, LocalName) {
        let e = self.el(n);
        (e.namespace, e.local)
    }

    fn current(&self) -> NodeId {
        *self.open.last().expect("stack of open elements is empty")
    }

    fn adjusted_current(&self) -> Option<NodeId> {
        match self.context {
            Some(ctx) if self.open.len() == 1 => Some(ctx),
            _ => self.open.last().copied(),
        }
    }

    fn is_special(&self, n: NodeId) -> bool {
        crate::open_stack::is_special(self.ns_name(n))
    }

    /// Is an HTML element named `name` in `scope`?
    fn in_scope(&self, name: LocalName, scope: Scope) -> bool {
        self.open.any_in_scope(&[name], scope)
    }

    /// Is any HTML element named one of `names` in `scope`?
    fn any_in_scope(&self, names: &[LocalName], scope: Scope) -> bool {
        self.open.any_in_scope(names, scope)
    }

    fn node_in_scope(&self, node: NodeId) -> bool {
        self.open.node_in_scope(node, self.ns_name(node))
    }

    fn template_on_stack(&self) -> bool {
        self.open.has_html(local::TEMPLATE)
    }

    fn push_open(&mut self, el: NodeId) {
        let nn = self.ns_name(el);
        self.open.push(el, nn.0, nn.1);
    }

    fn insert_open(&mut self, i: usize, el: NodeId) {
        let nn = self.ns_name(el);
        self.open.insert(i, el, nn.0, nn.1);
    }

    fn replace_open(&mut self, i: usize, el: NodeId) {
        let nn = self.ns_name(el);
        self.open.replace(i, el, nn.0, nn.1);
    }

    fn parsing_template_contents(&self) -> bool {
        self.template_on_stack()
            || self
                .context
                .is_some_and(|c| self.is_html(c, local::TEMPLATE))
    }

    fn is_mathml_text_ip(&self, n: NodeId) -> bool {
        let (ns, name) = self.ns_name(n);
        ns == Namespace::MathMl
            && matches!(
                name,
                local::MI | local::MO | local::MN | local::MS | local::MTEXT
            )
    }

    fn is_html_ip(&self, n: NodeId) -> bool {
        let (ns, name) = self.ns_name(n);
        (ns == Namespace::Svg && matches!(name, local::FOREIGN_OBJECT | local::DESC | local::TITLE))
            || (ns == Namespace::MathMl
                && name == local::ANNOTATION_XML
                && self.html_ip.contains(&n))
    }

    fn is_annotation_xml_html_ip(&self, tok: &TagTok) -> bool {
        tok.attr(local::ENCODING).is_some_and(|v| {
            v.eq_ignore_ascii_case("text/html") || v.eq_ignore_ascii_case("application/xhtml+xml")
        })
    }

    // ---- Stack of open elements ----------------------------------------------

    fn pop(&mut self) -> NodeId {
        self.open.pop().expect("pop from empty stack")
    }

    fn pop_until(&mut self, pred: impl Fn(&Self, NodeId) -> bool) {
        while let Some(n) = self.open.pop() {
            if pred(self, n) {
                return;
            }
        }
    }

    fn pop_until_html(&mut self, name: LocalName) {
        self.pop_until(|s, n| s.is_html(n, name));
    }

    fn remove_from_stack(&mut self, node: NodeId) {
        if let Some(i) = self.open.iter().rposition(|&n| n == node) {
            self.open.remove(i);
        }
    }

    fn generate_implied_end_tags(&mut self, except: Option<LocalName>) {
        while let Some(&n) = self.open.last() {
            let implied = self.is_html_one_of(
                n,
                &[
                    local::DD,
                    local::DT,
                    local::LI,
                    local::OPTGROUP,
                    local::OPTION,
                    local::P,
                    local::RB,
                    local::RP,
                    local::RT,
                    local::RTC,
                ],
            );
            if !implied || except.is_some_and(|e| self.is_html(n, e)) {
                return;
            }
            self.open.pop();
        }
    }

    fn generate_all_implied_end_tags_thoroughly(&mut self) {
        while let Some(&n) = self.open.last() {
            if !self.is_html_one_of(
                n,
                &[
                    local::CAPTION,
                    local::COLGROUP,
                    local::DD,
                    local::DT,
                    local::LI,
                    local::OPTGROUP,
                    local::OPTION,
                    local::P,
                    local::RB,
                    local::RP,
                    local::RT,
                    local::RTC,
                    local::TBODY,
                    local::TD,
                    local::TFOOT,
                    local::TH,
                    local::THEAD,
                    local::TR,
                ],
            ) {
                return;
            }
            self.open.pop();
        }
    }

    fn close_p(&mut self) {
        self.generate_implied_end_tags(Some(local::P));
        if !self.is_html(self.current(), local::P) {
            self.error("close a p element: current node is not p");
        }
        self.pop_until_html(local::P);
    }

    fn close_p_if_in_button_scope(&mut self) {
        if self.in_scope(local::P, Scope::Button) {
            self.close_p();
        }
    }

    /// "reset the insertion mode appropriately"
    fn reset_insertion_mode(&mut self) {
        for i in (0..self.open.len()).rev() {
            let mut node = self.open[i];
            let last = i == 0;
            if last && let Some(ctx) = self.context {
                node = ctx;
            }
            let (ns, name) = self.ns_name(node);
            if ns == Namespace::Html {
                let mode = match name {
                    local::TD | local::TH if !last => Some(Mode::InCell),
                    local::TR => Some(Mode::InRow),
                    local::TBODY | local::THEAD | local::TFOOT => Some(Mode::InTableBody),
                    local::CAPTION => Some(Mode::InCaption),
                    local::COLGROUP => Some(Mode::InColumnGroup),
                    local::TABLE => Some(Mode::InTable),
                    local::TEMPLATE => Some(*self.template_modes.last().expect("template mode")),
                    local::HEAD if !last => Some(Mode::InHead),
                    local::BODY => Some(Mode::InBody),
                    local::FRAMESET => Some(Mode::InFrameset),
                    local::HTML => Some(if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    }),
                    _ => None,
                };
                if let Some(m) = mode {
                    self.mode = m;
                    return;
                }
            }
            if last {
                self.mode = Mode::InBody;
                return;
            }
        }
        self.mode = Mode::InBody;
    }

    // ---- Active formatting elements -------------------------------------------

    fn push_afe(&mut self, node: NodeId, tok: TagTok) {
        self.afe.push(node, tok);
    }

    // Each element is in the list at most once, and the ones the parser
    // asks about are usually near the end, so search from there.
    fn afe_index(&self, node: NodeId) -> Option<usize> {
        if !self.afe.contains_node(node) {
            return None;
        }
        self.afe
            .iter()
            .rposition(|e| matches!(e, Afe::El(n, _) if *n == node))
    }

    fn reconstruct_afe(&mut self) {
        let Some(last) = self.afe.last() else { return };
        let on_stack = |s: &Self, e: &Afe| match e {
            Afe::Marker => true,
            Afe::El(n, _) => s.open.contains(*n),
        };
        if on_stack(self, last) {
            return;
        }
        let mut i = self.afe.len() - 1;
        // Rewind.
        while i > 0 {
            if on_stack(self, &self.afe[i - 1]) {
                break;
            }
            i -= 1;
        }
        // Advance and create.
        while i < self.afe.len() {
            let tok = match &self.afe[i] {
                Afe::El(_, t) => t.clone(),
                Afe::Marker => unreachable!("markers are on the stack by definition"),
            };
            let el = self.insert_html_element(&tok);
            self.afe.set_node(i, el);
            i += 1;
        }
    }

    fn clear_afe_to_marker(&mut self) {
        self.afe.clear_to_marker();
    }

    // ---- Creating and inserting nodes -----------------------------------------

    /// "the appropriate place for inserting a node"
    fn appropriate_place(&self, override_target: Option<NodeId>) -> Loc {
        let mut target = override_target.unwrap_or_else(|| self.current());
        let mut before = None;
        if self.foster_parenting
            && self.is_html_one_of(
                target,
                &[
                    local::TABLE,
                    local::TBODY,
                    local::TFOOT,
                    local::THEAD,
                    local::TR,
                ],
            )
        {
            let last = self
                .open
                .iter()
                .rposition(|&n| self.is_html(n, local::TEMPLATE) || self.is_html(n, local::TABLE));
            match last {
                None => {
                    return Loc {
                        parent: self.open[0],
                        before: None,
                    };
                }
                Some(i) => {
                    let n = self.open[i];
                    if self.is_html(n, local::TEMPLATE) {
                        target = n;
                    } else if let Some(p) = self.doc.parent(n) {
                        target = p;
                        before = Some(n);
                    } else {
                        target = self.open[i - 1];
                    }
                }
            }
        }
        if let Some(e) = self.doc.element(target)
            && e.is_html(local::TEMPLATE)
        {
            return Loc {
                parent: e.template_contents.expect("template has contents"),
                before: None,
            };
        }
        Loc {
            parent: target,
            before,
        }
    }

    /// "the adjusted insertion location"
    fn adjust(&self, loc: Loc) -> Loc {
        match self.fragment_redirect {
            Some((root, fragment)) if loc.parent == root => Loc {
                parent: fragment,
                before: None,
            },
            _ => loc,
        }
    }

    fn adjusted_place(&self) -> Loc {
        self.adjust(self.appropriate_place(None))
    }

    fn intended_parent(&self) -> NodeId {
        self.appropriate_place(None).parent
    }

    /// "create an element for the token"
    fn create_element_for(
        &mut self,
        tok: &TagTok,
        ns: Namespace,
        _intended_parent: NodeId,
    ) -> NodeId {
        let el = self.doc.create_element(ns, tok.name, tok.attrs.clone());
        if let Some(e) = self.doc.element(el) {
            let xmlns_ok = e
                .attributes
                .iter()
                .find(|a| a.namespace == Namespace::Xmlns && a.local == local::XMLNS)
                .is_none_or(|a| a.value == ns.url());
            let xlink_ok = e
                .attributes
                .iter()
                .find(|a| a.namespace == Namespace::Xmlns && a.local == local::XLINK)
                .is_none_or(|a| a.value == Namespace::XLink.url());
            if !xmlns_ok {
                self.error("xmlns attribute does not match the element's namespace");
            }
            if !xlink_ok {
                self.error("xmlns:xlink attribute is not the XLink namespace");
            }
        }
        if ns == Namespace::MathMl
            && tok.name == local::ANNOTATION_XML
            && self.is_annotation_xml_html_ip(tok)
        {
            self.html_ip.insert(el);
        }
        el
    }

    /// The DOM's pre-insertion validity check, for an element: inserting
    /// must not make a node its own ancestor, a document has at most one
    /// element child, and the reference child must be a child of the parent.
    /// Where it fails, the standard's insert steps are skipped.
    fn can_insert_element(&self, el: NodeId, loc: Loc) -> bool {
        !self.doc.would_cycle(el, loc.parent)
            && !(loc.parent == Document::ROOT && self.doc.has_element_child(Document::ROOT))
            && loc
                .before
                .is_none_or(|r| self.doc.parent(r) == Some(loc.parent))
    }

    /// "insert an element at the adjusted insertion location"
    fn insert_element_at(&mut self, el: NodeId, location: Option<Loc>) {
        let loc = self.adjust(location.unwrap_or_else(|| self.appropriate_place(None)));
        if self.doc.parent(el).is_none() && self.can_insert_element(el, loc) {
            self.doc.insert_before(loc.parent, el, loc.before);
        }
    }

    /// "insert a foreign element"
    fn insert_foreign_element(&mut self, tok: &TagTok, ns: Namespace) -> NodeId {
        self.flush_pending_text();
        let intended = self.intended_parent();
        let el = self.create_element_for(tok, ns, intended);
        self.insert_element_at(el, None);
        self.push_open(el);
        el
    }

    fn insert_html_element(&mut self, tok: &TagTok) -> NodeId {
        self.insert_foreign_element(tok, Namespace::Html)
    }

    fn insert_html_element_named(&mut self, name: LocalName) -> NodeId {
        self.insert_html_element(&TagTok::bare(name))
    }

    /// "insert a character"
    fn insert_chars(&mut self, data: Wtf8<'_>) {
        if data.is_empty() {
            return;
        }
        let loc = self.adjusted_place();
        if self.pending_text.as_ref().is_some_and(|(l, _)| *l != loc) {
            self.flush_pending_text();
        }
        if loc.parent == Document::ROOT {
            return;
        }
        if loc
            .before
            .is_some_and(|r| self.doc.parent(r) != Some(loc.parent))
        {
            return;
        }
        match &mut self.pending_text {
            None => self.pending_text = Some((loc, Wtf8Buf::from(data))),
            Some((_, buf)) => buf.push_wtf8(data),
        }
    }

    /// "flush pending text"
    fn flush_pending_text(&mut self) {
        let Some((loc, data)) = self.pending_text.take() else {
            return;
        };
        let prev = match loc.before {
            Some(r) => self.doc.prev_sibling(r),
            None => self.doc.last_child(loc.parent),
        };
        if let Some(p) = prev
            && let NodeData::Text(t) = self.doc.data_mut(p)
        {
            t.push_wtf8(data.as_wtf8());
            return;
        }
        let text = self.doc.create(NodeData::Text(data));
        self.doc.insert_before(loc.parent, text, loc.before);
    }

    fn insert_comment(&mut self, data: Wtf8<'_>, location: Option<Loc>) {
        let loc = self.adjust(location.unwrap_or_else(|| self.appropriate_place(None)));
        if loc
            .before
            .is_some_and(|r| self.doc.parent(r) != Some(loc.parent))
        {
            return;
        }
        let c = self.doc.create(NodeData::Comment(Wtf8Buf::from(data)));
        self.doc.insert_before(loc.parent, c, loc.before);
    }

    fn insert_pi(&mut self, target: Wtf8<'_>, data: Wtf8<'_>, location: Option<Loc>) {
        let loc = self.adjust(location.unwrap_or_else(|| self.appropriate_place(None)));
        if loc
            .before
            .is_some_and(|r| self.doc.parent(r) != Some(loc.parent))
        {
            return;
        }
        let pi = self.doc.create(NodeData::ProcessingInstruction {
            target: Wtf8Buf::from(target),
            data: Wtf8Buf::from(data),
        });
        self.doc.insert_before(loc.parent, pi, loc.before);
    }

    fn doc_loc() -> Loc {
        Loc {
            parent: Document::ROOT,
            before: None,
        }
    }

    fn insert_comment_tok(&mut self, tok: &Tok<'_>, loc: Option<Loc>) -> Step {
        match tok {
            Tok::Comment(c) => self.insert_comment(*c, loc),
            Tok::Pi(t, d) => self.insert_pi(*t, *d, loc),
            _ => unreachable!(),
        }
        Done
    }

    fn generic_text_element(&mut self, tok: &TagTok, state: InitialState) {
        self.insert_html_element(tok);
        self.switch_to = Some(state);
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    fn acknowledge(tok: &mut TagTok) {
        tok.ack = true;
    }

    // ---- Attribute adjustments --------------------------------------------------

    fn adjust_mathml_attributes(&mut self, tok: &mut TagTok) {
        let from = self.doc.names.intern_str("definitionurl");
        for a in &mut tok.attrs {
            if a.namespace == Namespace::None && a.local == from {
                a.local = local::DEFINITION_URL;
            }
        }
    }

    fn adjust_svg_attributes(&mut self, tok: &mut TagTok) {
        for a in &mut tok.attrs {
            if a.namespace != Namespace::None {
                continue;
            }
            let text = self.doc.names.text(a.local);
            if let Some(fixed) = text.as_str().and_then(svg_attribute_case) {
                a.local = self.doc.names.intern_str(fixed);
            }
        }
    }

    fn adjust_foreign_attributes(&mut self, tok: &mut TagTok) {
        for a in &mut tok.attrs {
            if a.namespace != Namespace::None {
                continue;
            }
            let text = self.doc.names.text(a.local);
            let Some(s) = text.as_str() else { continue };
            let (prefix, local_name, ns) = match s {
                "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show"
                | "xlink:title" | "xlink:type" => (Some("xlink"), &s[6..], Namespace::XLink),
                "xml:lang" | "xml:space" => (Some("xml"), &s[4..], Namespace::Xml),
                "xmlns" => (None, "xmlns", Namespace::Xmlns),
                "xmlns:xlink" => (Some("xmlns"), "xlink", Namespace::Xmlns),
                _ => continue,
            };
            let local_name = local_name.to_owned();
            a.prefix = prefix.map(|p| self.doc.names.intern_str(p));
            a.local = self.doc.names.intern_str(&local_name);
            a.namespace = ns;
        }
    }

    // ---- The dispatcher -------------------------------------------------------------

    fn dispatch(&mut self, tok: &mut Tok<'_>) {
        loop {
            if self.stopped {
                return;
            }
            let html_rules = match self.adjusted_current() {
                None => true,
                Some(acn) => {
                    let (ns, name) = self.ns_name(acn);
                    ns == Namespace::Html
                        || (self.is_mathml_text_ip(acn)
                            && match tok {
                                Tok::Start(t) => {
                                    t.name != local::MGLYPH && t.name != local::MALIGNMARK
                                }
                                Tok::Chars(..) => true,
                                _ => false,
                            })
                        || (ns == Namespace::MathMl
                            && name == local::ANNOTATION_XML
                            && matches!(tok, Tok::Start(t) if t.name == local::SVG))
                        || (self.is_html_ip(acn) && matches!(tok, Tok::Start(_) | Tok::Chars(..)))
                        || matches!(tok, Tok::Eof)
                }
            };
            let step = if html_rules {
                self.process(self.mode, tok)
            } else {
                self.foreign(tok)
            };
            if step == Done {
                return;
            }
        }
    }

    fn process(&mut self, mode: Mode, tok: &mut Tok<'_>) -> Step {
        match mode {
            Mode::Initial => self.initial(tok),
            Mode::BeforeHtml => self.before_html(tok),
            Mode::BeforeHead => self.before_head(tok),
            Mode::InHead => self.in_head(tok),
            Mode::InHeadNoscript => self.in_head_noscript(tok),
            Mode::AfterHead => self.after_head(tok),
            Mode::InBody => self.in_body(tok),
            Mode::Text => self.text(tok),
            Mode::InTable => self.in_table(tok),
            Mode::InTableText => self.in_table_text(tok),
            Mode::InCaption => self.in_caption(tok),
            Mode::InColumnGroup => self.in_column_group(tok),
            Mode::InTableBody => self.in_table_body(tok),
            Mode::InRow => self.in_row(tok),
            Mode::InCell => self.in_cell(tok),
            Mode::InTemplate => self.in_template(tok),
            Mode::AfterBody => self.after_body(tok),
            Mode::InFrameset => self.in_frameset(tok),
            Mode::AfterFrameset => self.after_frameset(tok),
            Mode::AfterAfterBody => self.after_after_body(tok),
            Mode::AfterAfterFrameset => self.after_after_frameset(tok),
        }
    }

    fn stop_parsing(&mut self) {
        self.flush_pending_text();
        self.open.clear();
        self.stopped = true;
    }

    // ---- The insertion modes ------------------------------------------------------------

    /// 13.2.6.4.1 The "initial" insertion mode
    fn initial(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, _) => Done,
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, Some(Self::doc_loc())),
            Tok::Doctype(d) => {
                let name = d.name.as_ref().map(|n| n.as_wtf8());
                let public = d.public_id.as_ref().map(|n| n.as_wtf8());
                let system = d.system_id.as_ref().map(|n| n.as_wtf8());
                if name.is_none_or(|n| n != "html")
                    || public.is_some()
                    || system.is_some_and(|s| s != "about:legacy-compat")
                {
                    self.error("non-conforming DOCTYPE");
                }
                let doctype = self.doc.create(NodeData::Doctype {
                    name: name.map(Wtf8Buf::from).unwrap_or_default(),
                    public_id: public.map(Wtf8Buf::from).unwrap_or_default(),
                    system_id: system.map(Wtf8Buf::from).unwrap_or_default(),
                });
                let has = self.doc.children(Document::ROOT).any(|c| {
                    matches!(
                        self.doc.data(c),
                        NodeData::Doctype { .. } | NodeData::Element(_)
                    )
                });
                if !has {
                    self.doc.append(Document::ROOT, doctype);
                }
                if !self.iframe_srcdoc {
                    self.doc.quirks_mode = doctype_quirks(d);
                }
                self.mode = Mode::BeforeHtml;
                Done
            }
            _ => {
                if !self.iframe_srcdoc {
                    self.error("missing DOCTYPE");
                    self.doc.quirks_mode = QuirksMode::Quirks;
                }
                self.mode = Mode::BeforeHtml;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.2 The "before html" insertion mode
    fn before_html(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Doctype(_) => {
                self.error("DOCTYPE in before html");
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, Some(Self::doc_loc())),
            Tok::Chars(CharClass::Whitespace, _) => Done,
            Tok::Start(t) if t.name == local::HTML => {
                let el = self.create_element_for(t, Namespace::Html, Document::ROOT);
                self.insert_element_at(el, Some(Self::doc_loc()));
                self.push_open(el);
                self.mode = Mode::BeforeHead;
                Done
            }
            Tok::End(t)
                if !matches!(t.name, local::HEAD | local::BODY | local::HTML | local::BR) =>
            {
                self.error("end tag in before html");
                Done
            }
            _ => {
                let el = self
                    .doc
                    .create_element(Namespace::Html, local::HTML, vec![]);
                self.insert_element_at(el, Some(Self::doc_loc()));
                self.push_open(el);
                self.mode = Mode::BeforeHead;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.3 The "before head" insertion mode
    fn before_head(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, _) => Done,
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in before head");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Start(t) if t.name == local::HEAD => {
                let h = self.insert_html_element(t);
                self.head = Some(h);
                self.mode = Mode::InHead;
                Done
            }
            Tok::End(t)
                if !matches!(t.name, local::HEAD | local::BODY | local::HTML | local::BR) =>
            {
                self.error("end tag in before head");
                Done
            }
            _ => {
                let h = self.insert_html_element_named(local::HEAD);
                self.head = Some(h);
                self.mode = Mode::InHead;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.4 The "in head" insertion mode
    fn in_head(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in head");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::BASE | local::BASEFONT | local::BGSOUND | local::LINK | local::META
                ) =>
            {
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                // A <meta charset> would change the encoding here, while the
                // encoding is tentative. Input is already decoded text.
                Done
            }
            Tok::Start(t) if t.name == local::TITLE => {
                let t = t.clone();
                self.generic_text_element(&t, InitialState::Rcdata);
                Done
            }
            Tok::Start(t)
                if (t.name == local::NOSCRIPT && self.scripting)
                    || matches!(t.name, local::NOFRAMES | local::STYLE) =>
            {
                let t = t.clone();
                self.generic_text_element(&t, InitialState::Rawtext);
                Done
            }
            Tok::Start(t) if t.name == local::NOSCRIPT => {
                self.insert_html_element(t);
                self.mode = Mode::InHeadNoscript;
                Done
            }
            Tok::Start(t) if t.name == local::SCRIPT => {
                self.flush_pending_text();
                let intended = self.intended_parent();
                let el = self.create_element_for(t, Namespace::Html, intended);
                self.insert_element_at(el, None);
                self.push_open(el);
                self.switch_to = Some(InitialState::ScriptData);
                self.original_mode = self.mode;
                self.mode = Mode::Text;
                Done
            }
            Tok::End(t) if t.name == local::HEAD => {
                self.pop();
                self.mode = Mode::AfterHead;
                Done
            }
            Tok::Start(t) if t.name == local::TEMPLATE => {
                self.afe.push_marker();
                self.frameset_ok = false;
                self.mode = Mode::InTemplate;
                self.template_modes.push(Mode::InTemplate);
                self.flush_pending_text();
                let intended = self.intended_parent();
                let template = self.create_element_for(t, Namespace::Html, intended);
                let loc = self.appropriate_place(None);
                self.push_open(template);
                // Declarative shadow roots are not enabled for this parser,
                // so the template is always inserted as a plain template.
                self.insert_element_at(template, Some(loc));
                Done
            }
            Tok::End(t) if t.name == local::TEMPLATE => {
                if !self.template_on_stack() {
                    self.error("template end tag with no template open");
                    return Done;
                }
                self.generate_all_implied_end_tags_thoroughly();
                if !self.is_html(self.current(), local::TEMPLATE) {
                    self.error("template end tag: current node is not template");
                }
                self.pop_until_html(local::TEMPLATE);
                self.clear_afe_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                Done
            }
            Tok::Start(t) if t.name == local::HEAD => {
                self.error("head start tag in head");
                Done
            }
            Tok::End(t) if !matches!(t.name, local::BODY | local::HTML | local::BR) => {
                self.error("end tag in head");
                Done
            }
            _ => {
                self.pop();
                self.mode = Mode::AfterHead;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.5 The "in head noscript" insertion mode
    fn in_head_noscript(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Doctype(_) => {
                self.error("DOCTYPE in head noscript");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::End(t) if t.name == local::NOSCRIPT => {
                self.pop();
                self.mode = Mode::InHead;
                Done
            }
            Tok::Chars(CharClass::Whitespace, _) | Tok::Comment(_) | Tok::Pi(..) => {
                self.in_head(tok)
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::BASEFONT
                        | local::BGSOUND
                        | local::LINK
                        | local::META
                        | local::NOFRAMES
                        | local::STYLE
                ) =>
            {
                self.in_head(tok)
            }
            Tok::Start(t) if matches!(t.name, local::HEAD | local::NOSCRIPT) => {
                self.error("start tag in head noscript");
                Done
            }
            Tok::End(t) if t.name != local::BR => {
                self.error("end tag in head noscript");
                Done
            }
            _ => {
                self.error("unexpected token in head noscript");
                self.pop();
                self.mode = Mode::InHead;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.6 The "after head" insertion mode
    fn after_head(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE after head");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Start(t) if t.name == local::BODY => {
                self.insert_html_element(t);
                self.frameset_ok = false;
                self.mode = Mode::InBody;
                Done
            }
            Tok::Start(t) if t.name == local::FRAMESET => {
                self.insert_html_element(t);
                self.mode = Mode::InFrameset;
                Done
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::BASE
                        | local::BASEFONT
                        | local::BGSOUND
                        | local::LINK
                        | local::META
                        | local::NOFRAMES
                        | local::SCRIPT
                        | local::STYLE
                        | local::TEMPLATE
                        | local::TITLE
                ) =>
            {
                self.error("head element after head");
                let head = self.head.expect("head pointer is set after head");
                self.push_open(head);
                let r = self.in_head(tok);
                self.remove_from_stack(head);
                r
            }
            Tok::End(t) if t.name == local::TEMPLATE => self.in_head(tok),
            Tok::Start(t) if t.name == local::HEAD => {
                self.error("head start tag after head");
                Done
            }
            Tok::End(t) if !matches!(t.name, local::BODY | local::HTML | local::BR) => {
                self.error("end tag after head");
                Done
            }
            _ => {
                self.insert_html_element_named(local::BODY);
                self.frameset_ok = true;
                self.mode = Mode::InBody;
                Reprocess
            }
        }
    }

    fn body_has_only_allowed_open(&self) -> bool {
        self.open.iter().all(|&n| {
            self.is_html_one_of(
                n,
                &[
                    local::DD,
                    local::DT,
                    local::LI,
                    local::OPTGROUP,
                    local::OPTION,
                    local::P,
                    local::RB,
                    local::RP,
                    local::RT,
                    local::RTC,
                    local::TBODY,
                    local::TD,
                    local::TFOOT,
                    local::TH,
                    local::THEAD,
                    local::TR,
                    local::BODY,
                    local::HTML,
                ],
            )
        })
    }

    /// 13.2.6.4.7 The "in body" insertion mode
    #[allow(clippy::cognitive_complexity)]
    fn in_body(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Null, _) => {
                self.error("NUL in body");
                Done
            }
            Tok::Chars(class, s) => {
                self.reconstruct_afe();
                self.insert_chars(*s);
                if *class == CharClass::Other {
                    self.frameset_ok = false;
                }
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in body");
                Done
            }
            Tok::Start(t) => {
                let t = t.clone();
                self.in_body_start(t, tok)
            }
            Tok::End(t) => {
                let t = t.clone();
                self.in_body_end(t)
            }
            Tok::Eof => {
                if !self.template_modes.is_empty() {
                    return self.in_template(tok);
                }
                if !self.body_has_only_allowed_open() {
                    self.error("EOF with elements open");
                }
                self.stop_parsing();
                Done
            }
        }
    }

    fn in_body_start(&mut self, mut t: TagTok, tok: &mut Tok<'_>) -> Step {
        let r = self.in_body_start_inner(&mut t, tok);
        // The rule may have acknowledged the self-closing flag on its copy.
        if let Tok::Start(orig) = tok
            && t.ack
        {
            orig.ack = true;
        }
        r
    }

    #[allow(clippy::cognitive_complexity)]
    fn in_body_start_inner(&mut self, t: &mut TagTok, tok: &mut Tok<'_>) -> Step {
        match t.name {
            local::HTML => {
                self.error("html start tag in body");
                if self.template_on_stack() {
                    return Done;
                }
                let html = self.open[0];
                self.add_missing_attributes(html, t);
                Done
            }
            local::BASE
            | local::BASEFONT
            | local::BGSOUND
            | local::LINK
            | local::META
            | local::NOFRAMES
            | local::SCRIPT
            | local::STYLE
            | local::TEMPLATE
            | local::TITLE => self.in_head(tok),
            local::BODY => {
                self.error("body start tag in body");
                if self.open.len() == 1
                    || !self.is_html(self.open[1], local::BODY)
                    || self.template_on_stack()
                {
                    return Done;
                }
                self.frameset_ok = false;
                let body = self.open[1];
                self.add_missing_attributes(body, t);
                Done
            }
            local::FRAMESET => {
                self.error("frameset start tag in body");
                if self.open.len() == 1 || !self.is_html(self.open[1], local::BODY) {
                    return Done;
                }
                if !self.frameset_ok {
                    return Done;
                }
                let body = self.open[1];
                self.doc.remove(body);
                self.open.truncate(1);
                self.insert_html_element(t);
                self.mode = Mode::InFrameset;
                Done
            }
            local::ADDRESS
            | local::ARTICLE
            | local::ASIDE
            | local::BLOCKQUOTE
            | local::CENTER
            | local::DETAILS
            | local::DIALOG
            | local::DIR
            | local::DIV
            | local::DL
            | local::FIELDSET
            | local::FIGCAPTION
            | local::FIGURE
            | local::FOOTER
            | local::HEADER
            | local::HGROUP
            | local::MAIN
            | local::MENU
            | local::NAV
            | local::OL
            | local::P
            | local::SEARCH
            | local::SECTION
            | local::SUMMARY
            | local::UL => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(t);
                Done
            }
            local::H1 | local::H2 | local::H3 | local::H4 | local::H5 | local::H6 => {
                self.close_p_if_in_button_scope();
                if self.is_html_one_of(
                    self.current(),
                    &[
                        local::H1,
                        local::H2,
                        local::H3,
                        local::H4,
                        local::H5,
                        local::H6,
                    ],
                ) {
                    self.error("nested heading");
                    self.pop();
                }
                self.insert_html_element(t);
                Done
            }
            local::PRE | local::LISTING => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(t);
                self.skip_next_lf = true;
                self.frameset_ok = false;
                Done
            }
            local::FORM => {
                if self.form.is_some() && !self.parsing_template_contents() {
                    self.error("nested form");
                    return Done;
                }
                self.close_p_if_in_button_scope();
                let f = self.insert_html_element(t);
                if !self.parsing_template_contents() {
                    self.form = Some(f);
                }
                Done
            }
            local::LI => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if self.is_html(node, local::LI) {
                        self.generate_implied_end_tags(Some(local::LI));
                        if !self.is_html(self.current(), local::LI) {
                            self.error("li: current node is not li");
                        }
                        self.pop_until_html(local::LI);
                        break;
                    }
                    if self.is_special(node)
                        && !self.is_html_one_of(node, &[local::ADDRESS, local::DIV, local::P])
                    {
                        break;
                    }
                }
                self.close_p_if_in_button_scope();
                self.insert_html_element(t);
                Done
            }
            local::DD | local::DT => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if self.is_html(node, local::DD) || self.is_html(node, local::DT) {
                        let name = self.ns_name(node).1;
                        self.generate_implied_end_tags(Some(name));
                        if !self.is_html(self.current(), name) {
                            self.error("dd/dt: current node mismatch");
                        }
                        self.pop_until_html(name);
                        break;
                    }
                    if self.is_special(node)
                        && !self.is_html_one_of(node, &[local::ADDRESS, local::DIV, local::P])
                    {
                        break;
                    }
                }
                self.close_p_if_in_button_scope();
                self.insert_html_element(t);
                Done
            }
            local::PLAINTEXT => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(t);
                self.switch_to = Some(InitialState::Plaintext);
                Done
            }
            local::BUTTON => {
                if self.in_scope(local::BUTTON, Scope::Default) {
                    self.error("nested button");
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(local::BUTTON);
                }
                self.reconstruct_afe();
                self.insert_html_element(t);
                self.frameset_ok = false;
                Done
            }
            local::A => {
                let existing =
                    self.afe
                        .last_named_after_marker(local::A)
                        .map(|i| match &self.afe[i] {
                            Afe::El(n, _) => *n,
                            Afe::Marker => unreachable!(),
                        });
                if let Some(a) = existing {
                    self.error("a inside a");
                    self.adoption_agency(local::A);
                    if let Some(i) = self.afe_index(a) {
                        self.afe.remove(i);
                    }
                    self.remove_from_stack(a);
                }
                self.reconstruct_afe();
                let el = self.insert_html_element(t);
                self.push_afe(el, t.clone());
                Done
            }
            local::B
            | local::BIG
            | local::CODE
            | local::EM
            | local::FONT
            | local::I
            | local::S
            | local::SMALL
            | local::STRIKE
            | local::STRONG
            | local::TT
            | local::U => {
                self.reconstruct_afe();
                let el = self.insert_html_element(t);
                self.push_afe(el, t.clone());
                Done
            }
            local::NOBR => {
                self.reconstruct_afe();
                if self.in_scope(local::NOBR, Scope::Default) {
                    self.error("nested nobr");
                    self.adoption_agency(local::NOBR);
                    self.reconstruct_afe();
                }
                let el = self.insert_html_element(t);
                self.push_afe(el, t.clone());
                Done
            }
            local::APPLET | local::MARQUEE | local::OBJECT => {
                self.reconstruct_afe();
                self.insert_html_element(t);
                self.afe.push_marker();
                self.frameset_ok = false;
                Done
            }
            local::TABLE => {
                if self.doc.quirks_mode != QuirksMode::Quirks {
                    self.close_p_if_in_button_scope();
                }
                self.insert_html_element(t);
                self.frameset_ok = false;
                self.mode = Mode::InTable;
                Done
            }
            local::AREA | local::BR | local::EMBED | local::IMG | local::KEYGEN | local::WBR => {
                self.reconstruct_afe();
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                self.frameset_ok = false;
                Done
            }
            local::INPUT => {
                if self.context.is_some_and(|c| self.is_html(c, local::SELECT)) {
                    self.error("input in select fragment");
                    return Done;
                }
                if self.in_scope(local::SELECT, Scope::Default) {
                    self.error("input in select");
                    self.pop_until_html(local::SELECT);
                }
                self.reconstruct_afe();
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                if !t
                    .attr(local::TYPE)
                    .is_some_and(|v| v.eq_ignore_ascii_case("hidden"))
                {
                    self.frameset_ok = false;
                }
                Done
            }
            local::PARAM | local::SOURCE | local::TRACK => {
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                Done
            }
            local::HR => {
                self.close_p_if_in_button_scope();
                if self.in_scope(local::SELECT, Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if self.in_scope(local::OPTION, Scope::Default)
                        || self.in_scope(local::OPTGROUP, Scope::Default)
                    {
                        self.error("hr in option");
                    }
                }
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                self.frameset_ok = false;
                Done
            }
            local::IMAGE => {
                self.error("image start tag");
                if let Tok::Start(orig) = tok {
                    orig.name = local::IMG;
                }
                Reprocess
            }
            local::TEXTAREA => {
                self.insert_html_element(t);
                self.skip_next_lf = true;
                self.switch_to = Some(InitialState::Rcdata);
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = Mode::Text;
                Done
            }
            local::XMP => {
                self.close_p_if_in_button_scope();
                self.reconstruct_afe();
                self.frameset_ok = false;
                self.generic_text_element(t, InitialState::Rawtext);
                Done
            }
            local::IFRAME => {
                self.frameset_ok = false;
                self.generic_text_element(t, InitialState::Rawtext);
                Done
            }
            local::NOEMBED => {
                self.generic_text_element(t, InitialState::Rawtext);
                Done
            }
            local::NOSCRIPT if self.scripting => {
                self.generic_text_element(t, InitialState::Rawtext);
                Done
            }
            local::SELECT => {
                if self.context.is_some_and(|c| self.is_html(c, local::SELECT)) {
                    self.error("select in select fragment");
                } else if self.in_scope(local::SELECT, Scope::Default) {
                    self.error("nested select");
                    self.pop_until_html(local::SELECT);
                } else {
                    self.reconstruct_afe();
                    self.insert_html_element(t);
                    self.frameset_ok = false;
                }
                Done
            }
            local::OPTION => {
                if self.in_scope(local::SELECT, Scope::Default) {
                    self.generate_implied_end_tags(Some(local::OPTGROUP));
                    if self.in_scope(local::OPTION, Scope::Default) {
                        self.error("nested option");
                    }
                } else if self.is_html(self.current(), local::OPTION) {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert_html_element(t);
                Done
            }
            local::OPTGROUP => {
                if self.in_scope(local::SELECT, Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if self.in_scope(local::OPTION, Scope::Default)
                        || self.in_scope(local::OPTGROUP, Scope::Default)
                    {
                        self.error("optgroup in option");
                    }
                } else if self.is_html(self.current(), local::OPTION) {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert_html_element(t);
                Done
            }
            local::RB | local::RTC => {
                if self.in_scope(local::RUBY, Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if !self.is_html(self.current(), local::RUBY) {
                        self.error("rb/rtc outside ruby");
                    }
                }
                self.insert_html_element(t);
                Done
            }
            local::RP | local::RT => {
                if self.in_scope(local::RUBY, Scope::Default) {
                    self.generate_implied_end_tags(Some(local::RTC));
                    if !self.is_html(self.current(), local::RTC)
                        && !self.is_html(self.current(), local::RUBY)
                    {
                        self.error("rp/rt outside ruby");
                    }
                }
                self.insert_html_element(t);
                Done
            }
            local::MATH => {
                self.reconstruct_afe();
                self.adjust_mathml_attributes(t);
                self.adjust_foreign_attributes(t);
                self.insert_foreign_element(t, Namespace::MathMl);
                if t.self_closing {
                    self.pop();
                    Self::acknowledge(t);
                }
                Done
            }
            local::SVG => {
                self.reconstruct_afe();
                self.adjust_svg_attributes(t);
                self.adjust_foreign_attributes(t);
                self.insert_foreign_element(t, Namespace::Svg);
                if t.self_closing {
                    self.pop();
                    Self::acknowledge(t);
                }
                Done
            }
            local::CAPTION
            | local::COL
            | local::COLGROUP
            | local::FRAME
            | local::HEAD
            | local::TBODY
            | local::TD
            | local::TFOOT
            | local::TH
            | local::THEAD
            | local::TR => {
                self.error("table or head start tag in body");
                Done
            }
            _ => {
                self.reconstruct_afe();
                self.insert_html_element(t);
                Done
            }
        }
    }

    fn add_missing_attributes(&mut self, el: NodeId, t: &TagTok) {
        let e = self.doc.element_mut(el).expect("element");
        for a in &t.attrs {
            if !e
                .attributes
                .iter()
                .any(|x| x.namespace == a.namespace && x.local == a.local)
            {
                e.attributes.push(a.clone());
            }
        }
    }

    #[allow(clippy::cognitive_complexity)]
    fn in_body_end(&mut self, t: TagTok) -> Step {
        match t.name {
            local::TEMPLATE => {
                let mut tok = Tok::End(t);
                self.in_head(&mut tok)
            }
            local::BODY | local::HTML => {
                if !self.in_scope(local::BODY, Scope::Default) {
                    self.error("body/html end tag without body in scope");
                    return Done;
                }
                if !self.body_has_only_allowed_open() {
                    self.error("body/html end tag with elements open");
                }
                self.mode = Mode::AfterBody;
                if t.name == local::HTML {
                    Reprocess
                } else {
                    Done
                }
            }
            local::ADDRESS
            | local::ARTICLE
            | local::ASIDE
            | local::BLOCKQUOTE
            | local::BUTTON
            | local::CENTER
            | local::DETAILS
            | local::DIALOG
            | local::DIR
            | local::DIV
            | local::DL
            | local::FIELDSET
            | local::FIGCAPTION
            | local::FIGURE
            | local::FOOTER
            | local::HEADER
            | local::HGROUP
            | local::LISTING
            | local::MAIN
            | local::MENU
            | local::NAV
            | local::OL
            | local::PRE
            | local::SEARCH
            | local::SECTION
            | local::SELECT
            | local::SUMMARY
            | local::UL => {
                if !self.in_scope(t.name, Scope::Default) {
                    self.error("end tag without matching element in scope");
                    return Done;
                }
                self.generate_implied_end_tags(None);
                if !self.is_html(self.current(), t.name) {
                    self.error("end tag: current node mismatch");
                }
                self.pop_until_html(t.name);
                Done
            }
            local::FORM => {
                if !self.parsing_template_contents() {
                    let node = self.form.take();
                    let Some(node) = node.filter(|&n| self.node_in_scope(n)) else {
                        self.error("form end tag without form in scope");
                        return Done;
                    };
                    self.generate_implied_end_tags(None);
                    if self.current() != node {
                        self.error("form end tag: current node mismatch");
                    }
                    self.remove_from_stack(node);
                } else {
                    if !self.in_scope(local::FORM, Scope::Default) {
                        self.error("form end tag without form in scope");
                        return Done;
                    }
                    self.generate_implied_end_tags(None);
                    if !self.is_html(self.current(), local::FORM) {
                        self.error("form end tag: current node mismatch");
                    }
                    self.pop_until_html(local::FORM);
                }
                Done
            }
            local::P => {
                if !self.in_scope(local::P, Scope::Button) {
                    self.error("p end tag without p in scope");
                    self.insert_html_element_named(local::P);
                }
                self.close_p();
                Done
            }
            local::LI => {
                if !self.in_scope(local::LI, Scope::ListItem) {
                    self.error("li end tag without li in scope");
                    return Done;
                }
                self.generate_implied_end_tags(Some(local::LI));
                if !self.is_html(self.current(), local::LI) {
                    self.error("li end tag: current node mismatch");
                }
                self.pop_until_html(local::LI);
                Done
            }
            local::DD | local::DT => {
                if !self.in_scope(t.name, Scope::Default) {
                    self.error("dd/dt end tag without element in scope");
                    return Done;
                }
                self.generate_implied_end_tags(Some(t.name));
                if !self.is_html(self.current(), t.name) {
                    self.error("dd/dt end tag: current node mismatch");
                }
                self.pop_until_html(t.name);
                Done
            }
            local::H1 | local::H2 | local::H3 | local::H4 | local::H5 | local::H6 => {
                const HS: [LocalName; 6] = [
                    local::H1,
                    local::H2,
                    local::H3,
                    local::H4,
                    local::H5,
                    local::H6,
                ];
                if !self.any_in_scope(&HS, Scope::Default) {
                    self.error("heading end tag without heading in scope");
                    return Done;
                }
                self.generate_implied_end_tags(None);
                if !self.is_html(self.current(), t.name) {
                    self.error("heading end tag: current node mismatch");
                }
                self.pop_until(|s, n| s.is_html_one_of(n, &HS));
                Done
            }
            local::A
            | local::B
            | local::BIG
            | local::CODE
            | local::EM
            | local::FONT
            | local::I
            | local::NOBR
            | local::S
            | local::SMALL
            | local::STRIKE
            | local::STRONG
            | local::TT
            | local::U => {
                self.adoption_agency(t.name);
                Done
            }
            local::APPLET | local::MARQUEE | local::OBJECT => {
                if !self.in_scope(t.name, Scope::Default) {
                    self.error("end tag without element in scope");
                    return Done;
                }
                self.generate_implied_end_tags(None);
                if !self.is_html(self.current(), t.name) {
                    self.error("end tag: current node mismatch");
                }
                self.pop_until_html(t.name);
                self.clear_afe_to_marker();
                Done
            }
            local::BR => {
                self.error("br end tag");
                let mut tok = Tok::Start(TagTok::bare(local::BR));
                self.in_body(&mut tok)
            }
            _ => {
                self.any_other_end_tag(t.name);
                Done
            }
        }
    }

    /// "Any other end tag" in the "in body" insertion mode.
    fn any_other_end_tag(&mut self, name: LocalName) {
        // The standard walks down from the current node to the first HTML
        // element with this name, giving up at a special element; the stack
        // answers the same question from its indexes.
        let Some(i) = self.open.end_tag_target(name) else {
            self.error("end tag hits a special element");
            return;
        };
        let node = self.open[i];
        self.generate_implied_end_tags(Some(name));
        if self.current() != node {
            self.error("end tag: current node mismatch");
        }
        let i = self
            .open
            .iter()
            .rposition(|&n| n == node)
            .expect("target still open");
        self.open.truncate(i);
    }

    /// The adoption agency algorithm.
    fn adoption_agency(&mut self, subject: LocalName) {
        let cur = self.current();
        if self.is_html(cur, subject) && self.afe_index(cur).is_none() {
            self.pop();
            return;
        }
        for _ in 0..8 {
            // The formatting element: the last entry after the last marker
            // with the subject's name.
            let Some(fe_index) = self.afe.last_named_after_marker(subject) else {
                self.any_other_end_tag(subject);
                return;
            };
            let Afe::El(fe, fe_tok) = self.afe[fe_index].clone() else {
                unreachable!()
            };
            let Some(fe_stack) = self.open.iter().rposition(|&n| n == fe) else {
                self.error("adoption agency: formatting element not open");
                self.afe.remove(fe_index);
                return;
            };
            if !self.node_in_scope(fe) {
                self.error("adoption agency: formatting element not in scope");
                return;
            }
            if fe != self.current() {
                self.error("adoption agency: formatting element is not the current node");
            }
            let furthest = self.open.special_above(fe_stack);
            let Some(fb_stack) = furthest else {
                self.open.truncate(fe_stack);
                self.afe.remove(fe_index);
                return;
            };
            let furthest_block = self.open[fb_stack];
            let common_ancestor = self.open[fe_stack - 1];
            // The bookmark, kept as the entry it follows rather than as an
            // index, so removals before it need no index arithmetic. None
            // means the formatting element's own position.
            let mut bookmark_after: Option<NodeId> = None;
            let mut node_index = fb_stack;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                node_index -= 1;
                let node = self.open[node_index];
                if node == fe {
                    break;
                }
                let mut afe_i = self.afe_index(node);
                if inner > 3
                    && let Some(i) = afe_i
                {
                    self.afe.remove(i);
                    afe_i = None;
                }
                let Some(ai) = afe_i else {
                    self.open.remove(node_index);
                    continue;
                };
                let Afe::El(_, node_tok) = self.afe[ai].clone() else {
                    unreachable!()
                };
                let new_el = self.create_element_for(&node_tok, Namespace::Html, common_ancestor);
                self.afe.set_node(ai, new_el);
                self.replace_open(node_index, new_el);
                if last_node == furthest_block {
                    bookmark_after = Some(new_el);
                }
                if self.doc.parent(last_node).is_some() {
                    self.doc.remove(last_node);
                }
                self.doc.append(new_el, last_node);
                last_node = new_el;
            }
            let loc = self.adjust(self.appropriate_place(Some(common_ancestor)));
            if self.doc.parent(last_node).is_some() {
                self.doc.remove(last_node);
            }
            if self.can_insert_element(last_node, loc) {
                self.doc.insert_before(loc.parent, last_node, loc.before);
            }
            let new_el = self.create_element_for(&fe_tok, Namespace::Html, furthest_block);
            self.doc.move_children(furthest_block, new_el);
            self.doc.append(furthest_block, new_el);
            // Remove the formatting element's entry and put the new element
            // at the bookmark.
            let fe_now = self.afe_index(fe).expect("formatting element still listed");
            match bookmark_after {
                None => self.afe.set_node(fe_now, new_el),
                Some(a) => {
                    self.afe.remove(fe_now);
                    let at = self.afe_index(a).expect("bookmark entry still listed") + 1;
                    self.afe.insert(at, new_el, fe_tok);
                }
            }
            self.remove_from_stack(fe);
            let fb_now = self
                .open
                .iter()
                .rposition(|&n| n == furthest_block)
                .expect("furthest block open");
            self.insert_open(fb_now + 1, new_el);
        }
    }

    /// 13.2.6.4.8 The "text" insertion mode
    fn text(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(_, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Eof => {
                self.error("EOF in text");
                self.pop();
                self.mode = self.original_mode;
                Reprocess
            }
            Tok::End(_) => {
                // For a script end tag the script would be prepared and run
                // here; there is no script engine yet.
                self.pop();
                self.mode = self.original_mode;
                Done
            }
            _ => unreachable!("the tokenizer only produces text, end tags and EOF in this mode"),
        }
    }

    fn clear_stack_to(&mut self, names: &[LocalName]) {
        while !self.is_html_one_of(self.current(), names) {
            self.pop();
        }
    }

    fn clear_to_table_context(&mut self) {
        self.clear_stack_to(&[local::TABLE, local::TEMPLATE, local::HTML]);
    }

    fn clear_to_table_body_context(&mut self) {
        self.clear_stack_to(&[
            local::TBODY,
            local::TFOOT,
            local::THEAD,
            local::TEMPLATE,
            local::HTML,
        ]);
    }

    fn clear_to_table_row_context(&mut self) {
        self.clear_stack_to(&[local::TR, local::TEMPLATE, local::HTML]);
    }

    /// 13.2.6.4.9 The "in table" insertion mode
    fn in_table(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(..)
                if self.is_html_one_of(
                    self.current(),
                    &[
                        local::TABLE,
                        local::TBODY,
                        local::TEMPLATE,
                        local::TFOOT,
                        local::THEAD,
                        local::TR,
                    ],
                ) =>
            {
                self.pending_table_text.clear();
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                Reprocess
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in table");
                Done
            }
            Tok::Start(t) => match t.name {
                local::CAPTION => {
                    self.clear_to_table_context();
                    self.afe.push_marker();
                    self.insert_html_element(t);
                    self.mode = Mode::InCaption;
                    Done
                }
                local::COLGROUP => {
                    self.clear_to_table_context();
                    self.insert_html_element(t);
                    self.mode = Mode::InColumnGroup;
                    Done
                }
                local::COL => {
                    self.clear_to_table_context();
                    self.insert_html_element_named(local::COLGROUP);
                    self.mode = Mode::InColumnGroup;
                    Reprocess
                }
                local::TBODY | local::TFOOT | local::THEAD => {
                    self.clear_to_table_context();
                    self.insert_html_element(t);
                    self.mode = Mode::InTableBody;
                    Done
                }
                local::TD | local::TH | local::TR => {
                    self.clear_to_table_context();
                    self.insert_html_element_named(local::TBODY);
                    self.mode = Mode::InTableBody;
                    Reprocess
                }
                local::TABLE => {
                    self.error("table in table");
                    if !self.in_scope(local::TABLE, Scope::Table) {
                        return Done;
                    }
                    self.pop_until_html(local::TABLE);
                    self.reset_insertion_mode();
                    Reprocess
                }
                local::STYLE | local::SCRIPT | local::TEMPLATE => self.in_head(tok),
                local::INPUT
                    if t.attr(local::TYPE)
                        .is_some_and(|v| v.eq_ignore_ascii_case("hidden")) =>
                {
                    self.error("hidden input in table");
                    self.insert_html_element(t);
                    self.pop();
                    Self::acknowledge(t);
                    Done
                }
                local::FORM => {
                    self.error("form in table");
                    if self.form.is_some() && !self.parsing_template_contents() {
                        return Done;
                    }
                    let f = self.insert_html_element(t);
                    if !self.parsing_template_contents() {
                        self.form = Some(f);
                    }
                    self.pop();
                    Done
                }
                _ => self.in_table_anything_else(tok),
            },
            Tok::End(t) => match t.name {
                local::TABLE => {
                    if !self.in_scope(local::TABLE, Scope::Table) {
                        self.error("table end tag without table in scope");
                        return Done;
                    }
                    self.pop_until_html(local::TABLE);
                    self.reset_insertion_mode();
                    Done
                }
                local::BODY
                | local::CAPTION
                | local::COL
                | local::COLGROUP
                | local::HTML
                | local::TBODY
                | local::TD
                | local::TFOOT
                | local::TH
                | local::THEAD
                | local::TR => {
                    self.error("end tag in table");
                    Done
                }
                local::TEMPLATE => self.in_head(tok),
                _ => self.in_table_anything_else(tok),
            },
            Tok::Eof => self.in_body(tok),
            Tok::Chars(..) => self.in_table_anything_else(tok),
        }
    }

    fn in_table_anything_else(&mut self, tok: &mut Tok<'_>) -> Step {
        self.error("foster-parented content in table");
        self.foster_in_body(tok)
    }

    /// The "anything else" entry of "in table" after its parse error.
    fn foster_in_body(&mut self, tok: &mut Tok<'_>) -> Step {
        self.foster_parenting = true;
        let r = self.in_body(tok);
        self.foster_parenting = false;
        r
    }

    /// 13.2.6.4.10 The "in table text" insertion mode
    fn in_table_text(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Null, _) => {
                self.error("NUL in table text");
                Done
            }
            Tok::Chars(class, s) => {
                self.pending_table_text.push((*class, Wtf8Buf::from(*s)));
                Done
            }
            _ => {
                let pending = std::mem::take(&mut self.pending_table_text);
                if pending.iter().any(|(c, _)| *c != CharClass::Whitespace) {
                    // One error for the whole run: the "anything else" entry
                    // would report one more per character token, but these
                    // are all the same mistake in one place.
                    self.error("non-space characters in table");
                    for (class, s) in &pending {
                        let mut t = Tok::Chars(*class, s.as_wtf8());
                        self.foster_in_body(&mut t);
                    }
                } else {
                    for (_, s) in &pending {
                        self.insert_chars(s.as_wtf8());
                    }
                }
                self.flush_pending_text();
                self.mode = self.original_mode;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.11 The "in caption" insertion mode
    fn in_caption(&mut self, tok: &mut Tok<'_>) -> Step {
        let closes_caption = match tok {
            Tok::End(t) if t.name == local::CAPTION => Some(false),
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::TBODY
                        | local::TD
                        | local::TFOOT
                        | local::TH
                        | local::THEAD
                        | local::TR
                ) =>
            {
                Some(true)
            }
            Tok::End(t) if t.name == local::TABLE => Some(true),
            _ => None,
        };
        if let Some(reprocess) = closes_caption {
            if !self.in_scope(local::CAPTION, Scope::Table) {
                self.error("caption not in table scope");
                return Done;
            }
            self.generate_implied_end_tags(None);
            if !self.is_html(self.current(), local::CAPTION) {
                self.error("caption: current node mismatch");
            }
            self.pop_until_html(local::CAPTION);
            self.clear_afe_to_marker();
            self.mode = Mode::InTable;
            return if reprocess { Reprocess } else { Done };
        }
        match tok {
            Tok::End(t)
                if matches!(
                    t.name,
                    local::BODY
                        | local::COL
                        | local::COLGROUP
                        | local::HTML
                        | local::TBODY
                        | local::TD
                        | local::TFOOT
                        | local::TH
                        | local::THEAD
                        | local::TR
                ) =>
            {
                self.error("end tag in caption");
                Done
            }
            _ => self.in_body(tok),
        }
    }

    /// 13.2.6.4.12 The "in column group" insertion mode
    fn in_column_group(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in column group");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Start(t) if t.name == local::COL => {
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                Done
            }
            Tok::End(t) if t.name == local::COLGROUP => {
                if !self.is_html(self.current(), local::COLGROUP) {
                    self.error("colgroup end tag: current node is not colgroup");
                    return Done;
                }
                self.pop();
                self.mode = Mode::InTable;
                Done
            }
            Tok::End(t) if t.name == local::COL => {
                self.error("col end tag");
                Done
            }
            Tok::Start(t) if t.name == local::TEMPLATE => self.in_head(tok),
            Tok::End(t) if t.name == local::TEMPLATE => self.in_head(tok),
            Tok::Eof => self.in_body(tok),
            _ => {
                if !self.is_html(self.current(), local::COLGROUP) {
                    self.error("content in column group");
                    return Done;
                }
                self.pop();
                self.mode = Mode::InTable;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.13 The "in table body" insertion mode
    fn in_table_body(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Start(t) if t.name == local::TR => {
                self.clear_to_table_body_context();
                self.insert_html_element(t);
                self.mode = Mode::InRow;
                Done
            }
            Tok::Start(t) if matches!(t.name, local::TH | local::TD) => {
                self.error("cell without row");
                self.clear_to_table_body_context();
                self.insert_html_element_named(local::TR);
                self.mode = Mode::InRow;
                Reprocess
            }
            Tok::End(t) if matches!(t.name, local::TBODY | local::TFOOT | local::THEAD) => {
                if !self.in_scope(t.name, Scope::Table) {
                    self.error("table section end tag not in scope");
                    return Done;
                }
                self.clear_to_table_body_context();
                self.pop();
                self.mode = Mode::InTable;
                Done
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::TBODY
                        | local::TFOOT
                        | local::THEAD
                ) =>
            {
                self.leave_table_body()
            }
            Tok::End(t) if t.name == local::TABLE => self.leave_table_body(),
            Tok::End(t)
                if matches!(
                    t.name,
                    local::BODY
                        | local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::HTML
                        | local::TD
                        | local::TH
                        | local::TR
                ) =>
            {
                self.error("end tag in table body");
                Done
            }
            _ => self.in_table(tok),
        }
    }

    fn leave_table_body(&mut self) -> Step {
        if !self.any_in_scope(&[local::TBODY, local::THEAD, local::TFOOT], Scope::Table) {
            self.error("no table section in scope");
            return Done;
        }
        self.clear_to_table_body_context();
        self.pop();
        self.mode = Mode::InTable;
        Reprocess
    }

    /// 13.2.6.4.14 The "in row" insertion mode
    fn in_row(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Start(t) if matches!(t.name, local::TH | local::TD) => {
                self.clear_to_table_row_context();
                self.insert_html_element(t);
                self.mode = Mode::InCell;
                self.afe.push_marker();
                Done
            }
            Tok::End(t) if t.name == local::TR => {
                if !self.in_scope(local::TR, Scope::Table) {
                    self.error("tr end tag not in scope");
                    return Done;
                }
                self.clear_to_table_row_context();
                self.pop();
                self.mode = Mode::InTableBody;
                Done
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::TBODY
                        | local::TFOOT
                        | local::THEAD
                        | local::TR
                ) =>
            {
                self.leave_row()
            }
            Tok::End(t) if t.name == local::TABLE => self.leave_row(),
            Tok::End(t) if matches!(t.name, local::TBODY | local::TFOOT | local::THEAD) => {
                if !self.in_scope(t.name, Scope::Table) {
                    self.error("table section end tag not in scope");
                    return Done;
                }
                if !self.in_scope(local::TR, Scope::Table) {
                    return Done;
                }
                self.clear_to_table_row_context();
                self.pop();
                self.mode = Mode::InTableBody;
                Reprocess
            }
            Tok::End(t)
                if matches!(
                    t.name,
                    local::BODY
                        | local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::HTML
                        | local::TD
                        | local::TH
                ) =>
            {
                self.error("end tag in row");
                Done
            }
            _ => self.in_table(tok),
        }
    }

    fn leave_row(&mut self) -> Step {
        if !self.in_scope(local::TR, Scope::Table) {
            self.error("no tr in table scope");
            return Done;
        }
        self.clear_to_table_row_context();
        self.pop();
        self.mode = Mode::InTableBody;
        Reprocess
    }

    /// 13.2.6.4.15 The "in cell" insertion mode
    fn in_cell(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::End(t) if matches!(t.name, local::TD | local::TH) => {
                if !self.in_scope(t.name, Scope::Table) {
                    self.error("cell end tag not in scope");
                    return Done;
                }
                self.generate_implied_end_tags(None);
                if !self.is_html(self.current(), t.name) {
                    self.error("cell end tag: current node mismatch");
                }
                self.pop_until_html(t.name);
                self.clear_afe_to_marker();
                self.mode = Mode::InRow;
                Done
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::CAPTION
                        | local::COL
                        | local::COLGROUP
                        | local::TBODY
                        | local::TD
                        | local::TFOOT
                        | local::TH
                        | local::THEAD
                        | local::TR
                ) =>
            {
                debug_assert!(
                    self.in_scope(local::TD, Scope::Table)
                        || self.in_scope(local::TH, Scope::Table)
                );
                self.close_cell();
                Reprocess
            }
            Tok::End(t)
                if matches!(
                    t.name,
                    local::BODY | local::CAPTION | local::COL | local::COLGROUP | local::HTML
                ) =>
            {
                self.error("end tag in cell");
                Done
            }
            Tok::End(t)
                if matches!(
                    t.name,
                    local::TABLE | local::TBODY | local::TFOOT | local::THEAD | local::TR
                ) =>
            {
                if !self.in_scope(t.name, Scope::Table) {
                    self.error("table end tag not in scope");
                    return Done;
                }
                self.close_cell();
                Reprocess
            }
            _ => self.in_body(tok),
        }
    }

    fn close_cell(&mut self) {
        self.generate_implied_end_tags(None);
        if !self.is_html_one_of(self.current(), &[local::TD, local::TH]) {
            self.error("close the cell: current node is not a cell");
        }
        self.pop_until(|s, n| s.is_html_one_of(n, &[local::TD, local::TH]));
        self.clear_afe_to_marker();
        self.mode = Mode::InRow;
    }

    /// 13.2.6.4.16 The "in template" insertion mode
    fn in_template(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(..) | Tok::Comment(_) | Tok::Pi(..) | Tok::Doctype(_) => self.in_body(tok),
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::BASE
                        | local::BASEFONT
                        | local::BGSOUND
                        | local::LINK
                        | local::META
                        | local::NOFRAMES
                        | local::SCRIPT
                        | local::STYLE
                        | local::TEMPLATE
                        | local::TITLE
                ) =>
            {
                self.in_head(tok)
            }
            Tok::End(t) if t.name == local::TEMPLATE => self.in_head(tok),
            Tok::Start(t) => {
                let mode = match t.name {
                    local::CAPTION
                    | local::COLGROUP
                    | local::TBODY
                    | local::TFOOT
                    | local::THEAD => Mode::InTable,
                    local::COL => Mode::InColumnGroup,
                    local::TR => Mode::InTableBody,
                    local::TD | local::TH => Mode::InRow,
                    _ => Mode::InBody,
                };
                self.template_modes.pop();
                self.template_modes.push(mode);
                self.mode = mode;
                Reprocess
            }
            Tok::End(_) => {
                self.error("end tag in template");
                Done
            }
            Tok::Eof => {
                if !self.template_on_stack() {
                    self.stop_parsing();
                    return Done;
                }
                self.error("EOF in template");
                self.pop_until_html(local::TEMPLATE);
                self.clear_afe_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                Reprocess
            }
        }
    }

    /// 13.2.6.4.17 The "after body" insertion mode
    fn after_body(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, _) => self.in_body(tok),
            Tok::Comment(_) | Tok::Pi(..) => {
                let html = self.open[0];
                self.insert_comment_tok(
                    tok,
                    Some(Loc {
                        parent: html,
                        before: None,
                    }),
                )
            }
            Tok::Doctype(_) => {
                self.error("DOCTYPE after body");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::End(t) if t.name == local::HTML => {
                if self.context.is_some() {
                    self.error("html end tag in fragment");
                    return Done;
                }
                self.mode = Mode::AfterAfterBody;
                Done
            }
            Tok::Eof => {
                self.stop_parsing();
                Done
            }
            _ => {
                self.error("content after body");
                self.mode = Mode::InBody;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.18 The "in frameset" insertion mode
    fn in_frameset(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in frameset");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Start(t) if t.name == local::FRAMESET => {
                self.insert_html_element(t);
                Done
            }
            Tok::End(t) if t.name == local::FRAMESET => {
                if self.current() == self.open[0] {
                    self.error("frameset end tag at root");
                    return Done;
                }
                self.pop();
                if self.context.is_none() && !self.is_html(self.current(), local::FRAMESET) {
                    self.mode = Mode::AfterFrameset;
                }
                Done
            }
            Tok::Start(t) if t.name == local::FRAME => {
                self.insert_html_element(t);
                self.pop();
                Self::acknowledge(t);
                Done
            }
            Tok::Start(t) if t.name == local::NOFRAMES => self.in_head(tok),
            Tok::Eof => {
                if self.current() != self.open[0] {
                    self.error("EOF in frameset");
                }
                self.stop_parsing();
                Done
            }
            _ => {
                self.error("unexpected token in frameset");
                Done
            }
        }
    }

    /// 13.2.6.4.19 The "after frameset" insertion mode
    fn after_frameset(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Whitespace, s) => {
                self.insert_chars(*s);
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE after frameset");
                Done
            }
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::End(t) if t.name == local::HTML => {
                self.mode = Mode::AfterAfterFrameset;
                Done
            }
            Tok::Start(t) if t.name == local::NOFRAMES => self.in_head(tok),
            Tok::Eof => {
                self.stop_parsing();
                Done
            }
            _ => {
                self.error("unexpected token after frameset");
                Done
            }
        }
    }

    /// 13.2.6.4.20 The "after after body" insertion mode
    fn after_after_body(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, Some(Self::doc_loc())),
            Tok::Doctype(_) | Tok::Chars(CharClass::Whitespace, _) => self.in_body(tok),
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Eof => {
                self.stop_parsing();
                Done
            }
            _ => {
                self.error("content after after body");
                self.mode = Mode::InBody;
                Reprocess
            }
        }
    }

    /// 13.2.6.4.21 The "after after frameset" insertion mode
    fn after_after_frameset(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, Some(Self::doc_loc())),
            Tok::Doctype(_) | Tok::Chars(CharClass::Whitespace, _) => self.in_body(tok),
            Tok::Start(t) if t.name == local::HTML => self.in_body(tok),
            Tok::Eof => {
                self.stop_parsing();
                Done
            }
            Tok::Start(t) if t.name == local::NOFRAMES => self.in_head(tok),
            _ => {
                self.error("unexpected token after after frameset");
                Done
            }
        }
    }

    /// 13.2.6.5 The rules for parsing tokens in foreign content
    fn foreign(&mut self, tok: &mut Tok<'_>) -> Step {
        match tok {
            Tok::Chars(CharClass::Null, s) => {
                self.error("NUL in foreign content");
                let n = s.len();
                let mut r = Wtf8Buf::with_capacity(n * 3);
                for _ in 0..n {
                    r.push_char('\u{FFFD}');
                }
                self.insert_chars(r.as_wtf8());
                Done
            }
            Tok::Chars(class, s) => {
                self.insert_chars(*s);
                if *class == CharClass::Other {
                    self.frameset_ok = false;
                }
                Done
            }
            Tok::Comment(_) | Tok::Pi(..) => self.insert_comment_tok(tok, None),
            Tok::Doctype(_) => {
                self.error("DOCTYPE in foreign content");
                Done
            }
            Tok::Start(t)
                if matches!(
                    t.name,
                    local::B
                        | local::BIG
                        | local::BLOCKQUOTE
                        | local::BODY
                        | local::BR
                        | local::CENTER
                        | local::CODE
                        | local::DD
                        | local::DIV
                        | local::DL
                        | local::DT
                        | local::EM
                        | local::EMBED
                        | local::H1
                        | local::H2
                        | local::H3
                        | local::H4
                        | local::H5
                        | local::H6
                        | local::HEAD
                        | local::HR
                        | local::I
                        | local::IMG
                        | local::LI
                        | local::LISTING
                        | local::MENU
                        | local::META
                        | local::NOBR
                        | local::OL
                        | local::P
                        | local::PRE
                        | local::RUBY
                        | local::S
                        | local::SMALL
                        | local::SPAN
                        | local::STRONG
                        | local::STRIKE
                        | local::SUB
                        | local::SUP
                        | local::TABLE
                        | local::TT
                        | local::U
                        | local::UL
                        | local::VAR
                ) || (t.name == local::FONT
                    && (t.attr(local::COLOR).is_some()
                        || t.attr(local::FACE).is_some()
                        || t.attr(local::SIZE).is_some())) =>
            {
                self.break_out_of_foreign(tok)
            }
            Tok::End(t) if matches!(t.name, local::BR | local::P) => self.break_out_of_foreign(tok),
            Tok::Start(t) => {
                let acn = self
                    .adjusted_current()
                    .expect("foreign content has a current node");
                let ns = self.ns_name(acn).0;
                if ns == Namespace::MathMl {
                    self.adjust_mathml_attributes(t);
                }
                if ns == Namespace::Svg {
                    let text = self.doc.names.text(t.name);
                    if let Some(fixed) = text.as_str().and_then(svg_tag_case) {
                        t.name = self.doc.names.intern_str(fixed);
                    }
                    self.adjust_svg_attributes(t);
                }
                self.adjust_foreign_attributes(t);
                self.insert_foreign_element(t, ns);
                if t.self_closing {
                    let script = self.doc.names.intern_str("script");
                    if t.name == script && ns == Namespace::Svg {
                        Self::acknowledge(t);
                        self.pop();
                    } else {
                        self.pop();
                        Self::acknowledge(t);
                    }
                }
                Done
            }
            Tok::End(t) => {
                let cur = self.current();
                let script = self.doc.names.intern_str("script");
                if t.name == script && self.ns_name(cur) == (Namespace::Svg, script) {
                    // An SVG script would be processed here.
                    self.pop();
                    return Done;
                }
                let token_name = self.doc.names.text(t.name).as_bytes().to_vec();
                let lower = |s: &Self, n: NodeId| {
                    s.doc
                        .names
                        .text(s.ns_name(n).1)
                        .as_bytes()
                        .to_ascii_lowercase()
                };
                let mut i = self.open.len() - 1;
                if lower(self, self.open[i]) != token_name {
                    self.error("end tag in foreign content does not match");
                }
                loop {
                    if i == 0 {
                        return Done;
                    }
                    let node = self.open[i];
                    if lower(self, node) == token_name {
                        self.open.truncate(i);
                        return Done;
                    }
                    i -= 1;
                    if self.ns_name(self.open[i]).0 == Namespace::Html {
                        let mode = self.mode;
                        return self.process(mode, tok);
                    }
                }
            }
            Tok::Eof => unreachable!("EOF always uses the insertion mode rules"),
        }
    }

    fn break_out_of_foreign(&mut self, tok: &mut Tok<'_>) -> Step {
        self.error("HTML tag in foreign content");
        while let Some(&n) = self.open.last() {
            if self.is_mathml_text_ip(n)
                || self.is_html_ip(n)
                || self.ns_name(n).0 == Namespace::Html
            {
                break;
            }
            self.pop();
        }
        let mode = self.mode;
        self.process(mode, tok)
    }

    // ---- Token intake --------------------------------------------------------------

    fn intern_tag(&mut self, tag: &tokenizer::Tag) -> TagTok {
        let name = self.doc.names.intern(tag.name.as_wtf8());
        let attrs = tag
            .attributes
            .iter()
            .map(|a| Attribute {
                namespace: Namespace::None,
                prefix: None,
                local: self.doc.names.intern(a.name.as_wtf8()),
                value: a.value.clone(),
            })
            .collect();
        TagTok {
            name,
            self_closing: tag.self_closing,
            ack: false,
            attrs,
        }
    }

    fn process_characters(&mut self, mut text: Wtf8<'_>) {
        if std::mem::take(&mut self.skip_next_lf) && text.as_bytes().first() == Some(&b'\n') {
            text = text.slice(1, text.len());
        }
        let bytes = text.as_bytes();
        let class_of = |b: u8| {
            if HTML_WS.contains(&b) {
                CharClass::Whitespace
            } else if b == 0 {
                CharClass::Null
            } else {
                CharClass::Other
            }
        };
        let mut start = 0;
        while start < bytes.len() {
            let class = class_of(bytes[start]);
            let mut end = start + 1;
            while end < bytes.len() && class_of(bytes[end]) == class {
                end += 1;
            }
            // Runs split at ASCII bytes, so both ends are code point boundaries.
            let mut tok = Tok::Chars(class, text.slice(start, end));
            self.in_char_piece = true;
            self.char_errors_cur.clear();
            self.dispatch(&mut tok);
            self.in_char_piece = false;
            std::mem::swap(&mut self.char_errors_prev, &mut self.char_errors_cur);
            start = end;
        }
    }
}

impl TokenSink for TreeBuilder {
    fn process(&mut self, token: Token<'_>) -> SinkResult {
        if self.stopped {
            return SinkResult::Continue;
        }
        if !matches!(token, Token::Characters(_)) {
            self.flush_pending_text();
            self.skip_next_lf = false;
            self.char_errors_prev.clear();
        }
        match token {
            Token::Characters(s) => self.process_characters(s),
            Token::Doctype(d) => self.dispatch(&mut Tok::Doctype(d)),
            Token::Tag(tag) => {
                let t = self.intern_tag(tag);
                match tag.kind {
                    tokenizer::TagKind::Start => {
                        let mut tok = Tok::Start(t);
                        self.dispatch(&mut tok);
                        if let Tok::Start(t) = &tok
                            && t.self_closing
                            && !t.ack
                        {
                            self.error("self-closing flag on non-void element");
                        }
                    }
                    tokenizer::TagKind::End => self.dispatch(&mut Tok::End(t)),
                }
            }
            Token::Comment(c) => self.dispatch(&mut Tok::Comment(c)),
            Token::ProcessingInstruction { target, data } => {
                self.dispatch(&mut Tok::Pi(target, data))
            }
            Token::Eof => {
                self.dispatch(&mut Tok::Eof);
                self.flush_pending_text();
            }
        }
        match self.switch_to.take() {
            Some(s) => SinkResult::SwitchTo(s),
            None => SinkResult::Continue,
        }
    }

    fn parse_error(&mut self, error: ParseError) {
        if self.collect_errors {
            self.errors.push(TreeError::Tokenizer(error));
        }
    }

    fn wants_errors(&self) -> bool {
        self.collect_errors
    }

    fn adjusted_current_node_is_foreign(&self) -> bool {
        self.adjusted_current()
            .is_some_and(|n| self.ns_name(n).0 != Namespace::Html)
    }
}

/// Do two attribute lists hold the same attributes, in any order?
/// The quirks mode a DOCTYPE token selects (13.2.6.4.1).
fn doctype_quirks(d: &tokenizer::Doctype) -> QuirksMode {
    const QUIRKS_PREFIXES: &[&str] = &[
        "+//silmaril//dtd html pro v0r11 19970101//",
        "-//as//dtd html 3.0 aswedit + extensions//",
        "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
        "-//ietf//dtd html 2.0 level 1//",
        "-//ietf//dtd html 2.0 level 2//",
        "-//ietf//dtd html 2.0 strict level 1//",
        "-//ietf//dtd html 2.0 strict level 2//",
        "-//ietf//dtd html 2.0 strict//",
        "-//ietf//dtd html 2.0//",
        "-//ietf//dtd html 2.1e//",
        "-//ietf//dtd html 3.0//",
        "-//ietf//dtd html 3.2 final//",
        "-//ietf//dtd html 3.2//",
        "-//ietf//dtd html 3//",
        "-//ietf//dtd html level 0//",
        "-//ietf//dtd html level 1//",
        "-//ietf//dtd html level 2//",
        "-//ietf//dtd html level 3//",
        "-//ietf//dtd html strict level 0//",
        "-//ietf//dtd html strict level 1//",
        "-//ietf//dtd html strict level 2//",
        "-//ietf//dtd html strict level 3//",
        "-//ietf//dtd html strict//",
        "-//ietf//dtd html//",
        "-//metrius//dtd metrius presentational//",
        "-//microsoft//dtd internet explorer 2.0 html strict//",
        "-//microsoft//dtd internet explorer 2.0 html//",
        "-//microsoft//dtd internet explorer 2.0 tables//",
        "-//microsoft//dtd internet explorer 3.0 html strict//",
        "-//microsoft//dtd internet explorer 3.0 html//",
        "-//microsoft//dtd internet explorer 3.0 tables//",
        "-//netscape comm. corp.//dtd html//",
        "-//netscape comm. corp.//dtd strict html//",
        "-//o'reilly and associates//dtd html 2.0//",
        "-//o'reilly and associates//dtd html extended 1.0//",
        "-//o'reilly and associates//dtd html extended relaxed 1.0//",
        "-//sq//dtd html 2.0 hotmetal + extensions//",
        "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
        "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
        "-//spyglass//dtd html 2.0 extended//",
        "-//sun microsystems corp.//dtd hotjava html//",
        "-//sun microsystems corp.//dtd hotjava strict html//",
        "-//w3c//dtd html 3 1995-03-24//",
        "-//w3c//dtd html 3.2 draft//",
        "-//w3c//dtd html 3.2 final//",
        "-//w3c//dtd html 3.2//",
        "-//w3c//dtd html 3.2s draft//",
        "-//w3c//dtd html 4.0 frameset//",
        "-//w3c//dtd html 4.0 transitional//",
        "-//w3c//dtd html experimental 19960712//",
        "-//w3c//dtd html experimental 970421//",
        "-//w3c//dtd w3 html//",
        "-//w3o//dtd w3 html 3.0//",
        "-//webtechs//dtd mozilla html 2.0//",
        "-//webtechs//dtd mozilla html//",
    ];
    let lower = |s: &Option<Wtf8Buf>| s.as_ref().map(|v| v.as_bytes().to_ascii_lowercase());
    let public = lower(&d.public_id);
    let system = lower(&d.system_id);
    let pub_is = |v: &str| public.as_deref() == Some(v.as_bytes());
    let pub_starts = |v: &str| public.as_ref().is_some_and(|p| p.starts_with(v.as_bytes()));
    let system_missing_or_empty = system.as_ref().is_none_or(|s| s.is_empty());
    if d.force_quirks
        || d.name.as_ref().is_none_or(|n| *n != "html")
        || pub_is("-//w3o//dtd w3 html strict 3.0//en//")
        || pub_is("-/w3c/dtd html 4.0 transitional/en")
        || pub_is("html")
        || system.as_deref()
            == Some(b"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd".as_slice())
        || QUIRKS_PREFIXES.iter().any(|p| pub_starts(p))
        || (system_missing_or_empty
            && (pub_starts("-//w3c//dtd html 4.01 frameset//")
                || pub_starts("-//w3c//dtd html 4.01 transitional//")))
    {
        return QuirksMode::Quirks;
    }
    if pub_starts("-//w3c//dtd xhtml 1.0 frameset//")
        || pub_starts("-//w3c//dtd xhtml 1.0 transitional//")
        || (!system_missing_or_empty
            && (pub_starts("-//w3c//dtd html 4.01 frameset//")
                || pub_starts("-//w3c//dtd html 4.01 transitional//")))
    {
        return QuirksMode::LimitedQuirks;
    }
    QuirksMode::NoQuirks
}

/// SVG attribute names whose case the parser fixes.
fn svg_attribute_case(s: &str) -> Option<&'static str> {
    Some(match s {
        "attributename" => "attributeName",
        "attributetype" => "attributeType",
        "basefrequency" => "baseFrequency",
        "baseprofile" => "baseProfile",
        "calcmode" => "calcMode",
        "clippathunits" => "clipPathUnits",
        "diffuseconstant" => "diffuseConstant",
        "edgemode" => "edgeMode",
        "filterunits" => "filterUnits",
        "glyphref" => "glyphRef",
        "gradienttransform" => "gradientTransform",
        "gradientunits" => "gradientUnits",
        "kernelmatrix" => "kernelMatrix",
        "kernelunitlength" => "kernelUnitLength",
        "keypoints" => "keyPoints",
        "keysplines" => "keySplines",
        "keytimes" => "keyTimes",
        "lengthadjust" => "lengthAdjust",
        "limitingconeangle" => "limitingConeAngle",
        "markerheight" => "markerHeight",
        "markerunits" => "markerUnits",
        "markerwidth" => "markerWidth",
        "maskcontentunits" => "maskContentUnits",
        "maskunits" => "maskUnits",
        "numoctaves" => "numOctaves",
        "pathlength" => "pathLength",
        "patterncontentunits" => "patternContentUnits",
        "patterntransform" => "patternTransform",
        "patternunits" => "patternUnits",
        "pointsatx" => "pointsAtX",
        "pointsaty" => "pointsAtY",
        "pointsatz" => "pointsAtZ",
        "preservealpha" => "preserveAlpha",
        "preserveaspectratio" => "preserveAspectRatio",
        "primitiveunits" => "primitiveUnits",
        "refx" => "refX",
        "refy" => "refY",
        "repeatcount" => "repeatCount",
        "repeatdur" => "repeatDur",
        "requiredextensions" => "requiredExtensions",
        "requiredfeatures" => "requiredFeatures",
        "specularconstant" => "specularConstant",
        "specularexponent" => "specularExponent",
        "spreadmethod" => "spreadMethod",
        "startoffset" => "startOffset",
        "stddeviation" => "stdDeviation",
        "stitchtiles" => "stitchTiles",
        "surfacescale" => "surfaceScale",
        "systemlanguage" => "systemLanguage",
        "tablevalues" => "tableValues",
        "targetx" => "targetX",
        "targety" => "targetY",
        "textlength" => "textLength",
        "viewbox" => "viewBox",
        "viewtarget" => "viewTarget",
        "xchannelselector" => "xChannelSelector",
        "ychannelselector" => "yChannelSelector",
        "zoomandpan" => "zoomAndPan",
        _ => return None,
    })
}

/// SVG element names whose case the parser fixes.
fn svg_tag_case(s: &str) -> Option<&'static str> {
    Some(match s {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        _ => return None,
    })
}
