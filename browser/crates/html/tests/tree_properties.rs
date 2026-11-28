//! Property tests for the tree builder on generated tag soup.
//!
//! The soup is drawn from the constructs that drive the hard parts of tree
//! construction: tables and foster parenting, misnested formatting elements
//! (the adoption agency), templates, foreign content and its integration
//! points, select, forms, framesets, raw text elements, comments, processing
//! instructions, doctypes, character references, NUL, CR and LF.
//!
//! Properties:
//! - parsing never panics, for documents and for fragments in any context;
//! - the DOM's own invariants hold afterwards (debug builds also check the
//!   parser's stack and formatting list bookkeeping on every finish);
//! - feeding the input whole, one code point at a time, or in random chunks
//!   gives the same tree and the same parse errors.

mod common;

use common::dump;
use optimal_dom::{Namespace, QuirksMode};
use optimal_html::tree_builder::{FragmentContext, ParseOptions, Parser, TreeError};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;

const PIECES: &[&str] = &[
    "<!DOCTYPE html>",
    "<!doctype html public \"-//W3C//DTD HTML 4.01//EN\">",
    "<html>",
    "</html>",
    "<head>",
    "</head>",
    "<body>",
    "</body>",
    "<p>",
    "</p>",
    "<div>",
    "</div>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<a href=x>",
    "</a>",
    "<a>",
    "<nobr>",
    "</nobr>",
    "<font color=red>",
    "</font>",
    "<b id=1>",
    "<b id=2>",
    "<table>",
    "</table>",
    "<tr>",
    "</tr>",
    "<td>",
    "</td>",
    "<th>",
    "<tbody>",
    "</tbody>",
    "<thead>",
    "<caption>",
    "</caption>",
    "<colgroup>",
    "<col>",
    "<template>",
    "</template>",
    "<svg>",
    "</svg>",
    "<math>",
    "</math>",
    "<mi>",
    "<mtext>",
    "<annotation-xml encoding=text/html>",
    "</annotation-xml>",
    "<foreignObject>",
    "</foreignObject>",
    "<desc>",
    "<path/>",
    "<circle />",
    "<select>",
    "</select>",
    "<option>",
    "<optgroup>",
    "<button>",
    "</button>",
    "<selectedcontent>",
    "<form>",
    "</form>",
    "<input>",
    "<textarea>",
    "</textarea>",
    "<title>",
    "</title>",
    "<style>",
    "</style>",
    "<script>",
    "</script>",
    "<noscript>",
    "</noscript>",
    "<plaintext>",
    "<xmp>",
    "</xmp>",
    "<iframe>",
    "</iframe>",
    "<frameset>",
    "</frameset>",
    "<frame>",
    "<noframes>",
    "<li>",
    "<ul>",
    "</ul>",
    "<dd>",
    "<dt>",
    "<h1>",
    "</h2>",
    "<pre>",
    "<listing>",
    "<marquee>",
    "</marquee>",
    "<object>",
    "<applet>",
    "<hr>",
    "<br>",
    "</br>",
    "<img>",
    "<image>",
    "<ruby>",
    "<rt>",
    "<rp>",
    "<!-- c -->",
    "<!-->",
    "<?pi data?>",
    "<?x>",
    "</>",
    "</ x>",
    "<",
    ">",
    "&amp;",
    "&lt",
    "&#0;",
    "&#x110000;",
    "&notit;",
    "\0",
    "\r",
    "\n",
    "\r\n",
    " ",
    "\t",
    "x",
    "text",
    "é",
    "😀",
    "<x-custom>",
    "</x-custom>",
    "<div title='a' title=b>",
    "<p/>",
    "<body onload=x>",
];

fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..60).prop_map(|v| v.concat())
}

