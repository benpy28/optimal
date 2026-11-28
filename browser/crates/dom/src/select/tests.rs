//! Select model tests.
//!
//! Hand-worked cases name the step of the standard that gives each result.
//! The random tests run the same operations on a normal document and on a
//! [`Document::new_reference`] document (the standard's words, literally)
//! and require the same tree and the same element state after every one.

use super::*;
use crate::{Attribute, Document, Namespace, NodeData, NodeId, local};
use optimal_wtf8::Wtf8Buf;
use proptest::prelude::*;

fn attr(d: &mut Document, name: &str, value: &str) -> Attribute {
    Attribute {
        namespace: Namespace::None,
        prefix: None,
        local: d.names.intern_str(name),
        value: Wtf8Buf::from(value),
    }
}

/// An HTML element with `name` and attributes like `"selected"` or
/// `"size=2"`.
fn el(d: &mut Document, name: &str, attrs: &[&str]) -> NodeId {
    let local = d.names.intern_str(name);
    let attrs = attrs
        .iter()
        .map(|a| {
            let (n, v) = a.split_once('=').unwrap_or((a, ""));
            attr(d, n, v)
        })
        .collect();
    d.create_element(Namespace::Html, local, attrs)
}

fn text(d: &mut Document, s: &str) -> NodeId {
    d.create(NodeData::Text(Wtf8Buf::from(s)))
}

/// A document with `<html><body>` and a select in the body.
fn doc_with_select(select_attrs: &[&str]) -> (Document, NodeId) {
    let mut d = Document::new();
    let html = el(&mut d, "html", &[]);
    d.append(Document::ROOT, html);
    let body = el(&mut d, "body", &[]);
    d.append(html, body);
    let s = el(&mut d, "select", select_attrs);
    d.append(body, s);
    (d, s)
}

fn selected(d: &Document, options: &[NodeId]) -> Vec<bool> {
    options
        .iter()
        .map(|&o| d.option_selectedness(o).unwrap())
        .collect()
}

/// The text of a subtree, for checking selectedcontent clones.
fn text_of(d: &Document, n: NodeId) -> String {
    let mut out = String::new();
    for x in d.descendants(n) {
        if let NodeData::Text(t) = d.data(x) {
            out.push_str(&t.to_string());
        }
    }
    out
}

/// Inserting leaf options one at a time, as the parser does. Selectedness
/// setting algorithm: A is the first enabled option and nothing is
/// selected, so A becomes selected; B has a selected attribute, so A and B
/// are both selected and the last in tree order (B) stays; C changes
/// nothing.
#[test]
fn appended_options() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    d.append(s, a);
    assert_eq!(selected(&d, &[a]), [true]);
    let b = el(&mut d, "option", &["selected"]);
    d.append(s, b);
    let c = el(&mut d, "option", &[]);
    d.append(s, c);
    assert_eq!(selected(&d, &[a, b, c]), [false, true, false]);
    assert_eq!(d.first_selected(s), Some(b));
    d.check_invariants();
}

/// A selected option inserted before the selected one: the list is
/// [B, A], both are selected, and the last (A) stays.
#[test]
fn selected_option_inserted_before() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    d.append(s, a);
    let b = el(&mut d, "option", &["selected"]);
    d.insert_before(s, b, Some(a));
    assert_eq!(selected(&d, &[b, a]), [false, true]);
    d.check_invariants();
}

/// Removing the selected option: the algorithm finds nothing selected and
/// selects the first enabled option, A.
#[test]
fn removing_the_selected_option() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    let b = el(&mut d, "option", &["selected"]);
    d.append(s, a);
    d.append(s, b);
    d.remove(b);
    assert_eq!(selected(&d, &[a, b]), [true, true]);
    // B keeps its selectedness: nothing resets a removed option.
    d.check_invariants();
}

/// Display size 2 (and 0, which parses): no option is selected for you.
/// An unparsable size falls back to 1.
#[test]
fn display_size() {
    for (size, auto) in [
        ("size=2", false),
        ("size=0", false),
        ("size=x", true),
        ("size=+1", true),
    ] {
        let (mut d, s) = doc_with_select(&[size]);
        let a = el(&mut d, "option", &[]);
        d.append(s, a);
        assert_eq!(selected(&d, &[a]), [auto], "{size}");
        d.check_invariants();
    }
}

/// With `multiple` the algorithm returns at once: both stay selected.
#[test]
fn multiple_keeps_every_selection() {
    let (mut d, s) = doc_with_select(&["multiple"]);
    let a = el(&mut d, "option", &["selected"]);
    let b = el(&mut d, "option", &["selected"]);
    let c = el(&mut d, "option", &[]);
    d.append(s, a);
    d.append(s, b);
    d.append(s, c);
    assert_eq!(selected(&d, &[a, b, c]), [true, true, false]);
    assert_eq!(d.display_size(s), 4);
    // Removing `multiple`: all but the first selected option are
    // deselected, then the select's selectedness is reset.
    d.remove_attribute(s, local::MULTIPLE);
    assert_eq!(selected(&d, &[a, b, c]), [true, false, false]);
    d.check_invariants();
}

