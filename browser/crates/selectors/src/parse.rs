//! Parsing selectors from component values (selectors-4 section 16, the
//! grammar, with the validity rules of 3.6 and 3.9 and of the pseudo-classes
//! and pseudo-elements themselves).
//!
//! Design choices (chosen, why, rejected):
//!
//! - **Recursive descent over component values, nesting capped.** The
//!   grammar only nests through functional pseudo-classes and
//!   pseudo-elements, so the parser recurses only there, at most
//!   [`MAX_NESTING`] levels deep; deeper input is refused with
//!   [`SelectorError::TooDeep`] instead of risking the stack (in parsing,
//!   serializing, specificity or drop). Everything else (blocks inside
//!   arguments, attribute selectors, the bad-token scan) is iterative.
//!   Rejected: an explicit-stack parser, which would be much harder to read
//!   against the grammar for a nesting depth no real style sheet reaches.
//! - **Restrictions travel in a context.** Where a selector sits limits what
//!   it may contain (only compound selectors in `:host()`, no `:has()` in
//!   `:has()`, only some pseudo-classes after a pseudo-element, and so on).
//!   [`Ctx`] carries these down, and the logical combination pseudo-classes
//!   pass them to their arguments (section 4).
//! - **Namespaces are resolved while parsing**, so a selector with an
//!   undeclared prefix is refused here (3.9) and each type, universal and
//!   attribute selector records the namespace it needs.

use crate::ast::*;
use optimal_css::{
    BlockKind, ComponentValue, HashKind, Kind, Parsed, Token, Values, parse_anb, parse_anb_prefix,
    parse_component_values,
};
use optimal_wtf8::Wtf8;
use std::ops::Range;

/// How deep functional pseudo-classes and pseudo-elements may nest inside
/// each other. Deeper selectors are refused.
pub const MAX_NESTING: u32 = 256;

/// Why a selector is invalid (3.9). Every invalid selector matches nothing;
/// the kind only says which rule it broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelectorError {
    /// There was no selector, or a comma-separated item was empty.
    Empty,
    /// The tokens don't match the grammar.
    Syntax,
    /// A pseudo-class that isn't supported (or used with or without
    /// arguments when it takes the other form).
    UnknownPseudoClass,
    /// A pseudo-element that isn't supported.
    UnknownPseudoElement,
    /// A namespace prefix that wasn't declared.
    UndeclaredPrefix,
    /// Known syntax in a place that doesn't allow it, such as a combinator
    /// in `:host()` or `:hover` after `::slotted()`.
    NotAllowedHere,
    /// Nested more than [`MAX_NESTING`] levels deep.
    TooDeep,
}

/// The namespace prefixes in scope, and the default namespace (3.8; in CSS
/// they come from `@namespace` rules).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Namespaces {
    default: Option<Box<str>>,
    prefixes: Vec<(Box<str>, Box<str>)>,
}

impl Namespaces {
    /// No prefixes and no default namespace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the default namespace.
    pub fn set_default(&mut self, url: &str) {
        self.default = Some(url.into());
    }

    /// Declares `prefix` (case-sensitive, css-namespaces-3 section 3). A
    /// later declaration of the same prefix replaces the earlier one.
    pub fn declare(&mut self, prefix: &str, url: &str) {
        self.prefixes.retain(|(p, _)| &**p != prefix);
        self.prefixes.push((prefix.into(), url.into()));
    }

    /// The default namespace, if declared.
    pub fn default_namespace(&self) -> Option<&str> {
        self.default.as_deref()
    }

    fn lookup(&self, prefix: &str) -> Option<&str> {
        self.prefixes
            .iter()
            .find(|(p, _)| &**p == prefix)
            .map(|(_, u)| &**u)
    }
}

/// "Parse a selector" (17.1) from text: a `<selector-list>`.
pub fn parse_selector(src: &str, ns: &Namespaces) -> Result<SelectorList, SelectorError> {
    let (parsed, range) = parse_component_values(Wtf8::from_str(src));
    parse_selector_list(&parsed, range, ns)
}

/// "Parse a relative selector" (17.2) from text: a
/// `<relative-selector-list>`.
pub fn parse_relative_selector(
    src: &str,
    ns: &Namespaces,
) -> Result<Vec<RelativeSelector>, SelectorError> {
    let (parsed, range) = parse_component_values(Wtf8::from_str(src));
    parse_relative_selector_list(&parsed, range, ns)
}