const CONTEXTS: &[(Namespace, &str)] = &[
    (Namespace::Html, "body"),
    (Namespace::Html, "div"),
    (Namespace::Html, "table"),
    (Namespace::Html, "tbody"),
    (Namespace::Html, "tr"),
    (Namespace::Html, "td"),
    (Namespace::Html, "caption"),
    (Namespace::Html, "colgroup"),
    (Namespace::Html, "select"),
    (Namespace::Html, "template"),
    (Namespace::Html, "html"),
    (Namespace::Html, "head"),
    (Namespace::Html, "frameset"),
    (Namespace::Html, "title"),
    (Namespace::Html, "textarea"),
    (Namespace::Html, "script"),
    (Namespace::Html, "style"),
    (Namespace::Html, "noscript"),
    (Namespace::Html, "plaintext"),
    (Namespace::Html, "form"),
    (Namespace::Svg, "svg"),
    (Namespace::Svg, "foreignObject"),
    (Namespace::Svg, "desc"),
    (Namespace::MathMl, "math"),
    (Namespace::MathMl, "mi"),
    (Namespace::MathMl, "annotation-xml"),
];

fn parse(
    input: &str,
    context: Option<usize>,
    scripting: bool,
    cuts: &[usize],
) -> (String, Vec<TreeError>) {
    let options = ParseOptions {
        scripting,
        iframe_srcdoc: false,
        collect_errors: true,
        reference_dom: false,
    };
    let mut p = match context {
        Some(c) => {
            let (namespace, name) = CONTEXTS[c % CONTEXTS.len()];
            Parser::new_fragment(
                &FragmentContext {
                    namespace,
                    local_name: name.to_string(),
                    attributes: vec![],
                    quirks_mode: QuirksMode::NoQuirks,
                },
                options,
            )
        }
        None => Parser::new(options),
    };
    let w = Wtf8::from_str(input);
    let mut start = 0;
    for &end in cuts {
        p.feed(w.slice(start, end));
        start = end;
    }
    p.feed(w.slice(start, input.len()));
    let parsed = p.finish();
    parsed.document.check_invariants();
    (dump(&parsed), parsed.errors)
}

fn char_cuts(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).skip(1).collect()
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn chunking_never_changes_the_document(input in soup(), scripting: bool, seed: u64) {
        let whole = parse(&input, None, scripting, &[]);
        let per_char = parse(&input, None, scripting, &char_cuts(&input));
        prop_assert_eq!(&whole, &per_char);
        let mut x = seed | 1;
        let random: Vec<usize> = char_cuts(&input)
            .into_iter()
            .filter(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x % 3 == 0
            })
            .collect();
        prop_assert_eq!(&whole, &parse(&input, None, scripting, &random));
    }

    #[test]
    fn chunking_never_changes_a_fragment(input in soup(), context in 0usize..CONTEXTS.len(), scripting: bool) {
        let whole = parse(&input, Some(context), scripting, &[]);
        let per_char = parse(&input, Some(context), scripting, &char_cuts(&input));
        prop_assert_eq!(whole, per_char);
    }

    #[test]
    fn errors_only_with_collect_errors(input in soup()) {
        let mut p = Parser::new(ParseOptions::default());
        p.feed(Wtf8::from_str(&input));
        let parsed = p.finish();
        prop_assert!(parsed.errors.is_empty());
        let (with, _) = parse(&input, None, false, &[]);
        prop_assert_eq!(dump(&parsed), with, "collecting errors must not change the tree");
    }
}

/// Deep trees are built without recursion (no stack overflow) and keep
/// their full depth: the parser does not cap nesting.
#[test]
fn deep_nesting_is_built_iteratively() {
    // Deep enough to overflow a recursive builder's stack in a debug build.
    let n = 100_000;
    let input = "<div>".repeat(n);
    let parsed = optimal_html::tree_builder::parse_document(&input, ParseOptions::default());
    parsed.document.check_invariants();
    let mut depth = 0;
    let mut node = optimal_dom::Document::ROOT;
    while let Some(c) = parsed.document.last_child(node) {
        node = c;
        depth += 1;
    }
    assert_eq!(depth, n + 2, "html, body, then every div");
}

