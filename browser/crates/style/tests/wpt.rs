//! The web-platform-tests in css/css-cascade and css/css-nesting that the
//! cascade can run without script, layout or computed values.
//!
//! The WPT files are testharness pages: a script adds style sheets and
//! asserts on `getComputedStyle()`. There is no script engine yet, so this
//! harness reads each file and does what its script does:
//!
//! - **Case tables** (`layer-basic.html`, `layer-important.html`,
//!   `layer-vs-inline-style.html`): each case's `style` (and `inlineStyle`)
//!   string is read from the script's array, applied to the page's tree,
//!   and the property the script checks must cascade to the value the
//!   script expects.
//! - **Static pages**: the page's `<style>` elements are its sheets, its
//!   markup (parsed by optimal-html) is the tree, and each assert of the
//!   script is written out below as (element, property, value), citing
//!   the file.
//! - **Rule counts** (`layer-rule-invalid-name.html`): each `<style>`'s
//!   `cssRules.length` is checked as the number of style rules collected.
//!
//! - **Reftests** of the defaulting keywords: a page that "passes if
//!   there is a filled green square" is checked by the specified values
//!   that make it so (written out below, with the reasoning). Pages whose
//!   result depends on the user agent style sheet get the rules they need,
//!   copied from the HTML standard's rendering section.
//!
//! Computed values don't exist yet, so a value is checked as the
//! specified value's text: a declaration's value as written, or the
//! property table's initial value. `rgb(0, 128, 0)` in an assert is the
//! computed value of the keyword `green` (and `rgb(255, 0, 0)` of `red`,
//! `rgba(0, 0, 0, 0)` of `transparent`, and so on, from css-color-4);
//! every other expected value is the specified text itself.
//!
//! The asserts that need something not built yet are listed in
//! conformance/README.md as waiting, with the reason.
//!
//! Suite location: `WPT_CSS` (default `browser/build/wpt-css/css`, fetched
//! by `scripts/fetch-test-suites.sh`).

mod common;

use common::specified::specified;
use common::tree::{HTML, Tree};
use optimal_dom::{Document, NodeData, NodeId};
use optimal_html::tree_builder::{ParseOptions, parse_document};
use optimal_selectors::{MatchContext, Namespaces, Scope, matches, parse_selector};
use optimal_style::{Origin, Source, StyleAttribute, StyleSet};
use optimal_wtf8::Wtf8;
use std::path::PathBuf;

fn suite_dir() -> PathBuf {
    std::env::var_os("WPT_CSS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/wpt-css/css")
        })
}

fn load(file: &str) -> String {
    std::fs::read_to_string(suite_dir().join(file))
        .unwrap_or_else(|e| panic!("{file}: {e} (run browser/scripts/fetch-test-suites.sh)"))
}

/// A page: its `<style>` texts in tree order, and its elements.
struct Page {
    styles: Vec<String>,
    tree: Tree,
}

fn page(html: &str) -> Page {
    let parsed = parse_document(html, ParseOptions::default());
    let doc = &parsed.document;
    let root = doc
        .children(Document::ROOT)
        .find(|&c| matches!(doc.data(c), NodeData::Element(_)))
        .expect("a root element");
    let mut tree = Tree::new(true, "html");
    let mut styles = Vec::new();
    copy(doc, root, 0, &mut tree, &mut styles, true);
    Page { styles, tree }
}

fn copy(
    doc: &Document,
    from: NodeId,
    to: usize,
    t: &mut Tree,
    styles: &mut Vec<String>,
    root: bool,
) {
    let NodeData::Element(e) = doc.data(from) else {
        unreachable!()
    };
    let attrs: Vec<(String, String)> = e
        .attributes
        .iter()
        .map(|a| {
            (
                doc.name_text(a.local).to_string_lossy(),
                a.value.as_wtf8().to_string_lossy(),
            )
        })
        .collect();
    let local = doc.name_text(e.local).to_string_lossy();
    let me = if root {
        t.nodes[0].attrs = attrs;
        0
    } else {
        let a: Vec<(&str, &str)> = attrs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        t.add(to, e.namespace.url(), &local, &a)
    };
    if local == "style" {
        let text: String = doc
            .children(from)
            .filter_map(|c| match doc.data(c) {
                NodeData::Text(s) => Some(s.as_wtf8().to_string_lossy()),
                _ => None,
            })
            .collect();
        styles.push(text);
    }
    for c in doc.children(from) {
        if matches!(doc.data(c), NodeData::Element(_)) {
            copy(doc, c, me, t, styles, false);
        }
    }
}