/// Parses the component values in `range` of `parsed` (a rule's prelude,
/// say) as a `<selector-list>`.
pub fn parse_selector_list(
    parsed: &Parsed<'_>,
    range: Range<u32>,
    ns: &Namespaces,
) -> Result<SelectorList, SelectorError> {
    let p = Parser { parsed, ns };
    let values: Vec<_> = parsed.values(range).collect();
    p.list(&values, Ctx::TOP)
}

/// Parses the component values in `range` of `parsed` as a
/// `<relative-selector-list>`.
pub fn parse_relative_selector_list(
    parsed: &Parsed<'_>,
    range: Range<u32>,
    ns: &Namespaces,
) -> Result<Vec<RelativeSelector>, SelectorError> {
    let p = Parser { parsed, ns };
    let values: Vec<_> = parsed.values(range).collect();
    p.relative_list(&values, Ctx::TOP)
}

type V<'t, 'a> = ComponentValue<'t, 'a>;

/// What the position being parsed allows.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    /// How many functional pseudo-classes and pseudo-elements enclose it.
    depth: u32,
    /// Only a compound selector: no combinators, no pseudo-elements.
    compound_only: bool,
    /// No pseudo-elements.
    no_pseudo_elements: bool,
    /// Inside `:has()`: no `:has()` (4.5).
    in_has: bool,
    /// Inside the argument of a logical combination pseudo-class that
    /// follows a pseudo-element: only the pseudo-classes allowed after that
    /// pseudo-element (3.6.3), `true` if it is element-backed.
    after_element_backed: Option<bool>,
    /// Inside the argument of an `:is()` or `:where()`, which has already
    /// been checked to be an `<any-value>`, nested arguments included.
    any_value_checked: bool,
}

impl Ctx {
    const TOP: Ctx = Ctx {
        depth: 0,
        compound_only: false,
        no_pseudo_elements: false,
        in_has: false,
        after_element_backed: None,
        any_value_checked: false,
    };

    fn deeper(self) -> Result<Ctx, SelectorError> {
        if self.depth >= MAX_NESTING {
            return Err(SelectorError::TooDeep);
        }
        Ok(Ctx {
            depth: self.depth + 1,
            ..self
        })
    }
}

struct Parser<'p, 'a> {
    parsed: &'p Parsed<'a>,
    ns: &'p Namespaces,
}

fn is_ws(v: &V<'_, '_>) -> bool {
    matches!(v.token(), Token::Whitespace)
}

fn trim<'s, 't, 'a>(mut s: &'s [V<'t, 'a>]) -> &'s [V<'t, 'a>] {
    while let [first, rest @ ..] = s
        && is_ws(first)
    {
        s = rest;
    }
    while let [rest @ .., last] = s
        && is_ws(last)
    {
        s = rest;
    }
    s
}

/// The items of a comma-separated list, each trimmed of whitespace.
fn split_commas<'s, 't, 'a>(s: &'s [V<'t, 'a>]) -> Vec<&'s [V<'t, 'a>]> {
    s.split(|v| matches!(v.token(), Token::Comma))
        .map(trim)
        .collect()
}

fn delim(v: Option<&V<'_, '_>>, c: char) -> bool {
    v.is_some_and(|v| *v.token() == Token::Delim(c))
}

fn ident<'t>(v: Option<&V<'t, '_>>) -> Option<&'t str> {
    match v?.token() {
        Token::Ident(s) => Some(s),
        _ => None,
    }
}

fn is_colon(v: Option<&V<'_, '_>>) -> bool {
    v.is_some_and(|v| matches!(v.token(), Token::Colon))
}

/// The four pseudo-elements that may be written with one colon (3.6.1).
fn legacy_pseudo_element(name: &str) -> Option<PseudoElementKeyword> {
    use PseudoElementKeyword::*;
    [Before, After, FirstLine, FirstLetter]
        .into_iter()
        .find(|k| k.name().eq_ignore_ascii_case(name))
}

/// Whether `s[i]` starts a pseudo-element: `::` or a legacy single-colon
/// name.
fn starts_pseudo_element(s: &[V<'_, '_>], i: usize) -> bool {
    is_colon(s.get(i))
        && (is_colon(s.get(i + 1))
            || ident(s.get(i + 1)).is_some_and(|n| legacy_pseudo_element(n).is_some()))
}

