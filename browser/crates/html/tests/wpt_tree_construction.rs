//! Runs the tree construction tests (`html/syntax/parsing/resources/*.dat`
//! in web-platform-tests, BSD 3-clause licence, the former html5lib-tests
//! tree-construction suite) against our parser.
//!
//! Every test is parsed three ways and all three must give the expected tree
//! (and the first two the same parse errors, in the same order):
//! 1. the whole input in one chunk,
//! 2. one code point per chunk,
//! 3. random chunk sizes from a seeded generator (seed printed on failure).
//!
//! Tests with neither `#script-on` nor `#script-off` run with scripting both
//! off and on, as the suite's README requires.
//!
//! Tests that need a script engine (the `scripted_*` files, whose expected
//! trees assume `<script>` runs and calls `document.write`) are counted
//! separately as blocked, never as passes. Corrections where the suite
//! contradicts the standard live in `browser/conformance/wpt-tree-errata.json`.
//!
//! The parse error count is reported but not gated: the suite says only the
//! count matters, and many expectations are known to be stale. The tree is
//! the gate.
//!
//! Suite location: `WPT_PARSING` (default
//! `browser/build/wpt/html/syntax/parsing/resources`, filled by
//! `browser/scripts/fetch-test-suites.sh`). A missing suite fails.

mod common;

use common::dump;
use optimal_dom::{Namespace, QuirksMode};
use optimal_html::tree_builder::{FragmentContext, ParseOptions, Parser};
use optimal_wtf8::Wtf8;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
struct Test {
    line: usize,
    data: String,
    errors: usize,
    error_lines: Vec<String>,
    fragment: Option<String>,
    script: Option<bool>,
    document: String,
}

fn parse_dat(text: &str) -> Vec<Test> {
    let mut tests = Vec::new();
    let mut cur: Option<Test> = None;
    let mut section = "";
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut prev_blank = true;
    let finish = |t: &mut Test| {
        // The blank line separating tests belongs to no section.
        if t.document.ends_with('\n') {
            t.document.pop();
        }
    };
    for (i, line) in lines.iter().enumerate() {
        let starts_test =
            prev_blank && section != "data" && (*line == "#data" || *line == "#description");
        if starts_test {
            if let Some(mut t) = cur.take() {
                finish(&mut t);
                tests.push(t);
            }
            cur = Some(Test {
                line: i + 1,
                ..Test::default()
            });
            section = if *line == "#data" {
                "data"
            } else {
                "description"
            };
            prev_blank = false;
            continue;
        }
        prev_blank = line.is_empty();
        let t = cur.as_mut().expect("content before first test");
        match (section, *line) {
            ("description", "#data") => section = "data",
            ("data", "#errors") => {
                t.data.pop(); // the final newline is not part of the data
                section = "errors";
            }
            (_, "#new-errors") if section != "data" && section != "document" => section = "errors",
            (_, "#document-fragment") if section != "data" && section != "document" => {
                section = "fragment"
            }
            (_, "#script-on") if section != "data" && section != "document" => {
                t.script = Some(true)
            }
            (_, "#script-off") if section != "data" && section != "document" => {
                t.script = Some(false)
            }
            (_, "#document") if section != "data" && section != "document" => section = "document",
            ("description", _) => {}
            ("data", l) => {
                t.data.push_str(l);
                t.data.push('\n');
            }
            ("errors", l) => {
                if !l.is_empty() {
                    t.errors += 1;
                    t.error_lines.push(l.to_string());
                }
            }
            ("fragment", l) => t.fragment = Some(l.to_string()),
            ("document", l) => {
                t.document.push_str(l);
                t.document.push('\n');
            }
            (s, l) => panic!("unexpected line {l:?} in section {s} at line {}", i + 1),
        }
    }
    if let Some(mut t) = cur.take() {
        finish(&mut t);
        tests.push(t);
    }
    for t in &mut tests {
        while t.document.ends_with('\n') {
            t.document.pop();
        }
    }
    tests
}

