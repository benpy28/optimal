//! Hand-worked tree construction cases for paths the WPT suite does not
//! reach. Each expected tree is worked out from the standard, with the step
//! named, not copied from a run of this parser.

mod common;

use common::dump;
use optimal_dom::{Namespace, QuirksMode};
use optimal_html::tree_builder::{
    FragmentContext, ParseOptions, TreeError, parse_document, parse_fragment,
};

fn options(scripting: bool) -> ParseOptions {
    ParseOptions {
        scripting,
        iframe_srcdoc: false,
        collect_errors: true,
    }
}

fn ctx(namespace: Namespace, name: &str, attributes: &[(&str, &str)]) -> FragmentContext {
    FragmentContext {
        namespace,
        local_name: name.to_string(),
        attributes: attributes
            .iter()
            .map(|&(n, v)| (n.to_string(), v.to_string()))
            .collect(),
        quirks_mode: QuirksMode::NoQuirks,
    }
}

fn fragment(input: &str, c: &FragmentContext, scripting: bool) -> String {
    let p = parse_fragment(input, c, options(scripting));
    p.document.check_invariants();
    dump(&p)
}

fn tree_errors(errors: &[TreeError]) -> Vec<&'static str> {
    errors
        .iter()
        .filter_map(|e| match e {
            TreeError::Tree(w) => Some(*w),
            TreeError::Tokenizer(_) => None,
        })
        .collect()
}

/// Fragment parsing step: a `noscript` context switches the tokenizer to
/// RAWTEXT only when scripting is enabled.
#[test]
fn noscript_context_depends_on_scripting() {
    let c = ctx(Namespace::Html, "noscript", &[]);
    assert_eq!(fragment("<b>x</b>", &c, true), "| \"<b>x</b>\"");
    assert_eq!(fragment("<b>x</b>", &c, false), "| <b>\n|   \"x\"");
}

/// Fragment parsing step: an `annotation-xml` context with an HTML encoding
/// is an HTML integration point, so the dispatcher sends a start tag such as
/// `<foo>` to the HTML rules. Without one (or for any other MathML context,
/// or an SVG `annotation-xml`) it is foreign content's "any other start tag"
/// and takes the adjusted current node's namespace. (A breakout tag such as
/// `<div>` would not tell them apart: in a fragment, the current node is the
/// HTML root, so the breakout pops nothing.)
#[test]
fn annotation_xml_context_integration_point() {
    let html = [("encoding", "text/html")];
    let c = ctx(Namespace::MathMl, "annotation-xml", &html);
    assert_eq!(fragment("<foo>", &c, false), "| <foo>");
    let c = ctx(Namespace::MathMl, "annotation-xml", &[("encoding", "application/xhtml+xml")]);
    assert_eq!(fragment("<foo>", &c, false), "| <foo>");
    let c = ctx(Namespace::MathMl, "annotation-xml", &[]);
    assert_eq!(fragment("<foo>", &c, false), "| <math foo>");
    let c = ctx(Namespace::MathMl, "mrow", &html);
    assert_eq!(fragment("<foo>", &c, false), "| <math foo>");
    let c = ctx(Namespace::Svg, "annotation-xml", &html);
    assert_eq!(fragment("<foo>", &c, false), "| <svg foo>");
}

/// Fragment parsing step: a `template` context pushes "in template" onto
/// the stack of template insertion modes, so `<td>` is accepted directly.
/// Any other context leaves that stack empty: in a `div` context, EOF with
/// an open `span` is handled by "in body" and is a parse error, where "in
/// template" would stop silently.
#[test]
fn template_context_uses_in_template() {
    let t = ctx(Namespace::Html, "template", &[]);
    assert_eq!(fragment("<td>x", &t, false), "| <td>\n|   \"x\"");
    let d = ctx(Namespace::Html, "div", &[]);
    assert_eq!(fragment("<td>x", &d, false), "| \"x\"");
    let p = parse_fragment("<span>", &d, options(false));
    assert_eq!(tree_errors(&p.errors).len(), 1, "{:?}", p.errors);
    // An SVG template is not a template.
    let s = ctx(Namespace::Svg, "template", &[]);
    assert_eq!(fragment("<td>x", &s, false), "| <svg td>\n|   \"x\"");
}

/// Fragment parsing step: the form element pointer is the context element
/// when that is an HTML `form`, so a `<form>` start tag is then ignored.
#[test]
fn form_context_sets_the_form_pointer() {
    let f = ctx(Namespace::Html, "form", &[]);
    assert_eq!(fragment("<form>x", &f, false), "| \"x\"");
    let d = ctx(Namespace::Html, "div", &[]);
    assert_eq!(fragment("<form>x", &d, false), "| <form>\n|   \"x\"");
    let s = ctx(Namespace::Svg, "form", &[]);
    assert_eq!(fragment("<form>x", &s, false), "| <svg form>\n|   \"x\"");
}

/// The tree builder reports its errors: an end tag with nothing to close.
#[test]
fn tree_errors_are_reported() {
    let p = parse_document("<!DOCTYPE html></x>", options(false));
    assert_eq!(tree_errors(&p.errors).len(), 1, "{:?}", p.errors);
    let p = parse_document("<!DOCTYPE html><p>x</p>", options(false));
    assert!(p.errors.is_empty(), "{:?}", p.errors);
}
