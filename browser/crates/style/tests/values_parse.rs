//! Parse trees (`src/values/parse.rs`): which way a value matched, on
//! small grammars and on the real tables, with the working in comments.
//! A tree is written as text: a component value as its source, a list as
//! `[i=… j=…]` (child index = its tree), a `|` as `i:…`, a function as
//! `name(…)`, a grammar comma as `,`; references and `!` add nothing.

use optimal_css::parse_component_values;
use optimal_style::property::{LONGHANDS, SHORTHANDS};
use optimal_style::values::parse::{Tree, TreeKind, parse};
use optimal_style::values::table::{Def, PRODUCTIONS, Production};
use optimal_style::values::{Compiled, compile_with};
use optimal_wtf8::Wtf8;

fn build(prods: &[(&'static str, &'static str)], grammar: &'static str) -> Compiled {
    let table: Vec<Production> = prods
        .iter()
        .map(|&(name, text)| Production {
            name,
            spec: "test",
            def: Def::Prod(text),
        })
        .collect();
    compile_with(&table, &[("p", grammar)], &[]).expect("compiles")
}

/// The real tables with one more longhand, `p`, last.
fn with_real(grammar: &'static str) -> Compiled {
    let mut l: Vec<(&str, &str)> = LONGHANDS.iter().map(|l| (l.name, l.value)).collect();
    l.push(("p", grammar));
    let s: Vec<(&str, &str)> = SHORTHANDS.iter().map(|s| (s.name, s.value)).collect();
    compile_with(PRODUCTIONS, &l, &s).expect("compiles")
}