/// `<any-value>`: no bad tokens and no unmatched closers, at any depth
/// (css-syntax-3 section 8.2). Walks nested blocks with an explicit stack.
fn is_any_value(values: Values<'_, '_>) -> bool {
    let mut stack = vec![values];
    while let Some(top) = stack.last_mut() {
        let Some(v) = top.next() else {
            stack.pop();
            continue;
        };
        match v.kind() {
            Kind::Token(
                Token::BadString
                | Token::BadUrl
                | Token::CloseParen
                | Token::CloseSquare
                | Token::CloseCurly,
            ) => return false,
            Kind::Token(_) => {}
            Kind::Function(_, inner) | Kind::Block(_, inner) => stack.push(inner),
        }
    }
    true
}

/// A `<custom-ident>` (css-values-4 4.2): not a CSS-wide keyword, nor
/// `default`.
fn is_custom_ident(s: &str) -> bool {
    ![
        "initial",
        "inherit",
        "unset",
        "revert",
        "revert-layer",
        "default",
    ]
    .iter()
    .any(|k| s.eq_ignore_ascii_case(k))
}

/// What was written before the `|` of a qualified name.
enum Written<'t> {
    /// No `|` at all.
    Implicit,
    /// `*|`
    Any,
    /// `|`
    Empty,
    /// `prefix|`
    Named(&'t str),
}

impl<'p, 'a> Parser<'p, 'a> {
    fn list(&self, s: &[V<'_, 'a>], ctx: Ctx) -> Result<SelectorList, SelectorError> {
        let mut out = Vec::new();
        for item in split_commas(s) {
            out.push(self.complex(item, 0, ctx)?);
        }
        Ok(SelectorList(out))
    }

    fn relative_list(
        &self,
        s: &[V<'_, 'a>],
        ctx: Ctx,
    ) -> Result<Vec<RelativeSelector>, SelectorError> {
        let mut out = Vec::new();
        for item in split_commas(s) {
            let combinator = match item.first().map(|v| v.token()) {
                Some(Token::Delim('>')) => Some(Combinator::Child),
                Some(Token::Delim('+')) => Some(Combinator::NextSibling),
                Some(Token::Delim('~')) => Some(Combinator::SubsequentSibling),
                _ => None,
            };
            let start = if combinator.is_some() {
                let mut i = 1;
                while item.get(i).is_some_and(is_ws) {
                    i += 1;
                }
                i
            } else {
                0
            };
            out.push(RelativeSelector {
                combinator: combinator.unwrap_or(Combinator::Descendant),
                explicit: combinator.is_some(),
                selector: self.complex(item, start, ctx)?,
            });
        }
        Ok(out)
    }

    /// A complex selector from `s[pos..]`, which must use all of it. `s`
    /// has no whitespace at either end.
    fn complex(
        &self,
        s: &[V<'_, 'a>],
        mut pos: usize,
        ctx: Ctx,
    ) -> Result<Selector, SelectorError> {
        if pos == s.len() {
            return Err(SelectorError::Empty);
        }
        let first = self.unit(s, &mut pos, ctx)?;
        let mut pseudo_before = !first.pseudos.is_empty();
        let mut rest = Vec::new();
        loop {
            let start = pos;
            while s.get(pos).is_some_and(is_ws) {
                pos += 1;
            }
            if pos == s.len() {
                break;
            }
            let combinator = match s[pos].token() {
                Token::Delim('>') => Combinator::Child,
                Token::Delim('+') => Combinator::NextSibling,
                Token::Delim('~') => Combinator::SubsequentSibling,
                // unit() only stops at whitespace, a combinator or the end.
                _ if pos > start => Combinator::Descendant,
                _ => return Err(SelectorError::Syntax),
            };
            if combinator != Combinator::Descendant {
                pos += 1;
                while s.get(pos).is_some_and(is_ws) {
                    pos += 1;
                }
                if pos == s.len() {
                    return Err(SelectorError::Syntax);
                }
            }
            // No combinators in a compound-only position, nor after a
            // pseudo-element (none defined here has internal structure,
            // 3.6.5).
            if ctx.compound_only || pseudo_before {
                return Err(SelectorError::NotAllowedHere);
            }
            let unit = self.unit(s, &mut pos, ctx)?;
            pseudo_before = !unit.pseudos.is_empty();
            rest.push((combinator, unit));
        }
        Ok(Selector { first, rest })
    }

