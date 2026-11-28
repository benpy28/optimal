//! A literal model of collection and the cascade, written from the spec
//! text without the production code's shortcuts, for differential tests:
//!
//! - placement of `@import` and `@namespace` checked against the list of
//!   every earlier valid rule, as the sentences of css-cascade-5 2 and
//!   css-namespaces-3 2.1 read (no state machine);
//! - layers kept as full names (lists of segments, anonymous segments
//!   unique), ordered by comparing names segment by segment (6.4.3), not by
//!   a tree walk;
//! - nested selectors made absolute as text: `& ` written in front of each
//!   relative selector, then parsed as an ordinary selector;
//! - every declaration of every sheet tried on every element (no buckets),
//!   each selector matched alone with a fresh matcher, and the declarations
//!   sorted with a comparison closure that follows 6.1 criterion by
//!   criterion.
//!
//! It shares the CSS parser and the selector parser and matcher with the
//! production code: those have their own suites and models.

#![allow(dead_code)]

use optimal_css::{
    AcceptAll, BlockItem, ComponentValue, Declaration, Kind, Parsed, Rule, RuleId, Token,
    Tokenizer, parse_declaration_list, parse_stylesheet,
};
use optimal_selectors::{
    Element, MatchContext, Namespaces, Scope, SelectorList, Specificity, matches, parse_selector,
};
use optimal_wtf8::Wtf8;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent,
    User,
    Author,
}

/// A layer name segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Named(String),
    Anonymous(u64),
}

/// What identifies a declaration in the output: (sheet, index of its
/// name token) for a sheet's, or the index in the style attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Id {
    Sheet(usize, u32),
    Attribute(usize),
}

#[derive(Debug)]
pub struct ModelRule {
    pub origin: Origin,
    /// Each selector alone, absolute.
    pub selectors: Vec<SelectorList>,
    /// The whole list, which `&` in nested rules stands for.
    pub list: SelectorList,
    /// The ancestor style rules, outermost first.
    pub ancestors: Vec<usize>,
}

#[derive(Debug)]
pub struct ModelDecl {
    pub id: Id,
    pub name: String,
    pub important: bool,
    pub rule: usize,
    pub layer: Vec<Seg>,
    pub order: usize,
}

#[derive(Debug, Default)]
pub struct Model {
    pub rules: Vec<ModelRule>,
    pub decls: Vec<ModelDecl>,
    /// Each origin's declared layer names, in first-declaration order.
    pub declared: Vec<(Origin, Vec<Seg>)>,
    next_anon: u64,
}

const GROUP_NOT_APPLIED: &[&str] = &["media", "supports", "container", "scope", "starting-style"];
const OTHER_NOT_APPLIED: &[&str] = &[
    "font-face",
    "keyframes",
    "page",
    "property",
    "counter-style",
    "font-feature-values",
    "font-palette-values",
    "view-transition",
    "position-try",
];
const CSS_WIDE: &[&str] = &[
    "initial",
    "inherit",
    "unset",
    "revert",
    "revert-layer",
    "revert-rule",
];

/// The kinds of valid top-level rules, for placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopKind {
    LayerStatement,
    Import,
    Namespace,
    Other,
}

struct SheetWalk<'p, 'a> {
    parsed: &'p Parsed<'a>,
    sheet: usize,
    origin: Origin,
    ns: Namespaces,
    earlier: Vec<TopKind>,
}

fn name_of(parsed: &Parsed<'_>, d: &Declaration) -> String {
    let n = parsed.name(d.name);
    if n.starts_with("--") {
        n.to_string()
    } else {
        n.to_ascii_lowercase()
    }
}

fn text(parsed: &Parsed<'_>, values: &[ComponentValue<'_, '_>]) -> String {
    match (values.first(), values.last()) {
        (Some(f), Some(l)) => parsed
            .source(f.span().start..l.span().end)
            .to_string_lossy(),
        _ => String::new(),
    }
}

