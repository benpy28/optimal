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
use optimal_style::values::{parse_value, serialize_specified, specified_value, value_matches};
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
    /// For a valid case, the serializations the test accepts (the value
    /// itself when the call gives none); `None` when the call gives one
    /// that isn't a literal.
    serialized: Option<Vec<String>>,
}

/// Reads the optional third argument of `test_valid_value`: a string
/// literal or an array of them. `Some(vec![])` when there is none.
fn serializations(r: &str) -> Option<Vec<String>> {
    let r = r.trim_start();
    if r.starts_with(')') {
        return Some(Vec::new());
    }
    let r = r.strip_prefix(',')?.trim_start();
    if let Some(mut list) = r.strip_prefix('[') {
        let mut out = Vec::new();
        loop {
            list = list.trim_start();
            if list.starts_with(']') {
                return Some(out);
            }
            let (v, rest) = js_string(list)?;
            out.push(v);
            list = rest.trim_start();
            list = list.strip_prefix(',').unwrap_or(list);
        }
    }
    let (v, rest) = js_string(r)?;
    let rest = rest.trim_start();
    (rest.starts_with(')') || rest.starts_with(',')).then(|| vec![v])
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
                (r.starts_with(',') || r.starts_with(')')).then_some((prop, value, r))
            });
            match parsed {
                Some((property, value, r)) => out.push(Case {
                    file: file.to_string(),
                    valid,
                    property,
                    serialized: serializations(r)
                        .map(|v| if v.is_empty() { vec![value.clone()] } else { v }),
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

/// How a css-values math serialization page's own helper calls
/// `test_specified_serialization`: the helper, and per call it makes, the
/// property (the page's default, which a `{prop: ...}` argument
/// overrides) and the value and expected templates (`{0}` is the
/// helper's first argument, and so on).
struct Helper {
    file: &'static str,
    name: &'static str,
    calls: &'static [(&'static str, &'static str, &'static str)],
}

/// The pages whose helpers are literal enough to read (the rest of the
/// css-values `*-serialize.html` pages compute their expectations in
/// script). `test_map`/`test_array` pages iterate a table: `{0}` is the
/// key, `{1}` the expected value.
const HELPERS: &[Helper] = &[
    Helper {
        file: "acos-asin-atan-atan2-serialize.html",
        name: "test_map",
        calls: &[
            ("transform", "rotate({0})", "rotate({1})"),
            ("transform", "rotate(calc({0}))", "rotate({1})"),
        ],
    },
    Helper {
        file: "calc-complex-unresolved-serialize.html",
        name: "test_serialization",
        calls: &[("orphans", "{0}", "{1}")],
    },
    Helper {
        file: "calc-infinity-nan-serialize-angle.html",
        name: "test_map",
        calls: &[("transform", "rotate(calc({0}))", "rotate({1})")],
    },
    Helper {
        file: "calc-infinity-nan-serialize-length.html",
        name: "test_map",
        calls: &[("width", "calc({0})", "{1}")],
    },
    Helper {
        file: "calc-infinity-nan-serialize-number.html",
        name: "test_map",
        calls: &[("opacity", "calc({0})", "{1}")],
    },
    Helper {
        file: "calc-infinity-nan-serialize-resolution.html",
        name: "test_map",
        calls: &[(
            "background-image",
            "image-set(url(\"\") calc({0}))",
            "image-set(url(\"\") {1})",
        )],
    },
    Helper {
        file: "calc-infinity-nan-serialize-time.html",
        name: "test_map",
        calls: &[("animation-duration", "calc({0})", "{1}")],
    },
    Helper {
        file: "clamp-length-serialize.html",
        name: "test_valid_length",
        calls: &[
            ("margin-top", "{0}", "{1}"),
            ("margin-top", "calc({0})", "{1}"),
        ],
    },
    Helper {
        file: "exp-log-serialize.html",
        name: "test_serialization",
        calls: &[
            ("opacity", "{0}", "{1}"),
            ("transform", "scale({0})", "scale({1})"),
        ],
    },
    Helper {
        file: "hypot-pow-sqrt-serialize.html",
        name: "test_serialization",
        calls: &[
            ("opacity", "{0}", "{1}"),
            ("transform", "scale({0})", "scale(calc({2}))"),
        ],
    },
    Helper {
        file: "minmax-angle-serialize.html",
        name: "test_serialization",
        calls: &[("transform", "rotate({0})", "rotate({1})")],
    },
    Helper {
        file: "minmax-length-percent-serialize.html",
        name: "test_serialization",
        calls: &[("text-indent", "{0}", "{1}")],
    },
    Helper {
        file: "minmax-length-serialize.html",
        name: "test_serialization",
        calls: &[("text-indent", "{0}", "{1}")],
    },
    Helper {
        file: "minmax-number-serialize.html",
        name: "test_serialization",
        calls: &[
            ("opacity", "{0}", "{1}"),
            ("transform", "scale({0})", "scale(calc({2}))"),
        ],
    },
    Helper {
        file: "minmax-percentage-serialize.html",
        name: "test_serialization",
        calls: &[("text-indent", "{0}", "{1}")],
    },
    Helper {
        file: "minmax-time-serialize.html",
        name: "test_serialization",
        calls: &[("transition-delay", "{0}", "{1}")],
    },
    Helper {
        file: "round-mod-rem-serialize.html",
        name: "test_serialization",
        calls: &[
            ("opacity", "{0}", "{1}"),
            ("transform", "scale({0})", "scale({1})"),
        ],
    },
    Helper {
        file: "signs-abs-serialize.html",
        name: "test_serialization",
        calls: &[("transform", "scale({0})", "scale(calc({2}))")],
    },
];

/// `aBcD` as `a-bc-d` (a CSSOM attribute name as a property name).
fn kebab(camel: &str) -> String {
    let mut out = String::new();
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The literal string arguments at the start of a call's argument list,
/// and a `{prop: ...}` option if one follows them.
fn literal_args(mut r: &str) -> (Vec<String>, Option<String>) {
    let mut args = Vec::new();
    while let Some((v, rest)) = js_string(r) {
        args.push(v);
        r = rest.trim_start();
        match r.strip_prefix(',') {
            Some(rest) => r = rest,
            None => return (args, None),
        }
    }
    let prop = r.trim_start().strip_prefix('{').and_then(|o| {
        let o = o.trim_start().strip_prefix("prop")?.trim_start();
        let o = o.strip_prefix(':').or_else(|| o.strip_prefix('='))?;
        js_string(o).map(|(p, _)| p)
    });
    (args, prop)
}

/// The `test_map = { "key": "value", ... }` entries of a page.
fn test_map(js: &str) -> Vec<Vec<String>> {
    let Some(at) = js.find("test_map = {") else {
        return Vec::new();
    };
    let mut r = &js[at + "test_map = {".len()..];
    let mut out = Vec::new();
    while let Some((k, rest)) = js_string(r) {
        let Some(rest) = rest.trim_start().strip_prefix(':') else {
            break;
        };
        let Some((v, rest)) = js_string(rest) else {
            break;
        };
        out.push(vec![k, v]);
        r = rest.trim_start();
        r = r.strip_prefix(',').unwrap_or(r);
    }
    out
}

/// The css-color pages' `fuzzy_test_valid_color(specified, expected)`
/// cases: literal calls, and `tests = [[specified, expected], ...]`
/// tables that a loop passes to it. They check `color`, comparing as
/// WPT's `css/support/color-testcommon.js` does ([`fuzzy_color_eq`]).
fn color_cases() -> Vec<Case> {
    let dir = "css/css-color/parsing";
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join(dir))
        .expect("css-color is fetched")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .filter(|p| !p.to_string_lossy().contains(".tentative."))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("UTF-8");
        let js = strip_comments(&scripts(&text));
        // color-mix() and relative colors are css-color-5 (a later level).
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if !js.contains("fuzzy_test_valid_color(")
            || name.contains("color-mix")
            || name.contains("relative-color")
        {
            continue;
        }
        let rel = format!("{dir}/{}", f.file_name().unwrap().to_string_lossy());
        let mut pairs: Vec<Vec<String>> = Vec::new();
        let mut rest = js.as_str();
        while let Some(at) = rest.find("fuzzy_test_valid_color(") {
            rest = &rest[at + "fuzzy_test_valid_color(".len()..];
            let (args, _) = literal_args(rest);
            if !args.is_empty() {
                pairs.push(args);
            }
        }
        if js.contains("fuzzy_test_valid_color(test[0], test[1])")
            && let Some(at) = js.find("tests = [")
        {
            let mut r = &js[at + "tests = [".len()..];
            loop {
                r = r.trim_start();
                let Some(inner) = r.strip_prefix('[') else {
                    break;
                };
                let (args, _) = literal_args(inner);
                let Some(end) = inner.find(']') else { break };
                if !args.is_empty() {
                    pairs.push(args);
                }
                r = inner[end + 1..].trim_start();
                r = r.strip_prefix(',').unwrap_or(r);
            }
        }
        for args in pairs {
            out.push(Case {
                file: rel.clone(),
                valid: true,
                property: "color".to_string(),
                serialized: Some(vec![format!("fuzzy:{}", args.get(1).unwrap_or(&args[0]))]),
                value: args[0].clone(),
            });
        }
    }
    // Exact tables: `tests = [...]` passed to test_valid_value.
    for name in ["color-valid-lab.html"] {
        let rel = format!("{dir}/{name}");
        let js = strip_comments(&scripts(
            &std::fs::read_to_string(root().join(&rel)).expect("fetched"),
        ));
        assert!(
            js.contains("test_valid_value(\"color\", test[0], test[1])"),
            "{rel}: changed shape"
        );
        let at = js.find("tests = [").expect("a table");
        let mut r = &js[at + "tests = [".len()..];
        loop {
            r = r.trim_start();
            let Some(inner) = r.strip_prefix('[') else {
                break;
            };
            let (args, _) = literal_args(inner);
            let Some(end) = inner.find(']') else { break };
            if let [value, want] = args.as_slice() {
                out.push(Case {
                    file: rel.clone(),
                    valid: true,
                    property: "color".to_string(),
                    serialized: Some(vec![want.clone()]),
                    value: value.clone(),
                });
            }
            r = inner[end + 1..].trim_start();
            r = r.strip_prefix(',').unwrap_or(r);
        }
    }
    // color-valid-color-function.html: template calls in a loop over the
    // color spaces, `xyz` written `xyz-d65` in the expectation.
    let rel = format!("{dir}/color-valid-color-function.html");
    let js = strip_comments(&scripts(
        &std::fs::read_to_string(root().join(&rel)).expect("fetched"),
    ));
    let at = js.find("for (const colorSpace of [").expect("the loop");
    let r = &js[at + "for (const colorSpace of [".len()..];
    let list = &r[..r.find(']').expect("the list")];
    let spaces: Vec<String> = list
        .split(',')
        .filter_map(|x| js_string(x).map(|(v, _)| v))
        .collect();
    assert!(spaces.len() >= 10, "{rel}: {spaces:?}");
    let mut rest = r;
    let mut templates = Vec::new();
    while let Some(at) = rest.find("test_valid_value(\"color\", `") {
        rest = &rest[at + "test_valid_value(\"color\", `".len()..];
        let Some(end) = rest.find('`') else { break };
        let value = rest[..end].to_string();
        let after = rest[end + 1..].trim_start().strip_prefix(", `");
        let Some(after) = after else { continue };
        let Some(end2) = after.find('`') else { break };
        templates.push((value, after[..end2].to_string()));
        rest = &after[end2 + 1..];
    }
    assert!(templates.len() > 20, "{rel}: {} templates", templates.len());
    for space in &spaces {
        let result = if space == "xyz" { "xyz-d65" } else { space };
        for (v, w) in &templates {
            out.push(Case {
                file: rel.clone(),
                valid: true,
                property: "color".to_string(),
                value: v.replace("${colorSpace}", space),
                serialized: Some(vec![w.replace("${resultColorSpace}", result)]),
            });
        }
    }
    out
}