/// Disabled options are skipped when choosing the first enabled one:
/// directly, and through an optgroup with `disabled`.
#[test]
fn disabled_options_are_not_chosen() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &["disabled"]);
    d.append(s, a);
    let g = el(&mut d, "optgroup", &["disabled"]);
    d.append(s, g);
    let b = el(&mut d, "option", &[]);
    d.append(g, b);
    assert_eq!(selected(&d, &[a, b]), [false, false]);
    assert!(d.option_disabled(b));
    let c = el(&mut d, "option", &[]);
    d.append(s, c);
    assert_eq!(selected(&d, &[a, b, c]), [false, false, true]);
    d.check_invariants();
}

/// Which options are in the list: an option inside a div is; options under
/// a datalist, an hr, another option or a second optgroup level are not.
#[test]
fn list_of_options() {
    let (mut d, s) = doc_with_select(&[]);
    let div = el(&mut d, "div", &[]);
    d.append(s, div);
    let in_div = el(&mut d, "option", &[]);
    d.append(div, in_div);
    let mut excluded = vec![];
    for wrapper in ["datalist", "hr", "option"] {
        let w = el(&mut d, wrapper, &[]);
        d.append(s, w);
        let o = el(&mut d, "option", &[]);
        d.append(w, o);
        excluded.push(o);
    }
    let g1 = el(&mut d, "optgroup", &[]);
    let g2 = el(&mut d, "optgroup", &[]);
    d.append(s, g1);
    d.append(g1, g2);
    let in_g1 = el(&mut d, "option", &[]);
    d.append(g1, in_g1);
    let in_g2 = el(&mut d, "option", &[]);
    d.append(g2, in_g2);
    excluded.push(in_g2);
    // The option wrapper itself is in the list.
    let wrapper_option = d.parent(excluded[2]).unwrap();
    assert_eq!(d.list_of_options(s), vec![in_div, wrapper_option, in_g1]);
    for o in excluded {
        assert!(matches!(d.behaviour(o), Behaviour::Option(st) if st.cached_select.is_none()));
    }
    d.check_invariants();
}

/// Attribute change steps: adding `selected` to an unselected option (not
/// dirty) selects it, and the reset deselects the old one; removing it
/// deselects it, and the reset selects the first enabled option.
#[test]
fn selected_attribute_changes() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    let b = el(&mut d, "option", &[]);
    d.append(s, a);
    d.append(s, b);
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    d.set_attribute(b, local::SELECTED, Wtf8Buf::from(""));
    assert_eq!(selected(&d, &[a, b]), [false, true]);
    d.remove_attribute(b, local::SELECTED);
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    // Disabling the selected option does not deselect it: the algorithm
    // only deselects when two are selected.
    d.set_attribute(a, local::DISABLED, Wtf8Buf::from(""));
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    d.check_invariants();
}

/// An optgroup's `disabled` has no change steps, so the selection stays;
/// but the select's record of "nothing to do" is gone, and the next
/// insertion runs the full algorithm.
#[test]
fn optgroup_disabled_change() {
    let (mut d, s) = doc_with_select(&[]);
    let g = el(&mut d, "optgroup", &[]);
    d.append(s, g);
    let a = el(&mut d, "option", &[]);
    d.append(g, a);
    assert_eq!(selected(&d, &[a]), [true]);
    d.set_attribute(g, local::DISABLED, Wtf8Buf::from(""));
    assert_eq!(selected(&d, &[a]), [true]);
    assert!(d.option_disabled(a));
    let b = el(&mut d, "option", &[]);
    d.append(s, b);
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    d.remove(a);
    assert_eq!(selected(&d, &[b]), [true]);
    d.check_invariants();
}

/// The other direction: with every option disabled nothing is selected;
/// enabling the optgroup has no change steps, so still nothing is; the next
/// insertion runs the algorithm, which picks the first enabled option, A,
/// not the inserted B.
#[test]
fn optgroup_enabled_later() {
    let (mut d, s) = doc_with_select(&[]);
    let g = el(&mut d, "optgroup", &["disabled"]);
    d.append(s, g);
    let a = el(&mut d, "option", &[]);
    d.append(g, a);
    assert_eq!(selected(&d, &[a]), [false]);
    d.remove_attribute(g, local::DISABLED);
    assert_eq!(selected(&d, &[a]), [false]);
    d.check_invariants();
    let b = el(&mut d, "option", &[]);
    d.append(s, b);
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    d.check_invariants();
}

/// selectedcontent mirrors the selected option: the option's insertion
/// queues an update and its post-connection steps run it.
#[test]
fn selectedcontent_follows_the_selection() {
    let (mut d, s) = doc_with_select(&[]);
    let button = el(&mut d, "button", &[]);
    d.append(s, button);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(button, sc);
    let a = el(&mut d, "option", &[]);
    let at = text(&mut d, "A");
    d.append(a, at);
    d.append(s, a);
    assert_eq!(text_of(&d, sc), "A");
    let b = el(&mut d, "option", &[]);
    let bt = text(&mut d, "B");
    d.append(b, bt);
    d.append(s, b);
    assert_eq!(text_of(&d, sc), "A");
    d.set_attribute(b, local::SELECTED, Wtf8Buf::from(""));
    assert_eq!(text_of(&d, sc), "B");
    // Removing B: its removing steps queue an update (no post-connection
    // steps run on removal), so the clone changes at the microtask.
    d.remove(b);
    assert_eq!(text_of(&d, sc), "B");
    assert!(d.has_microtasks());
    d.perform_microtask_checkpoint();
    assert_eq!(text_of(&d, sc), "A");
    assert!(!d.has_microtasks());
    d.check_invariants();
}