/// Parsing time grows linearly with input size on inputs that make a literal
/// implementation of the standard quadratic (deep stacks and long lists of
/// active formatting elements). Timing is only meaningful in release builds,
/// so this runs when `TREE_SCALING=1` (the CI deep run sets it).
#[test]
fn hostile_inputs_parse_in_linear_time() {
    if std::env::var_os("TREE_SCALING").is_none() {
        return;
    }
    type Gen = fn(usize) -> String;
    let cases: &[(&str, Gen)] = &[
        ("nested div", |n| "<div>".repeat(n)),
        ("div and p", |n| "<div><p>".repeat(n)),
        ("headings", |n| "<h1><div>".repeat(n)),
        ("nested b", |n| "<b>".repeat(n)),
        ("b then p", |n| {
            format!("{}{}", "<b>".repeat(n), "<p>x".repeat(n))
        }),
        ("nested table", |n| "<table><tr><td>".repeat(n)),
        ("table text", |n| format!("<table>{}", "x<b>".repeat(n))),
        ("distinct formatting", |n| {
            (0..n).map(|i| format!("<b id={i}>")).collect()
        }),
        ("distinct then same", |n| {
            format!(
                "{}{}",
                (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
                "<i>".repeat(n)
            )
        }),
        ("distinct then end tags", |n| {
            format!(
                "{}{}",
                (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
                "</i>".repeat(n)
            )
        }),
        ("distinct then a", |n| {
            format!(
                "{}{}",
                (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
                "<a></a>".repeat(n)
            )
        }),
        ("stray </p> under button", |n| {
            format!("<p><button>{}{}", "<span>".repeat(n), "</p>".repeat(n))
        }),
        ("custom end tags", |n| {
            format!("{}{}", "<b>".repeat(n), "</x-y>".repeat(n))
        }),
        ("li under many divs", |n| {
            format!("<ul>{}{}", "<div>".repeat(n), "<li>".repeat(n))
        }),
        ("select options", |n| {
            format!("<select>{}", "<option>x".repeat(n))
        }),
        ("selected options and a selectedcontent", |n| {
            format!(
                "<select><button><selectedcontent></button>{}",
                "<option selected>x".repeat(n)
            )
        }),
        ("options under deep divs", |n| {
            format!(
                "<select>{}{}",
                "<div>".repeat(n),
                "<option selected>x".repeat(n)
            )
        }),
        ("optgroups", |n| {
            format!("<select>{}", "<optgroup><option>x".repeat(n))
        }),
        ("adoption moving options", |n| {
            format!("<select>{}", "<b><div><option>x</b></div>".repeat(n))
        }),
        // Each </b> runs the adoption agency with the div as furthest
        // block, and the next <b><div> opens inside it: one level deeper
        // each time (issue 31).
        ("adoption nesting deeper", |n| "<b><div>x</b>".repeat(n)),
        // Every </b> moves the same div, with all its content, under the
        // next b down: a deep target and a big subtree each time.
        ("adoption moving one div down", |n| {
            format!(
                "{}<div>{}{}",
                (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
                "<p>x".repeat(n),
                "</b>".repeat(n)
            )
        }),
        ("many attributes", |n| {
            format!(
                "<div {}>",
                (0..n)
                    .map(|i| format!("a{i}=v"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }),
    ];
    let time = |input: &str| {
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let p = optimal_html::tree_builder::parse_document(input, ParseOptions::default());
                std::hint::black_box(p.document.node_count());
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let n = 25_000;
    for (name, make) in cases {
        let small = time(&make(n));
        let big = time(&make(4 * n));
        println!("{name:28} n={n}: {small:?}, 4n: {big:?}");
        // Linear growth is 4x; allow noise. Quadratic would be 16x.
        assert!(
            big <= small * 8 + std::time::Duration::from_millis(20),
            "{name}: 4x the input took {big:?} vs {small:?}: not linear"
        );
    }
}
