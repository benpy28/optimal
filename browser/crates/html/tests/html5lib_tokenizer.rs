//! Runs the html5lib-tests tokenizer suite (MIT licence, see
//! `browser/conformance/README.md`) against our tokenizer.
//!
//! Every test is run three ways and all three must give the same, expected,
//! tokens and parse errors (code, line and column):
//! 1. the whole input in one chunk,
//! 2. one code point per chunk (the hardest streaming case),
//! 3. random chunk sizes from a seeded generator (seed printed on failure).
//!
//! The suite location comes from `HTML5LIB_TESTS` (default:
//! `browser/build/html5lib-tests`, filled by `browser/scripts/fetch-test-suites.sh`).
//! A missing suite fails the test rather than skipping it, so CI can never
//! go green without running it.

use optimal_html::{
    InitialState, ParseError, SinkResult, Tag, TagKind, Token, TokenSink, Tokenizer,
};
use optimal_wtf8::{Wtf8, Wtf8Buf};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
enum T {
    Doctype(Option<Wtf8Buf>, Option<Wtf8Buf>, Option<Wtf8Buf>, bool),
    Start(Wtf8Buf, BTreeMap<Wtf8Buf, Wtf8Buf>, bool),
    End(Wtf8Buf),
    Comment(Wtf8Buf),
    Pi(Wtf8Buf, Wtf8Buf),
    Chars(Wtf8Buf),
}

#[derive(Default)]
struct Collect {
    tokens: Vec<T>,
    errors: Vec<(String, u64, u64)>,
    eof: bool,
}

impl TokenSink for Collect {
    fn process(&mut self, token: Token<'_>) -> SinkResult {
        assert!(!self.eof, "token after EOF");
        match token {
            Token::Doctype(d) => self.tokens.push(T::Doctype(
                d.name.clone(),
                d.public_id.clone(),
                d.system_id.clone(),
                !d.force_quirks,
            )),
            Token::Tag(Tag {
                kind: TagKind::Start,
                name,
                self_closing,
                attributes,
            }) => self.tokens.push(T::Start(
                name.clone(),
                attributes
                    .iter()
                    .map(|a| (a.name.clone(), a.value.clone()))
                    .collect(),
                *self_closing,
            )),
            Token::Tag(t) => self.tokens.push(T::End(t.name.clone())),
            Token::Comment(c) => self.tokens.push(T::Comment(c.into())),
            Token::ProcessingInstruction { target, data } => {
                self.tokens.push(T::Pi(target.into(), data.into()))
            }
            Token::Characters(c) => {
                assert!(!c.is_empty(), "empty character token");
                if let Some(T::Chars(prev)) = self.tokens.last_mut() {
                    prev.push_wtf8(c);
                } else {
                    self.tokens.push(T::Chars(c.into()));
                }
            }
            Token::Eof => self.eof = true,
        }
        SinkResult::Continue
    }

    fn parse_error(&mut self, e: ParseError) {
        self.errors.push((
            e.code.as_str().to_owned(),
            e.position.line,
            e.position.column,
        ));
    }

    fn wants_errors(&self) -> bool {
        true
    }
}