/// A select built while detached: insertion steps stop at "not connected",
/// so nothing is cloned until the select is inserted into the document.
#[test]
fn nothing_is_cloned_until_connected() {
    let mut d = Document::new();
    let html = el(&mut d, "html", &[]);
    d.append(Document::ROOT, html);
    let s = el(&mut d, "select", &[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &[]);
    let at = text(&mut d, "A");
    d.append(a, at);
    d.append(s, a);
    assert_eq!(selected(&d, &[a]), [true]);
    assert_eq!(text_of(&d, sc), "");
    d.append(html, s);
    assert_eq!(text_of(&d, sc), "A");
    d.check_invariants();
}

/// A selectedcontent inside an option, a second select or another
/// selectedcontent is disabled and never updated.
#[test]
fn disabled_selectedcontent() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    let at = text(&mut d, "A");
    d.append(a, at);
    d.append(s, a);
    let outer = el(&mut d, "selectedcontent", &[]);
    d.append(s, outer);
    assert_eq!(text_of(&d, outer), "A");
    let inner = el(&mut d, "selectedcontent", &[]);
    let in_option = el(&mut d, "selectedcontent", &[]);
    d.append(a, in_option);
    let s2 = el(&mut d, "select", &[]);
    let in_s2 = el(&mut d, "selectedcontent", &[]);
    d.append(s2, in_s2);
    d.append(s, s2);
    let b = el(&mut d, "option", &["selected"]);
    let bt = text(&mut d, "B");
    d.append(b, bt);
    d.append(s, b);
    // The clone of B replaced `outer`'s children.
    assert_eq!(text_of(&d, outer), "B");
    d.append(outer, inner);
    for c in [inner, in_option, in_s2] {
        assert!(
            matches!(d.behaviour(c), Behaviour::SelectedContent(st) if st.disabled),
            "{c:?}"
        );
    }
    d.check_invariants();
}

/// The clone leaves out script, SVG script, frame, iframe, embed and
/// object elements, wherever they are in the option.
#[test]
fn clone_leaves_out_scripts_and_frames() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &[]);
    let span = el(&mut d, "span", &[]);
    d.append(a, span);
    for name in ["script", "iframe", "frame", "embed", "object", "b"] {
        let e = el(&mut d, name, &[]);
        d.append(span, e);
    }
    let svg_script = d.create_element(Namespace::Svg, local::SCRIPT, vec![]);
    d.append(span, svg_script);
    d.append(s, a);
    let names: Vec<String> = d
        .descendants(sc)
        .into_iter()
        .map(|n| d.name_text(d.element(n).unwrap().local).to_string())
        .collect();
    assert_eq!(names, ["span", "b"]);
    d.check_invariants();
}

/// An option nested in the selected option: its clone joins the same
/// select, and under the standard's words each update would clone again
/// for ever. Here the update's own mutations do not re-trigger it: one
/// clone, the nested clone deselected (A is later in tree order).
#[test]
fn nested_option_clone_terminates() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &[]);
    let at = text(&mut d, "A");
    d.append(a, at);
    let nested = el(&mut d, "option", &["selected"]);
    d.append(a, nested);
    d.append(s, a);
    let clones: Vec<NodeId> = d
        .descendants(sc)
        .into_iter()
        .filter(|&n| is_html(&d, n, local::OPTION))
        .collect();
    assert_eq!(clones.len(), 1);
    assert_eq!(selected(&d, &[a, clones[0]]), [true, false]);
    d.perform_microtask_checkpoint();
    assert_eq!(d.descendants(sc).len(), 2, "text and the one cloned option");
    d.check_invariants();
}

/// Cloning is iterative (no stack overflow) and copies template contents.
#[test]
fn clone_node_deep_and_templates() {
    let mut d = Document::new();
    let root = el(&mut d, "div", &[]);
    let mut n = root;
    for _ in 0..100_000 {
        let c = el(&mut d, "div", &[]);
        d.append(n, c);
        n = c;
    }
    let copy = d.clone_node(root, true);
    assert_eq!(d.descendants(copy).len(), 100_000);
    let t = el(&mut d, "template", &["id=t"]);
    let contents = d.element(t).unwrap().template_contents.unwrap();
    let x = text(&mut d, "x");
    d.append(contents, x);
    let tc = d.clone_node(t, true);
    let tc_contents = d.element(tc).unwrap().template_contents.unwrap();
    assert_ne!(tc_contents, contents);
    assert_eq!(text_of(&d, tc_contents), "x");
    assert_eq!(
        d.element(tc).unwrap().attributes,
        d.element(t).unwrap().attributes
    );
    let shallow = d.clone_node(t, false);
    assert!(
        d.first_child(d.element(shallow).unwrap().template_contents.unwrap())
            .is_none()
    );
}

/// The rules for parsing non-negative integers.
#[test]
fn non_negative_integers() {
    for (s, v) in [
        ("1", Some(1)),
        ("  \t\n2", Some(2)),
        ("+3", Some(3)),
        ("-0", Some(0)),
        ("-1", None),
        ("4x", Some(4)),
        ("x", None),
        ("", None),
        ("+", None),
        (" ", None),
        ("99999999999999999999999", Some(u64::MAX)),
    ] {
        assert_eq!(parse_non_negative_integer(s.as_bytes()), v, "{s:?}");
    }
}

