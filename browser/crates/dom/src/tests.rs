//! DOM tree tests. The reference model is a plain map from each node to the
//! `Vec` of its children.

use super::*;
use proptest::prelude::*;

fn text(doc: &mut Document, s: &str) -> NodeId {
    doc.create(NodeData::Text(Wtf8Buf::from(s)))
}

#[test]
fn insert_and_remove_by_hand() {
    let mut d = Document::new();
    let html = d.create_element(Namespace::Html, local::HTML, vec![]);
    d.append(Document::ROOT, html);
    let a = text(&mut d, "a");
    let b = text(&mut d, "b");
    let c = text(&mut d, "c");
    d.append(html, a);
    d.append(html, c);
    d.insert_before(html, b, Some(c));
    assert_eq!(d.children(html).collect::<Vec<_>>(), vec![a, b, c]);
    d.remove(b);
    assert_eq!(d.children(html).collect::<Vec<_>>(), vec![a, c]);
    assert_eq!(d.parent(b), None);
    // Re-inserting a node moves it.
    d.insert_before(html, c, Some(a));
    assert_eq!(d.children(html).collect::<Vec<_>>(), vec![c, a]);
    d.check_invariants();
}

#[test]
fn template_gets_contents_fragment() {
    let mut d = Document::new();
    let t = d.create_element(Namespace::Html, local::TEMPLATE, vec![]);
    let contents = d.element(t).unwrap().template_contents.unwrap();
    assert_eq!(d.data(contents), &NodeData::DocumentFragment);
    let svg_t = d.create_element(Namespace::Svg, local::TEMPLATE, vec![]);
    assert!(d.element(svg_t).unwrap().template_contents.is_none());
}

#[test]
#[should_panic(expected = "cycle")]
fn refuses_to_make_a_cycle() {
    let mut d = Document::new();
    let a = d.create_element(Namespace::Html, local::DIV, vec![]);
    let b = d.create_element(Namespace::Html, local::DIV, vec![]);
    d.append(a, b);
    d.append(b, a);
}

#[test]
#[should_panic(expected = "reference is not a child")]
fn refuses_foreign_reference_child() {
    let mut d = Document::new();
    let a = d.create_element(Namespace::Html, local::DIV, vec![]);
    let b = d.create_element(Namespace::Html, local::DIV, vec![]);
    let c = d.create_element(Namespace::Html, local::DIV, vec![]);
    d.insert_before(a, b, Some(c));
}

#[test]
fn element_queries_by_hand() {
    let mut d = Document::new();
    let names = &mut d.names;
    let id_attr = Attribute {
        namespace: Namespace::None,
        prefix: None,
        local: local::ID,
        value: Wtf8Buf::from("x"),
    };
    let xlink_href = Attribute {
        namespace: Namespace::XLink,
        prefix: Some(names.intern_str("xlink")),
        local: names.intern_str("href"),
        value: Wtf8Buf::from("#a"),
    };
    let href = d.names.intern_str("href");
    let div = d.create_element(Namespace::Html, local::DIV, vec![xlink_href, id_attr]);
    let svg_div = d.create_element(Namespace::Svg, local::DIV, vec![]);
    let e = d.element(div).unwrap();
    assert!(e.is_html(local::DIV));
    assert!(!e.is_html(local::SPAN));
    assert!(!d.element(svg_div).unwrap().is_html(local::DIV));
    assert!(!d.element(svg_div).unwrap().is_html(local::SPAN));
    assert_eq!(e.attribute(local::ID), Some(Wtf8::from_str("x")));
    // Only attributes with no namespace count.
    assert_eq!(e.attribute(href), None);
    assert_eq!(e.attribute(local::CLASS), None);

    // element_mut reaches the same element; text nodes are not elements.
    d.element_mut(div).unwrap().attributes.pop();
    assert_eq!(d.element(div).unwrap().attribute(local::ID), None);
    let t = text(&mut d, "t");
    assert!(d.element(t).is_none() && d.element_mut(t).is_none());

    // has_element_child looks for elements, not just any child.
    let host = d.create_element(Namespace::Html, local::DIV, vec![]);
    d.append(host, t);
    assert!(!d.has_element_child(host));
    d.append(host, svg_div);
    assert!(d.has_element_child(host));
    assert_eq!(d.name_text(local::DIV), "div");
    assert_eq!(d.name_text(href), "href");
    // Root, div, svg div, text, host: and no template fragments.
    assert_eq!(d.node_count(), 5);
}

#[test]
fn links_by_hand() {
    let mut d = Document::new();
    let p = d.create_element(Namespace::Html, local::P, vec![]);
    let a = text(&mut d, "a");
    let b = text(&mut d, "b");
    d.append(p, a);
    d.append(p, b);
    assert_eq!(d.first_child(p), Some(a));
    assert_eq!(d.last_child(p), Some(b));
    assert_eq!(d.prev_sibling(b), Some(a));
    assert_eq!(d.prev_sibling(a), None);
    assert_eq!(d.next_sibling(a), Some(b));
    assert_eq!(d.next_sibling(b), None);
    assert_eq!((a.index(), b.index()), (2, 3));
}

