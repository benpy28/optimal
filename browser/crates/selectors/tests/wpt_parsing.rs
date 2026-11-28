//! web-platform-tests `css/selectors/parsing` (BSD-3-Clause), every test
//! file whose features are in the specs this crate follows.
//!
//! The test files are JavaScript. A small reader below evaluates the calls
//! they make (`test_valid_selector(sel, expected)` and the others) with
//! their string arguments, `+` joins, arrays and the two helper functions
//! the files define, and runs each case both ways WPT does: "parse a
//! selector" on the text (as `querySelector` does) and a style rule
//! `sel {}` whose prelude is parsed as a selector list (as `insertRule`
//! does). The two must agree, and a valid selector's serialization must
//! parse back to itself.
//!
//! Where the 2026 spec text disagrees with a test, the text wins and the
//! case is listed, with the reasoning, in
//! `browser/conformance/wpt-selectors-errata.json`. The harness fails if an
//! erratum stops being needed.

mod common;

use common::js::{Tok, lex};
use optimal_css::{AcceptAll, Rule, parse_rule};
use optimal_selectors::{Namespaces, parse_selector, parse_selector_list};
use optimal_wtf8::Wtf8;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn build_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build")
}

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

/// Files left out, with the reason. Every other file in the directory runs.
const SKIPPED: &[(&str, &str)] = &[
    (
        "parse-heading.html",
        ":heading and :heading() are Selectors Level 5, not yet followed here",
    ),
    (
        "parse-class-prefix.tentative.html",
        "class prefix selectors (.foo-*) are a tentative Selectors Level 5 feature",
    ),
    (
        "parse-has-class-prefix.tentative.html",
        "class prefix selectors (.foo-*) are a tentative Selectors Level 5 feature",
    ),
    (
        "parse-has-slotted.tentative.html",
        "functional :has-slotted() is tentative; css-shadow-1 defines only :has-slotted",
    ),
];

// --- A reader for the JavaScript in the test files ----------------------

#[derive(Clone, Debug)]
enum Val {
    Str(String),
    List(Vec<String>),
    Null,
    Bool,
}

type Env = BTreeMap<String, Val>;

/// expr := term ('+' term)*; term := string | name | [expr, ...] | null |
/// true | false.
fn expr(t: &[Tok], i: &mut usize, env: &Env) -> Val {
    let mut v = term(t, i, env);
    while t[*i] == Tok::Punct('+') {
        *i += 1;
        let r = term(t, i, env);
        v = match (v, r) {
            (Val::Str(a), Val::Str(b)) => Val::Str(a + &b),
            other => panic!("unsupported +: {other:?}"),
        };
    }
    v
}

