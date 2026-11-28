//! Runs romainmenke/css-tokenizer-tests (MIT licence, see
//! `browser/conformance/README.md`) against our tokenizer.
//!
//! Each test's expected tokens give the type, the raw source text, the start
//! and end index (UTF-16 code units, end exclusive) and the structured value
//! (unescaped value, numeric value, type flag, sign character, unit). All of
//! it is compared. Every test is then run again one code point per chunk and
//! in seeded random chunks through the stream tokenizer, which must give the
//! same tokens and errors as the whole-input run.
//!
//! The suite location comes from `CSS_TOKENIZER_TESTS` (default:
//! `browser/build/css-tokenizer-tests`, filled by
//! `browser/scripts/fetch-test-suites.sh`). A missing suite fails the test
//! rather than skipping it, so CI can never go green without running it.

mod common;

use common::{all_boundaries, random_boundaries, tokenize_chunked, tokenize_whole};
use optimal_css::{HashKind, Number, Sign, Spanned, Token};
use optimal_wtf8::Wtf8;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn suite_dir() -> PathBuf {
    match std::env::var_os("CSS_TOKENIZER_TESTS") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/css-tokenizer-tests"),
    }
}

fn errata_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/css-tokenizer-errata.json")
}

fn number(n: &Number, out: &mut serde_json::Map<String, Value>) {
    out.insert("value".into(), json!(n.value));
    out.insert(
        "type".into(),
        json!(if n.is_integer { "integer" } else { "number" }),
    );
    sign(n.sign, out);
}

fn sign(s: Option<Sign>, out: &mut serde_json::Map<String, Value>) {
    match s {
        Some(Sign::Plus) => {
            out.insert("signCharacter".into(), json!("+"));
        }
        Some(Sign::Minus) => {
            out.insert("signCharacter".into(), json!("-"));
        }
        None => {}
    }
}

/// Our token in the suite's format. Numbers are written as JSON floats; the
/// comparison below compares numbers by value.
fn to_suite(t: &Spanned<'_>, src: &str, utf16_at: &[usize]) -> Value {
    let mut s = serde_json::Map::new();
    let ty = match &t.token {
        Token::Ident(v) => {
            s.insert("value".into(), json!(v));
            "ident-token"
        }
        Token::Function(v) => {
            s.insert("value".into(), json!(v));
            "function-token"
        }
        Token::AtKeyword(v) => {
            s.insert("value".into(), json!(v));
            "at-keyword-token"
        }
        Token::Hash(v, k) => {
            s.insert("value".into(), json!(v));
            let k = match k {
                HashKind::Id => "id",
                HashKind::Unrestricted => "unrestricted",
            };
            s.insert("type".into(), json!(k));
            "hash-token"
        }
        Token::String(v) => {
            s.insert("value".into(), json!(v));
            "string-token"
        }
        Token::BadString => "bad-string-token",
        Token::Url(v) => {
            s.insert("value".into(), json!(v));
            "url-token"
        }
        Token::BadUrl => "bad-url-token",
        Token::Delim(c) => {
            s.insert("value".into(), json!(c.to_string()));
            "delim-token"
        }
        Token::Number(n) => {
            number(n, &mut s);
            "number-token"
        }
        Token::Percentage { value, sign: sg } => {
            s.insert("value".into(), json!(value));
            sign(*sg, &mut s);
            "percentage-token"
        }
        Token::Dimension(n, unit) => {
            number(n, &mut s);
            s.insert("unit".into(), json!(unit));
            "dimension-token"
        }
        Token::UnicodeRange { .. } => panic!("unicode-range token without the flag"),
        Token::Whitespace => "whitespace-token",
        Token::Cdo => "CDO-token",
        Token::Cdc => "CDC-token",
        Token::Colon => "colon-token",
        Token::Semicolon => "semicolon-token",
        Token::Comma => "comma-token",
        Token::OpenSquare => "[-token",
        Token::CloseSquare => "]-token",
        Token::OpenParen => "(-token",
        Token::CloseParen => ")-token",
        Token::OpenCurly => "{-token",
        Token::CloseCurly => "}-token",
        Token::Comment => "comment",
    };
    json!({
        "type": ty,
        "raw": &src[t.start..t.end],
        "startIndex": utf16_at[t.start],
        "endIndex": utf16_at[t.end],
        "structured": if s.is_empty() { Value::Null } else { Value::Object(s) },
    })
}