/// The elements matching `selector`, in tree order.
fn select(t: &Tree, selector: &str) -> Vec<usize> {
    let list = parse_selector(selector, &Namespaces::new()).expect("selector");
    let cx = MatchContext {
        scope: Scope::None,
        quirks: false,
        nesting: &[],
    };
    (0..t.nodes.len())
        .filter(|&i| matches(&list, &t.el(i), &cx))
        .collect()
}

/// The cascaded value's text of `prop` on each element.
fn cascaded(sheets: &[&str], t: &Tree, prop: &str) -> Vec<Option<String>> {
    let mut set = StyleSet::new();
    for s in sheets {
        set.add_sheet(Wtf8::from_str(s), Origin::Author);
    }
    let styles: Vec<_> = (0..t.nodes.len())
        .map(|i| {
            t.attr(i, "style")
                .map(|s| StyleAttribute::parse(Wtf8::from_str(s)))
        })
        .collect();
    let c = set.prepare();
    c.with_session(false, |s| {
        (0..t.nodes.len())
            .map(|i| {
                let out = s.cascade(&t.el(i), styles[i].as_ref());
                out.winner(prop).map(|e| {
                    let (p, v) = match e.source {
                        Source::Rule(d) => {
                            let d = &set.decls()[d as usize];
                            (&set.sheets()[d.sheet as usize].parsed, set.values(d))
                        }
                        Source::Attribute(a) => {
                            let st = styles[i].as_ref().unwrap();
                            (&st.parsed, st.values(a as usize))
                        }
                    };
                    let spans: Vec<_> = v.map(|c| c.span()).collect();
                    match (spans.first(), spans.last()) {
                        (Some(f), Some(l)) => p.source(f.start..l.end).to_string_lossy(),
                        _ => String::new(),
                    }
                })
            })
            .collect()
    })
}

/// An expected computed value, as the declared text it comes from.
fn declared(expected: &str) -> &str {
    match expected {
        "rgb(0, 128, 0)" => "green",
        "rgb(255, 0, 0)" => "red",
        "rgb(255, 255, 0)" => "yellow",
        "rgb(0, 0, 0)" => "black",
        "rgba(0, 0, 0, 0)" => "transparent",
        e => e,
    }
}

/// The user agent style sheet rules some pages need: from the HTML
/// standard's rendering section (15.3.3 Flow content), at the pinned
/// commit.
const UA_SHEET: &str = "address, blockquote, center, dialog, div, figure, figcaption, footer, form,
header, hr, legend, listing, main, p, plaintext, pre, search, xmp {
  display: block;
}";

/// JavaScript string literals ('', "" or ``, no escapes or substitutions
/// in the files read) for `key:` in each case object of a `testCases`
/// array, split at each `title:`.
fn js_cases(src: &str) -> Vec<Vec<(String, String)>> {
    let start = src.find("testCases = [").expect("a testCases array");
    let end = start + src[start..].find("\n];").expect("the end of the array");
    let body = &src[start..end];
    let mut cases = Vec::new();
    for chunk in body.split("title:").skip(1) {
        let mut fields = vec![("title".to_string(), literal(chunk).0)];
        for key in ["style:", "inlineStyle:"] {
            if let Some(i) = chunk.find(key) {
                fields.push((
                    key.trim_end_matches(':').to_string(),
                    literal(&chunk[i + key.len()..]).0,
                ));
            }
        }
        cases.push(fields);
    }
    cases
}

/// The first string literal in `s`, and what follows it.
fn literal(s: &str) -> (String, &str) {
    let open = s.find(['\'', '"', '`']).expect("a string literal");
    let q = s[open..].chars().next().unwrap();
    let rest = &s[open + 1..];
    let close = rest.find(q).expect("a closing quote");
    assert!(
        !rest[..close].contains('\\') && !rest[..close].contains("${"),
        "unsupported literal"
    );
    (rest[..close].to_string(), &rest[close + 1..])
}

fn field<'c>(case: &'c [(String, String)], key: &str) -> Option<&'c str> {
    case.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// `layer-basic.html` and `layer-important.html`: in every case, each
