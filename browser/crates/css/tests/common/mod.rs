//! Shared test helpers: running both tokenizers and comparing results.

#![allow(dead_code)]

pub mod reference;

use optimal_css::{Next, Spanned, StreamTokenizer, Token, TokenError, Tokenizer};
use optimal_wtf8::Wtf8;

/// Every token of `src` from the whole-input tokenizer, as owned tokens, and
/// the errors.
pub fn tokenize_whole(
    src: Wtf8<'_>,
    unicode_ranges: bool,
) -> (Vec<Spanned<'static>>, Vec<TokenError>) {
    let mut t = Tokenizer::new(src);
    let mut out = Vec::new();
    while let Some(s) = t.next_token_with(unicode_ranges) {
        out.push(Spanned {
            token: s.token.into_owned(),
            start: s.start,
            end: s.end,
        });
    }
    (out, t.errors().to_vec())
}

/// Split `src` at the given byte offsets (each a code point boundary, in
/// increasing order) and feed the pieces to a stream tokenizer, asking for
/// tokens after every piece.
pub fn tokenize_chunked(
    src: Wtf8<'_>,
    cuts: &[usize],
    unicode_ranges: bool,
) -> (Vec<Spanned<'static>>, Vec<TokenError>) {
    let mut t = StreamTokenizer::new();
    let mut out = Vec::new();
    let drain = |t: &mut StreamTokenizer, out: &mut Vec<Spanned<'static>>| loop {
        match t.next_token(unicode_ranges) {
            Next::Token(s) => out.push(Spanned {
                token: s.token.into_owned(),
                start: s.start,
                end: s.end,
            }),
            Next::NeedInput => return false,
            Next::End => return true,
        }
    };
    let mut prev = 0;
    for &cut in cuts.iter().chain(std::iter::once(&src.len())) {
        t.push(src.slice(prev, cut));
        prev = cut;
        assert!(!drain(&mut t, &mut out), "end of input before finish()");
    }
    t.finish();
    assert!(drain(&mut t, &mut out), "no end after finish()");
    assert_eq!(t.next_token(unicode_ranges), Next::End, "end is sticky");
    (out, t.errors().to_vec())
}

/// Feed `pieces` to a stream tokenizer one by one, asking for tokens after
/// every piece.
pub fn tokenize_pieces(
    pieces: &[optimal_wtf8::Wtf8Buf],
    unicode_ranges: bool,
) -> (Vec<Spanned<'static>>, Vec<TokenError>) {
    let mut t = StreamTokenizer::new();
    let mut out = Vec::new();
    let drain = |t: &mut StreamTokenizer, out: &mut Vec<Spanned<'static>>| loop {
        match t.next_token(unicode_ranges) {
            Next::Token(s) => out.push(Spanned {
                token: s.token.into_owned(),
                start: s.start,
                end: s.end,
            }),
            Next::NeedInput => return false,
            Next::End => return true,
        }
    };
    for p in pieces {
        t.push(p.as_wtf8());
        assert!(!drain(&mut t, &mut out), "end of input before finish()");
    }
    t.finish();
    assert!(drain(&mut t, &mut out), "no end after finish()");
    (out, t.errors().to_vec())
}

/// Every code point boundary of `src` except 0 and the end.
pub fn all_boundaries(src: Wtf8<'_>) -> Vec<usize> {
    let mut cuts = Vec::new();
    let mut i = 0;
    for cp in src.code_points() {
        i += optimal_wtf8::encoded_len(cp);
        if i < src.len() {
            cuts.push(i);
        }
    }
    cuts
}

/// The code point boundaries of `src` chosen by a seeded generator.
pub fn random_boundaries(src: Wtf8<'_>, seed: u64) -> Vec<usize> {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        // splitmix64
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let p = 1 + next() % 8;
    all_boundaries(src)
        .into_iter()
        .filter(|_| next() % p == 0)
        .collect()
}