fn strip_ws<'v, 't, 'a>(v: &'v [ComponentValue<'t, 'a>]) -> &'v [ComponentValue<'t, 'a>] {
    let s = v
        .iter()
        .position(|x| !matches!(x.token(), Token::Whitespace))
        .unwrap_or(v.len());
    let e = v
        .iter()
        .rposition(|x| !matches!(x.token(), Token::Whitespace))
        .map_or(s, |e| e + 1);
    &v[s..e.max(s)]
}

/// `<layer-name>`: read from its text, one segment per period.
fn layer_name(parsed: &Parsed<'_>, values: &[ComponentValue<'_, '_>]) -> Option<Vec<String>> {
    // Every value must be an ident or a '.' delim, alternating, starting
    // and ending with an ident.
    let mut segs = Vec::new();
    for (i, v) in values.iter().enumerate() {
        match (i % 2, v.token()) {
            (0, Token::Ident(s)) => segs.push(s.to_string()),
            (1, Token::Delim('.')) => {}
            _ => return None,
        }
    }
    let _ = parsed;
    if values.len() % 2 == 0 {
        return None;
    }
    if segs
        .iter()
        .any(|s| CSS_WIDE.iter().any(|k| s.eq_ignore_ascii_case(k)))
    {
        return None;
    }
    Some(segs)
}