/// `target` element's color is green.
#[test]
fn layer_case_tables() {
    let mut ran = 0;
    for file in [
        "css-cascade/layer-basic.html",
        "css-cascade/layer-important.html",
    ] {
        let src = load(file);
        let p = page(&src);
        assert!(p.styles.is_empty(), "{file}: the page's own sheets");
        let targets = select(&p.tree, "target");
        assert_eq!(targets.len(), 2);
        for case in js_cases(&src) {
            let title = field(&case, "title").unwrap();
            let style = field(&case, "style").unwrap();
            let got = cascaded(&[style], &p.tree, "color");
            for &t in &targets {
                assert_eq!(got[t].as_deref(), Some("green"), "{file}: {title}");
            }
            ran += 1;
        }
    }
    assert_eq!(ran, 34 + 9);
}

/// `layer-vs-inline-style.html`: the target's background is the
/// reference's (green), with the case's sheet and inline style.
#[test]
fn layer_vs_inline_style() {
    let file = "css-cascade/layer-vs-inline-style.html";
    let src = load(file);
    let mut p = page(&src);
    let target = select(&p.tree, "#target")[0];
    let reference = select(&p.tree, "#reference")[0];
    let cases = js_cases(&src);
    assert_eq!(cases.len(), 4);
    for case in cases {
        let title = field(&case, "title").unwrap();
        let mut sheets: Vec<&str> = p.styles.iter().map(String::as_str).collect();
        let style = field(&case, "style").unwrap().to_string();
        sheets.push(&style);
        p.tree.nodes[target].attrs.retain(|(k, _)| k != "style");
        p.tree.nodes[target]
            .attrs
            .push(("style".into(), field(&case, "inlineStyle").unwrap().into()));
        let got = cascaded(&sheets, &p.tree, "background-color");
        assert_eq!(got[target], got[reference], "{file}: {title}");
        assert_eq!(got[target].as_deref(), Some("green"), "{file}: {title}");
    }
}

/// `layer-rule-invalid-name.html`: `@layer` rules with invalid names are
/// dropped with their contents.
#[test]
fn layer_rule_invalid_name() {
    let file = "css-cascade/layer-rule-invalid-name.html";
    let src = load(file);
    let mut checked = 0;
    for (id, expected) in [
        ("layer-numeric", 0),
        ("layer-dot-numeric", 0),
        ("layer-dash-only", 0),
        ("layer-valid", 1),
    ] {
        // The test reads each <style id=...>'s cssRules.length.
        let assert = format!(
            "getElementById(\"{id}\");\n  assert_equals(sheet.cssRules.length, {expected});"
        );
        assert!(src.contains(&assert), "{file}: {id}");
        let open = format!("<style id=\"{id}\">");
        let i = src.find(&open).unwrap() + open.len();
        let css = &src[i..i + src[i..].find("</style>").unwrap()];
        let mut set = StyleSet::new();
        set.add_sheet(Wtf8::from_str(css), Origin::Author);
        assert_eq!(set.rules().len(), expected, "{file}: {id}");
        checked += 1;
    }
    assert_eq!(checked, 4);
}

