//! The select element's behaviour during parsing: hand-worked cases, and
//! the parser on the normal DOM against the parser on the literal one.
//!
//! Each expected tree is worked out from the standard (HTML 4.10.7,
//! 4.10.10 and the `selectedcontent` element), with the steps named.

mod common;

use common::dump;
use optimal_html::tree_builder::{ParseOptions, Parser, TreeError, parse_document};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;

fn options(reference_dom: bool) -> ParseOptions {
    ParseOptions {
        scripting: false,
        iframe_srcdoc: false,
        collect_errors: true,
        reference_dom,
    }
}

fn doc(input: &str) -> String {
    let p = parse_document(input, options(false));
    p.document.check_invariants();
    let reference = parse_document(input, options(true));
    assert_eq!(dump(&reference), dump(&p), "literal model differs");
    dump(&p)
}

/// Lines of the dump for a body holding only a select, after the `<body>`
/// line.
fn body(lines: &[&str]) -> String {
    let mut out = String::from("| <html>\n|   <head>\n|   <body>");
    for l in lines {
        out.push_str("\n|     ");
        out.push_str(l);
    }
    out
}

/// A first: the option's insertion selects it (nothing selected, first
/// enabled, display size 1), and its post-connection steps clone its
/// (still empty) children. The `<option selected>` start tag pops A, whose
/// children parsed steps clone "A". B's insertion leaves B selected (the
/// last selected option in tree order) and clones its empty children; C's
/// start tag pops B, and B's children parsed steps clone "B". C changes
/// nothing.
#[test]
fn selectedcontent_shows_the_last_selected_option() {
    assert_eq!(
        doc(
            "<select><button><selectedcontent></button><option>A<option selected>B<option>C</select>"
        ),
        body(&[
            "<select>",
            "  <button>",
            "    <selectedcontent>",
            "      \"B\"",
            "  <option>",
            "    \"A\"",
            "  <option>",
            "    selected=\"\"",
            "    \"B\"",
            "  <option>",
            "    \"C\"",
        ])
    );
}

/// With `multiple`, the selectedcontent insertion steps return ("select has
/// the multiple attribute") and so does every update.
#[test]
fn multiple_leaves_selectedcontent_empty() {
    assert_eq!(
        doc("<select multiple><button><selectedcontent></button><option selected>A</select>"),
        body(&[
            "<select>",
            "  multiple=\"\"",
            "  <button>",
            "    <selectedcontent>",
            "  <option>",
            "    selected=\"\"",
            "    \"A\"",
        ])
    );
}

/// Display size 3: nothing is selected for the user, so the update clears
/// the selectedcontent.
#[test]
fn display_size_above_one_selects_nothing() {
    assert_eq!(
        doc("<select size=3><button><selectedcontent></button><option>A</select>"),
        body(&[
            "<select>",
            "  size=\"3\"",
            "  <button>",
            "    <selectedcontent>",
            "  <option>",
            "    \"A\"",
        ])
    );
}

/// The first enabled option is chosen: A is disabled itself, B by its
/// optgroup.
#[test]
fn disabled_options_are_skipped() {
    let expect = |first: &[&str]| {
        let mut lines = vec![
            "<select>",
            "  <button>",
            "    <selectedcontent>",
            "      \"C\"",
        ];
        lines.extend_from_slice(first);
        lines.extend_from_slice(&["  <option>", "    \"C\""]);
        body(&lines)
    };
    assert_eq!(
        doc("<select><button><selectedcontent></button><option disabled>A<option>C</select>"),
        expect(&["  <option>", "    disabled=\"\"", "    \"A\""])
    );
    assert_eq!(
        doc("<select><button><selectedcontent></button>\
             <optgroup disabled><option>B</optgroup><option>C</select>"),
        expect(&[
            "  <optgroup>",
            "    disabled=\"\"",
            "    <option>",
            "      \"B\"",
        ])
    );
}

/// Every enabled selectedcontent gets its own clone.
#[test]
fn every_selectedcontent_gets_a_clone() {
    assert_eq!(
        doc("<select><button><selectedcontent></selectedcontent>\
             <selectedcontent></selectedcontent></button><option>A</select>"),
        body(&[
            "<select>",
            "  <button>",
            "    <selectedcontent>",
            "      \"A\"",
            "    <selectedcontent>",
            "      \"A\"",
            "  <option>",
            "    \"A\"",
        ])
    );
}