fn url_like(v: &ComponentValue<'_, '_>) -> Option<String> {
    match v.kind() {
        Kind::Token(Token::String(s)) | Kind::Token(Token::Url(s)) => Some(s.to_string()),
        Kind::Function(f, args) if f.eq_ignore_ascii_case("url") => {
            let args: Vec<_> = args.collect();
            match strip_ws(&args) {
                [a] => match a.token() {
                    Token::String(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a sheet; returns its parse (the caller keeps it for values).
    pub fn add_sheet(&mut self, parsed: &Parsed<'_>, top: &[RuleId], sheet: usize, origin: Origin) {
        let mut w = SheetWalk {
            parsed,
            sheet,
            origin,
            ns: Namespaces::new(),
            earlier: Vec::new(),
        };
        for &r in top {
            self.rule(&mut w, r, &[], None, true);
        }
    }

    fn declare(&mut self, origin: Origin, name: &[Seg]) {
        for k in 1..=name.len() {
            let prefix = name[..k].to_vec();
            if !self
                .declared
                .iter()
                .any(|(o, n)| *o == origin && *n == prefix)
            {
                self.declared.push((origin, prefix));
            }
        }
    }

    fn anon(&mut self) -> Seg {
        self.next_anon += 1;
        Seg::Anonymous(self.next_anon)
    }

    fn import_valid(earlier: &[TopKind]) -> bool {
        // Must precede every other valid rule (ignoring @layer statements),
        // with nothing but @import between it and an earlier @import.
        let first_import = earlier.iter().position(|k| *k == TopKind::Import);
        earlier
            .iter()
            .all(|k| matches!(k, TopKind::LayerStatement | TopKind::Import))
            && first_import.is_none_or(|f| earlier[f..].iter().all(|k| *k == TopKind::Import))
    }

    fn namespace_valid(earlier: &[TopKind]) -> bool {
        // After @charset and @import, before everything else; and any
        // @layer after an @import or @namespace ends that part.
        let first = earlier
            .iter()
            .position(|k| matches!(k, TopKind::Import | TopKind::Namespace));
        earlier.iter().all(|k| *k != TopKind::Other)
            && first.is_none_or(|f| {
                earlier[f..]
                    .iter()
                    .all(|k| matches!(k, TopKind::Import | TopKind::Namespace))
            })
            // @import may not follow @namespace.
            && !earlier
                .windows(2)
                .any(|w| w[0] == TopKind::Namespace && w[1] == TopKind::Import)
    }

    fn rule(
        &mut self,
        w: &mut SheetWalk<'_, '_>,
        r: RuleId,
        layer: &[Seg],
        style: Option<usize>,
        top: bool,
    ) {
        let parsed = w.parsed;
        match &parsed.rules[r as usize] {
            Rule::Qualified {
                prelude,
                declarations,
                children,
            } => {
                let values: Vec<_> = parsed.values(prelude.clone()).collect();
                let Some(id) = self.style_rule(w, &values, style) else {
                    return;
                };
                if top {
                    w.earlier.push(TopKind::Other);
                }
                self.decls(w, declarations, layer, Some(id));
                for &c in children {
                    match &parsed.rules[c as usize] {
                        Rule::NestedDeclarations(d) => self.decls(w, d, layer, Some(id)),
                        _ => self.rule(w, c, layer, Some(id), false),
                    }
                }
            }
            Rule::NestedDeclarations(d) => self.decls(w, d, layer, style),
            Rule::At {
                name,
                prelude,
                block,
            } => {
                let name = parsed.name(*name).to_ascii_lowercase();
                let values: Vec<_> = parsed.values(prelude.clone()).collect();
                let pre = strip_ws(&values);
                match (name.as_str(), block) {
                    // Only at-rules holding style rules nest in style rules.
                    ("layer", None) if style.is_none() => {
                        let names: Option<Vec<Vec<String>>> = pre
                            .split(|v| matches!(v.token(), Token::Comma))
                            .map(|p| layer_name(parsed, strip_ws(p)))
                            .collect();
                        let Some(names) = names else { return };
                        for n in names {
                            let mut full = layer.to_vec();
                            full.extend(n.into_iter().map(Seg::Named));
                            self.declare(w.origin, &full);
                        }
                        if top {
                            w.earlier.push(TopKind::LayerStatement);
                        }
                    }
                    ("layer", Some(items)) => {
                        let mut full = layer.to_vec();
                        if pre.is_empty() {
                            full.push(self.anon());
                        } else {
                            let Some(n) = layer_name(parsed, pre) else {
                                return;
                            };
                            full.extend(n.into_iter().map(Seg::Named));
                        }
                        self.declare(w.origin, &full);
                        if top {
                            w.earlier.push(TopKind::Other);
                        }
                        for item in items {
                            match item {
                                BlockItem::Rule(c) => self.rule(w, *c, &full, style, false),
                                BlockItem::Declarations(d) => self.decls(w, d, &full, style),
                            }
                        }
                    }
                    ("namespace", None) if top => {
                        if !Self::namespace_valid(&w.earlier) {
                            return;
                        }
                        let (prefix, rest) = match pre {
                            [p, rest @ ..] if matches!(p.token(), Token::Ident(_)) => {
                                (Some(p), strip_ws(rest))
                            }
                            _ => (None, pre),
                        };
                        if rest.len() != 1 {
                            return;
                        }
                        let Some(url) = url_like(&rest[0]) else {
                            return;
                        };
                        match prefix.map(|p| p.token()) {
                            Some(Token::Ident(p)) => w.ns.declare(p, &url),
                            _ => w.ns.set_default(&url),
                        }
                        w.earlier.push(TopKind::Namespace);
                    }
                    ("import", None) if top => {
                        if !Self::import_valid(&w.earlier) {
                            return;
                        }
                        let Some(first) = pre.first() else { return };
                        if url_like(first).is_none() {
                            return;
                        }
                        let rest = strip_ws(&pre[1..]);
                        let mut layer_of: Option<Option<Vec<String>>> = None;
                        let mut conditions = rest;
                        if let Some(v) = rest.first() {
                            match v.kind() {
                                Kind::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("layer") => {
                                    layer_of = Some(None);
                                    conditions = strip_ws(&rest[1..]);
                                }
                                Kind::Function(f, args) if f.eq_ignore_ascii_case("layer") => {
                                    let args: Vec<_> = args.collect();
                                    // Not a <layer-name>: then it is a
                                    // condition, not the layer clause.
                                    if let Some(n) = layer_name(parsed, strip_ws(&args)) {
                                        layer_of = Some(Some(n));
                                        conditions = strip_ws(&rest[1..]);
                                    }
                                }
                                _ => {}
                            }
                        }
                        w.earlier.push(TopKind::Import);
                        if conditions.is_empty() {
                            match layer_of {
                                Some(Some(n)) => {
                                    let full: Vec<Seg> = n.into_iter().map(Seg::Named).collect();
                                    self.declare(w.origin, &full);
                                }
                                Some(None) => {
                                    let a = self.anon();
                                    self.declare(w.origin, &[a]);
                                }
                                None => {}
                            }
                        }
                    }
                    (n, Some(_))
                        if top
                            && (GROUP_NOT_APPLIED.contains(&n)
                                || OTHER_NOT_APPLIED.contains(&n)) =>
                    {
                        w.earlier.push(TopKind::Other);
                    }
                    _ => {}
                }
            }
        }
    }

    fn decls(
        &mut self,
        w: &SheetWalk<'_, '_>,
        d: &[Declaration],
        layer: &[Seg],
        style: Option<usize>,
    ) {
        let Some(rule) = style else { return };
        for decl in d {
            let order = self.decls.len();
            self.decls.push(ModelDecl {
                id: Id::Sheet(w.sheet, decl.name),
                name: name_of(w.parsed, decl),
                important: decl.important,
                rule,
                layer: layer.to_vec(),
                order,
            });
        }
    }

    fn style_rule(
        &mut self,
        w: &SheetWalk<'_, '_>,
        prelude: &[ComponentValue<'_, '_>],
        parent: Option<usize>,
    ) -> Option<usize> {
        let parsed = w.parsed;
        let items: Vec<&[ComponentValue<'_, '_>]> = prelude
            .split(|v| matches!(v.token(), Token::Comma))
            .collect();
        let mut texts = Vec::new();
        for item in items {
            let item = strip_ws(item);
            let t = text(parsed, item);
            if item.is_empty() {
                return None;
            }
            if parent.is_none() {
                texts.push(t);
                continue;
            }
            let starts_with_combinator = matches!(
                item[0].token(),
                Token::Delim('>') | Token::Delim('+') | Token::Delim('~')
            );
            let has_amp = Tokenizer::new(Wtf8::from_str(&t)).any(|s| s.token == Token::Delim('&'));
            if !starts_with_combinator && has_amp {
                texts.push(t);
            } else {
                texts.push(format!("& {t}"));
            }
        }
        let mut selectors = Vec::new();
        for t in &texts {
            let l = parse_selector(t, &w.ns).ok()?;
            if l.0.len() != 1 {
                return None;
            }
            selectors.push(l);
        }
        let list = SelectorList(selectors.iter().map(|l| l.0[0].clone()).collect());
        let ancestors = match parent {
            Some(p) => {
                let mut a = self.rules[p].ancestors.clone();
                a.push(p);
                a
            }
            None => Vec::new(),
        };
        self.rules.push(ModelRule {
            origin: w.origin,
            selectors,
            list,
            ancestors,
        });
        Some(self.rules.len() - 1)
    }

    /// What `&` counts for in rules nested in `rule`.
    fn nesting_specificity(&self, rule: usize) -> Specificity {
        let r = &self.rules[rule];
        let outer = match r.ancestors.last() {
            Some(&p) => self.nesting_specificity(p),
            None => Specificity::ZERO,
        };
        r.list
            .0
            .iter()
            .map(|s| s.specificity(outer))
            .max()
            .unwrap_or(Specificity::ZERO)
    }

    /// The rank of a layer in its origin's layer order: count the layers
    /// (and the root) it comes after.
    fn layer_cmp(&self, origin: Origin, a: &[Seg], b: &[Seg]) -> Ordering {
        let pos = |n: &[Seg]| {
            self.declared
                .iter()
                .position(|(o, d)| *o == origin && d == n)
                .expect("declared")
        };
        let k = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        match (a.len() == k, b.len() == k) {
            (true, true) => Ordering::Equal,
            // a is b's ancestor: a's own rules come after b.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => pos(&a[..=k]).cmp(&pos(&b[..=k])),
        }
    }

    /// The cascade for one element: each property's declarations, the
    /// winner first, with names sorted.
    pub fn cascade<E: Element>(
        &self,
        e: &E,
        quirks: bool,
        style: Option<&Parsed<'_>>,
        style_decls: &[Declaration],
    ) -> Vec<(String, Vec<Id>)> {
        // (decl, level, attached, specificity)
        struct Applied<'m> {
            id: Id,
            name: String,
            level: u8,
            important: bool,
            attached: bool,
            origin: Origin,
            layer: Option<&'m [Seg]>,
            spec: Specificity,
            order: usize,
        }
        let level = |o: Origin, important: bool| -> u8 {
            match (important, o) {
                (false, Origin::UserAgent) => 0,
                (false, Origin::User) => 1,
                (false, Origin::Author) => 2,
                (true, Origin::Author) => 3,
                (true, Origin::User) => 4,
                (true, Origin::UserAgent) => 5,
            }
        };
        let mut applied: Vec<Applied<'_>> = Vec::new();
        for d in &self.decls {
            let r = &self.rules[d.rule];
            let chain: Vec<&SelectorList> =
                r.ancestors.iter().map(|&a| &self.rules[a].list).collect();
            let cx = MatchContext {
                scope: Scope::None,
                quirks,
                nesting: &chain,
            };
            let outer = match r.ancestors.last() {
                Some(&p) => self.nesting_specificity(p),
                None => Specificity::ZERO,
            };
            let spec = r
                .selectors
                .iter()
                .filter(|s| matches(s, e, &cx))
                .map(|s| s.0[0].specificity(outer))
                .max();
            let Some(spec) = spec else { continue };
            applied.push(Applied {
                id: d.id,
                name: d.name.clone(),
                level: level(r.origin, d.important),
                important: d.important,
                attached: false,
                origin: r.origin,
                layer: Some(&d.layer),
                spec,
                order: d.order,
            });
        }
        if let Some(p) = style {
            for (i, d) in style_decls.iter().enumerate() {
                applied.push(Applied {
                    id: Id::Attribute(i),
                    name: name_of(p, d),
                    level: level(Origin::Author, d.important),
                    important: d.important,
                    attached: true,
                    origin: Origin::Author,
                    layer: None,
                    spec: Specificity::ZERO,
                    order: self.decls.len() + i,
                });
            }
        }
        // Ascending precedence, criterion by criterion.
        applied.sort_by(|a, b| {
            a.level
                .cmp(&b.level)
                .then(a.attached.cmp(&b.attached))
                .then_with(|| match (a.layer, b.layer) {
                    (Some(la), Some(lb)) => {
                        // Same level: same origin and importance.
                        let c = self.layer_cmp(a.origin, la, lb);
                        if a.important { c.reverse() } else { c }
                    }
                    _ => Ordering::Equal,
                })
                .then(a.spec.cmp(&b.spec))
                .then(a.order.cmp(&b.order))
        });
        let mut names: Vec<String> = applied.iter().map(|a| a.name.clone()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|n| {
                let ids = applied
                    .iter()
                    .rev()
                    .filter(|a| a.name == n)
                    .map(|a| a.id)
                    .collect();
                (n, ids)
            })
            .collect()
    }
}

/// Parses a style attribute as the model reads it.
pub fn style_attribute(text: &str) -> (Parsed<'_>, Vec<Declaration>) {
    parse_declaration_list(Wtf8::from_str(text), &mut AcceptAll)
}

/// Parses a sheet as the model reads it.
pub fn sheet(text: &str) -> (Parsed<'_>, Vec<RuleId>) {
    parse_stylesheet(Wtf8::from_str(text), &mut AcceptAll)
}