// ---- The fast paths are taken -----------------------------------------
//
// These check work counters, not results: a broken fast path that still
// gives the right answer (just slower) must fail a test too.

#[test]
fn reference_documents_are_literal() {
    assert!(Document::new_reference().model.reference);
    assert!(!Document::new().model.reference);
}

/// 1000 appended options: one full run (the first, when the select has no
/// record yet), then the record is updated in O(1).
#[test]
fn appending_options_runs_the_algorithm_once() {
    let (mut d, s) = doc_with_select(&[]);
    let before = d.behaviour_stats().full_runs;
    for i in 0..1000 {
        let o = el(
            &mut d,
            "option",
            if i % 3 == 0 { &["selected"] } else { &[] },
        );
        d.append(s, o);
    }
    assert_eq!(d.behaviour_stats().full_runs - before, 1);
    let (mut r, rs) = {
        let mut r = Document::new_reference();
        let html = el(&mut r, "html", &[]);
        r.append(Document::ROOT, html);
        let s = el(&mut r, "select", &[]);
        r.append(html, s);
        (r, s)
    };
    for _ in 0..10 {
        let o = el(&mut r, "option", &[]);
        r.append(rs, o);
    }
    assert_eq!(
        r.behaviour_stats().full_runs,
        10,
        "the literal model runs every time"
    );
}

/// A subtree with one option moves like a single option: no full run.
/// Two options moving together need one full run per select.
#[test]
fn moving_one_option_in_a_subtree_is_incremental() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    d.append(s, a);
    let div = el(&mut d, "div", &[]);
    // Not selected: removing the selected option needs a full run to find
    // the first enabled option, whatever moved.
    let b = el(&mut d, "option", &[]);
    d.append(div, b);
    let before = d.behaviour_stats().full_runs;
    d.append(s, div);
    d.remove(div);
    d.append(s, div);
    assert_eq!(d.behaviour_stats().full_runs, before);
    let c = el(&mut d, "option", &[]);
    d.append(div, c);
    let before = d.behaviour_stats().full_runs;
    d.remove(div);
    assert_eq!(d.behaviour_stats().full_runs, before + 1);
    d.check_invariants();
}

/// Options under 2000 nested divs: each ancestor's context is computed
/// once, not once per option.
#[test]
fn ancestor_contexts_are_memoised() {
    let (mut d, s) = doc_with_select(&[]);
    let mut p = s;
    for _ in 0..2000 {
        let div = el(&mut d, "div", &[]);
        d.append(p, div);
        p = div;
    }
    let before = d.behaviour_stats().ctx_steps;
    for _ in 0..2000 {
        let o = el(&mut d, "option", &[]);
        d.append(p, o);
    }
    let steps = d.behaviour_stats().ctx_steps - before;
    // The first option walks the 2000 divs once.
    assert!(
        (2000..2 * 2000 + 100).contains(&steps),
        "{steps} context steps"
    );
}

/// Moving a subtree that never held an option or selectedcontent visits
/// one node (its root, to find it unflagged); one that holds one visits
/// only the path to it.
#[test]
fn moves_visit_flagged_nodes_only() {
    let (mut d, s) = doc_with_select(&[]);
    let big = el(&mut d, "div", &[]);
    for _ in 0..1000 {
        let x = el(&mut d, "span", &[]);
        d.append(big, x);
    }
    d.append(s, big);
    let before = d.behaviour_stats().visited;
    d.remove(big);
    d.append(s, big);
    assert_eq!(d.behaviour_stats().visited, before);
    let o = el(&mut d, "option", &[]);
    d.append(big, o);
    let before = d.behaviour_stats().visited;
    d.remove(big);
    // root and option, each in the removal walk
    assert_eq!(d.behaviour_stats().visited - before, 2);
}

// ---- The ancestor walks, literally ----------------------------------------
//
// The normal and the literal document share the fold behind every upward
// walk (`Document::ctx_step`), so comparing them cannot catch a mistake in
// it. These are the standard's algorithms word for word, independent of
// that fold.

fn has_attr(d: &Document, n: NodeId, name: LocalName) -> bool {
    d.element(n).is_some_and(|e| e.attribute(name).is_some())
}

fn ancestors(d: &Document, n: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut a = d.parent(n);
    while let Some(x) = a {
        out.push(x);
        a = d.parent(x);
    }
    out
}

/// "Get the nearest ancestor select" (4.10.10).
fn literal_nearest_select(d: &Document, o: NodeId) -> Option<NodeId> {
    let mut ancestor_optgroup = None;
    for a in ancestors(d, o) {
        if is_html(d, a, local::DATALIST)
            || is_html(d, a, local::HR)
            || is_html(d, a, local::OPTION)
        {
            return None;
        }
        if is_html(d, a, local::OPTGROUP) {
            if ancestor_optgroup.is_some() {
                return None;
            }
            ancestor_optgroup = Some(a);
        }
        if is_html(d, a, local::SELECT) {
            return Some(a);
        }
    }
    None
}