fn context(spec: &str) -> FragmentContext {
    let (namespace, local) = if let Some(n) = spec.strip_prefix("svg ") {
        (Namespace::Svg, n)
    } else if let Some(n) = spec.strip_prefix("math ") {
        (Namespace::MathMl, n)
    } else {
        (Namespace::Html, spec)
    };
    FragmentContext {
        namespace,
        local_name: local.to_string(),
        attributes: vec![],
        quirks_mode: QuirksMode::NoQuirks,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn run(t: &Test, scripting: bool, chunks: &[usize]) -> (String, usize) {
    let (d, e) = run_errors(t, scripting, chunks);
    (d, e.len())
}

fn run_errors(t: &Test, scripting: bool, chunks: &[usize]) -> (String, Vec<String>) {
    let options = ParseOptions {
        scripting,
        iframe_srcdoc: false,
        collect_errors: true,
    };
    let mut p = match &t.fragment {
        Some(c) => Parser::new_fragment(&context(c), options),
        None => Parser::new(options),
    };
    let bytes = Wtf8::from_str(&t.data);
    let mut start = 0;
    for &end in chunks {
        p.feed(bytes.slice(start, end));
        start = end;
    }
    p.feed(bytes.slice(start, t.data.len()));
    let parsed = p.finish();
    parsed.document.check_invariants();
    let errors = parsed.errors.iter().map(|e| format!("{e:?}")).collect();
    (dump(&parsed), errors)
}

fn boundaries(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).skip(1).collect()
}

fn suite_dir() -> PathBuf {
    match std::env::var_os("WPT_PARSING") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/wpt/html/syntax/parsing/resources"),
    }
}

fn conformance_list(file: &str, key: &str) -> Vec<Value> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance")
        .join(file);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let list = v[key].as_array().unwrap().clone();
    for e in &list {
        let g = e["group"].as_str().expect("every entry has a group");
        assert!(
            v["reason_by_group"][g].is_string(),
            "group {g} in {file} has no reason"
        );
    }
    list
}