    /// A `<complex-selector-unit>` at `s[*pos]`. Stops at whitespace, a
    /// combinator or the end; anything else there is an error.
    fn unit(&self, s: &[V<'_, 'a>], pos: &mut usize, ctx: Ctx) -> Result<Unit, SelectorError> {
        let mut compound = Vec::new();
        if let Some(t) = self.type_selector(s, pos)? {
            compound.push(t);
        }
        while let Some(v) = s.get(*pos) {
            let simple = match v.kind() {
                Kind::Token(Token::Hash(name, HashKind::Id)) => {
                    *pos += 1;
                    Simple::Id((**name).into())
                }
                Kind::Token(Token::Delim('.')) => {
                    let name = ident(s.get(*pos + 1)).ok_or(SelectorError::Syntax)?;
                    *pos += 2;
                    Simple::Class(name.into())
                }
                Kind::Block(BlockKind::Square, inner) => {
                    *pos += 1;
                    Simple::Attribute(Box::new(self.attribute(inner)?))
                }
                Kind::Token(Token::Colon) => {
                    if starts_pseudo_element(s, *pos) {
                        break;
                    }
                    Simple::PseudoClass(self.pseudo_class(s, pos, ctx)?)
                }
                Kind::Token(Token::Delim('&')) => {
                    *pos += 1;
                    Simple::Nesting
                }
                // Anything else ends the compound; complex() then refuses
                // what isn't whitespace or a combinator (a type selector
                // that isn't first, a hash that isn't an identifier...).
                _ => break,
            };
            compound.push(simple);
        }
        if let Some(eb) = ctx.after_element_backed
            && !compound.iter().all(|simple| match simple {
                Simple::PseudoClass(pc) => allowed_after(eb, pc),
                _ => false,
            })
        {
            return Err(SelectorError::NotAllowedHere);
        }
        let mut pseudos: Vec<PseudoCompound> = Vec::new();
        while starts_pseudo_element(s, *pos) {
            let element = self.pseudo_element(s, pos, ctx)?;
            if ctx.no_pseudo_elements || ctx.compound_only {
                return Err(SelectorError::NotAllowedHere);
            }
            let ok = match pseudos.last() {
                Some(prev) => pseudo_element_after(&prev.element, &element),
                None => !matches!(
                    element,
                    PseudoElement::Keyword(
                        PseudoElementKeyword::Prefix | PseudoElementKeyword::Suffix
                    )
                ),
            };
            if !ok {
                return Err(SelectorError::NotAllowedHere);
            }
            let eb = element.is_element_backed();
            let inner = Ctx {
                after_element_backed: Some(eb),
                ..ctx
            };
            let mut classes = Vec::new();
            while is_colon(s.get(*pos)) && !starts_pseudo_element(s, *pos) {
                let pc = self.pseudo_class(s, pos, inner)?;
                if !allowed_after(eb, &pc) {
                    return Err(SelectorError::NotAllowedHere);
                }
                classes.push(pc);
            }
            pseudos.push(PseudoCompound { element, classes });
            match s.get(*pos).map(|v| v.token()) {
                None | Some(Token::Colon | Token::Whitespace | Token::Delim('>' | '+' | '~')) => {}
                // A simple selector after a pseudo-compound.
                Some(_) => return Err(SelectorError::NotAllowedHere),
            }
        }
        if compound.is_empty() && pseudos.is_empty() {
            return Err(SelectorError::Syntax);
        }
        Ok(Unit { compound, pseudos })
    }