/// An option is disabled (4.10.10).
fn literal_option_disabled(d: &Document, o: NodeId) -> bool {
    if has_attr(d, o, local::DISABLED) {
        return true;
    }
    for a in ancestors(d, o) {
        if [local::SELECT, local::HR, local::DATALIST, local::OPTION]
            .iter()
            .any(|&n| is_html(d, a, n))
        {
            return false;
        }
        if is_html(d, a, local::OPTGROUP) {
            return has_attr(d, a, local::DISABLED);
        }
    }
    false
}

/// "Recalculate a selectedcontent element's disabledness": the cached
/// select and the disabled flag.
fn literal_recalc(d: &Document, c: NodeId) -> (Option<NodeId>, bool) {
    let mut nearest = None;
    let mut disabled = false;
    for a in ancestors(d, c) {
        if is_html(d, a, local::SELECT) {
            if nearest.is_none() {
                nearest = Some(a);
                continue;
            }
            disabled = true;
            break;
        }
        if is_html(d, a, local::OPTION) || is_html(d, a, local::SELECTEDCONTENT) {
            disabled = true;
            break;
        }
    }
    (nearest, disabled)
}

/// Every cached ancestor result in `d` against the literal walks.
fn check_literal(d: &mut Document) {
    for i in 0..d.node_count() {
        let n = NodeId(i as u32);
        let connected = n == Document::ROOT || ancestors(d, n).last() == Some(&Document::ROOT);
        if n != Document::ROOT {
            assert_eq!(d.is_connected_mut(n), connected, "connected {i}");
        }
        let mut selects: Vec<NodeId> = std::iter::once(n)
            .chain(ancestors(d, n))
            .filter(|&a| is_html(d, a, local::SELECT))
            .collect();
        assert_eq!(d.select_chain(n), selects, "selects at or above {i}");
        selects.clear();
        match d.behaviour(n).clone() {
            Behaviour::Option(st) => {
                assert_eq!(
                    st.cached_select,
                    literal_nearest_select(d, n),
                    "select of {i}"
                );
                let disabled = literal_option_disabled(d, n);
                assert_eq!(d.option_disabled(n), disabled, "option {i} disabled");
                assert_eq!(d.option_disabled_mut(n), disabled, "option {i} disabled");
            }
            Behaviour::SelectedContent(st) => {
                assert_eq!(
                    (st.cached_select, st.disabled),
                    literal_recalc(d, n),
                    "selectedcontent {i}"
                );
            }
            _ => {}
        }
    }
}

const CHAIN: &[(&str, &[&str])] = &[
    ("select", &[]),
    ("option", &[]),
    ("optgroup", &[]),
    ("optgroup", &["disabled"]),
    ("datalist", &[]),
    ("hr", &[]),
    ("selectedcontent", &[]),
    ("div", &[]),
];

const LEAVES: &[(&str, &[&str])] = &[
    ("option", &[]),
    ("option", &["disabled"]),
    ("selectedcontent", &[]),
    ("select", &[]),
];

/// Every chain of up to three of the elements that matter, ending in an
/// option, a selectedcontent or a select; connected or not; built top down
/// (each element appended to the last) and bottom up (the finished chain
/// inserted at once); in both documents.
#[test]
fn ancestor_walks_match_the_standard_on_every_short_chain() {
    let n = CHAIN.len();
    let mut chains: Vec<Vec<usize>> = vec![vec![]];
    for len in 1..=3 {
        for mut k in 0..n.pow(len) {
            let mut c = Vec::new();
            for _ in 0..len {
                c.push(k % n);
                k /= n;
            }
            chains.push(c);
        }
    }
    for chain in &chains {
        for leaf in LEAVES {
            for connected in [true, false] {
                for top_down in [true, false] {
                    for reference in [false, true] {
                        let mut d = if reference {
                            Document::new_reference()
                        } else {
                            Document::new()
                        };
                        let html = el(&mut d, "html", &[]);
                        if connected {
                            d.append(Document::ROOT, html);
                        }
                        let mut kinds: Vec<(&str, &[&str])> =
                            chain.iter().map(|&k| CHAIN[k]).collect();
                        kinds.push(*leaf);
                        let nodes: Vec<NodeId> =
                            kinds.iter().map(|(name, a)| el(&mut d, name, a)).collect();
                        if top_down {
                            let mut p = html;
                            for &x in &nodes {
                                d.append(p, x);
                                p = x;
                            }
                        } else {
                            for w in nodes.windows(2).rev() {
                                d.append(w[0], w[1]);
                            }
                            d.append(html, nodes[0]);
                        }
                        check_literal(&mut d);
                        d.check_invariants();
                    }
                }
            }
        }
    }
}

// ---- Steps with no visible difference between the two documents --------

/// Option A is created selected and inserted with its text. The algorithm
/// changes nothing, but A is selected, so "update an option's nearest
/// ancestor select" returns the select and the selectedcontent shows A.
#[test]
fn inserting_a_selected_option_updates_selectedcontent() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &["selected"]);
    let t = text(&mut d, "A");
    d.append(a, t);
    d.append(s, a);
    assert_eq!(text_of(&d, sc), "A");
    d.check_invariants();
}

