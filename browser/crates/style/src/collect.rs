//! Collecting style rules from style sheets: what the cascade sorts.
//!
//! A [`StyleSet`] takes parsed style sheets, each with its origin, in the
//! order of the document's final CSS style sheets, and builds:
//!
//! - **style rules** ([`StyleRule`]): the selector list, made absolute for
//!   nested rules (css-nesting-1: a relative selector is relative to `&`, a
//!   selector that contains `&` stands as written), each selector's
//!   specificity, and the nearest ancestor style rule, which `&` stands for;
//! - **declaration blocks** ([`Block`]): the rule's own declarations, and
//!   each nested declarations rule (declarations after a nested rule, or
//!   directly inside a nested group rule such as `@layer`), which match
//!   exactly as the parent rule does, each in its cascade layer;
//! - **declarations** ([`Decl`]): the name (ASCII-lowercased, except custom
//!   properties), the value, the important flag, and the order of
//!   appearance across all sheets (css-cascade-5 6.1; nested rules come
//!   after their parent's own declarations);
//! - **cascade layers** ([`Layers`]): `@layer` blocks and statements,
//!   anonymous layers, nested layer names (6.4), and the layers named by
//!   `@import ... layer(...)`.
//!
//! `@namespace` (css-namespaces-3) declares the prefixes selectors use. The
//! placement rules for `@import` and `@namespace` are applied: each must
//! come before the other rules of its sheet (ignoring `@charset` and `@layer`
//! statements before them), or it is ignored.
//!
//! Not applied yet, each recorded as a known gap in the crate docs:
//! `@import` does not load the imported sheet (its layer is still
//! declared, as css-cascade-5 6.4.3 asks even when loading fails, unless
//! it has import conditions); the conditional group rules (`@media`,
//! `@supports`, `@container`) and `@scope` are dropped with their
//! contents; every declaration is kept, whatever its name and value,
//! until the property table can check them.
//!
//! Design choices (chosen, why, rejected):
//!
//! - **Parse with every rule accepted, then check validity in one walk.**
//!   css-syntax-3 asks the parser whether each rule and declaration is
//!   "valid in the current context". That depends on context the parser
//!   doesn't have (whether an ancestor is a style rule), so the sheet is
//!   parsed with [`AcceptAll`] and the walk here drops what is invalid.
//!   This gives the same rules and declarations: a rule the parser would
//!   have refused only changes where a run of declarations is split into
//!   a nested declarations rule, and the pieces of a split run match the
//!   same elements with the same specificity, in the same order. Rejected:
//!   a validity hook that tracks the open blocks, which needs the parser
//!   to report them.
//! - **The walk is iterative** (an explicit stack), because blocks nest as
//!   deep as the input does.
//! - **Nested style rules are limited in depth.** Matching follows `&`
//!   from a nested rule to its parent's selectors recursively, so a rule's
//!   nesting depth is the sum, along its chain of ancestors, of each
//!   selector's depth of functional pseudo-classes, plus one per level. A
//!   rule whose depth would pass [`MAX_RULE_NESTING`] is refused with its
//!   contents ([`StyleSet::refused_too_deep`] counts them), so matching
//!   stays within the stack the selectors crate already needs for its own
//!   limit. No real style sheet comes near it; the spec sets no limit.

use optimal_css::{
    AcceptAll, BlockItem, ComponentValue, Declaration, Kind, Parsed, Rule, RuleId, Token, Values,
    parse_declaration_list, parse_stylesheet,
};
use optimal_selectors::{
    Forgiving, ForgivingItem, MAX_NESTING, Namespaces, PseudoClass, PseudoElement, Selector,
    SelectorList, Simple, Specificity, Unit, parse_relative_selector_list, parse_selector_list,
};
use optimal_wtf8::Wtf8;
use std::ops::Range;

use crate::layer::{LayerId, Layers, layer_name, split_commas, trim};

/// A cascade origin (css-cascade-5 6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    /// The user agent's default style sheet.
    UserAgent,
    /// The user's style sheets.
    User,
    /// The document's style sheets and style attributes.
    Author,
}

