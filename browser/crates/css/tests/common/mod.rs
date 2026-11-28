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