/// Display size 2: adding selected to A selects it and the algorithm has
/// nothing left to change, so only hasChanged makes the update happen.
#[test]
fn selected_attribute_with_display_size_two_updates_selectedcontent() {
    let (mut d, s) = doc_with_select(&["size=2"]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &[]);
    let t = text(&mut d, "A");
    d.append(a, t);
    d.append(s, a);
    d.perform_microtask_checkpoint();
    assert_eq!(text_of(&d, sc), "");
    d.set_attribute(a, local::SELECTED, Wtf8Buf::new());
    assert_eq!(text_of(&d, sc), "A");
    d.check_invariants();
}

/// A disabled attribute is not a selected attribute: adding it to B
/// leaves B unselected and A selected.
#[test]
fn disabled_attribute_does_not_select() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    let b = el(&mut d, "option", &[]);
    d.append(s, a);
    d.append(s, b);
    d.set_attribute(b, local::DISABLED, Wtf8Buf::new());
    assert_eq!(selected(&d, &[a, b]), [true, false]);
    d.check_invariants();
}

/// Attribute change steps that return early run nothing: another
/// attribute on an option, a select or an optgroup; a value change of
/// multiple. Removing selected from an option the algorithm already
/// deselected runs the algorithm but updates nothing.
#[test]
fn attribute_changes_that_change_nothing_run_nothing() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let g = el(&mut d, "optgroup", &[]);
    d.append(s, g);
    // B then A, both with selected: the algorithm keeps the last, A.
    let b = el(&mut d, "option", &["selected"]);
    d.append(g, b);
    let a = el(&mut d, "option", &["selected"]);
    d.append(s, a);
    d.perform_microtask_checkpoint();
    assert_eq!(selected(&d, &[b, a]), [false, true]);
    let label = d.names.intern_str("label");
    let name = d.names.intern_str("name");
    let quiet = |d: &mut Document, f: &dyn Fn(&mut Document)| {
        let before = d.behaviour_stats();
        f(d);
        let after = d.behaviour_stats();
        let what = d.behaviour_dump();
        assert_eq!(after.full_runs, before.full_runs, "{what}");
        assert_eq!(after.content_updates, before.content_updates, "{what}");
        assert!(d.select_ref(s).settled);
        d.check_invariants();
    };
    quiet(&mut d, &|d| d.set_attribute(a, label, Wtf8Buf::from("x")));
    quiet(&mut d, &|d| {
        d.set_attribute(a, local::SELECTED, Wtf8Buf::from("x"))
    });
    quiet(&mut d, &|d| d.set_attribute(s, name, Wtf8Buf::from("x")));
    quiet(&mut d, &|d| d.remove_attribute(s, name));
    quiet(&mut d, &|d| d.set_attribute(g, label, Wtf8Buf::from("x")));
    // The steps go on to reset the select's selectedness (one run of the
    // algorithm), which changes nothing, and hasChanged is false: no
    // update.
    let before = d.behaviour_stats();
    d.remove_attribute(b, local::SELECTED);
    assert_eq!(d.behaviour_stats().full_runs, before.full_runs + 1);
    assert_eq!(d.behaviour_stats().content_updates, before.content_updates);
    assert_eq!(selected(&d, &[b, a]), [false, true]);
    // The optgroup's context memo survives an unrelated change.
    assert!(d.model.memo.contains_key(&g));
    let m = el(&mut d, "select", &["multiple"]);
    d.append(s, m);
    let before = d.behaviour_stats();
    d.set_attribute(m, local::MULTIPLE, Wtf8Buf::from("x"));
    assert_eq!(d.behaviour_stats().content_updates, before.content_updates);
}

/// Setting an attribute replaces the one with that name and no namespace,
/// and leaves the others.
#[test]
fn set_attribute_replaces_only_its_own_attribute() {
    let mut d = Document::new();
    let o = el(&mut d, "option", &["label=L", "selected=1"]);
    let xlink = d.names.intern_str("selected");
    d.element_mut(o).unwrap().attributes.insert(
        0,
        Attribute {
            namespace: Namespace::XLink,
            prefix: None,
            local: xlink,
            value: Wtf8Buf::from("ns"),
        },
    );
    d.set_attribute(o, local::SELECTED, Wtf8Buf::from("2"));
    let attrs: Vec<String> = d
        .element(o)
        .unwrap()
        .attributes
        .iter()
        .map(|a| format!("{:?} {}", a.namespace, a.value))
        .collect();
    assert_eq!(attrs, ["XLink ns", "None L", "None 2"]);
}

/// Queueing twice queues once; a select with no selectedcontent queues
/// nothing.
#[test]
fn queue_update_queues_once_and_only_with_selectedcontent() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    d.append(s, a);
    assert!(!d.has_microtasks());
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    d.perform_microtask_checkpoint();
    d.queue_update(s);
    d.queue_update(s);
    assert_eq!(d.model.microtasks.len(), 1);
    let before = d.behaviour_stats().content_updates;
    d.perform_microtask_checkpoint();
    assert_eq!(d.behaviour_stats().content_updates, before + 1);
    // Nothing pending: a stale queue entry does nothing.
    d.model.microtasks.push_back(s);
    d.perform_microtask_checkpoint();
    assert_eq!(d.behaviour_stats().content_updates, before + 1);
}

