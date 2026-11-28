//! Property tests for the parser, on random inputs built from the pieces
//! that steer css-syntax-3 section 5 (braces, brackets, functions, colons,
//! semicolons, `!important`, custom properties, at-rules, CDO/CDC,
//! unicode-range descriptors): every entry point must give exactly what the
//! literal model in `common/parse_reference.rs` gives, results and parser
//! errors, with validity rules that reject some declarations and rules so
//! the "invalid rule" paths run too.
//!
//! The hostile-input timing test runs when `CSS_SCALING=1` (release builds).

mod common;

use common::parse_reference::{self, Entry};
use optimal_wtf8::Wtf8Buf;
use proptest::prelude::*;

const PIECES: &[&str] = &[
    "a",
    "b",
    "x",
    "bad",
    "--v",
    "--",
    "-",
    "color",
    "unicode-range",
    "U+1-2",
    "u+?",
    "@m",
    "@bad",
    "@",
    ":",
    ";",
    ",",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "f(",
    "url(",
    "url(x)",
    "!",
    "important",
    "IMPORTANT",
    " ",
    "\n",
    "/**/",
    "<!--",
    "-->",
    "1",
    "2px",
    "#h",
    "'s'",
    "'\n",
    "\\",
    "\\}",
];

fn input() -> impl Strategy<Value = Wtf8Buf> {
    prop::collection::vec(0..PIECES.len(), 0..30)
        .prop_map(|ps| Wtf8Buf::from(ps.iter().map(|&i| PIECES[i]).collect::<String>()))
}

const ENTRIES: [Entry; 7] = [
    Entry::Stylesheet,
    Entry::DeclarationList,
    Entry::Rule,
    Entry::Declaration,
    Entry::ComponentValue,
    Entry::ComponentValues,
    Entry::CommaSeparated,
];

proptest! {
    #[test]
    fn matches_the_literal_model(src in input(), e in 0..ENTRIES.len()) {
        let w = src.as_wtf8();
        let ours = common::parse_json(w, ENTRIES[e]);
        let model = parse_reference::parse(w, ENTRIES[e]);
        prop_assert_eq!(ours, model, "{:?} on {:?}", ENTRIES[e], w);
    }
}

/// Parsing time grows linearly with input size on inputs that make a literal
/// implementation of section 5 quadratic (re-reading nested blocks after
/// each failed declaration attempt) or recursive. Timing is only meaningful
/// in release builds, so this runs when `CSS_SCALING=1`.
#[test]
fn hostile_inputs_parse_in_linear_time() {
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    type Gen = fn(usize) -> String;
    let cases: &[(&str, Gen)] = &[
        ("nested rules", |n| "a{".repeat(n)),
        ("nested rules with colon", |n| "a:b{".repeat(n)),
        ("nested at-rules", |n| "@m{".repeat(n)),
        ("nested parens", |n| "(".repeat(n)),
        ("nested functions", |n| "f(".repeat(n)),
        ("block then junk, nested", |n| {
            format!("{}{}", "a:{".repeat(n), "}x".repeat(n))
        }),
        ("custom property blocks", |n| {
            format!("a{{--x:{}", "{".repeat(n))
        }),
        ("custom look-alikes", |n| {
            format!("a{{{}", "--x:y{}".repeat(n))
        }),
        ("declarations", |n| {
            format!("a{{{}}}", "b:c !important;".repeat(n))
        }),
        ("failed declarations", |n| {
            format!("a{{{}}}", "b;".repeat(n))
        }),
        ("rules", |n| "a{}".repeat(n)),
        ("stray closers", |n| "}".repeat(n)),
        ("long prelude", |n| format!("{}{{}}", "a ".repeat(n))),
        ("unicode ranges", |n| {
            format!("@f{{{}}}", "unicode-range:U+0-7F;".repeat(n))
        }),
        ("long declaration value", |n| {
            format!("a{{b:{}}}", "c ".repeat(n))
        }),
    ];
    let time = |input: &str| {
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let (p, rules) = optimal_css::parse_stylesheet(
                    optimal_wtf8::Wtf8::from_str(input),
                    &mut optimal_css::AcceptAll,
                );
                std::hint::black_box((p.rules.len(), rules.len()));
                drop(p);
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    // The cost per byte steps up (about 2x) once the tokens (56 bytes each)
    // outgrow the caches; examples/parse_time.rs shows it. A step only
    // affects one size ratio, while a quadratic cost shows in every one, so
    // each case is timed at n, 4n and 16n and must be linear (at most 8x,
    // against 16x for quadratic) across at least one of the two steps.
    let n = 50_000;
    for (name, make) in cases {
        let t1 = time(&make(n));
        let t4 = time(&make(4 * n));
        let t16 = time(&make(16 * n));
        println!("{name:26} n={n}: {t1:?}, 4n: {t4:?}, 16n: {t16:?}");
        let slack = std::time::Duration::from_millis(20);
        assert!(
            t4 <= t1 * 8 + slack || t16 <= t4 * 8 + slack,
            "{name}: growth {t1:?} -> {t4:?} -> {t16:?} is not linear"
        );
    }
}
