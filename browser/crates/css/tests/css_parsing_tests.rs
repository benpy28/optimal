//! Runs SimonSapin/css-parsing-tests (CC0, see `browser/conformance/README.md`)
//! against our parser.
//!
//! The suite was written for the 2021 css-syntax-3 draft; we follow the
//! 2026 text. Differences are handled in two documented ways:
//!
//! 1. Mechanical, representation-only conversions of the expected output,
//!    applied to every test (see [`normalize_expected`]):
//!    - `["error", "eof-in-string"]`, `["error", "eof-in-url"]` and
//!      `["error", "invalid"]` entries are markers the suite's reference
//!      implementation inserts where a parse error happened or a construct was
//!      dropped. The spec's output has nothing there (parse errors are
//!      reported apart, and checked against the literal model in
//!      `parser_properties.rs`), so they are removed.
//!    - `"~="`, `"|="`, `"^="`, `"$="`, `"*="` and `"||"` were tokens in the
//!      2021 draft; css-syntax-3 now has no such tokens, and each is two
//!      `<delim-token>`s, so they are split into two delims.
//! 2. Everything else that differs is an erratum in
//!    `conformance/css-parsing-errata.json`, with the 2026 rule that decides
//!    it and the corrected expectation. The harness fails if an erratum stops
//!    differing from the (normalized) suite.
//!
//! Blocks of rules are compared as the suite writes them: the component
//! values between the braces. Their structure (declarations, nested rules)
//! is checked by `blocks_contents.json` and the property tests.
//!
//! Not run: `stylesheet_bytes.json` (byte decoding, not part of this crate
//! yet), `An+B.json` and `color_*.json` (later stages).

use optimal_css::{
    BlockItem, Declaration, HashKind, Kind, Parsed, Rule, RuleId, SyntaxError, Token, Values,
};
use optimal_wtf8::Wtf8;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn suite_dir() -> PathBuf {
    match std::env::var_os("CSS_PARSING_TESTS") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/css-parsing-tests"),
    }
}

fn errata_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/css-parsing-errata.json")
}

fn num_type(repr: &str) -> &'static str {
    if repr.contains(['.', 'e', 'E']) {
        "number"
    } else {
        "integer"
    }
}

/// A list of component values in the suite's format.
fn values_json(p: &Parsed<'_>, values: Values<'_, '_>) -> Vec<Value> {
    values.map(|v| value_json(p, v)).collect()
}

fn value_json(p: &Parsed<'_>, v: optimal_css::ComponentValue<'_, '_>) -> Value {
    match v.kind() {
        Kind::Function(name, contents) => {
            let mut a = vec![json!("function"), json!(name)];
            a.extend(values_json(p, contents));
            Value::Array(a)
        }
        Kind::Block(kind, contents) => {
            let tag = match kind {
                optimal_css::BlockKind::Curly => "{}",
                optimal_css::BlockKind::Square => "[]",
                optimal_css::BlockKind::Paren => "()",
            };
            let mut a = vec![json!(tag)];
            a.extend(values_json(p, contents));
            Value::Array(a)
        }
        Kind::Token(t) => {
            let s = p.tokens.get(v.index());
            let raw = p.source(s.start..s.end).to_string_lossy();
            match t {
                Token::Ident(x) => json!(["ident", x]),
                Token::AtKeyword(x) => json!(["at-keyword", x]),
                Token::Hash(x, k) => json!([
                    "hash",
                    x,
                    if *k == HashKind::Id {
                        "id"
                    } else {
                        "unrestricted"
                    }
                ]),
                Token::String(x) => json!(["string", x]),
                Token::BadString => json!(["error", "bad-string"]),
                Token::Url(x) => json!(["url", x]),
                Token::BadUrl => json!(["error", "bad-url"]),
                Token::Delim(c) => json!(c.to_string()),
                Token::Number(n) => json!([
                    "number",
                    raw,
                    n.value,
                    if n.is_integer { "integer" } else { "number" }
                ]),
                Token::Percentage { value, .. } => {
                    let repr = raw.trim_end_matches('%');
                    json!(["percentage", repr, value, num_type(repr)])
                }
                Token::Dimension(n, unit) => {
                    // The representation is the number's text, before the unit.
                    let repr = numeric_prefix(&raw);
                    json!([
                        "dimension",
                        repr,
                        n.value,
                        if n.is_integer { "integer" } else { "number" },
                        unit
                    ])
                }
                Token::UnicodeRange { start, end } => json!(["unicode-range", start, end]),
                Token::Whitespace => json!(" "),
                Token::Cdo => json!("<!--"),
                Token::Cdc => json!("-->"),
                Token::Colon => json!(":"),
                Token::Semicolon => json!(";"),
                Token::Comma => json!(","),
                Token::CloseSquare => json!(["error", "]"]),
                Token::CloseParen => json!(["error", ")"]),
                Token::CloseCurly => json!(["error", "}"]),
                Token::Function(_)
                | Token::OpenSquare
                | Token::OpenParen
                | Token::OpenCurly
                | Token::Comment => unreachable!("not a preserved token"),
            }
        }
    }
}

