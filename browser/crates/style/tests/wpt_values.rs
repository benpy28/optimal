//! The web-platform-tests property value parsing tests (WPT commit
//! 6852422, fetched into `browser/build/wpt-css` by
//! `scripts/fetch-test-suites.sh`; `WPT_CSS` overrides the path): every
//! literal `test_valid_value(property, value, ...)` and
//! `test_invalid_value(property, value)` call in `css/css-values` and the
//! `parsing` directory of each module the property table draws from.
//!
//! A call counts when its first two arguments are string literals and the
//! property is in the property table (longhand, shorthand or alias); the
//! rest are counted as skipped, and so are `*.tentative.*` pages (tests of
//! proposals, not of spec text). Only validity is checked here: whether the
//! declaration is accepted. The serialization the valid cases also give
//! is a later step's (specified value serialization).
//!
//! Cases this engine fails on purpose are listed in
//! `conformance/wpt-values-expected.json`, each with the reason: a feature
//! from a spec level the tables don't follow yet, or a test that
//! disagrees with the spec text (an erratum, with the section). The
//! harness fails when a listed case passes, so the list can't go stale.
//! `WPT_VALUES_OUT=path` writes the failing cases there, as that file's
//! shape, to regenerate it.

use optimal_css::parse_component_values;
use optimal_css::{ComponentValue, Kind, Token};
use optimal_style::property::{Target, classify, target};
use optimal_style::values::parse::{Tree, TreeKind};
use optimal_style::values::{parse_value, value_matches};
use optimal_wtf8::Wtf8;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    std::env::var_os("WPT_CSS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/wpt-css"))
}

const DIRS: &[&str] = &[
    "css/css-values",
    "css/css-align/parsing",
    "css/css-backgrounds/parsing",
    "css/css-box/parsing",
    "css/css-break/parsing",
    "css/css-color/parsing",
    "css/css-content/parsing",
    "css/css-display/parsing",
    "css/css-flexbox/parsing",
    "css/css-fonts/parsing",
    "css/css-images/parsing",
    "css/css-inline/parsing",
    "css/css-lists/parsing",
    "css/css-overflow/parsing",
    "css/css-position/parsing",
    "css/css-sizing/parsing",
    "css/css-tables/parsing",
    "css/css-text/parsing",
    "css/css-transforms/parsing",
    "css/css-ui/parsing",
    "css/css-writing-modes/parsing",
];

/// One call: the file, whether it expects the value valid, the property
/// and the value.
#[derive(Clone, Debug)]
struct Case {
    file: String,
    valid: bool,
    property: String,
    value: String,
}

/// Reads a JavaScript string literal at the start of `s` ('...', "..." or
/// a `...` template without substitutions): its value and the rest.
fn js_string(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let quote = s.chars().next()?;
    if !matches!(quote, '"' | '\'' | '`') {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            c if c == quote => return Some((out, &s[1 + i + 1..])),
            '$' if quote == '`' && s[1 + i + 1..].starts_with('{') => return None,
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'f' => out.push('\u{c}'),
                    'b' => out.push('\u{8}'),
                    'v' => out.push('\u{b}'),
                    '0' => out.push('\0'),
                    '\n' => {}
                    'u' => {
                        let rest = &s[1 + i + 2..];
                        let (hex, len) = if let Some(r) = rest.strip_prefix('{') {
                            let end = r.find('}')?;
                            (&r[..end], end + 2)
                        } else {
                            (rest.get(..4)?, 4)
                        };
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        for _ in 0..len {
                            chars.next();
                        }
                    }
                    'x' => {
                        let hex = s.get(1 + i + 2..1 + i + 4)?;
                        out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                        chars.next();
                        chars.next();
                    }
                    e => out.push(e),
                }
            }
            c => out.push(c),
        }
    }
    None
}

/// Removes `/* */` comments and `//` line comments outside strings.
fn strip_comments(s: &str) -> String {
    let mut out = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut j = i + 2;
            while j + 1 < b.len() && !(b[j] == '*' && b[j + 1] == '/') {
                j += 1;
            }
            i = j + 2;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The script text of an HTML test page.
fn scripts(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find("<script") {
        let after = &rest[at..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let body = &after[open_end + 1..];
        let Some(end) = body.find("</script>") else {
            break;
        };
        if !after[..open_end].contains("src=") {
            out.push_str(&body[..end]);
            out.push('\n');
        }
        rest = &body[end..];
    }
    out
}

/// Every call in a file: the literal ones, and how many weren't.
fn calls(file: &str, text: &str) -> (Vec<Case>, usize) {
    let js = strip_comments(&scripts(text));
    let mut out = Vec::new();
    let mut skipped = 0;
    for (name, valid) in [("test_valid_value(", true), ("test_invalid_value(", false)] {
        let mut rest = js.as_str();
        while let Some(at) = rest.find(name) {
            // Not the definition of a wrapper, and not part of a longer name.
            let before = rest[..at].chars().next_back();
            rest = &rest[at + name.len()..];
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let parsed = js_string(rest).and_then(|(prop, r)| {
                let r = r.trim_start().strip_prefix(',')?;
                let (value, r) = js_string(r)?;
                let r = r.trim_start();
                (r.starts_with(',') || r.starts_with(')')).then_some((prop, value))
            });
            match parsed {
                Some((property, value)) => out.push(Case {
                    file: file.to_string(),
                    valid,
                    property,
                    value,
                }),
                None => skipped += 1,
            }
        }
    }
    (out, skipped)
}

fn all_cases() -> (Vec<Case>, usize) {
    let mut cases = Vec::new();
    let mut skipped = 0;
    for dir in DIRS {
        let path = root().join(dir);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e} (run browser/scripts/fetch-test-suites.sh)",
                    path.display()
                )
            })
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "html" || x == "htm"))
            // Tentative tests are for proposals, not specs.
            .filter(|p| !p.to_string_lossy().contains(".tentative."))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).expect("a test page is UTF-8");
            let rel = f
                .strip_prefix(root())
                .unwrap_or(Path::new(""))
                .display()
                .to_string();
            let (c, s) = calls(&rel, &text);
            cases.extend(c);
            skipped += s;
        }
    }
    (cases, skipped)
}

