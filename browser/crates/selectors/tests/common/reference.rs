//! A literal model of selector parsing, for differential tests.
//!
//! It is written separately from the crate and differently: it first
//! builds its own tree of component values from the tokenizer's flat
//! tokens, then parses the grammar of selectors-4 section 16 production by
//! production into a plain syntax tree (no restrictions, no namespaces),
//! and only then checks, in a second pass over the whole tree, each
//! validity rule as the specs list them. Its serializer follows cssom-1
//! 5.2 step by step. Name tables are copied from the specs as strings.
//!
//! Only An+B is shared: the argument's text is handed to `optimal_css`,
//! whose An+B parser has its own literal model and suites.

use optimal_css::{Token, Tokenizer, parse_anb, parse_anb_prefix, parse_component_values};
use optimal_wtf8::Wtf8;

// --- Component values -------------------------------------------------

#[derive(Clone, Debug)]
pub enum Node {
    Tok(Token<'static>),
    Func(String, Vec<Item>),
    /// `[`, `(` or `{` block.
    Block(char, Vec<Item>),
}

/// A component value and its source span (comments included).
#[derive(Clone, Debug)]
pub struct Item {
    pub node: Node,
    pub start: usize,
    /// End of its last token (the closer, if it has one).
    pub end: usize,
    /// For functions and blocks: whether the closer was there.
    pub closed: bool,
}

fn owned(t: Token<'_>) -> Token<'static> {
    use std::borrow::Cow::Owned;
    match t {
        Token::Ident(s) => Token::Ident(Owned(s.into_owned())),
        Token::Function(s) => Token::Function(Owned(s.into_owned())),
        Token::AtKeyword(s) => Token::AtKeyword(Owned(s.into_owned())),
        Token::Hash(s, k) => Token::Hash(Owned(s.into_owned()), k),
        Token::String(s) => Token::String(Owned(s.into_owned())),
        Token::Url(s) => Token::Url(Owned(s.into_owned())),
        Token::Dimension(n, s) => Token::Dimension(n, Owned(s.into_owned())),
        Token::BadString => Token::BadString,
        Token::BadUrl => Token::BadUrl,
        Token::Delim(c) => Token::Delim(c),
        Token::Number(n) => Token::Number(n),
        Token::Percentage { value, sign } => Token::Percentage { value, sign },
        Token::UnicodeRange { start, end } => Token::UnicodeRange { start, end },
        Token::Whitespace => Token::Whitespace,
        Token::Cdo => Token::Cdo,
        Token::Cdc => Token::Cdc,
        Token::Colon => Token::Colon,
        Token::Semicolon => Token::Semicolon,
        Token::Comma => Token::Comma,
        Token::OpenSquare => Token::OpenSquare,
        Token::CloseSquare => Token::CloseSquare,
        Token::OpenParen => Token::OpenParen,
        Token::CloseParen => Token::CloseParen,
        Token::OpenCurly => Token::OpenCurly,
        Token::CloseCurly => Token::CloseCurly,
        Token::Comment => Token::Comment,
    }
}

/// css-syntax-3 5.5.7 "consume a list of component values" and 5.5.8 to
/// 5.5.10, recursively.
/// Returns the values, where the last token read ends, and whether the
/// closer was found.
fn tree(
    toks: &[(Token<'static>, usize, usize)],
    i: &mut usize,
    closer: Option<Token<'static>>,
    src_len: usize,
) -> (Vec<Item>, usize, bool) {
    let mut out = Vec::new();
    let mut last_end = None;
    while *i < toks.len() {
        let (t, start, end) = toks[*i].clone();
        *i += 1;
        if Some(&t) == closer.as_ref() {
            return (out, end, true);
        }
        let (open, close) = match &t {
            Token::OpenSquare => (Some('['), Token::CloseSquare),
            Token::OpenParen => (Some('('), Token::CloseParen),
            Token::OpenCurly => (Some('{'), Token::CloseCurly),
            Token::Function(_) => (None, Token::CloseParen),
            _ => {
                out.push(Item {
                    node: Node::Tok(t),
                    start,
                    end,
                    closed: true,
                });
                last_end = Some(end);
                continue;
            }
        };
        let (inner, inner_end, closed) = tree(toks, i, Some(close), src_len);
        let end = if closed {
            inner_end
        } else {
            inner_end.max(end)
        };
        last_end = Some(end);
        let node = match (open, t) {
            (Some(c), _) => Node::Block(c, inner),
            (None, Token::Function(name)) => Node::Func(name.into_owned(), inner),
            _ => unreachable!(),
        };
        out.push(Item {
            node,
            start,
            end,
            closed,
        });
    }
    let _ = src_len;
    (out, last_end.unwrap_or(0), false)
}

pub fn component_values(src: &str) -> Vec<Item> {
    let mut t = Tokenizer::new(Wtf8::from_str(src));
    let toks: Vec<_> = std::iter::from_fn(|| t.next_token())
        .filter(|s| s.token != Token::Comment)
        .map(|s| (owned(s.token), s.start, s.end))
        .collect();
    tree(&toks, &mut 0, None, src.len()).0
}

/// The children of a function or block.
fn children(item: &Item) -> &[Item] {
    match &item.node {
        Node::Func(_, c) | Node::Block(_, c) => c,
        Node::Tok(_) => &[],
    }
}

// --- Syntax tree --------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Prefix {
    Any,
    Empty,
    Named(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum S {
    Type(Option<Prefix>, String),
    Universal(Option<Prefix>),
    Id(String),
    Class(String),
    Attr {
        prefix: Option<Prefix>,
        name: String,
        test: Option<(String, String)>,
        modifier: Option<char>,
    },
    Pc(Pc),
    Nest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pc {
    /// Lowercased name.
    pub name: String,
    pub arg: Option<Arg>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// Forgiving items: parsed, or not, each with its raw text.
    Forgiving(Vec<(Option<Complex>, String)>),
    List(Vec<Complex>),
    Relative(Vec<(Option<char>, Complex)>),
    Nth(String, Option<Vec<Complex>>),
    Strings(Vec<String>),
    Ident(String),
    Compound(Vec<S>),
    Idents(Vec<String>),
    Highlight(Option<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pe {
    pub name: String,
    pub arg: Option<Arg>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub compound: Vec<S>,
    pub pseudo: Vec<(Pe, Vec<Pc>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Complex {
    pub units: Vec<Unit>,
    /// Between units: ' ', '>', '+', '~'.
    pub combinators: Vec<char>,
}

// Name tables, from the specs' definitions.
const PSEUDO_CLASSES: &[&str] = &[
    "active",
    "any-link",
    "autofill",
    "buffering",
    "checked",
    "default",
    "defined",
    "disabled",
    "empty",
    "enabled",
    "first-child",
    "first-of-type",
    "focus",
    "focus-visible",
    "focus-within",
    "fullscreen",
    "has-slotted",
    "host",
    "hover",
    "in-range",
    "indeterminate",
    "invalid",
    "last-child",
    "last-of-type",
    "link",
    "modal",
    "muted",
    "only-child",
    "only-of-type",
    "open",
    "optional",
    "out-of-range",
    "paused",
    "picture-in-picture",
    "placeholder-shown",
    "playing",
    "popover-open",
    "read-only",
    "read-write",
    "required",
    "root",
    "scope",
    "seeking",
    "stalled",
    "target",
    "unchecked",
    "user-invalid",
    "user-valid",
    "valid",
    "visited",
    "volume-locked",
];
const FUNCTIONAL_PSEUDO_CLASSES: &[&str] = &[
    "is",
    "where",
    "not",
    "has",
    "nth-child",
    "nth-last-child",
    "nth-of-type",
    "nth-last-of-type",
    "lang",
    "dir",
    "state",
    "host",
    "host-context",
];
const PSEUDO_ELEMENTS: &[&str] = &[
    "after",
    "before",
    "details-content",
    "file-selector-button",
    "first-letter",
    "first-line",
    "grammar-error",
    "interest-button",
    "marker",
    "placeholder",
    "prefix",
    "search-text",
    "selection",
    "spelling-error",
    "suffix",
    "target-text",
];
const LEGACY: &[&str] = &["before", "after", "first-line", "first-letter"];

fn ws(n: Option<&Item>) -> bool {
    matches!(n.map(|i| &i.node), Some(Node::Tok(Token::Whitespace)))
}
fn tok(n: Option<&Item>) -> Option<&Token<'static>> {
    match n.map(|i| &i.node) {
        Some(Node::Tok(t)) => Some(t),
        _ => None,
    }
}
fn ident_of(n: Option<&Item>) -> Option<String> {
    match tok(n) {
        Some(Token::Ident(s)) => Some(s.to_string()),
        _ => None,
    }
}
fn delim_is(n: Option<&Item>, c: char) -> bool {
    tok(n) == Some(&Token::Delim(c))
}

fn trim(mut s: &[Item]) -> &[Item] {
    while !s.is_empty() && ws(s.first()) {
        s = &s[1..];
    }
    while !s.is_empty() && ws(s.last()) {
        s = &s[..s.len() - 1];
    }
    s
}

fn split(s: &[Item]) -> Vec<&[Item]> {
    s.split(|i| matches!(i.node, Node::Tok(Token::Comma)))
        .map(trim)
        .collect()
}

/// `<any-value>`, recursively.
fn any_value(items: &[Item]) -> bool {
    items.iter().all(|n| match &n.node {
        Node::Tok(t) => !matches!(
            t,
            Token::BadString
                | Token::BadUrl
                | Token::CloseParen
                | Token::CloseSquare
                | Token::CloseCurly
        ),
        Node::Func(_, c) | Node::Block(_, c) => any_value(c),
    })
}

pub struct Ref<'s> {
    pub src: &'s str,
}

type P<T> = Option<T>;

impl<'s> Ref<'s> {
    /// The item's text, with closers for what the end of input left open.
    fn raw(&self, s: &[Item], src: &str) -> String {
        let (Some(a), Some(b)) = (s.first(), s.last()) else {
            return String::new();
        };
        let mut out = src[a.start..b.end].to_owned();
        let mut closers = String::new();
        let mut cur = Some(b);
        while let Some(it) = cur {
            if it.closed {
                break;
            }
            closers.insert(
                0,
                match &it.node {
                    Node::Block('[', _) => ']',
                    Node::Block('{', _) => '}',
                    _ => ')',
                },
            );
            cur = children(it).last();
        }
        out += &closers;
        out
    }

    /// <complex-selector-list> etc.: items, each a complex selector.
    pub fn list(&self, s: &[Item], src: &str) -> P<Vec<Complex>> {
        split(s).into_iter().map(|i| self.complex(i, src)).collect()
    }

    /// <complex-selector> = <unit> [ <combinator>? <unit> ]*, whitespace
    /// required when the combinator is left out.
    pub fn complex(&self, s: &[Item], src: &str) -> P<Complex> {
        let mut i = 0;
        let mut units = vec![self.unit(s, &mut i, src)?];
        let mut combinators = Vec::new();
        while i < s.len() {
            let mut had_ws = false;
            while ws(s.get(i)) {
                i += 1;
                had_ws = true;
            }
            let c = match tok(s.get(i)) {
                Some(Token::Delim(c @ ('>' | '+' | '~'))) => {
                    i += 1;
                    while ws(s.get(i)) {
                        i += 1;
                    }
                    *c
                }
                _ if had_ws => ' ',
                _ => return None,
            };
            combinators.push(c);
            units.push(self.unit(s, &mut i, src)?);
        }
        Some(Complex { units, combinators })
    }

    /// <complex-selector-unit> = [ <compound-selector>? <pseudo-compound-selector>* ]!
    fn unit(&self, s: &[Item], i: &mut usize, src: &str) -> P<Unit> {
        let mut compound = Vec::new();
        // <type-selector>? : <wq-name> | <ns-prefix>? '*'
        let (prefix, at) =
            if let (Some(p), true) = (ident_of(s.get(*i)), delim_is(s.get(*i + 1), '|')) {
                (Some(Prefix::Named(p)), *i + 2)
            } else if delim_is(s.get(*i), '*') && delim_is(s.get(*i + 1), '|') {
                (Some(Prefix::Any), *i + 2)
            } else if delim_is(s.get(*i), '|') {
                (Some(Prefix::Empty), *i + 1)
            } else {
                (None, *i)
            };
        if let Some(n) = ident_of(s.get(at)) {
            compound.push(S::Type(prefix, n));
            *i = at + 1;
        } else if delim_is(s.get(at), '*') {
            compound.push(S::Universal(prefix));
            *i = at + 1;
        } else if prefix.is_some() {
            return None;
        }
        // <subclass-selector>* (and `&`)
        loop {
            let here = s.get(*i);
            match here.map(|x| &x.node) {
                Some(Node::Tok(Token::Hash(v, optimal_css::HashKind::Id))) => {
                    compound.push(S::Id(v.to_string()));
                    *i += 1;
                }
                Some(Node::Tok(Token::Delim('.'))) => {
                    compound.push(S::Class(ident_of(s.get(*i + 1))?));
                    *i += 2;
                }
                Some(Node::Tok(Token::Delim('&'))) => {
                    compound.push(S::Nest);
                    *i += 1;
                }
                Some(Node::Block('[', inner)) => {
                    compound.push(self.attr(inner)?);
                    *i += 1;
                }
                Some(Node::Tok(Token::Colon)) => {
                    if tok(s.get(*i + 1)) == Some(&Token::Colon)
                        || ident_of(s.get(*i + 1))
                            .is_some_and(|n| LEGACY.iter().any(|l| l.eq_ignore_ascii_case(&n)))
                    {
                        break;
                    }
                    compound.push(S::Pc(self.pc(s, i, src)?));
                }
                _ => break,
            }
        }
        // <pseudo-compound-selector>* = <pseudo-element-selector> <pseudo-class-selector>*
        let mut pseudo = Vec::new();
        while tok(s.get(*i)) == Some(&Token::Colon) {
            let pe = if tok(s.get(*i + 1)) == Some(&Token::Colon) {
                *i += 2;
                self.pe(s, i, src)?
            } else {
                let n = ident_of(s.get(*i + 1))?;
                if !LEGACY.iter().any(|l| l.eq_ignore_ascii_case(&n)) {
                    return None;
                }
                *i += 2;
                Pe {
                    name: n.to_ascii_lowercase(),
                    arg: None,
                }
            };
            let mut classes = Vec::new();
            while tok(s.get(*i)) == Some(&Token::Colon)
                && tok(s.get(*i + 1)) != Some(&Token::Colon)
                && !ident_of(s.get(*i + 1))
                    .is_some_and(|n| LEGACY.iter().any(|l| l.eq_ignore_ascii_case(&n)))
            {
                classes.push(self.pc(s, i, src)?);
            }
            pseudo.push((pe, classes));
        }
        if compound.is_empty() && pseudo.is_empty() {
            return None;
        }
        // The unit ends at whitespace, a combinator or the end.
        match tok(s.get(*i)) {
            None if *i >= s.len() => {}
            Some(Token::Whitespace | Token::Delim('>' | '+' | '~')) => {}
            _ => return None,
        }
        Some(Unit { compound, pseudo })
    }

    /// <attribute-selector>
    fn attr(&self, inner: &[Item]) -> P<S> {
        let s = trim(inner);
        let mut i = 0;
        // <wq-name> = <ns-prefix>? <ident-token>, where '|' '=' is a matcher.
        let not_matcher = |k: usize| delim_is(s.get(k), '|') && !delim_is(s.get(k + 1), '=');
        let prefix = if ident_of(s.first()).is_some() && not_matcher(1) {
            i = 2;
            Some(Prefix::Named(ident_of(s.first())?))
        } else if delim_is(s.first(), '*') && not_matcher(1) {
            i = 2;
            Some(Prefix::Any)
        } else if not_matcher(0) {
            i = 1;
            Some(Prefix::Empty)
        } else {
            None
        };
        let name = ident_of(s.get(i))?;
        i += 1;
        while ws(s.get(i)) {
            i += 1;
        }
        if i == s.len() {
            return Some(S::Attr {
                prefix,
                name,
                test: None,
                modifier: None,
            });
        }
        let op = if delim_is(s.get(i), '=') {
            i += 1;
            "=".to_owned()
        } else {
            let c = match tok(s.get(i)) {
                Some(Token::Delim(c @ ('~' | '|' | '^' | '$' | '*'))) => *c,
                _ => return None,
            };
            if !delim_is(s.get(i + 1), '=') {
                return None;
            }
            i += 2;
            format!("{c}=")
        };
        while ws(s.get(i)) {
            i += 1;
        }
        let value = match tok(s.get(i)) {
            Some(Token::Ident(v) | Token::String(v)) => v.to_string(),
            _ => return None,
        };
        i += 1;
        while ws(s.get(i)) {
            i += 1;
        }
        let mut modifier = None;
        if let Some(m) = ident_of(s.get(i)) {
            modifier = match m.to_ascii_lowercase().as_str() {
                "i" => Some('i'),
                "s" => Some('s'),
                _ => return None,
            };
            i += 1;
        }
        while ws(s.get(i)) {
            i += 1;
        }
        (i == s.len()).then_some(S::Attr {
            prefix,
            name,
            test: Some((op, value)),
            modifier,
        })
    }

    /// <pseudo-class-selector> = ':' <ident-token> | ':' <function-token> <any-value> ')'
    fn pc(&self, s: &[Item], i: &mut usize, src: &str) -> P<Pc> {
        let item = s.get(*i + 1)?;
        *i += 2;
        match &item.node {
            Node::Tok(Token::Ident(n)) => {
                let lower = n.to_ascii_lowercase();
                let name = if lower == "-webkit-autofill" {
                    "autofill".to_owned()
                } else {
                    lower
                };
                PSEUDO_CLASSES
                    .contains(&name.as_str())
                    .then_some(Pc { name, arg: None })
            }
            Node::Func(n, _) => {
                let name = n.to_ascii_lowercase();
                if !FUNCTIONAL_PSEUDO_CLASSES.contains(&name.as_str()) {
                    return None;
                }
                let args = children(item);
                let a = trim(args);
                let text = src;
                let arg = match name.as_str() {
                    "is" | "where" => {
                        if !any_value(args) {
                            return None;
                        }
                        Arg::Forgiving(
                            split(a)
                                .into_iter()
                                .map(|it| (self.complex(it, text), self.raw(it, text)))
                                .collect(),
                        )
                    }
                    "not" => Arg::List(self.list(a, text)?),
                    "has" => Arg::Relative(
                        split(a)
                            .into_iter()
                            .map(|it| {
                                let (c, rest) = match tok(it.first()) {
                                    Some(Token::Delim(c @ ('>' | '+' | '~'))) => {
                                        (Some(*c), trim(&it[1..]))
                                    }
                                    _ => (None, it),
                                };
                                Some((c, self.complex(rest, text)?))
                            })
                            .collect::<Option<_>>()?,
                    ),
                    "nth-child" | "nth-last-child" => {
                        // An+B (from its text), then optionally `of` and a
                        // selector list, found by where the An+B ended.
                        let base = args.first().map_or(0, |x| x.start);
                        let arg_text = &src[base..args.last().map_or(base, |x| x.end)];
                        let (p, range) = parse_component_values(Wtf8::from_str(arg_text));
                        let (anb, rest) = parse_anb_prefix(p.values(range))?;
                        let r = match rest.clone().next() {
                            Some(v) => {
                                let at = base + p.tokens.get(v.index()).start;
                                let k = args.iter().position(|x| x.start == at)?;
                                trim(&args[k..])
                            }
                            None => &[],
                        };
                        let of = if r.is_empty() {
                            None
                        } else if ident_of(r.first()).is_some_and(|w| w.eq_ignore_ascii_case("of"))
                        {
                            Some(self.list(trim(&r[1..]), text)?)
                        } else {
                            return None;
                        };
                        Arg::Nth(anb.to_string(), of)
                    }
                    "nth-of-type" | "nth-last-of-type" => {
                        let base = args.first().map_or(0, |x| x.start);
                        let arg_text = &src[base..args.last().map_or(base, |x| x.end)];
                        let (p, range) = parse_component_values(Wtf8::from_str(arg_text));
                        Arg::Nth(parse_anb(p.values(range))?.to_string(), None)
                    }
                    "lang" => Arg::Strings(
                        split(a)
                            .into_iter()
                            .map(|it| match it {
                                [one] => match tok(Some(one)) {
                                    Some(Token::Ident(v) | Token::String(v)) => Some(v.to_string()),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect::<Option<_>>()?,
                    ),
                    "dir" | "state" => match a {
                        [one] => Arg::Ident(ident_of(Some(one))?),
                        _ => return None,
                    },
                    _ => {
                        // host, host-context: <compound-selector>
                        let c = self.complex(a, text)?;
                        if c.units.len() != 1 || !c.units[0].pseudo.is_empty() {
                            return None;
                        }
                        Arg::Compound(c.units[0].compound.clone())
                    }
                };
                Some(Pc {
                    name,
                    arg: Some(arg),
                })
            }
            _ => None,
        }
    }

    fn pe(&self, s: &[Item], i: &mut usize, src: &str) -> P<Pe> {
        let item = s.get(*i)?;
        *i += 1;
        match &item.node {
            Node::Tok(Token::Ident(n)) => {
                let name = n.to_ascii_lowercase();
                (PSEUDO_ELEMENTS.contains(&name.as_str()) || name.starts_with("-webkit-"))
                    .then_some(Pe { name, arg: None })
            }
            Node::Func(n, _) => {
                let name = n.to_ascii_lowercase();
                let a = trim(children(item));
                let text = src;
                let arg = match name.as_str() {
                    "part" => {
                        let mut names = Vec::new();
                        for it in a {
                            match &it.node {
                                Node::Tok(Token::Ident(v)) => names.push(v.to_string()),
                                Node::Tok(Token::Whitespace) => {}
                                _ => return None,
                            }
                        }
                        if names.is_empty() {
                            return None;
                        }
                        Arg::Idents(names)
                    }
                    "slotted" => {
                        let c = self.complex(a, text)?;
                        if c.units.len() != 1 || !c.units[0].pseudo.is_empty() {
                            return None;
                        }
                        Arg::Compound(c.units[0].compound.clone())
                    }
                    "highlight" => match a {
                        [one] if delim_is(Some(one), '*') => Arg::Highlight(None),
                        [one] => {
                            let n = ident_of(Some(one))?;
                            let lower = n.to_ascii_lowercase();
                            // <custom-ident>: not a CSS-wide keyword or `default`.
                            if [
                                "initial",
                                "inherit",
                                "unset",
                                "revert",
                                "revert-layer",
                                "default",
                            ]
                            .contains(&lower.as_str())
                            {
                                return None;
                            }
                            Arg::Highlight(Some(n))
                        }
                        _ => return None,
                    },
                    _ => return None,
                };
                Some(Pe {
                    name,
                    arg: Some(arg),
                })
            }
            _ => None,
        }
    }
}

// --- Validity -----------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Where {
    /// Only a compound selector.
    compound: bool,
    /// No pseudo-elements.
    no_pe: bool,
    in_has: bool,
    /// Inside a logical combination after a pseudo-element: Some(element-backed?).
    after_pe: Option<bool>,
}

fn element_backed(pe: &Pe) -> bool {
    matches!(
        pe.name.as_str(),
        "part" | "file-selector-button" | "details-content"
    )
}
fn tree_abiding(pe: &Pe) -> bool {
    element_backed(pe)
        || matches!(
            pe.name.as_str(),
            "before" | "after" | "marker" | "placeholder" | "interest-button"
        )
}

/// selectors-4 3.6.3 and css-pseudo-4 section 5.
fn pc_after_pe(eb: bool, pc: &Pc) -> bool {
    eb || matches!(
        pc.name.as_str(),
        "hover" | "active" | "focus" | "focus-visible" | "focus-within" | "is" | "where" | "not"
    )
}

/// 3.6.4 and the specs that define sub-pseudo-elements.
fn pe_after_pe(prev: &Pe, next: &Pe) -> bool {
    if next.name == "prefix" || next.name == "suffix" {
        return prev.name == "first-letter";
    }
    element_backed(prev)
        || (matches!(prev.name.as_str(), "before" | "after") && next.name == "marker")
        || (prev.name == "slotted" && tree_abiding(next))
}

pub struct Checker<'n> {
    pub prefixes: &'n [(&'n str, &'n str)],
    pub default: Option<&'n str>,
}

impl Checker<'_> {
    fn prefix_ok(&self, p: &Option<Prefix>) -> bool {
        match p {
            Some(Prefix::Named(n)) => self.prefixes.iter().any(|(k, _)| k == n),
            _ => true,
        }
    }

    fn complex(&self, c: &Complex, w: Where) -> bool {
        if w.compound && c.units.len() != 1 {
            return false;
        }
        // 3.6.5: nothing after a unit with a pseudo-element.
        for u in &c.units[..c.units.len() - 1] {
            if !u.pseudo.is_empty() {
                return false;
            }
        }
        c.units.iter().all(|u| self.unit(u, w))
    }

    fn unit(&self, u: &Unit, w: Where) -> bool {
        if let Some(eb) = w.after_pe {
            // Only allowed pseudo-classes in this compound.
            if !u
                .compound
                .iter()
                .all(|s| matches!(s, S::Pc(pc) if pc_after_pe(eb, pc)))
            {
                return false;
            }
        }
        if !u.compound.iter().all(|s| self.simple(s, w)) {
            return false;
        }
        if !u.pseudo.is_empty() && (w.no_pe || w.compound) {
            return false;
        }
        for (k, (pe, pcs)) in u.pseudo.iter().enumerate() {
            let ok = if k == 0 {
                pe.name != "prefix" && pe.name != "suffix"
            } else {
                pe_after_pe(&u.pseudo[k - 1].0, pe)
            };
            if !ok || !self.pe(pe, w) {
                return false;
            }
            let eb = element_backed(pe);
            for pc in pcs {
                if !pc_after_pe(eb, pc)
                    || !self.pc(
                        pc,
                        Where {
                            after_pe: Some(eb),
                            ..w
                        },
                    )
                {
                    return false;
                }
            }
        }
        true
    }

    fn simple(&self, s: &S, w: Where) -> bool {
        match s {
            S::Type(p, _) | S::Universal(p) => self.prefix_ok(p),
            S::Attr { prefix, .. } => self.prefix_ok(prefix),
            S::Pc(pc) => self.pc(pc, w),
            _ => true,
        }
    }

    fn compound_arg(&self, c: &[S], w: Where) -> bool {
        let inner = Where {
            compound: true,
            no_pe: true,
            in_has: w.in_has,
            after_pe: None,
        };
        c.iter().all(|s| self.simple(s, inner))
    }

    fn pc(&self, pc: &Pc, w: Where) -> bool {
        let logical = Where {
            no_pe: true,
            compound: w.compound || w.after_pe.is_some(),
            ..w
        };
        let fresh = Where {
            no_pe: true,
            compound: false,
            after_pe: None,
            in_has: w.in_has,
        };
        match (&pc.name[..], &pc.arg) {
            ("is" | "where", _) => true,
            ("not", Some(Arg::List(l))) => l.iter().all(|c| self.complex(c, logical)),
            ("has", Some(Arg::Relative(l))) => {
                !w.in_has
                    && l.iter().all(|(_, c)| {
                        self.complex(
                            c,
                            Where {
                                in_has: true,
                                ..fresh
                            },
                        )
                    })
            }
            (_, Some(Arg::Nth(_, Some(l)))) => l.iter().all(|c| self.complex(c, fresh)),
            ("host" | "host-context", Some(Arg::Compound(c))) => self.compound_arg(c, w),
            _ => true,
        }
    }

    fn pe(&self, pe: &Pe, w: Where) -> bool {
        match &pe.arg {
            Some(Arg::Compound(c)) => self.compound_arg(c, w),
            _ => true,
        }
    }

    /// A forgiving item is kept as a selector only if valid where it is.
    fn forgiving_valid(&self, c: &Complex, w: Where) -> bool {
        self.complex(
            c,
            Where {
                no_pe: true,
                compound: w.compound || w.after_pe.is_some(),
                ..w
            },
        )
    }

    /// Validity of `:is()`/`:where()` items, recorded for serialization:
    /// returns the list with invalid items turned into raw text.
    fn settle(&self, list: &mut [Complex], w: Where) {
        for c in list {
            self.settle_complex(c, w);
        }
    }

    fn settle_complex(&self, c: &mut Complex, w: Where) {
        let last = c.units.len();
        for (k, u) in c.units.iter_mut().enumerate() {
            let _ = k == last;
            for s in &mut u.compound {
                if let S::Pc(pc) = s {
                    self.settle_pc(pc, w);
                }
            }
            for (pe, pcs) in &mut u.pseudo {
                if let Some(Arg::Compound(c)) = &mut pe.arg {
                    self.settle_compound(c, w);
                }
                let eb = element_backed(pe);
                for pc in pcs {
                    self.settle_pc(
                        pc,
                        Where {
                            after_pe: Some(eb),
                            ..w
                        },
                    );
                }
            }
        }
    }

    fn settle_pc(&self, pc: &mut Pc, w: Where) {
        let logical = Where {
            no_pe: true,
            compound: w.compound || w.after_pe.is_some(),
            ..w
        };
        let fresh = Where {
            no_pe: true,
            compound: false,
            after_pe: None,
            in_has: w.in_has,
        };
        match &mut pc.arg {
            Some(Arg::Forgiving(items)) => {
                for (c, _) in items.iter_mut() {
                    if let Some(sel) = c {
                        if self.forgiving_valid(sel, w) {
                            self.settle_complex(sel, logical);
                        } else {
                            *c = None;
                        }
                    }
                }
            }
            Some(Arg::List(l)) => self.settle(l, logical),
            Some(Arg::Relative(l)) => {
                for (_, c) in l {
                    self.settle_complex(
                        c,
                        Where {
                            in_has: true,
                            ..fresh
                        },
                    );
                }
            }
            Some(Arg::Nth(_, Some(l))) => self.settle(l, fresh),
            Some(Arg::Compound(c)) => self.settle_compound(c, w),
            _ => {}
        }
    }

    /// A <compound-selector> argument (of :host(), :host-context(),
    /// ::slotted()): only compounds, no pseudo-elements, anywhere in it.
    fn settle_compound(&self, c: &mut [S], w: Where) {
        let inner = Where {
            compound: true,
            no_pe: true,
            in_has: w.in_has,
            after_pe: None,
        };
        for s in c {
            if let S::Pc(pc) = s {
                self.settle_pc(pc, inner);
            }
        }
    }

    /// The whole selector list: valid or not, and (if valid) its items with
    /// forgiving arguments settled.
    pub fn check(&self, mut list: Vec<Complex>) -> Option<Vec<Complex>> {
        let w = Where::default();
        self.settle(&mut list, w);
        list.iter().all(|c| self.complex(c, w)).then_some(list)
    }

    pub fn check_relative(
        &self,
        mut list: Vec<(Option<char>, Complex)>,
    ) -> Option<Vec<(Option<char>, Complex)>> {
        let w = Where::default();
        for (_, c) in &mut list {
            self.settle_complex(c, w);
        }
        list.iter().all(|(_, c)| self.complex(c, w)).then_some(list)
    }
}

// --- Serialization (cssom-1 5.2) ----------------------------------------

pub fn ident(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (k, &c) in cs.iter().enumerate() {
        let hex = |c: char| format!("\\{:x} ", c as u32);
        if c == '\0' {
            out.push('\u{FFFD}');
        } else if ('\u{1}'..='\u{1f}').contains(&c) || c == '\u{7f}' {
            out += &hex(c);
        } else if k == 0 && c.is_ascii_digit() {
            out += &hex(c);
        } else if k == 1 && c.is_ascii_digit() && cs[0] == '-' {
            out += &hex(c);
        } else if k == 0 && c == '-' && cs.len() == 1 {
            out += "\\-";
        } else if c as u32 >= 0x80 || c == '-' || c == '_' || c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

pub fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\0' {
            out.push('\u{FFFD}');
        } else if ('\u{1}'..='\u{1f}').contains(&c) || c == '\u{7f}' {
            out += &format!("\\{:x} ", c as u32);
        } else if c == '"' || c == '\\' {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out + "\""
}

impl Checker<'_> {
    fn url(&self, p: &str) -> &str {
        self.prefixes
            .iter()
            .rev()
            .find(|(k, _)| *k == p)
            .map(|(_, v)| *v)
            .unwrap()
    }

    /// The text of a type/universal selector's namespace prefix.
    fn element_prefix(&self, p: &Option<Prefix>) -> String {
        match p {
            None => String::new(),
            Some(Prefix::Any) => {
                if self.default.is_some() {
                    "*|".into()
                } else {
                    String::new()
                }
            }
            Some(Prefix::Empty) => "|".into(),
            Some(Prefix::Named(n)) => {
                if Some(self.url(n)) == self.default {
                    String::new()
                } else {
                    format!("{}|", ident(n))
                }
            }
        }
    }

    pub fn ser_list(&self, l: &[Complex]) -> String {
        l.iter()
            .map(|c| self.ser_complex(c))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn ser_complex(&self, c: &Complex) -> String {
        let mut out = self.ser_unit(&c.units[0]);
        for (k, comb) in c.combinators.iter().enumerate() {
            out += &match comb {
                ' ' => " ".to_owned(),
                c => format!(" {c} "),
            };
            out += &self.ser_unit(&c.units[k + 1]);
        }
        out
    }

    fn ser_compound(&self, c: &[S], alone_ok: bool) -> String {
        let mut out = String::new();
        for s in c {
            if let S::Universal(p) = s
                && self.element_prefix(p).is_empty()
                && !(alone_ok && c.len() == 1)
            {
                continue;
            }
            out += &self.ser_simple(s);
        }
        out
    }

    fn ser_unit(&self, u: &Unit) -> String {
        let mut out = self.ser_compound(&u.compound, u.pseudo.is_empty());
        for (pe, pcs) in &u.pseudo {
            out += "::";
            out += &match (&pe.name[..], &pe.arg) {
                (_, None) => ident(&pe.name),
                ("part", Some(Arg::Idents(n))) => format!(
                    "part({})",
                    n.iter().map(|x| ident(x)).collect::<Vec<_>>().join(" ")
                ),
                ("slotted", Some(Arg::Compound(c))) => {
                    format!("slotted({})", self.ser_compound(c, true))
                }
                ("highlight", Some(Arg::Highlight(None))) => "highlight(*)".into(),
                ("highlight", Some(Arg::Highlight(Some(n)))) => format!("highlight({})", ident(n)),
                other => panic!("{other:?}"),
            };
            for pc in pcs {
                out += &self.ser_pc(pc);
            }
        }
        out
    }

    fn ser_simple(&self, s: &S) -> String {
        match s {
            S::Type(p, n) => self.element_prefix(p) + &ident(n),
            S::Universal(p) => self.element_prefix(p) + "*",
            S::Id(v) => format!("#{}", ident(v)),
            S::Class(v) => format!(".{}", ident(v)),
            S::Nest => "&".into(),
            S::Pc(pc) => self.ser_pc(pc),
            S::Attr {
                prefix,
                name,
                test,
                modifier,
            } => {
                let mut out = String::from("[");
                match prefix {
                    Some(Prefix::Any) => out += "*|",
                    Some(Prefix::Named(n)) => out += &format!("{}|", ident(n)),
                    _ => {}
                }
                out += &ident(name);
                if let Some((op, v)) = test {
                    out += op;
                    out += &string(v);
                }
                if let Some(m) = modifier {
                    out.push(' ');
                    out.push(*m);
                }
                out + "]"
            }
        }
    }

    fn ser_pc(&self, pc: &Pc) -> String {
        let args = match &pc.arg {
            None => return format!(":{}", pc.name),
            Some(Arg::Forgiving(items)) => items
                .iter()
                .map(|(c, raw)| match c {
                    Some(c) => self.ser_complex(c),
                    None => raw.clone(),
                })
                .collect::<Vec<_>>()
                .join(", "),
            Some(Arg::List(l)) => self.ser_list(l),
            Some(Arg::Relative(l)) => l
                .iter()
                .map(|(comb, c)| match comb {
                    Some(k) => format!("{k} {}", self.ser_complex(c)),
                    None => self.ser_complex(c),
                })
                .collect::<Vec<_>>()
                .join(", "),
            Some(Arg::Nth(anb, of)) => match of {
                Some(l) => format!("{anb} of {}", self.ser_list(l)),
                None => anb.clone(),
            },
            Some(Arg::Strings(v)) => v.iter().map(|x| string(x)).collect::<Vec<_>>().join(", "),
            Some(Arg::Ident(v)) => ident(v),
            Some(Arg::Compound(c)) => self.ser_compound(c, true),
            other => panic!("{other:?}"),
        };
        format!(":{}({args})", pc.name)
    }
}

/// The model's answer for a selector list: its serialization, or `None`.
pub fn selector(src: &str, prefixes: &[(&str, &str)], default: Option<&str>) -> Option<String> {
    let items = component_values(src);
    let r = Ref { src };
    let list = r.list(trim(&items), src)?;
    let c = Checker { prefixes, default };
    let list = c.check(list)?;
    Some(c.ser_list(&list))
}