/// How deep style rules may nest, counting each level and the depth of
/// functional pseudo-classes in each selector along the way (see the module
/// documentation). Deeper rules are refused.
pub const MAX_RULE_NESTING: u32 = MAX_NESTING;

/// Index of a sheet in a [`StyleSet`].
pub type SheetId = u32;

/// A parsed style sheet and its origin.
#[derive(Debug)]
pub struct Sheet<'a> {
    /// Its origin.
    pub origin: Origin,
    /// The parse.
    pub parsed: Parsed<'a>,
}

/// A declaration of a style rule.
#[derive(Clone, Debug)]
pub struct Decl {
    /// The property name: ASCII-lowercased, except custom properties
    /// (`--*`), whose names are case-sensitive.
    pub name: Box<str>,
    /// The sheet it is in.
    pub sheet: SheetId,
    /// The declaration as parsed (its value is in the sheet's tokens).
    pub decl: Declaration,
    /// Its place in the order of appearance of all sheets' declarations.
    pub order: u32,
}

/// A run of declarations that matches as its style rule does: the rule's
/// own declarations or a nested declarations rule.
#[derive(Clone, Debug)]
pub struct Block {
    /// The cascade layer it is in.
    pub layer: LayerId,
    /// Its declarations, as indices into [`StyleSet::decls`].
    pub decls: Range<u32>,
}

/// A style rule.
#[derive(Clone, Debug)]
pub struct StyleRule {
    /// The origin of its sheet.
    pub origin: Origin,
    /// Its selectors. For a nested rule, relative selectors are made
    /// absolute by putting `&` and the combinator in front.
    pub selectors: SelectorList,
    /// Each selector alone, for matching one at a time.
    pub(crate) singles: Vec<SelectorList>,
    /// Each selector's specificity.
    pub specificity: Vec<Specificity>,
    /// The nearest ancestor style rule, which `&` stands for.
    pub parent: Option<u32>,
    /// Its declaration blocks, in order of appearance.
    pub blocks: Vec<Block>,
    /// The largest specificity among its selectors: what `&` counts for in
    /// rules nested in it.
    nesting_specificity: Specificity,
    /// Its nesting depth (see [`MAX_RULE_NESTING`]).
    depth: u32,
}

/// Every style rule of a document's style sheets, with their declarations
/// and cascade layers.
#[derive(Debug, Default)]
pub struct StyleSet<'a> {
    sheets: Vec<Sheet<'a>>,
    rules: Vec<StyleRule>,
    decls: Vec<Decl>,
    layers: Layers,
    refused: usize,
}

/// The declarations of a `style` attribute (css-style-attr: parsed as a
/// list of declarations), attached to an element.
#[derive(Debug)]
pub struct StyleAttribute<'a> {
    /// The parse.
    pub parsed: Parsed<'a>,
    /// The declarations, with their names as in [`Decl::name`].
    pub decls: Vec<(Box<str>, Declaration)>,
}

impl<'a> StyleAttribute<'a> {
    /// Parses a `style` attribute's value.
    pub fn parse(text: Wtf8<'a>) -> Self {
        let (parsed, list) = parse_declaration_list(text, &mut AcceptAll);
        let decls = list
            .into_iter()
            .map(|d| (property_name(parsed.name(d.name)), d))
            .collect();
        StyleAttribute { parsed, decls }
    }

    /// A declaration's value.
    pub fn values(&self, i: usize) -> Values<'_, 'a> {
        self.parsed.values(self.decls[i].1.value.clone())
    }
}

/// A property name as the cascade compares it.
pub(crate) fn property_name(name: &str) -> Box<str> {
    if name.starts_with("--") {
        name.into()
    } else {
        name.to_ascii_lowercase().into()
    }
}

/// Where a rule sits, for the walk.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    layer: LayerId,
    /// The nearest ancestor style rule.
    style: Option<u32>,
    /// Directly in the sheet (not in any block).
    top: bool,
}

