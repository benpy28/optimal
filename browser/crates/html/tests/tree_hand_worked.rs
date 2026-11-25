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
    let c = ctx(
        Namespace::MathMl,
        "annotation-xml",
        &[("encoding", "application/xhtml+xml")],
    );
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

fn doc(input: &str) -> (String, Vec<&'static str>) {
    let p = parse_document(input, options(false));
    p.document.check_invariants();
    (dump(&p), tree_errors(&p.errors))
}

/// Which mode reports an error, and where a comment lands, show which rule
/// handled a token. In each case below the wrong rule would build the same
/// elements, so the comment or the error is what tells them apart.
#[test]
fn head_modes_by_hand() {
    // "before html": a DOCTYPE and a stray end tag are ignored right there.
    let base = "| <!DOCTYPE html>\n| <html>\n|   <head>\n|   <body>";
    assert_eq!(
        doc("<!DOCTYPE html><!DOCTYPE html>"),
        (base.into(), vec!["DOCTYPE in before html"])
    );
    assert_eq!(
        doc("<!DOCTYPE html></p>"),
        (base.into(), vec!["end tag in before html"])
    );
    // "before head": a DOCTYPE is ignored; <html> goes to "in body", which
    // merges attributes and leaves the mode alone, so the comment is a child
    // of html, before head.
    assert_eq!(
        doc("<!DOCTYPE html><html><!DOCTYPE html>"),
        (base.into(), vec!["DOCTYPE in before head"])
    );
    assert_eq!(
        doc("<!DOCTYPE html><html><html a><!--x-->").0,
        "| <!DOCTYPE html>\n| <html>\n|   a=\"\"\n|   <!-- x -->\n|   <head>\n|   <body>"
    );
    // "in head": <html> goes to "in body" without leaving head.
    assert_eq!(
        doc("<!DOCTYPE html><head><html a><!--x-->").0,
        "| <!DOCTYPE html>\n| <html>\n|   a=\"\"\n|   <head>\n|     <!-- x -->\n|   <body>"
    );
    // "after head": a DOCTYPE is ignored; <html> and <head> leave the mode
    // alone, so the comment lands between head and body; </template> goes
    // to "in head".
    assert_eq!(
        doc("<!DOCTYPE html><head></head><!DOCTYPE html>"),
        (base.into(), vec!["DOCTYPE after head"])
    );
    let between = "| <!DOCTYPE html>\n| <html>\n|   <head>\n|   <!-- x -->\n|   <body>";
    assert_eq!(
        doc("<!DOCTYPE html><head></head><head><!--x-->"),
        (between.into(), vec!["head start tag after head"])
    );
    assert_eq!(
        doc("<!DOCTYPE html><head></head><html a><!--x-->").0,
        "| <!DOCTYPE html>\n| <html>\n|   a=\"\"\n|   <head>\n|   <!-- x -->\n|   <body>"
    );
    assert_eq!(
        doc("<!DOCTYPE html><head></head></template>"),
        (base.into(), vec!["template end tag with no template open"])
    );
}

/// "in body" end tags dd and dt, and the select-related start tags, by the
/// errors they report.
#[test]
fn in_body_errors_by_hand() {
    let (_, e) = doc("<!DOCTYPE html><dl><dd><span></dd>");
    assert_eq!(
        e,
        [
            "dd/dt end tag: current node mismatch",
            "EOF with elements open"
        ]
    );
    let (_, e) = doc("<!DOCTYPE html><dl><dd></dd></dl>");
    assert!(e.is_empty(), "{e:?}");
    let (_, e) = doc("<!DOCTYPE html><dl><dt></dd>");
    assert_eq!(
        e,
        [
            "dd/dt end tag without element in scope",
            "EOF with elements open"
        ]
    );
    // An optgroup still in scope after the implied end tags (a span keeps it
    // open) makes <hr> and <optgroup> errors; an option alone would too.
    let (_, e) = doc("<!DOCTYPE html><select><optgroup><span><hr>");
    assert_eq!(e, ["hr in option", "EOF with elements open"]);
    let (_, e) = doc("<!DOCTYPE html><select><optgroup><span><optgroup>");
    assert_eq!(e, ["optgroup in option", "EOF with elements open"]);
    let (_, e) = doc("<!DOCTYPE html><select><option><span><hr>");
    assert_eq!(e, ["hr in option", "EOF with elements open"]);
}

/// Adoption agency runs that reach the outer loop's limit of eight, so the
/// bookmark decides where the last new formatting element goes in the list,
/// and later text shows it through reconstruction. WPT has no such case.
/// These trees are this parser's output (it passes every runnable WPT tree
/// test), pinned so the bookmark handling cannot change unnoticed; each was
/// checked to change under a wrong bookmark.
#[test]
fn adoption_agency_bookmark_cases() {
    let cases: &[(&str, &str)] = &[
        (
            "<em><div><div><div><div><div><code><div><a><div><s><div></em>x</div>x",
            "| <html>\n|   <head>\n|   <body>\n|     <em>\n|     <div>\n|       <em>\n|       <div>\n|         <em>\n|         <div>\n|           <em>\n|           <div>\n|             <em>\n|             <div>\n|               <em>\n|                 <code>\n|               <code>\n|                 <div>\n|                   <em>\n|                     <a>\n|                   <a>\n|                     <div>\n|                       <em>\n|                         <s>\n|                       <s>\n|                         <div>\n|                           <em>\n|                             \"x\"\n|                         <em>\n|                           \"x\"",
        ),
        (
            "<s><div><div><div><em><div><div><div><div><div><div><b></s>xxx</b></em></div>x</div>x</em>y",
            "| <html>\n|   <head>\n|   <body>\n|     <s>\n|     <div>\n|       <s>\n|       <div>\n|         <s>\n|         <div>\n|           <s>\n|             <em>\n|           <em>\n|           <div>\n|             <em>\n|               <s>\n|             <div>\n|               <em>\n|                 <s>\n|               <div>\n|                 <em>\n|                   <s>\n|                 <div>\n|                   <em>\n|                     <s>\n|                   <div>\n|                     <em>\n|                       <s>\n|                     <s>\n|                       <div>\n|                         <em>\n|                           <b>\n|                             \"xxx\"\n|                       \"x\"\n|                   <s>\n|                     \"xy\"",
        ),
        (
            "<b><i><div><div><div><div><div><div><div><div><div></b></div></div>x",
            "| <html>\n|   <head>\n|   <body>\n|     <b>\n|       <i>\n|     <i>\n|       <div>\n|         <b>\n|         <div>\n|           <b>\n|           <div>\n|             <b>\n|             <div>\n|               <b>\n|               <div>\n|                 <b>\n|                 <div>\n|                   <b>\n|                   <div>\n|                     <b>\n|                     <div>\n|                       <b>\n|                         <div>\n|                     <b>\n|                       \"x\"",
        ),
    ];
    for (input, want) in cases {
        let p = parse_document(input, options(false));
        p.document.check_invariants();
        assert_eq!(&dump(&p), want, "{input}");
    }
}
