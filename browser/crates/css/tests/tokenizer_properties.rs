//! Property tests for the tokenizer, on random inputs built from the pieces
//! that matter to css-syntax-3 (escapes, comment and URL openers, quotes,
//! signs, exponents, CR LF, NUL, lone surrogates, non-ASCII ident code
//! points):
//!
//! - the whole-input tokenizer gives exactly what the literal model in
//!   `common/reference.rs` gives: tokens, values (compared through `Debug`,
//!   so `-0.0` and `0.0` differ), offsets and errors, with unicode ranges
//!   both off and on;
//! - token spans cover the input exactly, in order;
//! - the stream tokenizer gives the same as the whole-input one for every
//!   random split.
//!
//! `PROPTEST_CASES` sets the number of cases (CI's deep run uses many more).
//! The hostile-input timing test runs when `CSS_SCALING=1` (release builds).

mod common;

use common::{assert_covers, reference, tokenize_chunked, tokenize_pieces, tokenize_whole};
use optimal_wtf8::{Wtf8, Wtf8Buf};
use proptest::prelude::*;

const PIECES: &[&str] = &[
    "a", "u", "U", "url", "URL", "uRl", "e", "E", "f", "x", "-", "--", "+", ".", "0", "1", "9",
    "12", "%", "#", "@", "\\", "\\\n", "\\41", "\\0", "\\d800", "\\110000", "\\10FFFF", "\\ ",
    "/*", "*/", "*", "/", "(", ")", "[", "]", "{", "}", ",", ":", ";", "\"", "'", "<!--", "-->",
    "<", "!", ">", " ", "\t", "\n", "\r", "\r\n", "\u{c}", "\0", "\u{1}", "\u{7f}", "\u{b}", "?",
    "??", "é", "·", "\u{200d}", "\u{2010}", "\u{fffd}", "😀", "_",
];

/// A random input as UTF-16 code units: pieces, plus lone surrogates (only
/// possible in WTF-8).
fn input_units() -> impl Strategy<Value = Vec<u16>> {
    let piece = prop_oneof![
        8 => (0..PIECES.len()).prop_map(|i| PIECES[i].encode_utf16().collect::<Vec<u16>>()),
        1 => prop_oneof![Just(vec![0xD800u16]), Just(vec![0xDC00u16]), Just(vec![0xDBFFu16])],
    ];
    prop::collection::vec(piece, 0..40).prop_map(|ps| ps.concat())
}

fn input() -> impl Strategy<Value = Wtf8Buf> {
    input_units().prop_map(|u| Wtf8Buf::from_utf16(&u))
}

fn debug<T: std::fmt::Debug>(t: &T) -> String {
    format!("{t:?}")
}

proptest! {
    #[test]
    fn matches_the_literal_model(src in input(), ur in any::<bool>()) {
        let w = src.as_wtf8();
        let ours = tokenize_whole(w, ur);
        let model = reference::tokenize(w, ur);
        prop_assert_eq!(debug(&ours), debug(&model), "input {:?}", w);
        assert_covers(&ours.0, w.len());
    }

    #[test]
    fn chunking_changes_nothing(
        units in input_units(),
        ur in any::<bool>(),
        picks in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
    ) {
        // Cuts are between UTF-16 code units, so a surrogate pair can be
        // split into two chunks (as `document.write` can do); the stream
        // tokenizer must join it.
        let src = Wtf8Buf::from_utf16(&units);
        let w = src.as_wtf8();
        let whole = tokenize_whole(w, ur);
        let mut cuts: Vec<usize> = picks.iter().map(|p| p.index(units.len() + 1)).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let chunks: Vec<Wtf8Buf> = std::iter::once(0)
            .chain(cuts.iter().copied())
            .zip(cuts.iter().copied().chain(std::iter::once(units.len())))
            .map(|(a, b)| Wtf8Buf::from_utf16(&units[a..b]))
            .collect();
        let chunked = tokenize_pieces(&chunks, ur);
        prop_assert_eq!(debug(&chunked), debug(&whole), "input {:?} cut at {:?}", w, cuts);
    }
}