#[test]
fn wpt_tree_construction_suite() {
    let dir = suite_dir();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| {
            panic!(
                "WPT parsing tests not found at {} ({e}). Run browser/scripts/fetch-test-suites.sh",
                dir.display()
            )
        })
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "dat"))
        .collect();
    files.sort();
    assert!(!files.is_empty());

    let seed: u64 = std::env::var("TREE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x0007_12EE_5EED_0001);
    let random_rounds: usize = std::env::var("TREE_RANDOM_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let errata = conformance_list("wpt-tree-errata.json", "errata");
    let mut used = vec![false; errata.len()];
    let blocked_list = conformance_list("wpt-tree-blocked.json", "blocked");
    let mut blocked_used = vec![false; blocked_list.len()];
    let mut blocked_dom = 0;
    let mut error_diffs: std::collections::BTreeMap<i64, usize> = Default::default();
    let mut rng = Rng(seed);

    let mut per_file = Vec::new();
    let mut failures = Vec::new();
    let (mut total, mut passed, mut blocked, mut errors_match) = (0, 0, 0, 0);
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(f).unwrap();
        let needs_js = name.starts_with("scripted_");
        let (mut fp, mut ft, mut fb) = (0, 0, 0);
        for t in parse_dat(&text) {
            let modes: Vec<bool> = match t.script {
                Some(s) => vec![s],
                None => vec![false, true],
            };
            for scripting in modes {
                total += 1;
                ft += 1;
                if needs_js {
                    blocked += 1;
                    fb += 1;
                    continue;
                }
                let mut want = t.document.clone();
                for (i, e) in errata.iter().enumerate() {
                    if e["file"] == name.as_str()
                        && e["data"] == t.data.as_str()
                        && e["fragment"].as_str() == t.fragment.as_deref()
                        && e["scripting"].as_bool().is_none_or(|s| s == scripting)
                    {
                        assert_eq!(
                            e["suite_document"].as_str(),
                            Some(want.as_str()),
                            "erratum for {name}:{} no longer matches the suite: update or remove it",
                            t.line
                        );
                        want = e["corrected_document"].as_str().unwrap().to_string();
                        used[i] = true;
                    }
                }
                let (got, whole_errors) = run_errors(&t, scripting, &[]);
                let nerr = whole_errors.len();
                let mut ok = got == want;
                let mut how = "whole";
                if ok {
                    let (g, e) = run_errors(&t, scripting, &boundaries(&t.data));
                    if g != want {
                        ok = false;
                        how = "per code point";
                    } else if e != whole_errors {
                        ok = false;
                        how = "per code point (same tree, different parse errors)";
                    }
                }
                for _ in 0..random_rounds {
                    if !ok {
                        break;
                    }
                    let b = boundaries(&t.data);
                    let mut cuts: Vec<usize> =
                        b.iter().copied().filter(|_| rng.next() % 4 == 0).collect();
                    cuts.dedup();
                    let (g, _) = run(&t, scripting, &cuts);
                    if g != want {
                        ok = false;
                        how = "random chunks";
                    }
                }
                let bi = blocked_list.iter().position(|b| {
                    b["file"] == name.as_str()
                        && b["data"] == t.data.as_str()
                        && b["fragment"].as_str() == t.fragment.as_deref()
                });
                if let Some(bi) = bi {
                    blocked_used[bi] = true;
                    if ok {
                        failures.push(format!(
                            "{name}:{} now passes but is listed in wpt-tree-blocked.json: remove it",
                            t.line
                        ));
                    } else {
                        blocked_dom += 1;
                    }
                    continue;
                }
                if ok {
                    *error_diffs
                        .entry(nerr as i64 - t.errors as i64)
                        .or_default() += 1;
                    if nerr != t.errors && std::env::var_os("TREE_SHOW_ERROR_DIFFS").is_some() {
                        let (_, ours) = run_errors(&t, scripting, &[]);
                        println!(
                            "ERRDIFF {name}:{} scripting {scripting}\n#data\n{}\n#expected\n{}\n#ours\n{}\n",
                            t.line,
                            t.data,
                            t.error_lines.join("\n"),
                            ours.join("\n")
                        );
                    }
                    passed += 1;
                    fp += 1;
                    if nerr == t.errors {
                        errors_match += 1;
                    }
                } else {
                    failures.push(format!(
                        "FAIL {name}:{} ({how}, scripting {scripting}, fragment {:?})\n#data\n{}\n#want\n{want}\n#got\n{got}\n",
                        t.line, t.fragment, t.data
                    ));
                }
            }
        }
        per_file.push((name, fp, ft, fb));
    }
    for (i, u) in used.iter().enumerate() {
        assert!(
            *u,
            "erratum {:?} matched no test: remove it",
            errata[i]["description"]
        );
    }
    for (i, u) in blocked_used.iter().enumerate() {
        assert!(
            *u,
            "blocked entry {:?} matched no test: remove it",
            blocked_list[i]["data"]
        );
    }
    let runnable = total - blocked - blocked_dom;
    println!("WPT tree construction results (seed {seed:#x}):");
    for (n, p, t, b) in &per_file {
        if *b > 0 {
            println!("  {n:40} {p:5}/{t} ({b} blocked on the JS engine)");
        } else {
            println!("  {n:40} {p:5}/{t}");
        }
    }
    println!(
        "  {:40} {passed:5}/{runnable} runnable; blocked: {blocked} on the JS engine, {blocked_dom} on DOM element behaviour",
        "TOTAL"
    );
    println!("  ours minus expected parse error count: {error_diffs:?}");
    println!("  parse error count matches in {errors_match}/{passed} passing tests (not gated)");
    if let Ok(out) = std::env::var("TREE_CONFORMANCE_OUT") {
        let report = serde_json::json!({
            "suite": "wpt/html/syntax/parsing (tree construction)",
            "passed": passed,
            "runnable": runnable,
            "blocked_on_js": blocked,
            "blocked_on_dom": blocked_dom,
            "error_count_matches": errors_match,
            "files": per_file.iter().map(|(n, p, t, b)| serde_json::json!({"file": n, "passed": p, "total": t, "blocked": b})).collect::<Vec<_>>(),
        });
        std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    let limit: usize = std::env::var("TREE_SHOW_FAILURES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    for f in failures.iter().take(limit) {
        println!("{f}");
    }
    assert!(
        failures.is_empty(),
        "{} of {runnable} runnable tree tests failed",
        failures.len()
    );
}