fn term(t: &[Tok], i: &mut usize, env: &Env) -> Val {
    let tok = t[*i].clone();
    *i += 1;
    match tok {
        Tok::Str(s) => Val::Str(s),
        Tok::Word(w) if w == "null" => Val::Null,
        Tok::Word(w) if w == "true" || w == "false" => Val::Bool,
        Tok::Word(w) => env
            .get(&w)
            .cloned()
            .unwrap_or_else(|| panic!("unknown name {w}")),
        Tok::Punct('[') => {
            let mut items = Vec::new();
            while t[*i] != Tok::Punct(']') {
                match expr(t, i, env) {
                    Val::Str(s) => items.push(s),
                    other => panic!("array item {other:?}"),
                }
                if t[*i] == Tok::Punct(',') {
                    *i += 1;
                }
            }
            *i += 1;
            Val::List(items)
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// What a test expects of a selector.
#[derive(Clone, Debug, PartialEq)]
enum Expect {
    /// Valid, serializing to one of these.
    Valid(Vec<String>),
    Invalid,
}

#[derive(Clone, Debug)]
struct Case {
    input: String,
    expect: Expect,
}

fn valid(input: String, expected: Option<Val>) -> Case {
    let expect = match expected {
        None | Some(Val::Null) => Expect::Valid(vec![input.clone()]),
        Some(Val::Str(s)) => Expect::Valid(vec![s]),
        Some(Val::List(l)) => Expect::Valid(l),
        Some(other) => panic!("expected serialization {other:?}"),
    };
    Case { input, expect }
}

/// Runs the statements in `t`, collecting the cases their calls make.
fn run(
    t: &[Tok],
    env: &Env,
    functions: &BTreeMap<String, (Vec<String>, Vec<Tok>)>,
    out: &mut Vec<Case>,
) {
    let mut i = 0;
    while i < t.len() {
        let Tok::Word(name) = &t[i] else {
            i += 1;
            continue;
        };
        if t.get(i + 1) != Some(&Tok::Punct('('))
            || (i > 0 && t[i - 1] == Tok::Word("function".into()))
        {
            i += 1;
            continue;
        }
        let known = [
            "test_valid_selector",
            "test_valid_forgiving_selector",
            "assert_selector_serializes_to",
            "test_invalid_selector",
            "assert_invalid_selector",
            "assert_valid",
        ];
        if !known.contains(&name.as_str()) && !functions.contains_key(name) {
            i += 1;
            continue;
        }
        let name = name.clone();
        i += 2;
        let mut args = Vec::new();
        while t[i] != Tok::Punct(')') {
            args.push(expr(t, &mut i, env));
            if t[i] == Tok::Punct(',') {
                i += 1;
            }
        }
        i += 1;
        let s = |v: &Val| match v {
            Val::Str(s) => s.clone(),
            other => panic!("{name}: expected a string, got {other:?}"),
        };
        match name.as_str() {
            "test_valid_selector"
            | "test_valid_forgiving_selector"
            | "assert_selector_serializes_to" => out.push(valid(s(&args[0]), args.get(1).cloned())),
            "test_invalid_selector" | "assert_invalid_selector" => out.push(Case {
                input: s(&args[0]),
                expect: Expect::Invalid,
            }),
            // parse-is-where.html's helper: the pattern with {} as :is and
            // as :where; valid or forgiving-valid (both serialize as
            // given).
            "assert_valid" => {
                for pseudo in [":is", ":where"] {
                    let input = s(&args[1]).replace("{}", pseudo);
                    let expected = match &args[2] {
                        Val::Null => None,
                        v => Some(Val::Str(s(v).replace("{}", pseudo))),
                    };
                    out.push(valid(input, expected));
                }
            }
            _ => {
                let (params, body) = &functions[&name];
                let mut inner = env.clone();
                for (p, a) in params.iter().zip(args) {
                    inner.insert(p.clone(), a);
                }
                run(body, &inner, functions, out);
            }
        }
    }
}

/// The cases a test file makes, in order.
fn cases(file: &str) -> Vec<Case> {
    let text =
        std::fs::read_to_string(build_dir().join("wpt-css/css/selectors/parsing").join(file))
            .unwrap();
    let mut out = Vec::new();
    for chunk in text.split("<script>").skip(1) {
        let script = chunk.split("</script>").next().unwrap();
        let toks = lex(script);
        // Collect the file's own helper functions whose bodies are only
        // calls (run_tests_on_anplusb_selector), and leave out the ones
        // read natively above.
        let mut functions = BTreeMap::new();
        let mut i = 0;
        while i < toks.len() {
            if toks[i] == Tok::Word("function".into())
                && let Tok::Word(name) = &toks[i + 1]
            {
                let mut j = i + 3;
                let mut params = Vec::new();
                while toks[j] != Tok::Punct(')') {
                    if let Tok::Word(p) = &toks[j] {
                        params.push(p.clone());
                    }
                    j += 1;
                }
                j += 1;
                assert_eq!(toks[j], Tok::Punct('{'));
                let start = j + 1;
                let mut depth = 1;
                while depth > 0 {
                    j += 1;
                    match toks[j] {
                        Tok::Punct('{') => depth += 1,
                        Tok::Punct('}') => depth -= 1,
                        _ => {}
                    }
                }
                if name == "run_tests_on_anplusb_selector" {
                    functions.insert(name.clone(), (params, toks[start..j].to_vec()));
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
        // Run only the top-level statements: drop function definitions.
        let mut top = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if toks[i] == Tok::Word("function".into()) {
                let mut j = i;
                while toks[j] != Tok::Punct('{') {
                    j += 1;
                }
                let mut depth = 1;
                while depth > 0 {
                    j += 1;
                    match toks[j] {
                        Tok::Punct('{') => depth += 1,
                        Tok::Punct('}') => depth -= 1,
                        _ => {}
                    }
                }
                i = j + 1;
                continue;
            }
            top.push(toks[i].clone());
            i += 1;
        }
        run(&top, &Env::new(), &functions, &mut out);
    }
    out
}

/// Our result for a selector: its serialization, or `None` if invalid.
/// Panics if "parse a selector" and the style rule route disagree, or if a
/// serialization doesn't parse back to itself.
fn ours(input: &str) -> Option<String> {
    let ns = Namespaces::new();
    let direct = parse_selector(input, &ns).ok().map(|l| l.to_string());
    let rule_text = format!("{input}{{}}");
    let (parsed, rule) = parse_rule(Wtf8::from_str(&rule_text), &mut AcceptAll);
    let via_rule = match rule.ok().map(|id| &parsed.rules[id as usize]) {
        Some(Rule::Qualified { prelude, .. }) => parse_selector_list(&parsed, prelude.clone(), &ns)
            .ok()
            .map(|l| l.to_string()),
        _ => None,
    };
    assert_eq!(
        direct, via_rule,
        "{input:?}: parse a selector and the style rule disagree"
    );
    if let Some(s) = &direct {
        let again = parse_selector(s, &ns)
            .unwrap_or_else(|e| panic!("{input:?} serialized to {s:?}, which is invalid: {e:?}"));
        assert_eq!(
            &again.to_string(),
            s,
            "{input:?}: serialization does not round-trip"
        );
    }
    direct
}

#[test]
fn wpt_selector_parsing() {
    let dir = build_dir().join("wpt-css/css/selectors/parsing");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|_| {
            panic!(
                "{} not found (run browser/scripts/fetch-test-suites.sh)",
                dir.display()
            )
        })
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.ends_with(".html"))
        .collect();
    files.sort();
    let errata: BTreeMap<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(conformance_dir().join("wpt-selectors-errata.json")).unwrap(),
    )
    .unwrap();
    let mut used = Vec::new();
    let mut failures = Vec::new();
    let (mut total, mut passed) = (0, 0);
    for file in &files {
        if SKIPPED.iter().any(|(f, _)| f == file) {
            continue;
        }
        for (n, case) in cases(file).into_iter().enumerate() {
            total += 1;
            let key = format!("{file}#{n}");
            let got = ours(&case.input);
            let matches = match (&case.expect, &got) {
                (Expect::Valid(options), Some(s)) => options.contains(s),
                (Expect::Invalid, None) => true,
                _ => false,
            };
            match errata.get(&key) {
                Some(e) => {
                    used.push(key.clone());
                    assert_eq!(
                        e["input"].as_str(),
                        Some(case.input.as_str()),
                        "{key}: erratum input changed"
                    );
                    let want = e["expected"].as_str().map(str::to_owned);
                    if matches {
                        failures.push(format!(
                            "{key}: erratum no longer needed ({:?})",
                            case.input
                        ));
                    } else if got != want {
                        failures.push(format!(
                            "{key}: {:?}: erratum expects {want:?}, got {got:?}",
                            case.input
                        ));
                    } else {
                        passed += 1;
                    }
                }
                None if matches => passed += 1,
                None => failures.push(format!(
                    "{key}: {:?}: expected {:?}, got {got:?}",
                    case.input, case.expect
                )),
            }
        }
    }
    for key in errata.keys() {
        if !used.contains(key) {
            failures.push(format!("{key}: erratum matches no case"));
        }
    }
    eprintln!("wpt css/selectors/parsing: {passed}/{total}");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // Pin the count so a change in the reader shows up.
    assert_eq!(total, EXPECTED_CASES, "number of cases read changed");
}

const EXPECTED_CASES: usize = 352;
