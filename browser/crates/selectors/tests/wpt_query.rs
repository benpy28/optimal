//! Runs the Selectors API tests of web-platform-tests (`dom/nodes`,
//! BSD 3-clause licence): the selectors and expected matches in
//! `selectors.js`, against the fixture `ParentNode-querySelector-All-content.html`
//! parsed with our HTML parser, set up as `ParentNode-querySelector-All.html`
//! and `ParentNode-querySelector-All.js` do it, in its four contexts
//! (document, element in the document, detached element, fragment).
//!
//! Every case of `validSelectors` marked for `querySelectorAll()` runs
//! through [`query_all`] (and `querySelector()`, its first result); every
//! case of `invalidSelectors` must fail to parse. The special tests that
//! need no script engine run too: `null` and `undefined` as tag names, and
//! `*` giving tree order. Corrections where the suite contradicts the
//! specifications are in `browser/conformance/wpt-query-errata.json`;
//! a stale erratum fails.
//!
//! Suite location: `WPT_DOM` (default `browser/build/wpt-dom/dom/nodes`,
//! filled by `browser/scripts/fetch-test-suites.sh`). A missing suite fails.

mod common;

use common::js::{Tok, lex};
use common::tree::{El, HTML, Kind, Tree};
use optimal_html::tree_builder::{ParseOptions, parse_document};
use optimal_selectors::{MatchContext, Namespaces, Scope, parse_selector, query_all};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn suite_dir() -> PathBuf {
    std::env::var_os("WPT_DOM")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/wpt-dom/dom/nodes")
        })
}

/// One `{...}` entry of a selector array.
#[derive(Debug, Default)]
struct Entry {
    name: String,
    selector: String,
    expect: Vec<String>,
    exclude: Vec<String>,
    qsa: bool,
}

/// The entries of `var <name> = [ ... ];`.
fn entries(toks: &[Tok], name: &str) -> Vec<Entry> {
    let start = toks
        .windows(3)
        .position(|w| {
            w[0] == Tok::Word("var".into())
                && w[1] == Tok::Word(name.into())
                && w[2] == Tok::Punct('=')
        })
        .unwrap_or_else(|| panic!("no {name}"));
    let mut i = start + 3;
    assert_eq!(toks[i], Tok::Punct('['));
    i += 1;
    let mut out = Vec::new();
    while toks[i] != Tok::Punct(']') {
        assert_eq!(toks[i], Tok::Punct('{'), "at {:?}", &toks[i..i + 5]);
        i += 1;
        let mut e = Entry::default();
        while toks[i] != Tok::Punct('}') {
            let Tok::Word(key) = toks[i].clone() else {
                panic!("key {:?}", toks[i])
            };
            assert_eq!(toks[i + 1], Tok::Punct(':'));
            i += 2;
            let mut value = Vec::new();
            let mut depth = 0;
            while depth > 0 || !matches!(toks[i], Tok::Punct(',' | '}')) {
                match toks[i] {
                    Tok::Punct('[') => depth += 1,
                    Tok::Punct(']') => depth -= 1,
                    _ => {}
                }
                value.push(toks[i].clone());
                i += 1;
            }
            if toks[i] == Tok::Punct(',') {
                i += 1;
            }
            let strings = || -> Vec<String> {
                value
                    .iter()
                    .filter_map(|t| match t {
                        Tok::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect()
            };
            match key.as_str() {
                "name" => e.name = strings().concat(),
                "selector" => e.selector = strings().concat(),
                "expect" => e.expect = strings(),
                "exclude" => e.exclude = strings(),
                "testType" => e.qsa = value.contains(&Tok::Word("TEST_QSA".into())),
                "level" | "unexpected" | "ctx" | "ref" => {}
                other => panic!("unknown key {other}"),
            }
        }
        i += 1;
        if toks[i] == Tok::Punct(',') {
            i += 1;
        }
        out.push(e);
    }
    out
}

/// The document as the test page has it when the valid selector tests run
/// in `context`, and the node they run on.
struct Setup {
    tree: Tree,
    doc_root: usize,
    root: usize,
    detached: usize,
    fragment: usize,
    out_of_scope: usize,
    body: usize,
}

fn setup() -> Setup {
    let src =
        std::fs::read_to_string(suite_dir().join("ParentNode-querySelector-All-content.html"))
            .expect("the WPT dom/nodes suite (run browser/scripts/fetch-test-suites.sh)");
    let parsed = parse_document(&src, ParseOptions::default());
    assert_eq!(
        parsed.document.quirks_mode,
        optimal_dom::QuirksMode::NoQuirks
    );
    let mut t = Tree::from_dom(&parsed.document);
    // The page loads the fixture with the fragment #target.
    t.target = t.by_id(0, "target");
    let root = t.by_id(0, "root").unwrap();
    // setupSpecialElements(doc, root)
    for name in ["null", "undefined"] {
        let e = t.element(HTML, name, &[]);
        t.append(root, e);
    }
    for prefix in ["any-namespace", "no-namespace"] {
        let holder = t.element(HTML, "div", &[("id", prefix)]);
        for (k, ns) in [HTML, HTML, "", "http://www.example.org/ns"]
            .into_iter()
            .enumerate()
        {
            let id = format!("{prefix}-div{}", k + 1);
            let d = t.element(ns, "div", &[("id", &id)]);
            t.append(holder, d);
        }
        t.append(root, holder);
    }
    let i1 = t.by_id(0, "attr-presence-i1").unwrap();
    t.set_attribute(i1, "http://www.example.org/ns", "title", "");
    // The clones.
    let out_of_scope = t.clone_subtree(root);
    for n in t.subtree(out_of_scope) {
        if t.is_element(n) {
            t.set_attribute(n, "", "data-clone", "");
        }
    }
    let detached = t.clone_subtree(root);
    let fragment = t.create(Kind::Fragment);
    let fc = t.clone_subtree(root);
    t.append(fragment, fc);
    // verifyStaticList appends a div to each of document.body, the
    // detached element, the fragment and the in-document element.
    let body = t.by_id(0, "body").unwrap();
    for parent in [body, detached, fragment, root] {
        let d = t.element(HTML, "div", &[]);
        t.append(parent, d);
    }
    Setup {
        tree: t,
        doc_root: 0,
        root,
        detached,
        fragment,
        out_of_scope,
        body,
    }
}

fn scope_for<'t>(t: &'t Tree, n: usize) -> Scope<El<'t>> {
    match t.nodes[n].kind {
        Kind::Document | Kind::Fragment => Scope::Root {
            first_child: t.first_element_child(n).map(|i| El::new(t, i)),
        },
        _ => Scope::Element(El::new(t, n)),
    }
}

