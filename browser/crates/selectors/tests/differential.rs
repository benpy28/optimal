//! The crate against the literal model in `common/reference.rs`, on random
//! selector-like text: both must agree on validity and serialization.
//! Seeded by proptest (failures print the seed and the shrunk input; the
//! case count is PROPTEST_CASES, default 4096 here).

mod common;

use common::reference;
use optimal_selectors::{Namespaces, parse_relative_selector, parse_selector};
use proptest::prelude::*;

/// Fragments that make up random selectors: every kind of simple selector,
/// pseudo-class and pseudo-element, each kind of argument, combinators,
/// commas, whitespace, closers, and some near misses.
const FRAGMENTS: &[&str] = &[
    "a",
    "DIV",
    "-x",
    "--y",
    "\\31 a",
    "é",
    "*",
    "|",
    "ns|",
    "*|",
    "x|",
    ".",
    ".c",
    ".C",
    "#i",
    "#1",
    "#-",
    "&",
    "[a]",
    "[a=b]",
    "[ns|a='b' i]",
    "[*|a~=\"b\"]",
    "[|a|=b s]",
    "[a^=b x]",
    "[a$=1]",
    "[a*=b",
    "[",
    "]",
    ":hover",
    ":HOVER",
    ":focus-visible",
    ":first-child",
    ":host",
    ":has-slotted",
    ":-webkit-autofill",
    ":unknown",
    ":state(x)",
    ":dir(ltr)",
    ":lang(en, \"fr\")",
    ":lang()",
    ":nth-child(2n+1)",
    ":nth-child(odd of ",
    ":nth-of-type(-n+3)",
    ":nth-last-child(3 of a",
    ":nth-of-type(2 of a)",
    ":is(",
    ":where(",
    ":not(",
    ":has(",
    ":host(",
    ":host-context(",
    ":hover(",
    ")",
    ",",
    " ",
    "  ",
    ">",
    "+",
    "~",
    " > ",
    " + ",
    " ~ ",
    "::before",
    ":before",
    "::after",
    ":first-line",
    "::first-letter",
    "::marker",
    "::prefix",
    "::suffix",
    "::placeholder",
    "::selection",
    "::file-selector-button",
    "::details-content",
    "::part(p)",
    "::part(p q)",
    "::part()",
    "::slotted(",
    "::highlight(h)",
    "::highlight(*)",
    "::highlight(default)",
    "::-webkit-thing",
    "::-webkit-x()",
    "::unknown",
    ":",
    "::",
    "1",
    "\"s\"",
    "}",
    "{",
    "!",
    "/**/",
];

fn fragments() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 1..14).prop_map(|v| v.concat())
}

const SIMPLE: &[&str] = &[
    "a",
    "*",
    "ns|b",
    "*|*",
    "|c",
    ".c",
    "#i",
    "[a]",
    "[a=b i]",
    "&",
    ":hover",
    ":first-child",
    ":host",
    ":state(s)",
    ":lang(en)",
    ":nth-child(2n+1)",
    ":nth-of-type(odd)",
    ":dir(rtl)",
];
const PSEUDO: &[&str] = &[
    "::before",
    "::after",
    "::marker",
    "::first-letter",
    "::prefix",
    "::part(p)",
    "::slotted(.s)",
    "::placeholder",
    "::selection",
    "::details-content",
    ":hover",
    ":focus",
    ":first-child",
];
const COMBINATOR: &[&str] = &[" ", " > ", "+", " ~ "];
const FUNCTION: &[&str] = &[
    ":is(",
    ":where(",
    ":not(",
    ":has(",
    ":host(",
    "::slotted(",
    ":nth-child(2 of ",
];