/// check_invariants must notice each kind of broken link. The tree is broken
/// on purpose through the private fields.
#[test]
fn invariant_check_catches_bad_links() {
    fn tree() -> (Document, NodeId, NodeId, NodeId) {
        let mut d = Document::new();
        let p = d.create_element(Namespace::Html, local::P, vec![]);
        let a = text(&mut d, "a");
        let b = text(&mut d, "b");
        d.append(p, a);
        d.append(p, b);
        d.check_invariants();
        (d, p, a, b)
    }
    type Break = fn(&mut Document, NodeId, NodeId, NodeId);
    let breaks: [(&str, Break); 5] = [
        ("child's parent link", |d, _, a, _| {
            d.nodes[a.index()].parent = None
        }),
        ("prev link", |d, _, _, b| {
            d.nodes[b.index()].prev_sibling = None
        }),
        ("last child link", |d, p, a, _| {
            d.nodes[p.index()].last_child = Some(a)
        }),
        ("detached", |d, _, _, b| {
            d.remove(b);
            d.nodes[b.index()].next_sibling = Some(b);
        }),
        ("cycle", |d, p, a, _| d.nodes[p.index()].parent = Some(a)),
    ];
    for (what, f) in breaks {
        let (mut d, p, a, b) = tree();
        f(&mut d, p, a, b);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.check_invariants()));
        assert!(r.is_err(), "missed a broken {what}");
    }
}

#[test]
fn moving_children_to_the_same_parent_keeps_them() {
    let mut d = Document::new();
    let p = d.create_element(Namespace::Html, local::P, vec![]);
    let q = d.create_element(Namespace::Html, local::P, vec![]);
    let a = text(&mut d, "a");
    let b = text(&mut d, "b");
    d.append(p, a);
    d.append(p, b);
    d.move_children(p, p);
    assert_eq!(d.children(p).collect::<Vec<_>>(), vec![a, b]);
    d.move_children(p, q);
    assert_eq!(d.children(q).collect::<Vec<_>>(), vec![a, b]);
    assert_eq!(d.first_child(p), None);
}

#[derive(Debug, Clone)]
enum Op {
    Create,
    Insert {
        parent: usize,
        child: usize,
        reference: Option<usize>,
    },
    Remove(usize),
    MoveChildren(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        1 => Just(Op::Create),
        4 => (any::<usize>(), any::<usize>(), prop::option::of(any::<usize>()))
            .prop_map(|(parent, child, reference)| Op::Insert { parent, child, reference }),
        2 => any::<usize>().prop_map(Op::Remove),
        1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::MoveChildren(a, b)),
    ]
}

fn model_is_ancestor(parent: &[Option<usize>], a: usize, b: usize) -> bool {
    let mut n = Some(b);
    while let Some(x) = n {
        if x == a {
            return true;
        }
        n = parent[x];
    }
    false
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(2048)
    ))]

    #[test]
    fn tree_matches_model(ops in prop::collection::vec(op(), 1..80)) {
        let mut d = Document::new();
        let mut ids = vec![Document::ROOT];
        let mut kids: Vec<Vec<usize>> = vec![vec![]];
        let mut parent: Vec<Option<usize>> = vec![None];
        for o in ops {
            match o {
                Op::Create => {
                    ids.push(d.create_element(Namespace::Html, local::DIV, vec![]));
                    kids.push(vec![]);
                    parent.push(None);
                }
                Op::Insert { parent: p, child: c, reference: r } => {
                    let n = ids.len();
                    let (p, c) = (p % n, c % n);
                    // The document node is never a child.
                    if c == 0 || model_is_ancestor(&parent, c, p) {
                        continue;
                    }
                    let r = r.and_then(|r| {
                        let list = &kids[p];
                        if list.is_empty() { None } else { Some(list[r % list.len()]) }
                    });
                    d.insert_before(ids[p], ids[c], r.map(|r| ids[r]));
                    if r == Some(c) {
                        continue;
                    }
                    if let Some(old) = parent[c] {
                        kids[old].retain(|&x| x != c);
                    }
                    let pos = r.map(|r| kids[p].iter().position(|&x| x == r).unwrap()).unwrap_or(kids[p].len());
                    kids[p].insert(pos, c);
                    parent[c] = Some(p);
                }
                Op::Remove(x) => {
                    let x = x % ids.len();
                    d.remove(ids[x]);
                    if let Some(old) = parent[x].take() {
                        kids[old].retain(|&y| y != x);
                    }
                }
                Op::MoveChildren(a, b) => {
                    let n = ids.len();
                    let (a, b) = (a % n, b % n);
                    // Moving into one's own subtree is a cycle; skip as the DOM would refuse.
                    if kids[a].iter().any(|&k| model_is_ancestor(&parent, k, b)) {
                        continue;
                    }
                    d.move_children(ids[a], ids[b]);
                    if a == b {
                        continue;
                    }
                    let moved = std::mem::take(&mut kids[a]);
                    for &m in &moved {
                        parent[m] = Some(b);
                    }
                    kids[b].extend(moved);
                }
            }
            d.check_invariants();
        }
        for (i, list) in kids.iter().enumerate() {
            let got: Vec<NodeId> = d.children(ids[i]).collect();
            let want: Vec<NodeId> = list.iter().map(|&k| ids[k]).collect();
            prop_assert_eq!(d.first_child(ids[i]), want.first().copied());
            prop_assert_eq!(d.last_child(ids[i]), want.last().copied());
            prop_assert_eq!(d.has_element_child(ids[i]), !want.is_empty());
            for (k, &w) in want.iter().enumerate() {
                prop_assert_eq!(d.prev_sibling(w), k.checked_sub(1).map(|j| want[j]));
                prop_assert_eq!(d.next_sibling(w), want.get(k + 1).copied());
            }
            prop_assert_eq!(&got, &want);
            prop_assert_eq!(d.parent(ids[i]), parent[i].map(|p| ids[p]));
            prop_assert_eq!(ids[i].index(), i);
        }
        prop_assert_eq!(d.node_count(), ids.len());
    }
}