    /// The `<ns-prefix>? <ident>` or `<ns-prefix>? '*'` at `s[*pos]`, if it
    /// starts one.
    fn type_selector(
        &self,
        s: &[V<'_, 'a>],
        pos: &mut usize,
    ) -> Result<Option<Simple>, SelectorError> {
        let at = |i: usize| s.get(*pos + i);
        // (what was written before `|`, index of the name).
        let (written, name_at) = match at(0).map(|v| v.token()) {
            Some(Token::Ident(p)) if delim(at(1), '|') => (Written::Named(p), 2),
            Some(Token::Delim('*')) if delim(at(1), '|') => (Written::Any, 2),
            Some(Token::Delim('|')) => (Written::Empty, 1),
            Some(Token::Ident(_) | Token::Delim('*')) => (Written::Implicit, 0),
            _ => return Ok(None),
        };
        let name = at(name_at);
        let local = match name.map(|v| v.token()) {
            Some(Token::Ident(n)) => Some(n),
            Some(Token::Delim('*')) => None,
            _ => return Err(SelectorError::Syntax),
        };
        let (ns, prefix) = self.element_namespace(written)?;
        *pos += name_at + 1;
        Ok(Some(match local {
            Some(n) => Simple::Type {
                ns,
                prefix,
                local: (**n).into(),
            },
            None => Simple::Universal { ns, prefix },
        }))
    }

    /// The namespace a type or universal selector requires (5.3), and the
    /// prefix its serialization needs (cssom-1 "serialize a simple
    /// selector": none when it is the default namespace's, `*` for any
    /// namespace when there is a default, empty for no namespace).
    fn element_namespace(
        &self,
        w: Written<'_>,
    ) -> Result<(Namespace, Option<Box<str>>), SelectorError> {
        let default = self.ns.default_namespace();
        Ok(match w {
            Written::Implicit => (
                default.map_or(Namespace::Any, |u| Namespace::Url(u.into())),
                None,
            ),
            Written::Any => (Namespace::Any, default.map(|_| "*".into())),
            Written::Empty => (Namespace::None, Some("".into())),
            Written::Named(p) => {
                let url = self.ns.lookup(p).ok_or(SelectorError::UndeclaredPrefix)?;
                let prefix = (default != Some(url)).then(|| p.into());
                (Namespace::Url(url.into()), prefix)
            }
        })
    }

    /// The contents of an attribute selector's `[]` block (6.1 to 6.4).
    fn attribute(&self, inner: Values<'_, 'a>) -> Result<Attribute, SelectorError> {
        let all: Vec<_> = inner.collect();
        let s = trim(&all);
        let at = |i: usize| s.get(i);
        // A `|` followed by `=` is the `|=` matcher, not a prefix.
        let bar = |i: usize| delim(at(i), '|') && !delim(at(i + 1), '=');
        let (written, name_at) = match at(0).map(|v| v.token()) {
            Some(Token::Ident(p)) if bar(1) => (Written::Named(p), 2),
            // (`*|=` and `|=` here would leave no name, so these need no
            // look past the `|`.)
            Some(Token::Delim('*')) if delim(at(1), '|') => (Written::Any, 2),
            Some(Token::Delim('|')) => (Written::Empty, 1),
            Some(Token::Ident(_)) => (Written::Implicit, 0),
            _ => return Err(SelectorError::Syntax),
        };
        let local = ident(at(name_at)).ok_or(SelectorError::Syntax)?;
        // 6.4: no prefix and the empty prefix both mean no namespace;
        // cssom-1 5.2 serializes a prefix only for a namespace.
        let (ns, prefix) = match written {
            Written::Implicit | Written::Empty => (Namespace::None, None),
            Written::Any => (Namespace::Any, Some("*".into())),
            Written::Named(p) => {
                let url = self.ns.lookup(p).ok_or(SelectorError::UndeclaredPrefix)?;
                (Namespace::Url(url.into()), Some(p.into()))
            }
        };
        let mut i = name_at + 1;
        let skip_ws = |i: &mut usize| {
            while s.get(*i).is_some_and(is_ws) {
                *i += 1;
            }
        };
        skip_ws(&mut i);
        let mut attr = Attribute {
            ns,
            prefix,
            local: local.into(),
            test: None,
            case: None,
        };
        if i == s.len() {
            return Ok(attr);
        }
        let matcher = match at(i).map(|v| v.token()) {
            Some(Token::Delim('=')) => {
                i += 1;
                AttrMatcher::Equals
            }
            Some(Token::Delim(c @ ('~' | '|' | '^' | '$' | '*'))) if delim(at(i + 1), '=') => {
                i += 2;
                match c {
                    '~' => AttrMatcher::Includes,
                    '|' => AttrMatcher::DashMatch,
                    '^' => AttrMatcher::Prefix,
                    '$' => AttrMatcher::Suffix,
                    _ => AttrMatcher::Substring,
                }
            }
            _ => return Err(SelectorError::Syntax),
        };
        skip_ws(&mut i);
        let value = match at(i).map(|v| v.token()) {
            Some(Token::Ident(v) | Token::String(v)) => v,
            _ => return Err(SelectorError::Syntax),
        };
        attr.test = Some((matcher, (**value).into()));
        i += 1;
        skip_ws(&mut i);
        if let Some(m) = ident(at(i)) {
            attr.case = Some(if m.eq_ignore_ascii_case("i") {
                AttrCase::Insensitive
            } else if m.eq_ignore_ascii_case("s") {
                AttrCase::Sensitive
            } else {
                return Err(SelectorError::Syntax);
            });
            i += 1;
        }
        if i != s.len() {
            return Err(SelectorError::Syntax);
        }
        Ok(attr)
    }