/// The leading number of a dimension's source text (sign, digits, fraction,
/// exponent), per 4.3.13.
fn numeric_prefix(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
    }
    raw[..i].to_owned()
}

fn declaration_json(p: &Parsed<'_>, d: &Declaration) -> Value {
    json!([
        "declaration",
        p.name(d.name),
        values_json(p, p.values(d.value.clone())),
        d.important
    ])
}

/// The contents of the block whose `{` is at token `open`, as component values.
fn raw_block(p: &Parsed<'_>, open: u32) -> Value {
    match value_json(p, p.tokens.value(open)) {
        Value::Array(mut a) => {
            a.remove(0);
            Value::Array(a)
        }
        _ => unreachable!(),
    }
}

fn rule_json(p: &Parsed<'_>, id: RuleId) -> Value {
    match &p.rules[id as usize] {
        Rule::At {
            name,
            prelude,
            block,
        } => json!([
            "at-rule",
            p.name(*name),
            values_json(p, p.values(prelude.clone())),
            match block {
                Some(_) => raw_block(p, prelude.end),
                None => Value::Null,
            }
        ]),
        Rule::Qualified { prelude, .. } => json!([
            "qualified rule",
            values_json(p, p.values(prelude.clone())),
            raw_block(p, prelude.end)
        ]),
        Rule::NestedDeclarations(_) => unreachable!("only inside qualified rules"),
    }
}

fn block_items_json(p: &Parsed<'_>, items: &[BlockItem]) -> Value {
    let mut out = Vec::new();
    for item in items {
        match item {
            BlockItem::Rule(r) => out.push(rule_json(p, *r)),
            BlockItem::Declarations(ds) => out.extend(ds.iter().map(|d| declaration_json(p, d))),
        }
    }
    Value::Array(out)
}

fn syntax_error(e: SyntaxError) -> Value {
    match e {
        SyntaxError::Empty => json!(["error", "empty"]),
        SyntaxError::Invalid => json!(["error", "invalid"]),
        SyntaxError::ExtraInput => json!(["error", "extra-input"]),
    }
}

/// Our result for `input` under the suite file's entry point.
fn run(file: &str, input: &str) -> Value {
    let w = Wtf8::from_str(input);
    let mut v = optimal_css::AcceptAll;
    match file {
        "component_value_list.json" => {
            let (p, r) = optimal_css::parse_component_values(w);
            Value::Array(values_json(&p, p.values(r)))
        }
        "one_component_value.json" => {
            let (p, r) = optimal_css::parse_component_value(w);
            match r {
                Ok(i) => value_json(&p, p.tokens.value(i)),
                Err(e) => syntax_error(e),
            }
        }
        "one_declaration.json" => {
            let (p, r) = optimal_css::parse_declaration(w, &mut v);
            match r {
                Ok(d) => declaration_json(&p, &d),
                Err(e) => syntax_error(e),
            }
        }
        "declaration_list.json" => {
            let (p, ds) = optimal_css::parse_declaration_list(w, &mut v);
            Value::Array(ds.iter().map(|d| declaration_json(&p, d)).collect())
        }
        "one_rule.json" => {
            let (p, r) = optimal_css::parse_rule(w, &mut v);
            match r {
                Ok(id) => rule_json(&p, id),
                Err(e) => syntax_error(e),
            }
        }
        "rule_list.json" | "stylesheet.json" => {
            let (p, rules) = optimal_css::parse_stylesheet(w, &mut v);
            Value::Array(rules.iter().map(|&r| rule_json(&p, r)).collect())
        }
        "blocks_contents.json" => {
            // 2026 has no "parse a block's contents" entry point; "consume a
            // block's contents" is what a rule's block gets. `x{` before the
            // input opens a block that the input fills up to the end of input,
            // and leaves the input's own tokens unchanged (a `{` token never
            // merges with what follows).
            let wrapped = format!("x{{{input}");
            let (p, rules) = optimal_css::parse_stylesheet(Wtf8::from_str(&wrapped), &mut v);
            let [id] = rules[..] else {
                panic!("one rule expected")
            };
            match &p.rules[id as usize] {
                Rule::Qualified {
                    declarations,
                    children,
                    ..
                } => {
                    let mut items = Vec::new();
                    if !declarations.is_empty() {
                        items.push(BlockItem::Declarations(declarations.clone()));
                    }
                    for &c in children {
                        match &p.rules[c as usize] {
                            Rule::NestedDeclarations(ds) => {
                                items.push(BlockItem::Declarations(ds.clone()))
                            }
                            _ => items.push(BlockItem::Rule(c)),
                        }
                    }
                    block_items_json(&p, &items)
                }
                other => panic!("{other:?}"),
            }
        }
        _ => unreachable!(),
    }
}

