//! Matching checked against the literal model (`tests/common/match_reference.rs`)
//! on random small trees and random selectors, for every element, with each
//! kind of scoping root, in and out of quirks mode and nesting; and
//! `query_all` against filtering the subtree with `matches`.

mod common;

use common::match_reference::{Model, RefScope};
use common::tree::{El, HTML, Kind, Tree};
use optimal_selectors::{MatchContext, Namespaces, Scope, SelectorList, matches, parse_selector, query_all};
use proptest::prelude::*;

/// A random tree: (parent choice, node kind) pairs, each node attached
/// under an earlier element (or the document, or a fragment).
#[derive(Clone, Debug)]
struct Spec {
    nodes: Vec<(usize, u8, u8, u8)>,
    html: bool,
}

const NAMES: &[&str] = &["a", "b", "C", "div"];
const NSS: &[&str] = &[HTML, HTML, HTML, "", "urn:x"];
const ATTRS: &[(&str, &str)] = &[
    ("class", "x y"),
    ("class", "X"),
    ("id", "i"),
    ("id", "I"),
    ("lang", "en-US"),
    ("lang", "fr"),
    ("type", "A"),
    ("title", "a-b c"),
    ("TITLE", "ab"),
    ("data-v", ""),
];

fn spec() -> impl Strategy<Value = Spec> {
    (
        prop::collection::vec((any::<usize>(), any::<u8>(), any::<u8>(), any::<u8>()), 1..14),
        any::<bool>(),
    )
        .prop_map(|(nodes, html)| Spec { nodes, html })
}

/// Builds the tree: node 0 the document, then one root element, then the
/// spec's nodes; plus a fragment holding a copy of the root's first child.
fn build(s: &Spec) -> (Tree, usize) {
    let mut t = Tree::new(s.html);
    let root = t.element(HTML, "html", &[]);
    t.append(0, root);
    let mut elements = vec![root];
    for &(p, k, a, b) in &s.nodes {
        let parent = elements[p % elements.len()];
        let n = match k % 8 {
            0 => t.create(Kind::Text(if a % 2 == 0 { " \n".into() } else { "t".into() })),
            1 => t.create(Kind::Comment),
            _ => {
                let ns = NSS[(a as usize) % NSS.len()];
                let local = NAMES[(b as usize) % NAMES.len()];
                let mut attrs = Vec::new();
                for (bit, at) in ATTRS.iter().enumerate() {
                    if (u32::from(a) * 7 + u32::from(b) * 13 + bit as u32 * 5) % 3 == 0 && !attrs.iter().any(|(k2, _): &(&str, &str)| *k2 == at.0) {
                        attrs.push(*at);
                    }
                }
                let e = t.element(ns, local, &attrs);
                if k % 8 == 7 {
                    t.set_attribute(e, "urn:x", "title", "q");
                }
                elements.push(e);
                e
            }
        };
        t.append(parent, n);
    }
    let fragment = t.create(Kind::Fragment);
    let copy = t.clone_subtree(root);
    t.append(fragment, copy);
    (t, fragment)
}

const PARTS: &[&str] = &[
    "*", "*", "*", "*", "a", "div", ":not(c)", ":not(.x)", ":is(a, div, b)", "a", "b", "c", "C", "*", "div", "|a", "*|b", "x|a", ".x", ".X", "#i", "[title]", "[TITLE]", "[*|title]", "[x|title]",
    "[class~=y]", "[class~=Y i]", "[type=a]", "[type=a s]", "[title|=a]", "[title^=a]", "[title$=c]", "[title*='b c']",
    "[data-v]", "[data-v='']", ":root", ":empty", ":first-child", ":last-child", ":only-child", ":first-of-type",
    ":last-of-type", ":only-of-type", ":nth-child(2n+1)", ":nth-last-child(-n+2)", ":nth-of-type(2)",
    ":nth-last-of-type(odd)", ":scope", "&", ":lang(en)", ":lang(\"\")", ":lang('*')", ":dir(ltr)", ":not(a)", ":is(a, .x)",
];

const COMB: &[&str] = &[" ", " ", " > ", " + ", " ~ "];

fn compound() -> impl Strategy<Value = String> {
    let leaf = prop::collection::vec(prop::sample::select(PARTS), 1..3).prop_map(|v| {
        // A type or universal selector must come first.
        let mut v = v;
        v.sort_by_key(|p| !(p.starts_with(|c: char| c.is_ascii_alphabetic() || c == '*' || c == '|')));
        let first = v[0];
        let mut s = first.to_string();
        for p in &v[1..] {
            if p.starts_with(|c: char| c.is_ascii_alphabetic() || c == '*' || c == '|') {
                continue;
            }
            s += p;
        }
        s
    });
    leaf.prop_recursive(2, 8, 2, |inner| {
        let complex = (inner.clone(), prop::sample::select(COMB), inner.clone()).prop_map(|(a, c, b)| format!("{a}{c}{b}"));
        let rel = (prop::sample::select(&[">", "+", "~", ""][..]), complex.clone()).prop_map(|(c, s)| format!("{c} {s}"));
        prop_oneof![
            6 => inner.clone(),
            1 => (inner.clone(), complex.clone()).prop_map(|(a, s)| format!("{a}:is({s})")),
            1 => (inner.clone(), complex.clone()).prop_map(|(a, s)| format!("{a}:not({s})")),
            2 => (inner.clone(), rel).prop_map(|(a, s)| format!("{a}:has({s})")),
            1 => (inner.clone(), complex.clone()).prop_map(|(a, s)| format!("{a}:nth-child(2n of {s})")),
            1 => (inner.clone(), complex).prop_map(|(a, s)| format!("{a}:nth-last-child(1 of {s})")),
        ]
    })
}