/// JSON equality with numbers compared by value (the suite writes `10`, we
/// write `10.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// UTF-16 offset of every byte offset of `src` (only code point boundaries
/// are meaningful).
fn utf16_offsets(src: &str) -> Vec<usize> {
    let mut at = vec![0; src.len() + 1];
    let mut n = 0;
    for (i, c) in src.char_indices() {
        at[i] = n;
        n += c.len_utf16();
    }
    at[src.len()] = n;
    at
}

#[test]
fn css_tokenizer_tests() {
    let root = suite_dir();
    let tests = root.join("tests");
    assert!(
        tests.is_dir(),
        "css-tokenizer-tests not found at {} (run browser/scripts/fetch-test-suites.sh)",
        root.display()
    );
    let errata: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(errata_path()).expect("errata file"))
            .expect("errata JSON");
    let mut used_errata = 0;
    let mut cases = Vec::new();
    for group in std::fs::read_dir(&tests).unwrap() {
        let group = group.unwrap().path();
        for case in std::fs::read_dir(&group).unwrap() {
            cases.push(case.unwrap().path());
        }
    }
    cases.sort();
    let rounds: u64 = std::env::var("TOKENIZER_RANDOM_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let seed: u64 = std::env::var("CSS_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x0c55);
    let mut failures = Vec::new();
    let mut per_group: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for case in &cases {
        let name = case
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let group = name.split('/').nth(1).unwrap().to_owned();
        let src = std::fs::read_to_string(case.join("source.css")).expect("source.css is UTF-8");
        let suite: Value =
            serde_json::from_str(&std::fs::read_to_string(case.join("tokens.json")).unwrap())
                .unwrap();
        let expected = match errata.get(&name) {
            Some(e) => {
                used_errata += 1;
                assert!(
                    !same(&e["tokens"], &suite),
                    "{name}: erratum matches the suite now; remove it"
                );
                e["tokens"].clone()
            }
            None => suite,
        };
        let w = Wtf8::from_str(&src);
        let (tokens, errors) = tokenize_whole(w, false);
        common::assert_covers(&tokens, src.len());
        let utf16_at = utf16_offsets(&src);
        let ours = Value::Array(
            tokens
                .iter()
                .map(|t| to_suite(t, &src, &utf16_at))
                .collect(),
        );
        let entry = per_group.entry(group).or_default();
        entry.1 += 1;
        let mut ok = same(&ours, &expected);
        if !ok {
            failures.push(format!(
                "{name}: source {src:?}\n  expected {expected}\n  got      {ours}"
            ));
        }
        // Chunking must not change anything, errors included.
        let mut splits = vec![("per code point".to_owned(), all_boundaries(w))];
        for r in 0..rounds {
            let s = seed.wrapping_add(r);
            splits.push((format!("random seed {s}"), random_boundaries(w, s)));
        }
        for (how, cuts) in splits {
            let (t2, e2) = tokenize_chunked(w, &cuts, false);
            if t2 != tokens || e2 != errors {
                ok = false;
                failures.push(format!("{name}: {how} chunking differs: {t2:?} {e2:?}"));
            }
        }
        if ok {
            entry.0 += 1;
        }
    }
    assert_eq!(
        used_errata,
        errata.len(),
        "an erratum names a test that doesn't exist"
    );
    println!("css-tokenizer-tests results (seed {seed}, {rounds} random chunkings each):");
    let (mut pass, mut total) = (0, 0);
    for (g, (p, t)) in &per_group {
        println!("  {g:24} {p:4} / {t}");
        pass += p;
        total += t;
    }
    println!("  TOTAL                    {pass:4} / {total}");
    if let Some(out) = std::env::var_os("CSS_TOKENIZER_CONFORMANCE_OUT") {
        let groups: BTreeMap<_, _> = per_group
            .iter()
            .map(|(g, (p, t))| (g.clone(), json!({"passed": p, "total": t})))
            .collect();
        std::fs::write(
            out,
            serde_json::to_string_pretty(
                &json!({"passed": pass, "total": total, "groups": groups}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