/// WPT's fuzzy color comparison (color-testcommon.js): the text with
/// digits and `.` removed must be equal, and the numbers (split at
/// spaces, `(` and `,`) equal within 0.01 each.
fn fuzzy_color_eq(actual: &str, expected: &str) -> bool {
    let non_numbers = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_ascii_digit() && *c != '.')
            .collect()
    };
    let numbers = |s: &str| -> Vec<f64> {
        s.split([' ', '(', ','])
            .filter_map(|e| {
                // parseFloat: the longest numeric prefix.
                let end = e
                    .char_indices()
                    .take_while(|&(i, c)| {
                        c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+'))
                    })
                    .map(|(i, c)| i + c.len_utf8())
                    .last()?;
                e[..end].parse::<f64>().ok()
            })
            .collect()
    };
    let (a, b) = (numbers(actual), numbers(expected));
    non_numbers(actual) == non_numbers(expected)
        && a.len() == b.len()
        && a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 0.01)
}

/// The cases of the [`HELPERS`] pages.
fn math_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for h in HELPERS {
        let rel = format!("css/css-values/{}", h.file);
        let text = std::fs::read_to_string(root().join(&rel))
            .unwrap_or_else(|e| panic!("{rel}: {e} (run browser/scripts/fetch-test-suites.sh)"));
        let js = strip_comments(&scripts(&text));
        let mut calls: Vec<(Vec<String>, Option<String>)> = Vec::new();
        if h.name == "test_map" {
            calls.extend(test_map(&js).into_iter().map(|a| (a, None)));
        } else {
            let pat = format!("{}(", h.name);
            let mut rest = js.as_str();
            while let Some(at) = rest.find(&pat) {
                let before = rest[..at].chars().next_back();
                rest = &rest[at + pat.len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                    continue;
                }
                let (args, prop) = literal_args(rest);
                if !args.is_empty() {
                    calls.push((args, prop));
                }
            }
        }
        assert!(!calls.is_empty(), "{rel}: no cases read");
        for (args, prop) in calls {
            for &(default, value, want) in h.calls {
                let fill = |t: &str| {
                    let mut t = t.to_string();
                    for (i, a) in args.iter().enumerate() {
                        t = t.replace(&format!("{{{i}}}"), a);
                    }
                    t
                };
                if want.contains(&format!("{{{}}}", args.len())) {
                    continue; // the helper call didn't give that argument
                }
                out.push(Case {
                    file: rel.clone(),
                    valid: true,
                    property: kebab(prop.as_deref().unwrap_or(default)),
                    value: fill(value),
                    serialized: Some(vec![fill(want)]),
                });
            }
        }
    }
    out
}