/// In template contents nothing is connected, so insertion steps never
/// queue an update; the option's children parsed steps do not check
/// connectedness, and they fill the selectedcontent.
#[test]
fn template_contents_update_from_children_parsed() {
    assert_eq!(
        doc(
            "<template><select><button><selectedcontent></button><option>A</option></select></template>"
        ),
        "| <html>\n|   <head>\n|     <template>\n|       content\n|         <select>\n\
         |           <button>\n|             <selectedcontent>\n|               \"A\"\n\
         |           <option>\n|             \"A\"\n|   <body>"
    );
}

/// The clone leaves out script and iframe elements, and the option's
/// label attribute plays no part.
#[test]
fn clone_skips_scripts_and_ignores_label() {
    assert_eq!(
        doc("<select><button><selectedcontent></button>\
             <option label=L>A<b>B</b><iframe></iframe><script></script></select>"),
        body(&[
            "<select>",
            "  <button>",
            "    <selectedcontent>",
            "      \"A\"",
            "      <b>",
            "        \"B\"",
            "  <option>",
            "    label=\"L\"",
            "    \"A\"",
            "    <b>",
            "      \"B\"",
            "    <iframe>",
            "    <script>",
        ])
    );
}

/// An option nested in the selected option (a parse error, but the parser
/// builds it). Its clone inside the selectedcontent is in the select's list
/// of options, before A, so A stays selected. Under the standard's words
/// the clone's insertion would start another update, and so on for ever;
/// here an update's own mutations do not, so there is one clone.
#[test]
fn nested_option_is_cloned_once() {
    let input =
        "<select><button><selectedcontent></button><option>A<div><option selected>B</select>";
    assert_eq!(
        doc(input),
        body(&[
            "<select>",
            "  <button>",
            "    <selectedcontent>",
            "      \"A\"",
            "      <div>",
            "        <option>",
            "          selected=\"\"",
            "          \"B\"",
            "  <option>",
            "    \"A\"",
            "    <div>",
            "      <option>",
            "        selected=\"\"",
            "        \"B\"",
        ])
    );
}

// ---- Normal DOM against the literal one, on generated soup -------------

const PIECES: &[&str] = &[
    "<select>",
    "</select>",
    "<select multiple>",
    "<select size=2>",
    "<option>",
    "<option selected>",
    "<option disabled>",
    "</option>",
    "<optgroup>",
    "<optgroup disabled>",
    "</optgroup>",
    "<button>",
    "</button>",
    "<selectedcontent>",
    "</selectedcontent>",
    "<datalist>",
    "</datalist>",
    "<hr>",
    "<div>",
    "</div>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<p>",
    "<table>",
    "<td>",
    "</table>",
    "<template>",
    "</template>",
    "<script></script>",
    "<svg>",
    "</svg>",
    "x",
    "y",
];

fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..40).prop_map(|v| v.concat())
}

fn parse(input: &str, reference_dom: bool, cuts: &[usize]) -> (String, String, Vec<TreeError>) {
    let mut p = Parser::new(options(reference_dom));
    let w = Wtf8::from_str(input);
    let mut start = 0;
    for &end in cuts {
        p.feed(w.slice(start, end));
        start = end;
    }
    p.feed(w.slice(start, input.len()));
    let parsed = p.finish();
    parsed.document.check_invariants();
    (
        dump(&parsed),
        parsed.document.behaviour_dump(),
        parsed.errors,
    )
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn normal_dom_matches_the_literal_one(input in soup(), seed: u64) {
        let fast = parse(&input, false, &[]);
        prop_assert_eq!(&fast, &parse(&input, true, &[]));
        let mut x = seed | 1;
        let cuts: Vec<usize> = input
            .char_indices()
            .map(|(i, _)| i)
            .skip(1)
            .filter(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x % 3 == 0
            })
            .collect();
        prop_assert_eq!(&fast, &parse(&input, false, &cuts));
    }
}