/// The option children parsed steps clone the option as it is now.
#[test]
fn children_parsed_clones_the_finished_option() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let a = el(&mut d, "option", &[]);
    d.append(s, a);
    d.perform_microtask_checkpoint();
    let t = text(&mut d, "A");
    d.append(a, t);
    assert_eq!(text_of(&d, sc), "");
    d.children_parsed(a);
    assert_eq!(text_of(&d, sc), "A");
    // Not an option, or not selected: nothing.
    let b = el(&mut d, "option", &[]);
    let t = text(&mut d, "B");
    d.append(b, t);
    d.append(s, b);
    d.children_parsed(b);
    d.children_parsed(s);
    assert_eq!(text_of(&d, sc), "A");
}

/// On a settled select the first selected option is the recorded one; the
/// literal document always walks the list. (Shown by corrupting the
/// record.)
#[test]
fn first_selected_uses_the_record_only_when_fast() {
    for reference in [false, true] {
        let mut d = if reference {
            Document::new_reference()
        } else {
            Document::new()
        };
        let s = el(&mut d, "select", &[]);
        d.append(Document::ROOT, s);
        let a = el(&mut d, "option", &[]);
        let b = el(&mut d, "option", &[]);
        d.append(s, a);
        d.append(s, b);
        assert!(d.select_ref(s).settled);
        d.select_state(s).selected = Some(b);
        assert_eq!(d.first_selected(s), Some(if reference { a } else { b }));
    }
}

/// Memos made by the insertion steps stay after the post-connection steps.
#[test]
fn post_connection_keeps_memos() {
    let (mut d, s) = doc_with_select(&[]);
    let div = el(&mut d, "div", &[]);
    let o = el(&mut d, "option", &[]);
    d.append(div, o);
    d.append(s, div);
    assert!(d.model.memo.contains_key(&div));
}

/// Tree order between nodes where one contains the other, siblings either
/// way round, and cousins.
#[test]
fn tree_order() {
    let (mut d, s) = doc_with_select(&[]);
    let kids: Vec<NodeId> = (0..4)
        .map(|_| {
            let x = el(&mut d, "div", &[]);
            d.append(s, x);
            x
        })
        .collect();
    let inner = el(&mut d, "div", &[]);
    d.append(kids[1], inner);
    assert!(d.precedes(s, inner));
    assert!(!d.precedes(inner, s));
    assert!(d.precedes(kids[1], inner));
    assert!(d.precedes(inner, kids[2]));
    assert!(!d.precedes(kids[3], inner));
    for i in 0..4 {
        for j in 0..4 {
            if i != j {
                assert_eq!(d.precedes(kids[i], kids[j]), i < j, "{i} {j}");
            }
        }
    }
}

/// The dump names each stateful element's state.
#[test]
fn behaviour_dump_lists_state() {
    let (mut d, s) = doc_with_select(&[]);
    let sc = el(&mut d, "selectedcontent", &[]);
    d.append(s, sc);
    let o = el(&mut d, "option", &[]);
    d.append(s, o);
    let dump = d.behaviour_dump();
    let (s, sc, o) = (s.index(), sc.index(), o.index());
    assert!(
        dump.contains(&format!("{s} select pending=false")),
        "{dump}"
    );
    assert!(
        dump.contains(&format!(
            "{sc} selectedcontent disabled=false select=Some(NodeId({s}))"
        )),
        "{dump}"
    );
    assert!(
        dump.contains(&format!(
            "{o} option selected=true dirty=false select=Some(NodeId({s}))"
        )),
        "{dump}"
    );
    // Each insertion queued a microtask; the post-connection steps did the
    // updates, so both entries will find nothing pending.
    assert!(
        dump.ends_with(&format!("microtasks [NodeId({s}), NodeId({s})]\n")),
        "{dump}"
    );
}

/// The invariant check finds broken state.
#[test]
#[should_panic(expected = "flags upward closed")]
fn invariants_catch_unflagged_parents() {
    let (mut d, s) = doc_with_select(&[]);
    let o = el(&mut d, "option", &[]);
    d.append(s, o);
    d.model.flagged[s.index()] = false;
    d.model.memo.remove(&s);
    d.check_invariants();
}

#[test]
#[should_panic(expected = "option's cached select")]
fn invariants_catch_a_wrong_cached_select() {
    let (mut d, s) = doc_with_select(&[]);
    let o = el(&mut d, "option", &[]);
    d.append(s, o);
    d.option_state(o).cached_select = None;
    d.check_invariants();
}

// ---- Random operations, fast against literal ---------------------------

const KINDS: &[(&str, &[&str])] = &[
    ("select", &[]),
    ("select", &["multiple"]),
    ("select", &["size=2"]),
    ("option", &[]),
    ("option", &["selected"]),
    ("option", &["disabled"]),
    ("option", &["selected", "disabled"]),
    ("optgroup", &[]),
    ("optgroup", &["disabled"]),
    ("datalist", &[]),
    ("hr", &[]),
    ("div", &[]),
    ("button", &[]),
    ("selectedcontent", &[]),
    ("script", &[]),
    ("template", &[]),
];

#[derive(Clone, Debug)]
enum Op {
    Create(usize),
    Text,
    Append(usize, usize),
    InsertBefore(usize, usize, usize),
    Remove(usize),
    MoveChildren(usize, usize),
    SetAttr(usize, usize),
    RemoveAttr(usize, usize),
    ChildrenParsed(usize),
    Checkpoint,
    Clone(usize, bool),
}

