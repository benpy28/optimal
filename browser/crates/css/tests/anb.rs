//! The An+B microsyntax (css-syntax-3 section 6), tested four ways:
//!
//! - SimonSapin/css-parsing-tests `An+B.json` (CC0), all cases.
//! - web-platform-tests `css/css-syntax/anb-parsing.html` and
//!   `anb-serialization.html` (BSD-3-Clause): each `testANB(input, output)`
//!   call is read from the file, parsed and serialized.
//! - Hand-worked cases for clamping, escapes, blocks and the prefix form.
//! - Random token streams against a literal model of the grammar of 6.2,
//!   which writes out each of its clauses as a sequence of token matchers.

use optimal_css::{AnB, Token, Values, parse_anb, parse_anb_prefix, parse_component_values};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;
use serde_json::Value;
use std::path::PathBuf;

fn build_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build")
}

fn anb(src: &str) -> Option<AnB> {
    let (p, range) = parse_component_values(Wtf8::from_str(src));
    parse_anb(p.values(range))
}

#[test]
fn css_parsing_tests_anb() {
    let path = build_dir().join("css-parsing-tests/An+B.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} not found (run browser/scripts/fetch-test-suites.sh)",
            path.display()
        )
    });
    let tests: Vec<Value> = serde_json::from_str(&text).unwrap();
    let mut failures = Vec::new();
    for pair in tests.chunks(2) {
        let input = pair[0].as_str().unwrap();
        let expected = match &pair[1] {
            Value::Null => None,
            Value::Array(ab) => Some(AnB {
                a: ab[0].as_i64().unwrap() as i32,
                b: ab[1].as_i64().unwrap() as i32,
            }),
            other => panic!("unexpected expectation {other}"),
        };
        let ours = anb(input);
        if ours != expected {
            failures.push(format!("{input:?}: expected {expected:?}, got {ours:?}"));
        }
    }
    assert_eq!(tests.len() / 2, 128, "An+B.json changed size");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The `testANB("…", "…")` calls in a WPT file, as (input, expected).
fn wpt_cases(file: &str) -> Vec<(String, String)> {
    let path = build_dir().join("wpt-css/css/css-syntax").join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} not found (run browser/scripts/fetch-test-suites.sh)",
            path.display()
        )
    });
    let mut cases = Vec::new();
    for line in text.lines() {
        let Some(args) = line.trim().strip_prefix("testANB(\"") else {
            continue;
        };
        let (input, rest) = args.split_once("\", \"").expect("two string arguments");
        let expected = rest.strip_suffix("\");").expect("call ends the line");
        assert!(
            !input.contains('\\') && !expected.contains('\\'),
            "escapes in a JS string are not read here: {line}"
        );
        cases.push((input.to_owned(), expected.to_owned()));
    }
    cases
}