/// The mechanical conversions described in the module docs.
fn normalize_expected(v: &Value) -> Value {
    match v {
        Value::Array(a) => {
            let mut out = Vec::new();
            for item in a {
                if let Value::Array(e) = item
                    && e.len() == 2
                    && e[0] == "error"
                    && matches!(
                        e[1].as_str(),
                        Some("eof-in-string" | "eof-in-url" | "invalid")
                    )
                {
                    continue;
                }
                if let Some(s @ ("~=" | "|=" | "^=" | "$=" | "*=" | "||")) = item.as_str() {
                    let mut cs = s.chars();
                    out.push(json!(cs.next().unwrap().to_string()));
                    out.push(json!(cs.next().unwrap().to_string()));
                    continue;
                }
                out.push(normalize_expected(item));
            }
            Value::Array(out)
        }
        other => other.clone(),
    }
}

/// JSON equality with numbers compared by value.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        _ => a == b,
    }
}

const FILES: &[&str] = &[
    "component_value_list.json",
    "one_component_value.json",
    "one_declaration.json",
    "declaration_list.json",
    "blocks_contents.json",
    "one_rule.json",
    "rule_list.json",
    "stylesheet.json",
];

#[test]
fn css_parsing_tests() {
    let root = suite_dir();
    assert!(
        root.join("stylesheet.json").is_file(),
        "css-parsing-tests not found at {} (run browser/scripts/fetch-test-suites.sh)",
        root.display()
    );
    let errata: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(errata_path()).expect("errata file"))
            .expect("errata JSON");
    let mut used = 0;
    let mut failures = Vec::new();
    let mut totals = Vec::new();
    let mut dump = Vec::new();
    for file in FILES {
        let tests: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(root.join(file)).unwrap()).unwrap();
        let (mut pass, mut total) = (0, 0);
        for (n, pair) in tests.chunks(2).enumerate() {
            let input = pair[0].as_str().expect("string input");
            let key = format!("{file}#{n}");
            let suite = normalize_expected(&pair[1]);
            let expected = match errata.get(&key) {
                Some(e) => {
                    used += 1;
                    assert_eq!(e["input"], pair[0], "{key}: erratum is for another input");
                    assert!(
                        !same(&e["expected"], &suite),
                        "{key}: erratum matches the suite now; remove it"
                    );
                    e["expected"].clone()
                }
                None => suite.clone(),
            };
            let ours = run(file, input);
            total += 1;
            if same(&ours, &expected) {
                pass += 1;
            } else {
                failures.push(format!(
                    "{key}: input {input:?}\n  expected {expected}\n  got      {ours}"
                ));
                dump.push(json!({"key": key, "input": input, "suite": suite, "ours": ours}));
            }
        }
        totals.push((*file, pass, total));
    }
    assert_eq!(
        used,
        errata.len(),
        "an erratum names a test that doesn't exist"
    );
    println!("css-parsing-tests results (seed 0, no random chunking):");
    let (mut p, mut t) = (0, 0);
    for (f, pass, total) in &totals {
        println!("  {f:28} {pass:4} / {total}");
        p += pass;
        t += total;
    }
    println!("  TOTAL                        {p:4} / {t}");
    if let Some(out) = std::env::var_os("CSS_PARSING_DIFFS_OUT") {
        std::fs::write(out, serde_json::to_string_pretty(&dump).unwrap()).unwrap();
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