/// The valid cases' serializations (cssom-1 "serialize a CSS value" of a
/// specified value; css-values-4, css-color-4 and each property's spec
/// for the details): every longhand case the engine accepts must
/// serialize as the test says, except those listed in
/// `conformance/wpt-serialization-expected.json`, each with its reason.
/// Shorthands serialize from their longhands, which needs shorthand
/// expansion (a later step), so they are counted, not run. Values with
/// an arbitrary substitution function are skipped (they serialize as
/// written until substitution, a later step). `WPT_SERIALIZATION_OUT=path`
/// writes the failures there, as the file's shape.
#[test]
fn wpt_valid_values_serialize() {
    let (mut cases, _) = all_cases();
    let mut math = math_cases();
    math.extend(color_cases());
    let math_count = math.len();
    cases.extend(math);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/wpt-serialization-expected.json");
    let expected: BTreeMap<String, String> = {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&text).expect("wpt-serialization-expected.json parses")
    };
    let (mut run, mut shorthands, mut no_literal) = (0, 0, 0);
    let mut failing = BTreeMap::new();
    let mut stale = Vec::new();
    for (n, c) in cases.iter().enumerate().filter(|(_, c)| c.valid) {
        let name = c.property.to_ascii_lowercase();
        let Some(t) = target(&name) else { continue };
        if matches!(t, Target::All | Target::Custom) {
            continue;
        }
        if !accepted(c) {
            // The page cases are gated on validity by wpt_value_parsing;
            // the helper pages' only here.
            if n >= cases.len() - math_count {
                run += 1;
                if !expected.contains_key(&key(c)) {
                    failing.insert(key(c), "not accepted".to_string());
                }
            }
            continue;
        }
        let Some(want) = &c.serialized else {
            no_literal += 1;
            continue;
        };
        let (parsed, range) = parse_component_values(Wtf8::from_str(&c.value));
        let keyword = classify(&name, parsed.values(range.clone()))
            .and_then(|k| k.keyword)
            .is_some();
        let got = if keyword {
            c.value.trim().to_ascii_lowercase()
        } else {
            if matches!(t, Target::Shorthand(_)) {
                shorthands += 1;
                continue;
            }
            match specified_value(t, parsed.values(range)) {
                Some(v) => serialize_specified(&v),
                // A value with a substitution function is accepted
                // without a grammar match.
                None => continue,
            }
        };
        run += 1;
        let ok = want.iter().any(|w| match w.strip_prefix("fuzzy:") {
            Some(w) => fuzzy_color_eq(&got, w),
            None => *w == got,
        });
        let k = key(c);
        match (ok, expected.get(&k)) {
            (false, None) => {
                failing.insert(k, format!("serializes as {got:?}, expected {want:?}"));
            }
            (true, Some(_)) => stale.push(k),
            _ => {}
        }
    }
    eprintln!(
        "WPT serialization: {run} cases run ({math_count} from math helper and color pages), {} failing (unlisted), {} listed, {shorthands} shorthand cases not run, {no_literal} with a non-literal expectation",
        failing.len(),
        expected.len()
    );
    if let Some(out) = std::env::var_os("WPT_SERIALIZATION_OUT") {
        let mut all = expected.clone();
        all.extend(failing.clone());
        for s in &stale {
            all.remove(s);
        }
        std::fs::write(out, serde_json::to_string_pretty(&all).unwrap() + "\n").unwrap();
    }
    let show: Vec<String> = failing
        .iter()
        .take(80)
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    assert!(
        failing.is_empty() && stale.is_empty(),
        "{} failing:\n{}\n{} stale expectations:\n{}",
        failing.len(),
        show.join("\n"),
        stale.len(),
        stale.join("\n")
    );
}