/// Turn a JSON string into WTF-8, undoing the extra `\uXXXX` escaping that
/// `doubleEscaped` tests use for lone surrogates.
fn text(v: &Value, double_escaped: bool) -> Wtf8Buf {
    let s = v.as_str().expect("string");
    if !double_escaped {
        return Wtf8Buf::from(s);
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        if units[i] == b'\\' as u16
            && units.get(i + 1) == Some(&(b'u' as u16))
            && i + 6 <= units.len()
        {
            let hex: String = String::from_utf16(&units[i + 2..i + 6]).unwrap();
            out.push(u16::from_str_radix(&hex, 16).expect("hex escape"));
            i += 6;
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    Wtf8Buf::from_utf16(&out)
}

fn opt_text(v: &Value, de: bool) -> Option<Wtf8Buf> {
    if v.is_null() { None } else { Some(text(v, de)) }
}

fn expected_tokens(output: &Value, de: bool) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for t in output.as_array().unwrap() {
        let a = t.as_array().unwrap();
        let tok = match a[0].as_str().unwrap() {
            "DOCTYPE" => T::Doctype(
                opt_text(&a[1], de),
                opt_text(&a[2], de),
                opt_text(&a[3], de),
                a[4].as_bool().unwrap(),
            ),
            "StartTag" => T::Start(
                text(&a[1], de),
                a[2].as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (text(&Value::String(k.clone()), de), text(v, de)))
                    .collect(),
                a.get(3).and_then(Value::as_bool).unwrap_or(false),
            ),
            "EndTag" => T::End(text(&a[1], de)),
            "Comment" => T::Comment(text(&a[1], de)),
            "ProcessingInstruction" => {
                let target = text(&a[1], de);
                let data = text(&a[2], de);
                T::Pi(target, data)
            }
            "Character" => T::Chars(text(&a[1], de)),
            other => panic!("unknown token type {other}"),
        };
        // The suite coalesces adjacent character tokens; so do we.
        if let (T::Chars(c), Some(T::Chars(prev))) = (&tok, out.last_mut()) {
            prev.push_wtf8(c.as_wtf8());
            continue;
        }
        out.push(tok);
    }
    out
}

fn initial_state(name: &str) -> InitialState {
    match name {
        "Data state" => InitialState::Data,
        "PLAINTEXT state" => InitialState::Plaintext,
        "RCDATA state" => InitialState::Rcdata,
        "RAWTEXT state" => InitialState::Rawtext,
        "Script data state" => InitialState::ScriptData,
        "CDATA section state" => InitialState::CdataSection,
        other => panic!("unknown initial state {other}"),
    }
}

/// Small deterministic generator (xorshift64*), so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Split points at code point boundaries.
fn boundaries(input: Wtf8<'_>) -> Vec<usize> {
    (0..=input.len())
        .filter(|&i| input.is_boundary(i))
        .collect()
}

fn run(input: Wtf8<'_>, state: InitialState, last: Option<&str>, chunks: &[usize]) -> Collect {
    let mut t = Tokenizer::new();
    t.set_state(state);
    t.set_last_start_tag(last);
    let mut sink = Collect::default();
    let mut prev = 0;
    for &end in chunks.iter().chain(std::iter::once(&input.len())) {
        if end <= prev {
            continue;
        }
        t.feed(input.slice(prev, end));
        prev = end;
        assert_eq!(t.run(&mut sink), optimal_html::RunResult::NeedInput);
    }
    t.end();
    assert_eq!(t.run(&mut sink), optimal_html::RunResult::Done);
    assert!(sink.eof, "no EOF token");
    sink
}

fn suite_dir() -> PathBuf {
    match std::env::var_os("HTML5LIB_TESTS") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/html5lib-tests"),
    }
}