const ATTRS: &[LocalName] = &[
    local::SELECTED,
    local::DISABLED,
    local::MULTIPLE,
    local::SIZE,
];

fn op() -> impl Strategy<Value = Op> {
    let i = || 0usize..64;
    prop_oneof![
        3 => (0..KINDS.len()).prop_map(Op::Create),
        1 => Just(Op::Text),
        6 => (i(), i()).prop_map(|(a, b)| Op::Append(a, b)),
        2 => (i(), i(), i()).prop_map(|(a, b, c)| Op::InsertBefore(a, b, c)),
        2 => i().prop_map(Op::Remove),
        1 => (i(), i()).prop_map(|(a, b)| Op::MoveChildren(a, b)),
        2 => (i(), 0..ATTRS.len()).prop_map(|(a, b)| Op::SetAttr(a, b)),
        1 => (i(), 0..ATTRS.len()).prop_map(|(a, b)| Op::RemoveAttr(a, b)),
        2 => i().prop_map(Op::ChildrenParsed),
        1 => Just(Op::Checkpoint),
        1 => (i(), any::<bool>()).prop_map(|(a, b)| Op::Clone(a, b)),
    ]
}

/// Links and data of every node, without the behaviour caches.
fn shape(d: &Document) -> Vec<String> {
    (0..d.node_count())
        .map(|i| {
            let n = NodeId(i as u32);
            let data = match d.data(n) {
                NodeData::Element(e) => format!(
                    "{:?} {:?} {:?} {:?}",
                    e.namespace, e.local, e.attributes, e.template_contents
                ),
                other => format!("{other:?}"),
            };
            format!(
                "{i}: parent={:?} children={:?} {data}",
                d.parent(n),
                d.children(n).collect::<Vec<_>>()
            )
        })
        .collect()
}

/// Apply `op` to both documents, if it is valid. Indices pick nodes modulo
/// the node count; node 0 is the document.
fn apply(docs: &mut [Document; 2], op: &Op) {
    let count = docs[0].node_count();
    let pick = |i: usize| NodeId((i % count) as u32);
    for d in docs.iter_mut() {
        match *op {
            Op::Create(k) => {
                let (name, attrs) = KINDS[k];
                el(d, name, attrs);
            }
            Op::Text => {
                text(d, "t");
            }
            Op::Append(p, c) | Op::InsertBefore(p, c, _) => {
                let (p, c) = (pick(p), pick(c));
                let reference = match *op {
                    Op::InsertBefore(.., r) => d.children(p).nth(r % 4),
                    _ => None,
                };
                let parent_ok = match d.data(p) {
                    NodeData::Element(_) | NodeData::DocumentFragment => true,
                    NodeData::Document => !d.has_element_child(p),
                    _ => false,
                };
                let child_ok = match d.data(c) {
                    NodeData::Element(_) => true,
                    NodeData::Text(_) => p != Document::ROOT,
                    _ => false,
                };
                if parent_ok && child_ok && !d.would_cycle(c, p) {
                    d.insert_before(p, c, reference);
                }
            }
            Op::Remove(n) => d.remove(pick(n)),
            Op::MoveChildren(a, b) => {
                let (a, b) = (pick(a), pick(b));
                let ok = d.element(a).is_some()
                    && d.element(b).is_some()
                    && !d
                        .children(a)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .any(|c| d.would_cycle(c, b));
                if ok {
                    d.move_children(a, b);
                }
            }
            Op::SetAttr(n, a) => {
                let n = pick(n);
                if d.element(n).is_some() {
                    let v = if ATTRS[a] == local::SIZE { "1" } else { "" };
                    d.set_attribute(n, ATTRS[a], Wtf8Buf::from(v));
                }
            }
            Op::RemoveAttr(n, a) => {
                let n = pick(n);
                if d.element(n).is_some() {
                    d.remove_attribute(n, ATTRS[a]);
                }
            }
            Op::ChildrenParsed(n) => d.children_parsed(pick(n)),
            Op::Checkpoint => d.perform_microtask_checkpoint(),
            Op::Clone(n, deep) => {
                let n = pick(n);
                if n != Document::ROOT {
                    d.clone_node(n, deep);
                }
            }
        }
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn fast_model_matches_the_literal_one(ops in prop::collection::vec(op(), 1..120)) {
        let mut docs = [Document::new(), Document::new_reference()];
        for d in docs.iter_mut() {
            let html = el(d, "html", &[]);
            d.append(Document::ROOT, html);
        }
        for (i, op) in ops.iter().enumerate() {
            apply(&mut docs, op);
            for d in docs.iter_mut() {
                d.check_invariants();
                check_literal(d);
            }
            prop_assert_eq!(shape(&docs[0]), shape(&docs[1]), "tree after op {} {:?}", i, op);
            prop_assert_eq!(
                docs[0].behaviour_dump(),
                docs[1].behaviour_dump(),
                "state after op {} {:?}", i, op
            );
        }
    }
}

#[test]
#[should_panic(expected = "settled record")]
fn invariants_catch_a_wrong_settled_record() {
    let (mut d, s) = doc_with_select(&[]);
    let a = el(&mut d, "option", &[]);
    let b = el(&mut d, "option", &[]);
    d.append(s, a);
    d.append(s, b);
    d.select_state(s).selected = Some(b);
    d.check_invariants();
}