/// `querySelectorAll(q)` on node `n`: the matches' IDs, and whether any is
/// a clone; `None` if the selector doesn't parse.
fn qsa(t: &Tree, n: usize, q: &str) -> Option<Vec<(Option<String>, bool, usize)>> {
    let list = parse_selector(q, &Namespaces::new()).ok()?;
    let cx = MatchContext {
        scope: scope_for(t, n),
        ..MatchContext::new()
    };
    Some(
        query_all(&list, &cx)
            .into_iter()
            .map(|e| {
                (
                    t.attr(e.i, "", "id").map(String::from),
                    t.attr(e.i, "", "data-clone").is_some(),
                    e.i,
                )
            })
            .collect(),
    )
}

#[test]
fn wpt_query_selector_all() {
    let js = std::fs::read_to_string(suite_dir().join("selectors.js")).unwrap();
    let toks = lex(&js);
    let invalid = entries(&toks, "invalidSelectors");
    let valid = entries(&toks, "validSelectors");
    let errata: BTreeMap<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../conformance/wpt-query-errata.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut used = std::collections::BTreeSet::new();

    let mut s = setup();
    let mut failures = Vec::new();
    let mut cases = 0;
    let contexts = [
        ("Document", "document", s.doc_root),
        ("Detached Element", "detached", s.detached),
        ("Fragment", "fragment", s.fragment),
        ("In-document Element", "element", s.root),
    ];
    for (label, node_type, n) in contexts {
        if node_type == "element" {
            // Appended after the document tests, before the element ones.
            s.tree.append(s.body, s.out_of_scope);
        }
        let t = &s.tree;
        // runInvalidSelectorTest
        for e in &invalid {
            cases += 1;
            if qsa(t, n, &e.selector).is_some() {
                failures.push(format!(
                    "{label}: {}: {:?} should not parse",
                    e.name, e.selector
                ));
            }
        }
        // runSpecialSelectorTests: null and undefined, and tree order.
        for name in ["null", "undefined"] {
            cases += 1;
            let r = qsa(t, n, name).unwrap();
            if r.len() != 1 {
                failures.push(format!("{label}: {name} gave {} elements", r.len()));
            }
        }
        cases += 1;
        let all: Vec<usize> = qsa(t, n, "*").unwrap().into_iter().map(|r| r.2).collect();
        let order: Vec<usize> = t
            .subtree(n)
            .into_iter()
            .skip(1)
            .filter(|&x| t.is_element(x))
            .collect();
        if all != order {
            failures.push(format!("{label}: * is not in tree order"));
        }
        // runValidSelectorTest
        for e in &valid {
            if !e.qsa || e.exclude.iter().any(|x| x == node_type || x == "html") {
                continue;
            }
            cases += 1;
            let key = format!("{node_type} {}", e.selector);
            let expect: Vec<String> = match errata.get(&key) {
                Some(v) => {
                    used.insert(key.clone());
                    assert_ne!(
                        v["expect"],
                        serde_json::json!(e.expect),
                        "erratum {key} agrees with the suite"
                    );
                    v["expect"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect()
                }
                None => e.expect.clone(),
            };
            let Some(got) = qsa(t, n, &e.selector) else {
                failures.push(format!(
                    "{label}: {}: {:?} did not parse",
                    e.name, e.selector
                ));
                continue;
            };
            let ids: Vec<String> = got
                .iter()
                .map(|r| r.0.clone().unwrap_or_default())
                .collect();
            if ids != expect || got.iter().any(|r| r.1) {
                failures.push(format!(
                    "{label}: {}: {:?}\n  expected {expect:?}\n  got      {ids:?}{}",
                    e.name,
                    e.selector,
                    if got.iter().any(|r| r.1) {
                        " (with clones)"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    for k in errata.keys() {
        if !used.contains(k) {
            failures.push(format!("stale erratum {k}"));
        }
    }
    println!("{cases} cases, {} failures", failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases, EXPECTED_CASES, "the number of cases changed");
}

const EXPECTED_CASES: usize = 941;