#[test]
fn html5lib_tokenizer_suite() {
    let dir = suite_dir().join("tokenizer");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| {
            panic!(
                "html5lib-tests not found at {} ({e}). Run browser/scripts/fetch-test-suites.sh",
                dir.display()
            )
        })
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "test"))
        .collect();
    files.sort();
    assert!(!files.is_empty());

    let seed: u64 = std::env::var("TOKENIZER_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x0005_EED0_F097_13A1);
    let random_rounds: usize = std::env::var("TOKENIZER_RANDOM_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let mut rng = Rng(seed);

    // Known mistakes in the suite's expected data, each with the reason and
    // the spec-derived correction. Our output is checked against the
    // correction, never skipped.
    let errata: Value = serde_json::from_str(include_str!(
        "../../../conformance/html5lib-tokenizer-errata.json"
    ))
    .unwrap();
    let errata = errata["errata"].as_array().unwrap();
    let mut errata_used = vec![false; errata.len()];
    let parse_errors = |v: &Value| -> Vec<(String, u64, u64)> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        (
                            e["code"].as_str().unwrap().to_owned(),
                            e["line"].as_u64().unwrap(),
                            e["col"].as_u64().unwrap(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut total = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut per_file: Vec<(String, usize, usize)> = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let json: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        let tests = json
            .get("tests")
            .or_else(|| json.get("xmlViolationTests"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // xmlViolation.test expects the "coercing an HTML DOM into an
        // infoset" tweaks, which belong to an XML serializer, not the HTML
        // tokenizer. It is counted as not applicable.
        if json.get("xmlViolationTests").is_some() {
            per_file.push((name, 0, 0));
            continue;
        }
        let (mut file_total, mut file_pass) = (0, 0);
        for test in &tests {
            let de = test
                .get("doubleEscaped")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let input = text(&test["input"], de);
            let expected = expected_tokens(&test["output"], de);
            let mut expected_errors = test.get("errors").map(&parse_errors).unwrap_or_default();
            for (i, e) in errata.iter().enumerate() {
                if e["file"] == name.as_str()
                    && e["description"] == test["description"]
                    && e["input"] == test["input"]
                {
                    assert_eq!(
                        parse_errors(&e["suite_errors"]),
                        expected_errors,
                        "erratum for {:?} no longer matches the suite: remove or update it",
                        e["description"]
                    );
                    expected_errors = parse_errors(&e["corrected_errors"]);
                    errata_used[i] = true;
                }
            }
            let states: Vec<String> = test
                .get("initialStates")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|s| s.as_str().unwrap().to_owned()).collect())
                .unwrap_or_else(|| vec!["Data state".to_owned()]);
            let last = test.get("lastStartTag").and_then(Value::as_str);
            for st in &states {
                total += 1;
                file_total += 1;
                let state = initial_state(st);
                let w = input.as_wtf8();
                let bounds = boundaries(w);
                let mut runs = vec![("whole".to_owned(), run(w, state, last, &[]))];
                runs.push(("per-code-point".to_owned(), run(w, state, last, &bounds)));
                for _ in 0..random_rounds {
                    let round_seed = rng.next();
                    let mut r = Rng(round_seed | 1);
                    let mut cuts: Vec<usize> = bounds
                        .iter()
                        .copied()
                        .filter(|_| r.next() % 3 == 0)
                        .collect();
                    cuts.sort();
                    runs.push((
                        format!("random seed {round_seed:#x}"),
                        run(w, state, last, &cuts),
                    ));
                }
                let mut ok = true;
                for (mode, got) in &runs {
                    if got.tokens != expected || got.errors != expected_errors {
                        ok = false;
                        failures.push(format!(
                            "{name}: {:?} [{st}] ({mode})\n  input:    {:?}\n  expected: {:?}\n  got:      {:?}\n  expected errors: {:?}\n  got errors:      {:?}",
                            test["description"].as_str().unwrap_or(""),
                            input,
                            expected,
                            got.tokens,
                            expected_errors,
                            got.errors
                        ));
                        break;
                    }
                }
                if ok {
                    file_pass += 1;
                }
            }
        }
        per_file.push((name, file_pass, file_total));
    }

    for (i, used) in errata_used.iter().enumerate() {
        assert!(
            used,
            "erratum {:?} matched no test: remove it",
            errata[i]["description"]
        );
    }
    println!("html5lib tokenizer results (seed {seed:#x}):");
    let mut passed = 0;
    for (name, p, t) in &per_file {
        passed += p;
        println!("  {name:32} {p:5}/{t}");
    }
    println!("  {:32} {passed:5}/{total}", "TOTAL");
    if let Ok(out) = std::env::var("CONFORMANCE_OUT") {
        let report = serde_json::json!({
            "suite": "html5lib-tests/tokenizer",
            "passed": passed,
            "total": total,
            "files": per_file.iter().map(|(n, p, t)| serde_json::json!({"file": n, "passed": p, "total": t})).collect::<Vec<_>>(),
        });
        std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    for f in failures.iter().take(40) {
        println!("{f}");
    }
    assert!(
        failures.is_empty(),
        "{} of {total} tokenizer tests failed",
        failures.len()
    );
}