/// Static pages and their asserts: (file, element selector, property,
/// expected value as the assert writes it).
type Check = (&'static str, &'static str, &'static str);
const STATIC: &[(&str, &[Check])] = &[
    (
        "css-cascade/layers-basic.html",
        &[
            ("#testP", "color", "rgb(255, 255, 0)"),
            ("#testP", "background-color", "rgb(0, 0, 0)"),
            // font-size computes 4rem to 64px; the cascaded value is 4rem.
            ("#testP", "font-size", "4rem"),
        ],
    ),
    (
        "css-cascade/important-vs-inline-001.html",
        // The first assert, before the script sets inline styles.
        &[("#el", "opacity", "0.5")],
    ),
    (
        "css-nesting/nesting-layer.html",
        &[
            ("main > .a", "z-index", "1"),
            ("main > .a > .b", "background-color", "rgb(0, 128, 0)"),
        ],
    ),
    (
        "css-nesting/top-level-parent-pseudo-specificity.html",
        &[(":root", "color", "rgb(0, 128, 0)")],
    ),
    (
        "css-nesting/implicit-nesting-ident.html",
        &[("#target", "color", "rgb(0, 128, 0)")],
    ),
    (
        "css-nesting/nested-error-recovery.html",
        // #test1 waits for @media.
        &[
            (":root", "z-index", "0"),
            ("#test2", "z-index", "2"),
            ("#test3", "z-index", "5"),
        ],
    ),
    (
        "css-cascade/revert-rule-basic.html",
        &[
            ("#test1", "color", "rgb(0, 128, 0)"),
            ("#test2", "color", "rgb(0, 128, 0)"),
            ("#test3", "color", "rgb(0, 128, 0)"),
            ("#test4", "z-index", "2"),
        ],
    ),
    (
        "css-cascade/revert-rule-important.html",
        &[
            ("#test1", "color", "rgb(0, 128, 0)"),
            ("#test2", "color", "rgb(0, 128, 0)"),
            ("#test3", "color", "rgb(0, 128, 0)"),
        ],
    ),
    (
        "css-cascade/revert-rule-layer.html",
        &[
            ("#test1", "color", "rgb(0, 128, 0)"),
            ("#test2", "color", "rgb(0, 128, 0)"),
        ],
    ),
    (
        "css-cascade/revert-rule-custom-property.html",
        &[("#test1", "--a", "green"), ("#test1", "--b", "green")],
    ),
    (
        "css-cascade/revert-rule-revert-layer.html",
        &[
            ("#test1", "color", "rgb(0, 128, 0)"),
            ("#test1", "background-color", "rgb(0, 128, 0)"),
            ("#test2", "color", "rgb(0, 128, 0)"),
            ("#test2", "background-color", "rgb(0, 128, 0)"),
            ("#test3", "color", "rgb(0, 128, 0)"),
            ("#test3", "background-color", "rgb(0, 128, 0)"),
            ("#test4", "color", "rgb(0, 128, 0)"),
            ("#test4", "background-color", "rgb(0, 128, 0)"),
        ],
    ),
    (
        // #test4 waits for @scope.
        "css-nesting/nesting-revert-rule.html",
        &[
            ("#test1", "color", "rgb(0, 128, 0)"),
            ("#test2", "color", "rgb(0, 128, 0)"),
            ("#test3", "color", "rgb(0, 128, 0)"),
        ],
    ),
    (
        // overflow is a shorthand: both longhands are visible.
        "css-cascade/inherit-initial.html",
        &[
            (":root", "z-index", "auto"),
            (":root", "position", "static"),
            (":root", "overflow-x", "visible"),
            (":root", "overflow-y", "visible"),
            (":root", "background-color", "rgba(0, 0, 0, 0)"),
        ],
    ),
    (
        "css-nesting/nested-declarations-matching.html",
        // The asserts that involve @media, @scope or ::after wait.
        &[
            (".trailing", "--x", "PASS"),
            (".trailing_no_leading", "--x", "PASS"),
            (".trailing_multiple", "--x", "PASS"),
            (".trailing_multiple", "--y", "PASS"),
            (".trailing_multiple", "--z", "PASS"),
            (".trailing_multiple", "--w", "PASS"),
            (".trailing_specificity", "--x", "PASS"),
            (".specificity_top_level", "--x", "PASS"),
            (".specificity_top_level_max", "--x", "PASS"),
            // Before the script changes the selector text.
            (".set_parent_selector_text > .a1", "color", "rgb(0, 128, 0)"),
            (".set_parent_selector_text > .a2", "color", "rgb(255, 0, 0)"),
        ],
    ),
];

#[test]
fn static_pages() {
    for (file, checks) in STATIC {
        let src = load(file);
        let p = page(&src);
        let sheets: Vec<&str> = p.styles.iter().map(String::as_str).collect();
        for (sel, prop, expected) in *checks {
            // The script asserts this.
            let quoted = [format!("'{expected}'"), format!("\"{expected}\"")];
            assert!(
                quoted.iter().any(|q| src.contains(q.as_str())) || *prop == "font-size",
                "{file}: no assert of {expected}"
            );
            let els = select(&p.tree, sel);
            assert!(!els.is_empty(), "{file}: {sel}");
            let with_origin: Vec<_> = sheets.iter().map(|s| (*s, Origin::Author)).collect();
            let got = specified(&with_origin, &p.tree, prop);
            assert_eq!(
                got[els[0]].as_deref(),
                Some(declared(expected)),
                "{file}: {sel} {prop}"
            );
        }
    }
}

/// Reftests: (file, whether it needs [`UA_SHEET`], checks as (element,
/// property, specified value)). Each page says "Test passes if there is a
/// filled green square and no red" (or names its own condition), and the
/// checks are what that takes.
type RefCheck = (&'static str, &'static str, &'static str);
const REFTESTS: &[(&str, bool, &[RefCheck])] = &[
    // revert-layer in a layer rolls back to the layer before.
    (
        "css-cascade/revert-layer-001.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // From the implicit outer layer to a layer.
    (
        "css-cascade/revert-layer-002.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // all: revert-layer rolls back every property.
    (
        "css-cascade/revert-layer-003.html",
        false,
        &[
            ("#target", "background-color", "green"),
            ("#target", "width", "100px"),
            ("#target", "height", "100px"),
        ],
    ),
    // Important revert-layer: rolls back past its layer's normal
    // declarations too, and the later layer's red !important lost to it.
    (
        "css-cascade/revert-layer-005.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // With no other layer, revert-layer acts as revert: the UA's block.
    (
        "css-cascade/revert-layer-006.html",
        true,
        &[
            ("#inner", "display", "block"),
            ("#inner", "background-color", "green"),
        ],
    ),
    // A chain of revert-layer across layers.
    (
        "css-cascade/revert-layer-007.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // From the style attribute to the sheets.
    (
        "css-cascade/revert-layer-009.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // Important in the style attribute: back to the sheet's important.
    (
        "css-cascade/revert-layer-012.html",
        false,
        &[("#target", "background-color", "green")],
    ),
    // revert: to the UA's display: block for a div.
    (
        "css-cascade/revert-val-001.html",
        true,
        &[
            ("#inner", "display", "block"),
            ("#inner", "background-color", "green"),
        ],
    ),
    // revert loses to an important author declaration.
    (
        "css-cascade/revert-val-002.html",
        true,
        &[("#inner", "display", "block")],
    ),
    // unset: color inherits green from .outer, background-color is
    // initial (transparent), so .under's green shows.
    (
        "css-cascade/unset-val-001.html",
        false,
        &[
            (".inner", "color", "green"),
            (".inner", "background-color", "transparent"),
            (".under", "background-color", "green"),
        ],
    ),
    // unset on display: initial, inline (so the red span has no size).
    (
        "css-cascade/unset-val-002.html",
        false,
        &[(".fail", "display", "inline")],
    ),
    // initial: color takes its initial value (not the parent's red), the
    // background is transparent.
    (
        "css-cascade/initial-color-background-001.html",
        false,
        &[
            (".inner", "color", "CanvasText"),
            (".inner", "background-color", "transparent"),
        ],
    ),
    // all: initial keeps direction and unicode-bidi, resets the rest.
    (
        "css-cascade/all-prop-001.html",
        false,
        &[
            (".test", "direction", "rtl"),
            (".test", "unicode-bidi", "bidi-override"),
            (".test", "color", "CanvasText"),
            (".test", "float", "none"),
            (".test", "display", "inline"),
            (".test", "border-top-style", "none"),
            (".test", "font-style", "normal"),
            (".test", "margin-left", "0"),
            (".test", "overflow-x", "visible"),
            (".test", "text-align-all", "start"),
        ],
    ),
    // all: inherit makes display inherit the span's (initial) inline.
    (
        "css-cascade/all-prop-002.html",
        false,
        &[(".inline", "display", "inline")],
    ),
    // all with a later longhand: the longhand wins.
    (
        "css-cascade/all-prop-inherit-color.html",
        false,
        &[(".inner", "color", "green")],
    ),
    (
        "css-cascade/all-prop-initial-color.html",
        false,
        &[(".inner", "color", "green")],
    ),
    (
        "css-cascade/all-prop-revert-color.html",
        false,
        &[(".inner", "color", "green")],
    ),
    (
        "css-cascade/all-prop-unset-color.html",
        false,
        &[(".inner", "color", "green")],
    ),
];

#[test]
fn reftests() {
    for (file, ua, checks) in REFTESTS {
        let src = load(file);
        assert!(src.contains("Test passes if"), "{file}: not a reftest page");
        let p = page(&src);
        let mut sheets: Vec<(&str, Origin)> = Vec::new();
        if *ua {
            sheets.push((UA_SHEET, Origin::UserAgent));
        }
        sheets.extend(p.styles.iter().map(|s| (s.as_str(), Origin::Author)));
        for (sel, prop, expected) in *checks {
            let els = select(&p.tree, sel);
            assert!(!els.is_empty(), "{file}: {sel}");
            let got = specified(&sheets, &p.tree, prop);
            for &e in &els {
                assert_eq!(got[e].as_deref(), Some(*expected), "{file}: {sel} {prop}");
            }
        }
    }
}

#[test]
fn page_tree_is_html() {
    let p = page("<!DOCTYPE html><p id=x style='color: red'>");
    assert_eq!(p.tree.nodes[0].ns, HTML);
    assert_eq!(select(&p.tree, "p#x").len(), 1);
}

/// The string arguments of each call to `f(` in `src`, JavaScript
/// escapes (`\n`, `\"`, `\'`, `\\`) resolved.
fn calls(src: &str, f: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(&format!("{f}(")) {
        rest = &rest[i + f.len() + 1..];
        let mut args = Vec::new();
        loop {
            rest = rest.trim_start();
            let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                break;
            };
            let mut s = String::new();
            let mut chars = rest[1..].char_indices();
            let end = loop {
                let (i, c) = chars.next().expect("closed string");
                match c {
                    '\\' => s.push(match chars.next().expect("escape").1 {
                        'n' => '\n',
                        c => c,
                    }),
                    c if c == q => break i + 2,
                    c => s.push(c),
                }
            };
            args.push(s);
            rest = rest[end..].trim_start();
            match rest.strip_prefix(',') {
                Some(r) => rest = r,
                None => break,
            }
        }
        // A definition (`function f(rule, ...)`) has no string arguments.
        if !args.is_empty() {
            out.push(args);
        }
    }
    out
}

/// `parsing/layer.html`: `test_valid_rule` and `test_invalid_rule` on
/// `@layer`. A valid rule declares a layer; an invalid one declares none.
#[test]
fn layer_rule_parsing() {
    let src = load("css-cascade/parsing/layer.html");
    let mut n = 0;
    for (f, valid) in [("test_valid_rule", true), ("test_invalid_rule", false)] {
        for args in calls(&src, f) {
            let rule = &args[0];
            let mut set = StyleSet::new();
            set.add_sheet(Wtf8::from_str(rule), Origin::Author);
            assert_eq!(set.layers().len() > 3, valid, "{rule}");
            n += 1;
        }
    }
    assert_eq!(n, 11);
}

/// `parsing/layer-import-parsing.html`: a valid layered import declares
/// its layer; one whose layer() is invalid is still a valid import (its
/// layer() is read as a condition), so the @import after it still counts.
#[test]
fn layer_import_parsing() {
    let src = load("css-cascade/parsing/layer-import-parsing.html");
    let mut n = 0;
    for (f, valid_layer) in [
        ("test_valid_layer_import", true),
        ("test_invalid_layer_import", false),
    ] {
        for args in calls(&src, f) {
            let rule = &args[0];
            let css = format!("{rule} @import url(after.css) layer(after);");
            let mut set = StyleSet::new();
            set.add_sheet(Wtf8::from_str(&css), Origin::Author);
            // Three roots, the rule's layer (A.B declares two), "after".
            let own = if !valid_layer {
                0
            } else if rule.contains("A.B") {
                2
            } else {
                1
            };
            assert_eq!(set.layers().len(), 3 + own + 1, "{rule}");
            n += 1;
        }
    }
    assert_eq!(n, 13);
}

/// The crash tests: every page's sheets collected and every element
/// cascaded, without a panic.
#[test]
fn crash_tests() {
    let mut n = 0;
    for dir in ["css-cascade", "css-nesting"] {
        let mut files: Vec<_> = std::fs::read_dir(suite_dir().join(dir))
            .expect("the suite")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|f| f.contains("crash") && f.ends_with(".html"))
            .collect();
        files.sort();
        for f in files {
            let p = page(&load(&format!("{dir}/{f}")));
            let sheets: Vec<&str> = p.styles.iter().map(String::as_str).collect();
            let _ = cascaded(&sheets, &p.tree, "color");
            n += 1;
        }
    }
    assert_eq!(n, 14);
}
