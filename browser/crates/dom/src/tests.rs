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
            prop_assert_eq!(got, want);
            prop_assert_eq!(d.parent(ids[i]), parent[i].map(|p| ids[p]));
        }
    }
}