/// What the walk still has to do, last first.
enum Work<'p> {
    Rule(RuleId, Ctx),
    Decls(&'p [Declaration], Ctx),
}

/// The placement state of a sheet's top-level rules: what may still come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Only `@layer` statements so far: `@import` and `@namespace` may come.
    Start,
    /// After `@import`: more `@import`, or `@namespace`.
    Imports,
    /// After `@namespace`: more `@namespace`.
    Namespaces,
    /// After any other rule: neither.
    Body,
}

/// At-rules that are valid but not applied yet (the conditional group
/// rules and `@scope`, whose rules are dropped for now, and the at-rules
/// that hold no style rules). They count as rules for the placement of
/// `@import` and `@namespace`. The group rules may also be nested in style
/// rules; the others only stand at the top level or in `@layer`.
const GROUP_RULES_NOT_APPLIED: &[&str] = &["media", "supports", "container", "scope", "starting-style"];
const OTHER_RULES_NOT_APPLIED: &[&str] = &[
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

impl<'a> StyleSet<'a> {
    /// No sheets.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `text` as a style sheet of `origin` and adds its rules after
    /// those of the sheets added before.
    pub fn add_sheet(&mut self, text: Wtf8<'a>, origin: Origin) -> SheetId {
        let (parsed, top) = parse_stylesheet(text, &mut AcceptAll);
        self.add_parsed(parsed, &top, origin)
    }

    /// Adds an already parsed sheet (`top` its top-level rules).
    pub fn add_parsed(&mut self, parsed: Parsed<'a>, top: &[RuleId], origin: Origin) -> SheetId {
        let id = self.sheets.len() as SheetId;
        Walk {
            set: self,
            parsed: &parsed,
            sheet: id,
            origin,
            ns: Namespaces::new(),
            phase: Phase::Start,
        }
        .run(top);
        self.sheets.push(Sheet { origin, parsed });
        id
    }

    /// The sheets, in order.
    pub fn sheets(&self) -> &[Sheet<'a>] {
        &self.sheets
    }

    /// The style rules, in order of appearance.
    pub fn rules(&self) -> &[StyleRule] {
        &self.rules
    }

    /// Every declaration of every style rule, in order of appearance.
    pub fn decls(&self) -> &[Decl] {
        &self.decls
    }

    /// The cascade layers.
    pub fn layers(&self) -> &Layers {
        &self.layers
    }

    /// The value of a declaration.
    pub fn values(&self, d: &Decl) -> Values<'_, 'a> {
        self.sheets[d.sheet as usize]
            .parsed
            .values(d.decl.value.clone())
    }

    /// How many nested style rules were refused for passing
    /// [`MAX_RULE_NESTING`].
    pub fn refused_too_deep(&self) -> usize {
        self.refused
    }

    /// One past the order of the last declaration: where element-attached
    /// declarations start.
    pub(crate) fn end_order(&self) -> u32 {
        self.decls.len() as u32
    }
}

struct Walk<'w, 's, 'p, 'a> {
    set: &'w mut StyleSet<'s>,
    parsed: &'p Parsed<'a>,
    sheet: SheetId,
    origin: Origin,
    ns: Namespaces,
    phase: Phase,
}

impl<'p, 'a> Walk<'_, '_, 'p, 'a> {
    fn run(&mut self, top: &[RuleId]) {
        let ctx = Ctx {
            layer: Layers::root(self.origin),
            style: None,
            top: true,
        };
        let mut stack: Vec<Work<'p>> = top.iter().rev().map(|&r| Work::Rule(r, ctx)).collect();
        while let Some(w) = stack.pop() {
            match w {
                Work::Decls(decls, ctx) => self.decls(decls, ctx),
                Work::Rule(r, ctx) => self.rule(r, ctx, &mut stack),
            }
        }
    }

    /// Declarations: a block of the nearest style rule, if there is one
    /// (elsewhere declarations are invalid).
    fn decls(&mut self, decls: &[Declaration], ctx: Ctx) {
        let Some(style) = ctx.style else { return };
        if decls.is_empty() {
            return;
        }
        let start = self.set.decls.len() as u32;
        for d in decls {
            self.set.decls.push(Decl {
                name: property_name(self.parsed.name(d.name)),
                sheet: self.sheet,
                decl: d.clone(),
                order: self.set.decls.len() as u32,
            });
        }
        let end = self.set.decls.len() as u32;
        self.set.rules[style as usize].blocks.push(Block {
            layer: ctx.layer,
            decls: start..end,
        });
    }

    fn values(&self, range: Range<u32>) -> Vec<ComponentValue<'p, 'a>> {
        self.parsed.values(range).collect()
    }

    fn rule(&mut self, r: RuleId, ctx: Ctx, stack: &mut Vec<Work<'p>>) {
        let parsed: &'p Parsed<'a> = self.parsed;
        match &parsed.rules[r as usize] {
            Rule::Qualified {
                prelude,
                declarations,
                children,
            } => {
                let Some(id) = self.style_rule(prelude.clone(), ctx) else {
                    return;
                };
                if ctx.top {
                    self.phase = Phase::Body;
                }
                let inner = Ctx {
                    layer: ctx.layer,
                    style: Some(id),
                    top: false,
                };
                for &c in children.iter().rev() {
                    stack.push(match &parsed.rules[c as usize] {
                        Rule::NestedDeclarations(d) => Work::Decls(d, inner),
                        _ => Work::Rule(c, inner),
                    });
                }
                stack.push(Work::Decls(declarations, inner));
            }
            Rule::NestedDeclarations(d) => stack.push(Work::Decls(d, ctx)),
            Rule::At {
                name,
                prelude,
                block,
            } => {
                let name = parsed.name(*name).to_ascii_lowercase();
                let prelude = self.values(prelude.clone());
                let prelude = trim(&prelude);
                match (name.as_str(), block) {
                    ("layer", None) => self.layer_statement(prelude, ctx),
                    ("layer", Some(items)) => {
                        let Some(layer) = self.layer_block(prelude, ctx) else {
                            return;
                        };
                        if ctx.top {
                            self.phase = Phase::Body;
                        }
                        let inner = Ctx {
                            layer,
                            style: ctx.style,
                            top: false,
                        };
                        for item in items.iter().rev() {
                            stack.push(match item {
                                BlockItem::Rule(c) => Work::Rule(*c, inner),
                                BlockItem::Declarations(d) => Work::Decls(d, inner),
                            });
                        }
                    }
                    ("namespace", None) if ctx.top => self.namespace(prelude),
                    ("import", None) if ctx.top => self.import(prelude, ctx),
                    (n, Some(_))
                        if ctx.top
                            && (GROUP_RULES_NOT_APPLIED.contains(&n)
                                || OTHER_RULES_NOT_APPLIED.contains(&n)) =>
                    {
                        self.phase = Phase::Body;
                    }
                    // Unknown, or not allowed here: ignored.
                    _ => {}
                }
            }
        }
    }

    /// `@layer <layer-name>#;`: declares each layer, in order.
    fn layer_statement(&mut self, prelude: &[ComponentValue<'_, '_>], ctx: Ctx) {
        let names: Option<Vec<_>> = split_commas(prelude).into_iter().map(layer_name).collect();
        let Some(names) = names else { return };
        if names.is_empty() {
            return;
        }
        for n in &names {
            self.set.layers.path(ctx.layer, n);
        }
        if ctx.top && matches!(self.phase, Phase::Imports | Phase::Namespaces) {
            self.phase = Phase::Body;
        }
    }

    /// `@layer <layer-name>? { ... }`: the layer its rules go in.
    fn layer_block(&mut self, prelude: &[ComponentValue<'_, '_>], ctx: Ctx) -> Option<LayerId> {
        if prelude.is_empty() {
            return Some(self.set.layers.anonymous(ctx.layer));
        }
        let name = layer_name(prelude)?;
        Some(self.set.layers.path(ctx.layer, &name))
    }

    /// `@namespace <namespace-prefix>? [ <string> | <url> ];`
    fn namespace(&mut self, prelude: &[ComponentValue<'_, '_>]) {
        if !matches!(self.phase, Phase::Start | Phase::Imports | Phase::Namespaces) {
            return;
        }
        let (prefix, rest) = match prelude {
            [p, rest @ ..] if matches!(p.token(), Token::Ident(_)) => (Some(*p), trim(rest)),
            _ => (None, prelude),
        };
        let [url] = rest else { return };
        let Some(url) = string_or_url(*url) else {
            return;
        };
        match prefix.map(|p| p.token()) {
            Some(Token::Ident(p)) => self.ns.declare(p, &url),
            _ => self.ns.set_default(&url),
        }
        self.phase = Phase::Namespaces;
    }

    /// `@import [ <url> | <string> ] [ layer | layer(<layer-name>) ]?
    /// <import-conditions>;`. The sheet isn't loaded yet; the layer is
    /// declared when there are no import conditions.
    fn import(&mut self, prelude: &[ComponentValue<'_, '_>], ctx: Ctx) {
        if !matches!(self.phase, Phase::Start | Phase::Imports) {
            return;
        }
        let [url, rest @ ..] = prelude else { return };
        if string_or_url(*url).is_none() {
            return;
        }
        let rest = trim(rest);
        let (layer, conditions) = match rest.first().map(|v| v.kind()) {
            Some(Kind::Token(Token::Ident(s))) if s.eq_ignore_ascii_case("layer") => {
                (Some(None), trim(&rest[1..]))
            }
            Some(Kind::Function(f, args)) if f.eq_ignore_ascii_case("layer") => {
                let args: Vec<_> = args.collect();
                match layer_name(trim(&args)) {
                    Some(n) => (Some(Some(n)), trim(&rest[1..])),
                    // An invalid layer() makes the rule invalid.
                    None => return,
                }
            }
            _ => (None, rest),
        };
        self.phase = Phase::Imports;
        if conditions.is_empty() {
            match layer {
                Some(Some(name)) => {
                    self.set.layers.path(ctx.layer, &name);
                }
                Some(None) => {
                    self.set.layers.anonymous(ctx.layer);
                }
                None => {}
            }
        }
    }

    /// Parses a style rule's prelude and adds the rule, or refuses it.
    fn style_rule(&mut self, prelude: Range<u32>, ctx: Ctx) -> Option<u32> {
        let parent = ctx.style.map(|p| &self.set.rules[p as usize]);
        let list = match parent {
            None => parse_selector_list(self.parsed, prelude, &self.ns).ok()?,
            Some(_) => {
                let rel = parse_relative_selector_list(self.parsed, prelude.clone(), &self.ns).ok()?;
                let contains = contains_nesting(self.parsed, prelude);
                debug_assert_eq!(rel.len(), contains.len());
                SelectorList(
                    rel.into_iter()
                        .zip(contains)
                        .map(|(r, has_nesting)| {
                            if !r.explicit && has_nesting {
                                r.selector
                            } else {
                                let mut rest = Vec::with_capacity(r.selector.rest.len() + 1);
                                rest.push((r.combinator, r.selector.first));
                                rest.extend(r.selector.rest);
                                Selector {
                                    first: Unit {
                                        compound: vec![Simple::Nesting],
                                        pseudos: Vec::new(),
                                    },
                                    rest,
                                }
                            }
                        })
                        .collect(),
                )
            }
        };
        let (nest_spec, parent_depth) = match parent {
            Some(p) => (p.nesting_specificity, p.depth + 1),
            None => (Specificity::ZERO, 0),
        };
        let depth = parent_depth + list.0.iter().map(selector_depth).max().unwrap_or(0);
        if depth > MAX_RULE_NESTING {
            self.set.refused += 1;
            return None;
        }
        let specificity: Vec<Specificity> =
            list.0.iter().map(|s| s.specificity(nest_spec)).collect();
        let id = self.set.rules.len() as u32;
        self.set.rules.push(StyleRule {
            origin: self.origin,
            singles: list.0.iter().map(|s| SelectorList(vec![s.clone()])).collect(),
            nesting_specificity: specificity.iter().copied().max().unwrap_or_default(),
            specificity,
            selectors: list,
            parent: ctx.style,
            blocks: Vec::new(),
            depth,
        });
        Some(id)
    }
}

/// A `<string>` or `<url>` (a url token, or `url()` with a string).
fn string_or_url(v: ComponentValue<'_, '_>) -> Option<String> {
    match v.kind() {
        Kind::Token(Token::String(s) | Token::Url(s)) => Some(s.to_string()),
        Kind::Function(f, args) if f.eq_ignore_ascii_case("url") => {
            let args: Vec<_> = args.collect();
            match trim(&args) {
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

/// For each comma-separated item of a prelude, whether it "contains the
/// nesting selector": has a `&` delim token at any depth (css-nesting-1,
/// which counts one even inside an unknown pseudo-class's argument).
fn contains_nesting(parsed: &Parsed<'_>, range: Range<u32>) -> Vec<bool> {
    let mut out = vec![false];
    for v in parsed.values(range) {
        match v.token() {
            Token::Comma => out.push(false),
            _ => {
                let span = v.index()..v.index() + token_count(v);
                if span.into_iter().any(|i| matches!(parsed.tokens.get(i).token, Token::Delim('&'))) {
                    *out.last_mut().expect("not empty") = true;
                }
            }
        }
    }
    out
}

/// How many tokens a component value covers.
fn token_count(v: ComponentValue<'_, '_>) -> u32 {
    match v.kind() {
        Kind::Token(_) => 1,
        Kind::Function(_, inner) | Kind::Block(_, inner) => {
            // The opener, the contents, and the closer if there is one.
            1 + (inner.range().end - inner.range().start) + u32::from(v.is_closed())
        }
    }
}

/// How deeply functional pseudo-classes and pseudo-elements nest in a
/// selector (at most [`MAX_NESTING`], which the parser enforces).
fn selector_depth(s: &Selector) -> u32 {
    std::iter::once(&s.first)
        .chain(s.rest.iter().map(|(_, u)| u))
        .map(|u| {
            let simple = u.compound.iter().map(simple_depth);
            let pseudo = u.pseudos.iter().flat_map(|p| {
                let e = match &p.element {
                    PseudoElement::Slotted(c) => 1 + c.iter().map(simple_depth).max().unwrap_or(0),
                    _ => 0,
                };
                std::iter::once(e).chain(p.classes.iter().map(class_depth))
            });
            simple.chain(pseudo).max().unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn simple_depth(s: &Simple) -> u32 {
    match s {
        Simple::PseudoClass(pc) => class_depth(pc),
        _ => 0,
    }
}

fn class_depth(pc: &PseudoClass) -> u32 {
    let list = |l: &SelectorList| l.0.iter().map(selector_depth).max().unwrap_or(0);
    let forgiving = |f: &Forgiving| {
        f.0.iter()
            .map(|i| match i {
                ForgivingItem::Valid(s) => selector_depth(s),
                ForgivingItem::Invalid(_) => 0,
            })
            .max()
            .unwrap_or(0)
    };
    1 + match pc {
        PseudoClass::Is(f) | PseudoClass::Where(f) => forgiving(f),
        PseudoClass::Not(l) => list(l),
        PseudoClass::Has(rel) => rel
            .iter()
            .map(|r| selector_depth(&r.selector))
            .max()
            .unwrap_or(0),
        PseudoClass::NthChild(_, Some(l)) | PseudoClass::NthLastChild(_, Some(l)) => list(l),
        PseudoClass::Host(c) | PseudoClass::HostContext(c) => {
            c.iter().map(simple_depth).max().unwrap_or(0)
        }
        _ => return 0,
    }
}