    /// The pseudo-class at `s[*pos]` (a colon, not starting a
    /// pseudo-element).
    fn pseudo_class(
        &self,
        s: &[V<'_, 'a>],
        pos: &mut usize,
        ctx: Ctx,
    ) -> Result<PseudoClass, SelectorError> {
        let v = s.get(*pos + 1).ok_or(SelectorError::Syntax)?;
        *pos += 2;
        match v.kind() {
            Kind::Token(Token::Ident(name)) => {
                if name.eq_ignore_ascii_case("-webkit-autofill") {
                    // The legacy alias of selectors-4 Appendix B.
                    return Ok(PseudoClass::Keyword(PseudoClassKeyword::Autofill));
                }
                PseudoClassKeyword::from_name(name)
                    .map(PseudoClass::Keyword)
                    .ok_or(SelectorError::UnknownPseudoClass)
            }
            Kind::Function(name, args) => self.functional_pseudo_class(name, args, ctx),
            _ => Err(SelectorError::Syntax),
        }
    }

    fn functional_pseudo_class(
        &self,
        name: &str,
        args: Values<'_, 'a>,
        ctx: Ctx,
    ) -> Result<PseudoClass, SelectorError> {
        let inner = ctx.deeper()?;
        let all: Vec<_> = args.collect();
        let s = trim(&all);
        let is = |n: &str| name.eq_ignore_ascii_case(n);
        // Arguments of :is(), :where() and :not() keep the restrictions of
        // where the pseudo-class is (section 4), and can't hold
        // pseudo-elements (4.2, 4.3).
        let logical = Ctx {
            no_pseudo_elements: true,
            compound_only: ctx.compound_only || ctx.after_element_backed.is_some(),
            ..inner
        };
        // Arguments of the other pseudo-classes start afresh, apart from
        // the ban on :has() in :has().
        let fresh = Ctx {
            no_pseudo_elements: true,
            compound_only: false,
            after_element_backed: None,
            ..inner
        };
        if is("is") || is("where") {
            // Scanned once, at the outermost :is() or :where(): a nested one
            // lies inside the part already scanned, and scanning again at
            // every level would cost depth times the input.
            if !ctx.any_value_checked && !is_any_value(args) {
                return Err(SelectorError::Syntax);
            }
            let mut logical = logical;
            logical.any_value_checked = true;
            let f = self.forgiving(s, logical)?;
            return Ok(if is("is") {
                PseudoClass::Is(f)
            } else {
                PseudoClass::Where(f)
            });
        }
        if is("not") {
            return Ok(PseudoClass::Not(self.list(s, logical)?));
        }
        if is("has") {
            // :has() can't nest (4.5). (Its arguments are relative selectors
            // whatever surrounds it: the restriction to compound selectors
            // in, say, ::slotted() applies to the compound :has() sits in,
            // and only the logical combination pseudo-classes pass
            // restrictions on to their arguments.)
            if ctx.in_has {
                return Err(SelectorError::NotAllowedHere);
            }
            let has = Ctx {
                in_has: true,
                ..fresh
            };
            return Ok(PseudoClass::Has(self.relative_list(s, has)?));
        }
        if is("nth-child") || is("nth-last-child") {
            let (anb, rest) = parse_anb_prefix(args).ok_or(SelectorError::Syntax)?;
            let rest: Vec<_> = rest.collect();
            let rest = trim(&rest);
            let of = match rest {
                [] => None,
                [of, tail @ ..]
                    if ident(Some(of)).is_some_and(|w| w.eq_ignore_ascii_case("of")) =>
                {
                    Some(self.list(trim(tail), fresh)?)
                }
                _ => return Err(SelectorError::Syntax),
            };
            return Ok(if is("nth-child") {
                PseudoClass::NthChild(anb, of)
            } else {
                PseudoClass::NthLastChild(anb, of)
            });
        }
        if is("nth-of-type") || is("nth-last-of-type") {
            let anb = parse_anb(args).ok_or(SelectorError::Syntax)?;
            return Ok(if is("nth-of-type") {
                PseudoClass::NthOfType(anb)
            } else {
                PseudoClass::NthLastOfType(anb)
            });
        }
        if is("lang") {
            let mut ranges = Vec::new();
            for item in split_commas(s) {
                match item {
                    [v] => match v.token() {
                        Token::Ident(r) | Token::String(r) => ranges.push((**r).into()),
                        _ => return Err(SelectorError::Syntax),
                    },
                    _ => return Err(SelectorError::Syntax),
                }
            }
            return Ok(PseudoClass::Lang(ranges));
        }
        if is("dir") || is("state") {
            let [v] = s else {
                return Err(SelectorError::Syntax);
            };
            let id: Box<str> = ident(Some(v)).ok_or(SelectorError::Syntax)?.into();
            return Ok(if is("dir") {
                PseudoClass::Dir(id)
            } else {
                PseudoClass::State(id)
            });
        }
        if is("host") || is("host-context") {
            let compound = self.compound_argument(s, inner)?;
            return Ok(if is("host") {
                PseudoClass::Host(compound)
            } else {
                PseudoClass::HostContext(compound)
            });
        }
        Err(SelectorError::UnknownPseudoClass)
    }