#[test]
fn every_single_code_point_split() {
    // Deterministic: each input below split at every boundary.
    for src in [
        "a\r\nb",
        "url( x )",
        "'a\\\r\nb'",
        "1.5e+3px",
        "--x:\\41 1",
        "#-\\",
        "/* x",
        "U+1?-",
        "url(\"x\")",
        "\\",
        "<!--",
        "-->",
    ] {
        let w = Wtf8::from_str(src);
        let whole = tokenize_whole(w, true);
        let chunked = tokenize_chunked(w, &common::all_boundaries(w), true);
        assert_eq!(debug(&chunked), debug(&whole), "{src:?}");
    }
}

/// Tokenizing time grows linearly with input size on inputs that make naive
/// tokenizers quadratic or worse: long tokens of every kind, unterminated
/// constructs, and long tokens arriving in one-byte chunks. Timing is only
/// meaningful in release builds, so this runs when `CSS_SCALING=1` (the CI
/// deep run sets it).
#[test]
fn hostile_inputs_tokenize_in_linear_time() {
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    type Gen = fn(usize) -> String;
    let cases: &[(&str, Gen)] = &[
        ("long ident", |n| "a".repeat(n)),
        ("escapes", |n| "\\41".repeat(n)),
        ("long hex escapes", |n| "\\1234567".repeat(n)),
        ("unterminated string", |n| format!("'{}", "a".repeat(n))),
        ("string of escapes", |n| format!("'{}'", "\\\n".repeat(n))),
        ("unterminated comment", |n| format!("/*{}", "*".repeat(n))),
        ("many comments", |n| "/**/".repeat(n)),
        ("unterminated url", |n| format!("url({}", "a".repeat(n))),
        ("url spaces", |n| format!("url({}x)", " ".repeat(n))),
        ("url( then spaces", |n| format!("url({}'x')", " ".repeat(n))),
        ("bad url", |n| format!("url(a b{}", "\\)".repeat(n))),
        ("long number", |n| "9".repeat(n)),
        ("long fraction", |n| format!("1.{}", "3".repeat(n))),
        ("long exponent", |n| format!("1e{}", "9".repeat(n))),
        ("numbers", |n| "1e-1 ".repeat(n)),
        ("dashes", |n| "-".repeat(n)),
        ("almost CDO", |n| "<!-".repeat(n)),
        ("hashes", |n| "#".repeat(n)),
        ("unicode ranges", |n| "U+??????".repeat(n)),
        ("CR LF", |n| "\r\n".repeat(n)),
    ];
    let time = |input: &str, chunked: bool| {
        (0..3)
            .map(|_| {
                let w = Wtf8::from_str(input);
                let t = std::time::Instant::now();
                let n = if chunked {
                    let mut s = optimal_css::StreamTokenizer::new();
                    let mut n = 0;
                    for i in 0..input.len() {
                        s.push(w.slice(i, i + 1));
                        while let optimal_css::Next::Token(_) = s.next_token(true) {
                            n += 1;
                        }
                    }
                    s.finish();
                    while let optimal_css::Next::Token(_) = s.next_token(true) {
                        n += 1;
                    }
                    n
                } else {
                    let mut tk = optimal_css::Tokenizer::new(w);
                    let mut n = 0;
                    while tk.next_token_with(true).is_some() {
                        n += 1;
                    }
                    n
                };
                std::hint::black_box(n);
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let n = 100_000;
    for (name, make) in cases {
        for chunked in [false, true] {
            let small = time(&make(n), chunked);
            let big = time(&make(4 * n), chunked);
            let how = if chunked { "1-byte chunks" } else { "whole" };
            println!("{name:22} {how:13} n={n}: {small:?}, 4n: {big:?}");
            // Linear growth is 4x; allow noise. Quadratic would be 16x.
            assert!(
                big <= small * 8 + std::time::Duration::from_millis(20),
                "{name} ({how}): 4x the input took {big:?} vs {small:?}: not linear"
            );
        }
    }
}