fn selector() -> impl Strategy<Value = String> {
    prop::collection::vec((compound(), prop::sample::select(COMB)), 1..3).prop_map(|v| {
        let mut s = String::new();
        for (k, (u, c)) in v.iter().enumerate() {
            if k > 0 {
                s += c;
            }
            s += u;
        }
        s
    })
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096),
        ..ProptestConfig::default()
    }
}

fn namespaces() -> Namespaces {
    let mut ns = Namespaces::new();
    ns.declare("x", "urn:x");
    ns
}

fn elements(t: &Tree) -> Vec<usize> {
    (0..t.nodes.len()).filter(|&n| t.is_element(n)).collect()
}

fn check(t: &Tree, list: &SelectorList, scope: RefScope, quirks: bool, nesting: &[&SelectorList]) -> Result<(), TestCaseError> {
    let cx_scope = match scope {
        RefScope::None => Scope::None,
        RefScope::Element(e) => Scope::Element(El::new(t, e)),
        RefScope::Root(r) => Scope::Root {
            first_child: t.first_element_child(r).map(|i| El::new(t, i)),
        },
    };
    let cx = MatchContext {
        scope: cx_scope,
        quirks,
        nesting,
    };
    let model = Model {
        t,
        scope,
        quirks,
        nesting: nesting.iter().map(|l| (*l).clone()).collect(),
    };
    for n in elements(t) {
        let ours = matches(list, &El::new(t, n), &cx);
        let theirs = model.list(list, n);
        prop_assert_eq!(ours, theirs, "element {} of {:?}", n, t);
    }
    // query_all is matches() over the scope's subtree, in tree order.
    let root = match scope {
        RefScope::None => return Ok(()),
        RefScope::Element(e) => e,
        RefScope::Root(r) => r,
    };
    let want: Vec<usize> = t
        .subtree(root)
        .into_iter()
        .skip(1)
        .filter(|&n| t.is_element(n) && model.list(list, n))
        .collect();
    let got: Vec<usize> = query_all(list, &cx).into_iter().map(|e| e.i).collect();
    prop_assert_eq!(got, want);
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn matching_agrees_with_the_model(s in spec(), sel in selector(), parent in selector(), pick in any::<usize>(), quirks in any::<bool>()) {
        let ns = namespaces();
        let Ok(list) = parse_selector(&sel, &ns) else { return Ok(()) };
        let (t, fragment) = build(&s);
        let els = elements(&t);
        let some = els[pick % els.len()];
        check(&t, &list, RefScope::None, quirks, &[])?;
        check(&t, &list, RefScope::Element(some), quirks, &[])?;
        check(&t, &list, RefScope::Root(0), quirks, &[])?;
        check(&t, &list, RefScope::Root(fragment), quirks, &[])?;
        if let Ok(p) = parse_selector(&parent, &ns) {
            check(&t, &list, RefScope::Element(some), quirks, &[&p])?;
        }
    }
}

/// The generators reach both outcomes often: a check that the comparison
/// above is not between two matchers that seldom say yes.
#[test]
fn share_of_matches() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (mut parsed, mut mixed) = (0, 0);
    for _ in 0..2000 {
        let s = spec().new_tree(&mut runner).unwrap().current();
        let sel = selector().new_tree(&mut runner).unwrap().current();
        let Ok(list) = parse_selector(&sel, &namespaces()) else { continue };
        parsed += 1;
        let (t, _) = build(&s);
        let hits = elements(&t)
            .into_iter()
            .filter(|&n| matches(&list, &El::new(&t, n), &MatchContext::new()))
            .count();
        mixed += usize::from(hits > 0 && hits < elements(&t).len());
    }
    println!("{parsed} of 2000 parsed; {mixed} matched some elements but not all");
    assert!(parsed > 1500, "{parsed}");
    assert!(mixed * 8 > parsed, "{mixed} of {parsed}");
}

#[test]
#[ignore]
fn print_samples() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    for _ in 0..15 {
        let sel = selector().new_tree(&mut runner).unwrap().current();
        let s = spec().new_tree(&mut runner).unwrap().current();
        let (t, _) = build(&s);
        println!("{sel}   [{} elements]", elements(&t).len());
    }
}