    /// A `<compound-selector>` argument (of `:host()`, `:host-context()`,
    /// `::slotted()`).
    fn compound_argument(&self, s: &[V<'_, 'a>], inner: Ctx) -> Result<Vec<Simple>, SelectorError> {
        // compound_only also rules out pseudo-elements.
        let ctx = Ctx {
            compound_only: true,
            after_element_backed: None,
            ..inner
        };
        let sel = self.complex(s, 0, ctx)?;
        // complex() refuses combinators and pseudo-elements here.
        Ok(sel.first.compound)
    }

    /// "Parse as a forgiving selector list" (16.1): each item is kept,
    /// valid or not; invalid ones only for serialization.
    fn forgiving(&self, s: &[V<'_, 'a>], ctx: Ctx) -> Result<Forgiving, SelectorError> {
        let mut items = Vec::new();
        for item in split_commas(s) {
            items.push(match self.complex(item, 0, ctx) {
                Ok(sel) => ForgivingItem::Valid(sel),
                // A nesting depth refusal is not forgiven: the whole
                // selector is refused, not silently cut short.
                Err(SelectorError::TooDeep) => return Err(SelectorError::TooDeep),
                Err(_) => ForgivingItem::Invalid(self.source_text(item)),
            });
        }
        Ok(Forgiving(items))
    }

    /// An item's text: from its first token to its last, plus the closers
    /// of any functions and blocks the end of input left open (so that the
    /// serialization parses back to the same item).
    fn source_text(&self, item: &[V<'_, 'a>]) -> Box<str> {
        let (Some(first), Some(last)) = (item.first(), item.last()) else {
            return "".into();
        };
        let range = first.span().start..last.span().end;
        let mut text = self.parsed.source(range).to_string_lossy();
        // Only the last value can be unclosed (it runs to the end), and
        // then only the last value inside it, and so on.
        let mut closers = Vec::new();
        let mut v = Some(*last);
        while let Some(cv) = v
            && !cv.is_closed()
        {
            v = match cv.kind() {
                Kind::Function(_, inner) | Kind::Block(BlockKind::Paren, inner) => {
                    closers.push(')');
                    inner.last()
                }
                Kind::Block(BlockKind::Square, inner) => {
                    closers.push(']');
                    inner.last()
                }
                Kind::Block(BlockKind::Curly, inner) => {
                    closers.push('}');
                    inner.last()
                }
                Kind::Token(_) => None,
            };
        }
        text.extend(closers.iter().rev());
        text.into()
    }