/// Check that the spans of `tokens` cover `len` bytes exactly, in order.
pub fn assert_covers(tokens: &[Spanned<'_>], len: usize) {
    let mut at = 0;
    for t in tokens {
        assert_eq!(t.start, at, "gap or overlap before {t:?}");
        assert!(t.end > t.start, "empty token {t:?}");
        at = t.end;
    }
    assert_eq!(at, len, "tokens don't reach the end of input");
}

/// A short name for a token kind, for messages.
pub fn kind(t: &Token<'_>) -> &'static str {
    match t {
        Token::Ident(_) => "ident",
        Token::Function(_) => "function",
        Token::AtKeyword(_) => "at-keyword",
        Token::Hash(..) => "hash",
        Token::String(_) => "string",
        Token::BadString => "bad-string",
        Token::Url(_) => "url",
        Token::BadUrl => "bad-url",
        Token::Delim(_) => "delim",
        Token::Number(_) => "number",
        Token::Percentage { .. } => "percentage",
        Token::Dimension(..) => "dimension",
        Token::UnicodeRange { .. } => "unicode-range",
        Token::Whitespace => "whitespace",
        Token::Cdo => "CDO",
        Token::Cdc => "CDC",
        Token::Colon => "colon",
        Token::Semicolon => "semicolon",
        Token::Comma => "comma",
        Token::OpenSquare => "[",
        Token::CloseSquare => "]",
        Token::OpenParen => "(",
        Token::CloseParen => ")",
        Token::OpenCurly => "{",
        Token::CloseCurly => "}",
        Token::Comment => "comment",
    }
}

pub mod parse_reference;

use optimal_css::{BlockItem, Declaration, Kind, ParseError, Parsed, Rule, RuleId, Values};
use parse_reference::Entry;
use serde_json::{Value, json};

/// The [`parse_reference`] validity rules, for the crate.
pub struct ModelValidity;

impl optimal_css::Validity for ModelValidity {
    fn at_rule(&mut self, p: &Parsed<'_>, rule: &Rule, _nested: bool) -> bool {
        let Rule::At { name, .. } = rule else {
            unreachable!()
        };
        parse_reference::at_valid(p.name(*name))
    }
    fn qualified_rule(&mut self, p: &Parsed<'_>, rule: &Rule, _nested: bool) -> bool {
        let Rule::Qualified { prelude, .. } = rule else {
            unreachable!()
        };
        let first = p
            .values(prelude.clone())
            .next()
            .and_then(|v| match v.token() {
                Token::Ident(s) => Some(s.to_string()),
                _ => None,
            });
        parse_reference::qualified_valid(first.as_deref())
    }
    fn declaration(&mut self, p: &Parsed<'_>, d: &Declaration, _nested: bool) -> bool {
        parse_reference::decl_valid(p.name(d.name))
    }
}

fn cv_json(p: &Parsed<'_>, v: optimal_css::ComponentValue<'_, '_>) -> Value {
    match v.kind() {
        Kind::Function(name, c) => json!(["f", name, cvs_json(p, c), v.is_closed()]),
        Kind::Block(_, c) => json!([
            "b",
            format!("{:?}", v.token()),
            cvs_json(p, c),
            v.is_closed()
        ]),
        Kind::Token(t) => {
            let s = p.tokens.get(v.index());
            json!(["t", format!("{t:?}"), s.start, s.end])
        }
    }
}

fn cvs_json(p: &Parsed<'_>, vs: Values<'_, '_>) -> Value {
    Value::Array(vs.map(|v| cv_json(p, v)).collect())
}

fn decl_json(p: &Parsed<'_>, d: &Declaration) -> Value {
    json!([
        "d",
        p.name(d.name),
        cvs_json(p, p.values(d.value.clone())),
        d.important,
        d.original_text.clone().map(|r| json!([r.start, r.end]))
    ])
}

fn decls_json(p: &Parsed<'_>, ds: &[Declaration]) -> Value {
    Value::Array(ds.iter().map(|d| decl_json(p, d)).collect())
}

/// A rule of the crate's output in the model's JSON shape.
pub fn rule_json(p: &Parsed<'_>, id: RuleId) -> Value {
    match &p.rules[id as usize] {
        Rule::At {
            name,
            prelude,
            block,
        } => json!([
            "at",
            p.name(*name),
            cvs_json(p, p.values(prelude.clone())),
            block.as_ref().map(|b| Value::Array(
                b.iter()
                    .map(|i| match i {
                        BlockItem::Rule(r) => rule_json(p, *r),
                        BlockItem::Declarations(d) => json!(["decls", decls_json(p, d)]),
                    })
                    .collect()
            ))
        ]),
        Rule::Qualified {
            prelude,
            declarations,
            children,
        } => json!([
            "qr",
            cvs_json(p, p.values(prelude.clone())),
            decls_json(p, declarations),
            Value::Array(children.iter().map(|&c| rule_json(p, c)).collect())
        ]),
        Rule::NestedDeclarations(d) => json!(["nd", decls_json(p, d)]),
    }
}

fn syntax_error(e: optimal_css::SyntaxError) -> Value {
    match e {
        optimal_css::SyntaxError::Empty => json!(["error", "empty"]),
        optimal_css::SyntaxError::Invalid => json!(["error", "invalid"]),
        optimal_css::SyntaxError::ExtraInput => json!(["error", "extra-input"]),
    }
}

fn parser_errors(p: &Parsed<'_>) -> Value {
    Value::Array(
        p.errors
            .iter()
            .filter_map(|e| match e {
                ParseError::Parser(k, at) => Some(json!([format!("{k:?}"), at])),
                ParseError::Token(_) => None,
            })
            .collect(),
    )
}

/// Run `entry` on `src` with the crate (validity as in the model): the
/// result and the parser errors, in the model's JSON shape.
pub fn parse_json(src: Wtf8<'_>, entry: Entry) -> (Value, Value) {
    let v = &mut ModelValidity;
    match entry {
        Entry::Stylesheet => {
            let (p, rules) = optimal_css::parse_stylesheet(src, v);
            (
                Value::Array(rules.iter().map(|&r| rule_json(&p, r)).collect()),
                parser_errors(&p),
            )
        }
        Entry::DeclarationList => {
            let (p, ds) = optimal_css::parse_declaration_list(src, v);
            (decls_json(&p, &ds), parser_errors(&p))
        }
        Entry::Rule => {
            let (p, r) = optimal_css::parse_rule(src, v);
            (
                r.map_or_else(syntax_error, |id| rule_json(&p, id)),
                parser_errors(&p),
            )
        }
        Entry::Declaration => {
            let (p, r) = optimal_css::parse_declaration(src, v);
            (
                r.map_or_else(syntax_error, |d| decl_json(&p, &d)),
                parser_errors(&p),
            )
        }
        Entry::ComponentValue => {
            let (p, r) = optimal_css::parse_component_value(src);
            (
                r.map_or_else(syntax_error, |i| cv_json(&p, p.tokens.value(i))),
                parser_errors(&p),
            )
        }
        Entry::ComponentValues => {
            let (p, r) = optimal_css::parse_component_values(src);
            (cvs_json(&p, p.values(r)), parser_errors(&p))
        }
        Entry::CommaSeparated => {
            let (p, groups) = optimal_css::parse_comma_separated_component_values(src);
            (
                Value::Array(
                    groups
                        .into_iter()
                        .map(|g| cvs_json(&p, p.values(g)))
                        .collect(),
                ),
                parser_errors(&p),
            )
        }
    }
}