fn run_wpt(file: &str, count: usize) {
    let cases = wpt_cases(file);
    assert_eq!(cases.len(), count, "{file} changed size");
    let mut failures = Vec::new();
    for (input, expected) in &cases {
        let ours = match anb(input) {
            Some(v) => v.to_string(),
            None => "parse error".to_owned(),
        };
        if &ours != expected {
            failures.push(format!("{input:?}: expected {expected:?}, got {ours:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn wpt_anb_parsing() {
    run_wpt("anb-parsing.html", 67);
}

#[test]
fn wpt_anb_serialization() {
    run_wpt("anb-serialization.html", 20);
}

#[test]
fn clamps_to_i32() {
    let ab = |a, b| Some(AnB { a, b });
    assert_eq!(anb("2147483647"), ab(0, i32::MAX));
    assert_eq!(anb("2147483648"), ab(0, i32::MAX));
    assert_eq!(anb("-2147483648"), ab(0, i32::MIN));
    assert_eq!(anb("-2147483649"), ab(0, i32::MIN));
    assert_eq!(
        anb("1e400"),
        None,
        "a number with an exponent is not an integer"
    );
    assert_eq!(anb("99999999999999999999n"), ab(i32::MAX, 0));
    assert_eq!(anb("-99999999999999999999n"), ab(i32::MIN, 0));
    // B is negated before it is clamped, so it reaches i32::MIN, not
    // -i32::MAX.
    assert_eq!(anb("n- 99999999999999999999"), ab(1, i32::MIN));
    assert_eq!(anb("-n-2147483648"), ab(-1, i32::MIN));
    assert_eq!(anb("n-2147483649"), ab(1, i32::MIN));
    assert_eq!(anb("n - 2147483648"), ab(1, i32::MIN));
    assert_eq!(anb("n + 2147483648"), ab(1, i32::MAX));
    assert_eq!(anb("5n-999999999999999999999999"), ab(5, i32::MIN));
}

#[test]
fn values_are_unescaped_first() {
    // The grammar is over token values, so escapes that spell "n" or
    // "odd" count (6.2 compares the ident's value).
    assert_eq!(anb("\\6e"), Some(AnB { a: 1, b: 0 }));
    assert_eq!(anb("\\6f dd"), Some(AnB { a: 2, b: 1 }));
    assert_eq!(anb("3\\6e-1"), Some(AnB { a: 3, b: -1 }));
    assert_eq!(anb("-\\6e-\\31"), Some(AnB { a: -1, b: -1 }));
    // Only ASCII digits count after "n-".
    assert_eq!(anb("n-\u{663}"), None);
    // Matching is ASCII case-insensitive only: fullwidth "n" and a dotless
    // "ı" in "odd"-like words don't count.
    assert_eq!(anb("\u{ff4e}"), None);
    assert_eq!(anb("EVEN"), Some(AnB { a: 2, b: 0 }));
    assert_eq!(anb("evın"), None);
}

#[test]
fn functions_and_blocks_never_match() {
    for src in [
        "n(", "(n)", "[1]", "{odd}", "n-(1)", "3n + (1)", "n + f(1)", "+(n)",
    ] {
        assert_eq!(anb(src), None, "{src:?}");
    }
}

#[test]
fn comments_are_not_whitespace() {
    // Comments are not tokens, so "+/**/n" has no whitespace between "+"
    // and "n" and matches; "+ n" does not.
    assert_eq!(anb("+/**/n"), Some(AnB { a: 1, b: 0 }));
    assert_eq!(anb("+ n"), None);
    assert_eq!(anb("/**/3n/**/+/**/1/**/"), Some(AnB { a: 3, b: 1 }));
}

#[test]
fn prefix_form() {
    let prefix = |src: &str| {
        let (p, range) = parse_component_values(Wtf8::from_str(src));
        parse_anb_prefix(p.values(range)).map(|(v, rest): (AnB, Values<'_, '_>)| {
            let rest: Vec<String> = rest
                .map(|v| match v.token() {
                    Token::Whitespace => " ".to_owned(),
                    Token::Ident(s) => s.to_string(),
                    t => format!("{t:?}"),
                })
                .collect();
            (v.a, v.b, rest.concat())
        })
    };
    assert_eq!(prefix(" 2n+1 of p"), Some((2, 1, " of p".to_owned())));
    assert_eq!(prefix("odd of p"), Some((2, 1, " of p".to_owned())));
    assert_eq!(prefix("-n- 3 of"), Some((-1, -3, " of".to_owned())));
    // The longest prefix that matches: "n" alone when the B part after it
    // is cut off.
    assert_eq!(prefix("n + of"), Some((1, 0, " Delim('+') of".to_owned())));
    assert_eq!(prefix("n +"), Some((1, 0, " Delim('+')".to_owned())));
    assert_eq!(prefix("3n -"), Some((3, 0, " Delim('-')".to_owned())));
    assert_eq!(prefix("3n of"), Some((3, 0, " of".to_owned())));
    // "n-" needs its integer; there is no shorter clause to fall back to.
    assert_eq!(prefix("n- of"), None);
    assert_eq!(prefix("of"), None);
    assert_eq!(prefix("   "), None);
    assert_eq!(prefix(""), None);
}

#[test]
fn serialization() {
    let s = |a, b| AnB { a, b }.to_string();
    assert_eq!(s(0, 0), "0");
    assert_eq!(s(0, -5), "-5");
    assert_eq!(s(0, 5), "5");
    assert_eq!(s(1, 0), "n");
    assert_eq!(s(-1, 0), "-n");
    assert_eq!(s(1, 1), "n+1");
    assert_eq!(s(-1, -1), "-n-1");
    assert_eq!(s(2, 0), "2n");
    assert_eq!(s(-2, 3), "-2n+3");
    assert_eq!(s(i32::MIN, i32::MIN), "-2147483648n-2147483648");
    assert_eq!(s(i32::MAX, i32::MAX), "2147483647n+2147483647");
}

#[test]
fn matches_hand_worked() {
    let m = |a, b, i| AnB { a, b }.matches(i);
    // odd: 1, 3, 5
    assert!(m(2, 1, 1) && !m(2, 1, 2) && m(2, 1, 3));
    // even: 2, 4 (n = 0 gives index 0, which no element has, though the
    // arithmetic still holds)
    assert!(!m(2, 0, 1) && m(2, 0, 2) && m(2, 0, 0));
    // -n+3: the first three
    assert!(m(-1, 3, 1) && m(-1, 3, 3) && !m(-1, 3, 4));
    // 0n+5: only the fifth
    assert!(m(0, 5, 5) && !m(0, 5, 4) && !m(0, 5, 10));
    // 3n-1: 2, 5, 8 (n = 0 gives -1)
    assert!(m(3, -1, 2) && m(3, -1, 5) && !m(3, -1, 3));
    // -2n-1 matches nothing
    assert!((0..20).all(|i| !m(-2, -1, i)));
    // Extremes don't overflow.
    assert!(m(i32::MIN, i32::MAX, i32::MAX as u32));
    assert!(!m(i32::MIN, i32::MAX, u32::MAX));
    assert!(
        !m(i32::MAX, i32::MIN, u32::MAX),
        "6442450943 is not a multiple"
    );
    assert!(m(i32::MAX, i32::MIN, 2_147_483_646), "n = 2");
    assert!(m(1, i32::MIN, u32::MAX));
}

// ---- The literal model -------------------------------------------------

/// A non-whitespace token as the model sees it, with whether whitespace
/// came right before it.
#[derive(Clone, Debug)]
struct T {
    ws_before: bool,
    kind: TK,
}

#[derive(Clone, Debug)]
enum TK {
    Ident(String),
    /// A number-token: value, integer type flag, sign written.
    Num(f64, bool, bool),
    /// A dimension-token: value, integer type flag, unit.
    Dim(f64, bool, String),
    Delim(char),
    Other,
}

/// The matchers the grammar of 6.2 is written with.
#[derive(Clone, Copy, Debug)]
enum M {
    Odd,
    Even,
    Integer,
    NDimension,
    NDashDimension,
    NDashDigitDimension,
    NDashDigitIdent,
    DashNDashDigitIdent,
    SignedInteger,
    SignlessInteger,
    /// The ident "n", "-n", "n-" or "-n-" (ASCII case-insensitive).
    Keyword(&'static str),
    /// A '+' delim, with no whitespace between it and the next token.
    PlusNoSpace,
    /// '+' or '-'.
    PlusOrMinus,
}

fn ci(s: &str, pat: &str) -> bool {
    s.eq_ignore_ascii_case(pat)
}

/// "n-*" (or "-n-*" with `dash`), * one or more ASCII digits.
fn dashdigit(s: &str, dash: bool) -> bool {
    let s = if dash {
        match s.strip_prefix('-') {
            Some(s) => s,
            None => return false,
        }
    } else {
        s
    };
    let b = s.as_bytes();
    b.len() >= 3
        && (b[0] == b'n' || b[0] == b'N')
        && b[1] == b'-'
        && b[2..].iter().all(u8::is_ascii_digit)
}

fn matches(m: M, t: &T) -> bool {
    match (m, &t.kind) {
        (M::Odd, TK::Ident(s)) => ci(s, "odd"),
        (M::Even, TK::Ident(s)) => ci(s, "even"),
        (M::Integer, TK::Num(_, int, _)) => *int,
        (M::NDimension, TK::Dim(_, int, u)) => *int && ci(u, "n"),
        (M::NDashDimension, TK::Dim(_, int, u)) => *int && ci(u, "n-"),
        (M::NDashDigitDimension, TK::Dim(_, int, u)) => *int && dashdigit(u, false),
        (M::NDashDigitIdent, TK::Ident(s)) => dashdigit(s, false),
        (M::DashNDashDigitIdent, TK::Ident(s)) => dashdigit(s, true),
        (M::SignedInteger, TK::Num(_, int, signed)) => *int && *signed,
        (M::SignlessInteger, TK::Num(_, int, signed)) => *int && !*signed,
        (M::Keyword(k), TK::Ident(s)) => ci(s, k),
        (M::PlusNoSpace, TK::Delim('+')) => true,
        (M::PlusOrMinus, TK::Delim(c)) => *c == '+' || *c == '-',
        _ => false,
    }
}

fn num(t: &T) -> f64 {
    match &t.kind {
        TK::Num(v, ..) | TK::Dim(v, ..) => *v,
        _ => unreachable!(),
    }
}

/// The digits after "n-" in a unit or ident, as the negative number they
/// spell ("with the first code point removed").
fn digits_b(t: &T) -> f64 {
    let s = match &t.kind {
        TK::Ident(s) | TK::Dim(_, _, s) => s.as_str(),
        _ => unreachable!(),
    };
    // "n-5" loses one code point, "-n-5" two; "-5" is left either way.
    let rest = if s.starts_with('-') { &s[2..] } else { &s[1..] };
    rest.parse().unwrap()
}

type Clause = (&'static [M], fn(&[T]) -> (f64, f64));

/// Every clause of the `<an+b>` production, in the spec's order, with the
/// interpretation the spec gives it. `'+'?` is written as two clauses.
fn clauses() -> Vec<Clause> {
    use M::*;
    vec![
        (&[Odd], |_| (2.0, 1.0)),
        (&[Even], |_| (2.0, 0.0)),
        (&[Integer], |t| (0.0, num(&t[0]))),
        (&[NDimension], |t| (num(&t[0]), 0.0)),
        (&[Keyword("n")], |_| (1.0, 0.0)),
        (&[PlusNoSpace, Keyword("n")], |_| (1.0, 0.0)),
        (&[Keyword("-n")], |_| (-1.0, 0.0)),
        (&[NDashDigitDimension], |t| (num(&t[0]), digits_b(&t[0]))),
        (&[NDashDigitIdent], |t| (1.0, digits_b(&t[0]))),
        (&[PlusNoSpace, NDashDigitIdent], |t| (1.0, digits_b(&t[1]))),
        (&[DashNDashDigitIdent], |t| (-1.0, digits_b(&t[0]))),
        (&[NDimension, SignedInteger], |t| (num(&t[0]), num(&t[1]))),
        (&[Keyword("n"), SignedInteger], |t| (1.0, num(&t[1]))),
        (&[PlusNoSpace, Keyword("n"), SignedInteger], |t| {
            (1.0, num(&t[2]))
        }),
        (&[Keyword("-n"), SignedInteger], |t| (-1.0, num(&t[1]))),
        (&[NDashDimension, SignlessInteger], |t| {
            (num(&t[0]), -num(&t[1]))
        }),
        (&[Keyword("n-"), SignlessInteger], |t| (1.0, -num(&t[1]))),
        (&[PlusNoSpace, Keyword("n-"), SignlessInteger], |t| {
            (1.0, -num(&t[2]))
        }),
        (&[Keyword("-n-"), SignlessInteger], |t| (-1.0, -num(&t[1]))),
        (&[NDimension, PlusOrMinus, SignlessInteger], |t| {
            (num(&t[0]), signed(&t[1], num(&t[2])))
        }),
        (&[Keyword("n"), PlusOrMinus, SignlessInteger], |t| {
            (1.0, signed(&t[1], num(&t[2])))
        }),
        (
            &[PlusNoSpace, Keyword("n"), PlusOrMinus, SignlessInteger],
            |t| (1.0, signed(&t[2], num(&t[3]))),
        ),
        (&[Keyword("-n"), PlusOrMinus, SignlessInteger], |t| {
            (-1.0, signed(&t[1], num(&t[2])))
        }),
    ]
}

fn signed(sign: &T, v: f64) -> f64 {
    match sign.kind {
        TK::Delim('-') => -v,
        _ => v,
    }
}

fn clamp(v: f64) -> i32 {
    v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// The model's result for exactly `tokens`. Fails the test if two clauses
/// match with different results (the grammar is unambiguous).
fn model(tokens: &[T]) -> Option<AnB> {
    let mut found = None;
    for (ms, f) in clauses() {
        if ms.len() != tokens.len() {
            continue;
        }
        let ok = ms.iter().zip(tokens).enumerate().all(|(i, (m, t))| {
            matches(*m, t)
                && !(matches!(m, M::PlusNoSpace) && tokens.get(i + 1).is_some_and(|n| n.ws_before))
        });
        if ok {
            let (a, b) = f(tokens);
            let v = AnB {
                a: clamp(a),
                b: clamp(b),
            };
            assert!(
                found.is_none_or(|f| f == v),
                "two clauses disagree on {tokens:?}"
            );
            found = Some(v);
        }
    }
    found
}

/// The model's tokens for `values`, each with the index after it.
fn model_tokens(values: Values<'_, '_>) -> Vec<(T, u32)> {
    let mut out = Vec::new();
    let mut ws = false;
    for v in values {
        let kind = match v.token() {
            Token::Whitespace => {
                ws = true;
                continue;
            }
            Token::Ident(s) => TK::Ident(s.to_string()),
            Token::Number(n) => TK::Num(n.value, n.is_integer, n.sign.is_some()),
            Token::Dimension(n, u) => TK::Dim(n.value, n.is_integer, u.to_string()),
            Token::Delim(c) => TK::Delim(*c),
            _ => TK::Other,
        };
        out.push((
            T {
                ws_before: ws,
                kind,
            },
            v.index(),
        ));
        ws = false;
    }
    out
}

const PIECES: &[&str] = &[
    "n",
    "N",
    "-n",
    "-N",
    "n-",
    "-n-",
    "n-5",
    "N-05",
    "-n-12",
    "n-x",
    "+",
    "-",
    "0",
    "5",
    "+5",
    "-5",
    "12",
    "3n",
    "-3n",
    "+3n",
    "3N-",
    "3n-7",
    "3n-",
    "0n",
    "odd",
    "EVEN",
    "ödd",
    "3.0",
    "3.0n",
    "1e1",
    "x",
    "of",
    "(",
    ")",
    "f(",
    "[",
    "]",
    "2147483648",
    "99999999999n",
    "-n-99999999999",
    "\\6e",
    "--n",
    "+n",
    "/**/",
    " ",
    " ",
    "\t",
    "%",
    "5%",
];

fn src_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..7).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000)
    ))]

    #[test]
    fn whole_matches_model(src in src_strategy()) {
        let (p, range) = parse_component_values(Wtf8::from_str(&src));
        let toks: Vec<T> = model_tokens(p.values(range.clone())).into_iter().map(|t| t.0).collect();
        prop_assert_eq!(parse_anb(p.values(range)), model(&toks), "{:?}", src);
    }

    #[test]
    fn prefix_matches_model(src in src_strategy()) {
        let (p, range) = parse_component_values(Wtf8::from_str(&src));
        let toks = model_tokens(p.values(range.clone()));
        let plain: Vec<T> = toks.iter().map(|t| t.0.clone()).collect();
        // The longest k with tokens[..k] matching; the rest starts right
        // after the k-th token.
        let expected = (1..=plain.len()).rev().find_map(|k| {
            model(&plain[..k]).map(|v| {
                let after = p.values(range.clone())
                    .find(|v| v.index() > toks[k - 1].1)
                    .map_or(range.end, |v| v.index());
                (v, after)
            })
        });
        let ours = parse_anb_prefix(p.values(range.clone()))
            .map(|(v, rest)| (v, rest.range().start));
        prop_assert_eq!(ours, expected, "{:?}", src);
    }

    #[test]
    fn serialization_round_trips(a in any::<i32>(), b in any::<i32>()) {
        let v = AnB { a, b };
        prop_assert_eq!(anb(&v.to_string()), Some(v));
    }

    #[test]
    fn matches_agrees_with_search(a in -6i32..7, b in -12i32..13, index in 0u32..40) {
        let v = AnB { a, b };
        let brute = (0i64..100).any(|n| i64::from(a) * n + i64::from(b) == i64::from(index));
        prop_assert_eq!(v.matches(index), brute);
    }
}