    /// The pseudo-element at `s[*pos]` (`::name`, `::name(...)`, or a legacy
    /// `:name`).
    fn pseudo_element(
        &self,
        s: &[V<'_, 'a>],
        pos: &mut usize,
        ctx: Ctx,
    ) -> Result<PseudoElement, SelectorError> {
        if !is_colon(s.get(*pos + 1)) {
            // A legacy single-colon pseudo-element (starts_pseudo_element
            // checked the name).
            let name = ident(s.get(*pos + 1)).ok_or(SelectorError::Syntax)?;
            *pos += 2;
            return legacy_pseudo_element(name)
                .map(PseudoElement::Keyword)
                .ok_or(SelectorError::Syntax);
        }
        let v = s.get(*pos + 2).ok_or(SelectorError::Syntax)?;
        *pos += 3;
        match v.kind() {
            Kind::Token(Token::Ident(name)) => {
                if let Some(k) = PseudoElementKeyword::from_name(name) {
                    return Ok(PseudoElement::Keyword(k));
                }
                let lower = name.to_ascii_lowercase();
                if lower.starts_with("-webkit-") {
                    return Ok(PseudoElement::UnknownWebkit(lower.into()));
                }
                Err(SelectorError::UnknownPseudoElement)
            }
            Kind::Function(name, args) => {
                let inner = ctx.deeper()?;
                let all: Vec<_> = args.collect();
                let s = trim(&all);
                if name.eq_ignore_ascii_case("part") {
                    let mut names = Vec::new();
                    for v in s {
                        match v.token() {
                            Token::Ident(n) => names.push((**n).into()),
                            Token::Whitespace => {}
                            _ => return Err(SelectorError::Syntax),
                        }
                    }
                    if names.is_empty() {
                        return Err(SelectorError::Syntax);
                    }
                    Ok(PseudoElement::Part(names))
                } else if name.eq_ignore_ascii_case("slotted") {
                    Ok(PseudoElement::Slotted(self.compound_argument(s, inner)?))
                } else if name.eq_ignore_ascii_case("highlight") {
                    match s {
                        [v] if *v.token() == Token::Delim('*') => {
                            Ok(PseudoElement::Highlight(None))
                        }
                        [v] => match ident(Some(v)) {
                            Some(n) if is_custom_ident(n) => {
                                Ok(PseudoElement::Highlight(Some(n.into())))
                            }
                            _ => Err(SelectorError::Syntax),
                        },
                        _ => Err(SelectorError::Syntax),
                    }
                } else {
                    Err(SelectorError::UnknownPseudoElement)
                }
            }
            _ => Err(SelectorError::Syntax),
        }
    }
}

/// Whether `pc` may follow a pseudo-element (element-backed or not):
/// after an element-backed one, any pseudo-class (css-pseudo-4 section 5);
/// after the others, the logical combination and user action pseudo-classes
/// (selectors-4 3.6.3). No spec at the pinned commit allows more.
fn allowed_after(element_backed: bool, pc: &PseudoClass) -> bool {
    element_backed
        || match pc {
            PseudoClass::Keyword(k) => k.is_user_action(),
            PseudoClass::Is(_) | PseudoClass::Where(_) | PseudoClass::Not(_) => true,
            _ => false,
        }
}

/// Whether pseudo-element `next` may follow `prev` (3.6.4): anything after
/// an element-backed one (css-pseudo-4 section 5); `::marker` after
/// `::before` and `::after` (css-pseudo-4 4.2); `::prefix` and `::suffix`
/// after `::first-letter`, and only there (css-pseudo-4 2.2); a
/// tree-abiding one after `::slotted()` (css-shadow-1).
fn pseudo_element_after(prev: &PseudoElement, next: &PseudoElement) -> bool {
    use PseudoElementKeyword::*;
    if let PseudoElement::Keyword(Prefix | Suffix) = next {
        return matches!(prev, PseudoElement::Keyword(FirstLetter));
    }
    if prev.is_element_backed() {
        return true;
    }
    match prev {
        PseudoElement::Keyword(Before | After) => matches!(next, PseudoElement::Keyword(Marker)),
        PseudoElement::Slotted(_) => next.is_tree_abiding(),
        _ => false,
    }
}