/// Selectors built from the grammar's shapes, nesting functional
/// pseudo-classes: mostly valid, with the restrictions hit by chance.
fn structured() -> impl Strategy<Value = String> {
    let leaf = prop::collection::vec(prop::sample::select(SIMPLE), 1..4).prop_map(|v| v.concat());
    let unit = leaf.prop_recursive(4, 32, 4, |inner| {
        (
            prop::collection::vec(prop::sample::select(SIMPLE), 0..3),
            prop::sample::select(FUNCTION),
            prop::collection::vec(inner, 1..3),
            prop::collection::vec(prop::sample::select(PSEUDO), 0..3),
        )
            .prop_map(|(pre, f, args, post)| {
                format!("{}{f}{}){}", pre.concat(), args.join(", "), post.concat())
            })
    });
    (prop::collection::vec((unit, prop::sample::select(COMBINATOR)), 1..4)).prop_map(|v| {
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

fn selector_text() -> impl Strategy<Value = String> {
    prop_oneof![fragments(), structured()]
}

fn namespaces() -> (Namespaces, [(&'static str, &'static str); 1]) {
    let mut ns = Namespaces::new();
    ns.declare("ns", "urn:ns");
    (ns, [("ns", "urn:ns")])
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

proptest! {
    #![proptest_config(config())]

    #[test]
    fn matches_the_model(src in selector_text()) {
        let (ns, prefixes) = namespaces();
        let ours = parse_selector(&src, &ns).ok().map(|l| l.to_string());
        let model = reference::selector(&src, &prefixes, None);
        prop_assert_eq!(ours, model, "{:?}", src);
    }

    #[test]
    fn matches_the_model_with_a_default_namespace(src in selector_text()) {
        let (mut ns, prefixes) = namespaces();
        ns.set_default("urn:ns");
        let ours = parse_selector(&src, &ns).ok().map(|l| l.to_string());
        let model = reference::selector(&src, &prefixes, Some("urn:ns"));
        prop_assert_eq!(ours, model, "{:?}", src);
    }

    #[test]
    fn serialization_round_trips(src in selector_text()) {
        let (ns, _) = namespaces();
        if let Ok(l) = parse_selector(&src, &ns) {
            let s = l.to_string();
            let again = parse_selector(&s, &ns);
            prop_assert!(again.is_ok(), "{:?} -> {:?} is invalid: {:?}", src, s, again);
            let again = again.unwrap();
            prop_assert_eq!(again.to_string(), s.clone());
            // Specificity survives the trip too.
            for (x, y) in l.0.iter().zip(&again.0) {
                prop_assert_eq!(x.specificity(Default::default()), y.specificity(Default::default()));
            }
        }
    }

    #[test]
    fn relative_lists_agree_with_plain_lists(src in selector_text()) {
        // A valid selector list is a valid relative selector list with the
        // same serialization, and a relative list with no leading
        // combinators is a plain list.
        let (ns, _) = namespaces();
        let plain = parse_selector(&src, &ns).ok().map(|l| l.to_string());
        let rel = parse_relative_selector(&src, &ns).ok();
        let all_implicit = rel.as_ref().is_some_and(|l| l.iter().all(|r| !r.explicit));
        let rel = rel.map(|l| l.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "));
        if plain.is_some() || all_implicit {
            prop_assert_eq!(plain, rel, "{:?}", src);
        }
    }
}

/// How much of the random input is valid, so the comparison isn't only
/// about rejecting junk. Run with --nocapture to see it.
#[test]
fn share_of_valid_inputs() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (ns, _) = namespaces();
    let (mut valid, mut nested) = (0, 0);
    let n = 20_000;
    for _ in 0..n {
        let src = selector_text().new_tree(&mut runner).unwrap().current();
        if let Ok(l) = parse_selector(&src, &ns) {
            valid += 1;
            if l.to_string().contains('(') {
                nested += 1;
            }
        }
    }
    eprintln!("valid: {valid}/{n}, with a functional pseudo: {nested}");
    assert!(valid * 10 > n, "fewer than 10% valid: {valid}/{n}");
    assert!(
        nested * 50 > n,
        "fewer than 2% with arguments: {nested}/{n}"
    );
}