fn show(src: &str, t: &Tree<'_, '_>) -> String {
    match &t.kind {
        TreeKind::Value(v) => src[v.span()].to_string(),
        TreeKind::Nested(v, inner) => {
            let s = &src[v.span()];
            let name = &s[..s.find('(').unwrap_or(0)];
            format!("{name}({})", show(src, inner))
        }
        TreeKind::Comma => ",".into(),
        TreeKind::Inner(t) => show(src, t),
        TreeKind::Alternative(i, t) => format!("{i}:{}", show(src, t)),
        TreeKind::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|(i, t)| format!("{i}={}", show(src, t)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

fn tree(g: &Compiled, value: &str) -> Option<String> {
    let (parsed, range) = parse_component_values(Wtf8::from_str(value));
    let top = *g.longhands.last().expect("p");
    parse(g, top, parsed.values(range), 0).map(|t| show(value, &t))
}

#[track_caller]
fn check(g: &Compiled, value: &str, want: &str) {
    assert_eq!(tree(g, value).as_deref(), Some(want), "{value:?}");
}

#[test]
fn greedy_left_to_right() {
    // a{1,2} a?: the first takes both a's when it can (rule 1).
    let g = build(&[], "a{1,2} a?");
    check(&g, "a a", "[0=[0=a 0=a] 1=[]]");
    check(&g, "a", "[0=[0=a] 1=[]]");
    // a* a*: the first takes all.
    let g = build(&[], "a* a*");
    check(&g, "a a a", "[0=[0=a 0=a 0=a] 1=[]]");
    check(&g, "", "[0=[] 1=[]]");
    // [a | a b] b?: the first alternative can't take both, the second
    // can, so the second.
    let g = build(&[], "[ a | a b ] b?");
    check(&g, "a b", "[0=1:[0=a 1=b] 1=[]]");
    // But only while the rest still matches: [a b?] b leaves the b.
    let g = build(&[], "a b? b");
    check(&g, "a b", "[0=a 1=[] 2=b]");
    check(&g, "a b b", "[0=a 1=[0=b] 2=b]");
    assert_eq!(tree(&g, "a"), None);
}

#[test]
fn alternatives_in_order() {
    // Both alternatives take `a`: the first (rule 3).
    let g = build(&[], "a | [ a | b ]");
    check(&g, "a", "0:a");
    check(&g, "b", "1:1:b");
}

#[test]
fn keywords_before_custom_ident() {
    // css-values-4 4.2's example: in `animation: ease-in ease-out`,
    // ease-in goes to the easing function and ease-out to the name,
    // whatever the order of the || children.
    let g = build(&[], "<custom-ident> || [ ease-in | ease-out ]");
    check(&g, "ease-in ease-out", "[1=0:ease-in 0=ease-out]");
    check(&g, "ease-out", "[1=1:ease-out]");
    check(&g, "x ease-in", "[0=x 1=0:ease-in]");
    let g = build(&[], "[ ease-in | ease-out ] || <custom-ident>");
    check(&g, "ease-in ease-out", "[0=0:ease-in 1=ease-out]");
    // A <custom-ident> inside a multiplier counts too.
    let g = build(&[], "<custom-ident>? || [ a | b ]");
    check(&g, "a", "[1=0:a]");
    check(&g, "x a", "[0=[0=x] 1=0:a]");
    // Also between alternatives.
    let g = build(&[("name", "<custom-ident>")], "<name> | auto");
    check(&g, "auto", "1:auto");
    check(&g, "x", "0:x");
}

#[test]
fn groups_stop_instead_of_taking_nothing() {
    // a? || b: on "b", the || takes b and stops; a? is not used.
    let g = build(&[], "a? || b");
    check(&g, "b", "[1=b]");
    check(&g, "b a", "[1=b 0=[0=a]]");
    // && must use every child, empty or not.
    let g = build(&[], "a? && b");
    check(&g, "b", "[1=b 0=[]]");
    // A repetition doesn't add empty repetitions.
    let g = build(&[], "[ a? ]*");
    check(&g, "", "[]");
    check(&g, "a a", "[0=[0=a] 0=[0=a]]");
    // Below its minimum it must: two empty ones.
    let g = build(&[], "[ a? ]{2,}");
    check(&g, "", "[0=[] 0=[]]");
}

#[test]
fn repetitions_of_a_comma() {
    // [ f(b) | <custom-ident> | ,? ]{2,4} on `c , f(b)`: c, then a
    // repetition that passes the grammar comma (its token goes with
    // f(b)), then f(b): three. Another empty repetition first would make
    // four; rule 4 takes the fewest.
    let g = build(&[("f", "f( b )")], "[ <f> | <custom-ident> | ,? ]{2,4}");
    check(&g, "c , f(b)", "[0=1:c 0=2:[0=,] 0=0:f(b)]");
}

#[test]
fn commas() {
    // a? , b?: the grammar comma matches the comma token, or is omitted
    // when a side is empty (css-values-4 2.1).
    let g = build(&[], "a? , b?");
    check(&g, "a, b", "[0=[0=a] 1=, 2=[0=b]]");
    check(&g, "b", "[0=[] 1=, 2=[0=b]]");
    check(&g, "a", "[0=[0=a] 1=, 2=[]]");
    let g = build(&[], "a#");
    check(&g, "a, a, a", "[0=a 0=a 0=a]");
    let g = build(&[], "[ a? , b? ]#");
    check(
        &g,
        "a, b, b",
        "[0=[0=[0=a] 1=, 2=[0=b]] 0=[0=[] 1=, 2=[0=b]]]",
    );
}

#[test]
fn functions_blocks_and_required() {
    let g = build(&[("f", "f( a? b? )")], "<f> [ ( a ) ]? [ a? b? ]!");
    check(
        &g,
        "f(b) (a) b",
        "[0=f([0=[] 1=[0=b]]) 1=[0=(a)] 2=[0=[] 1=[0=b]]]",
    );
    check(&g, "f() a", "[0=f([0=[] 1=[]]) 1=[] 2=[0=[0=a] 1=[]]]");
    assert_eq!(tree(&g, "f()"), None, "the ! group needs something");
    // A ! group in a juxtaposition ends where the rest still matches.
    let g = build(&[], "[ a b? ]! b");
    check(&g, "a b", "[0=[0=a 1=[]] 1=b]");
    check(&g, "a b b", "[0=[0=a 1=[0=b]] 1=b]");
}

#[test]
fn position_is_greedy() {
    // css-values-4 4.5.1: in `<position> <length>?`, `left 50px` is a
    // 2-value position, `top 50px` a 1-value one and a length.
    let g = with_real("<position> <length>?");
    let left = tree(&g, "left 50px").expect("valid");
    assert!(left.ends_with("1=[]]"), "{left}");
    let top = tree(&g, "top 50px").expect("valid");
    assert!(top.ends_with("1=[0=50px]]"), "{top}");
}