/// What the engine says about a case: is the declaration valid? Mirrors
/// CSSOM `style[property] = value`: the value is parsed alone, and an
/// `!important` in it makes it invalid.
fn accepted(c: &Case) -> bool {
    let name = c.property.to_ascii_lowercase();
    let (parsed, range) = parse_component_values(Wtf8::from_str(&c.value));
    classify(&name, parsed.values(range)).is_some()
}

fn key(c: &Case) -> String {
    format!("{} | {} | {}", c.file, c.property, c.value)
}

#[test]
fn wpt_value_parsing() {
    let (cases, skipped_calls) = all_cases();
    let expected: BTreeMap<String, String> = {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../conformance/wpt-values-expected.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&text).expect("wpt-values-expected.json parses")
    };
    let mut run = 0;
    let mut not_in_table = 0;
    let mut failing = BTreeMap::new();
    let mut stale = Vec::new();
    for c in &cases {
        let name = c.property.to_ascii_lowercase();
        if name.starts_with("--") || target(&name).is_none() {
            not_in_table += 1;
            continue;
        }
        run += 1;
        let ok = accepted(c) == c.valid;
        let k = key(c);
        match (ok, expected.get(&k)) {
            (false, None) => {
                failing.insert(
                    k,
                    format!("expected {}", if c.valid { "valid" } else { "invalid" }),
                );
            }
            (true, Some(_)) => stale.push(k),
            _ => {}
        }
    }
    eprintln!(
        "WPT value parsing: {run} cases run, {} failing (unlisted), {} listed, {not_in_table} for properties not in the table, {skipped_calls} non-literal calls skipped",
        failing.len(),
        expected.len()
    );
    if let Some(out) = std::env::var_os("WPT_VALUES_OUT") {
        let mut all = expected.clone();
        all.extend(failing.clone());
        for s in &stale {
            all.remove(s);
        }
        std::fs::write(out, serde_json::to_string_pretty(&all).unwrap() + "\n").unwrap();
    }
    let show: Vec<_> = failing.keys().take(60).collect();
    assert!(
        failing.is_empty() && stale.is_empty(),
        "{} failing:\n{}\n{} stale expectations:\n{}",
        failing.len(),
        show.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        stale.len(),
        stale.join("\n")
    );
}

/// The leaves of a parse tree (component values), in order.
fn leaves<'t, 'a>(t: &Tree<'t, 'a>, out: &mut Vec<ComponentValue<'t, 'a>>) {
    match &t.kind {
        TreeKind::Value(v) | TreeKind::Nested(v, _) => out.push(*v),
        TreeKind::Comma => {}
        TreeKind::Inner(t) | TreeKind::Alternative(_, t) => leaves(t, out),
        TreeKind::List(items) => items.iter().for_each(|(_, t)| leaves(t, out)),
    }
}

/// Every value the engine accepts has a parse tree, whose leaves are the
/// value's component values in order, each once, all but commas (a comma
/// token goes with a grammar comma or a `#` separator).
#[test]
fn wpt_valid_values_parse() {
    let (cases, _) = all_cases();
    let mut parsed_count = 0;
    for c in &cases {
        let name = c.property.to_ascii_lowercase();
        let Some(t) = target(&name) else { continue };
        if matches!(t, Target::All | Target::Custom) {
            continue;
        }
        let (parsed, range) = parse_component_values(Wtf8::from_str(&c.value));
        if !value_matches(t, parsed.values(range.clone())) {
            continue;
        }
        let tree = parse_value(t, parsed.values(range.clone()))
            .unwrap_or_else(|| panic!("{}: {} matches but has no parse", c.property, c.value));
        let mut got = Vec::new();
        leaves(&tree, &mut got);
        let items: Vec<ComponentValue<'_, '_>> = parsed
            .values(range)
            .filter(|v| !matches!(v.kind(), Kind::Token(Token::Whitespace)))
            .collect();
        let mut at = 0;
        for v in &got {
            while at < items.len() && items[at].index() != v.index() {
                assert!(
                    matches!(items[at].kind(), Kind::Token(Token::Comma)),
                    "{}: {}: a non-comma value is no leaf",
                    c.property,
                    c.value
                );
                at += 1;
            }
            assert!(
                at < items.len(),
                "{}: {}: leaves out of order",
                c.property,
                c.value
            );
            at += 1;
        }
        assert!(
            items[at..]
                .iter()
                .all(|v| matches!(v.kind(), Kind::Token(Token::Comma))),
            "{}: {}: trailing values are no leaves",
            c.property,
            c.value
        );
        parsed_count += 1;
    }
    assert!(parsed_count > 1_500, "{parsed_count} parsed");
}
