//! The cascade checked against the literal model (`tests/common/reference.rs`)
//! on random style sheets and random trees: for every element, each
//! property's declarations must come out the same and in the same order.
//! The sheets mix nesting, layers (named, nested, anonymous, statements,
//! invalid names), `@namespace` and `@import` placement, dropped at-rules,
//! invalid selectors, `!important`, the three origins and style
//! attributes. Each case prints its sheets and tree on failure.

mod common;

use common::reference::{self, Id, MValue, Model};
use common::tree::{HTML, SVG, Tree};
use optimal_style::property::LONGHANDS;
use optimal_style::{Cascaded, Origin, Source, Style, StyleAttribute, StyleSet, Value};
use optimal_wtf8::Wtf8;
use proptest::prelude::*;

/// A small deterministic generator (xorshift), so a failing case is
/// replayed from its seed.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'p>(&mut self, pool: &[&'p str]) -> &'p str {
        pool[self.below(pool.len())]
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const TOP_SELECTORS: &[&str] = &[
    "a",
    "b",
    "div",
    "DIV",
    ".x",
    ".X",
    "#i",
    "#I",
    "*",
    "a.x",
    "div > b",
    "a b",
    ".x + .y",
    "b ~ a",
    ":is(.x, #i)",
    ":where(#i)",
    ":not(.x)",
    "&",
    "& .x",
    "a::before",
    ":root",
    "svg|a",
    "*|b",
    "|a",
    "a!",
    ":nth-child(2)",
    ":first-child",
    "[title]",
    "[class~=y]",
    ".y.x",
    "#i.x",
    ":is(a, .x::after)",
];
const NESTED_SELECTORS: &[&str] = &[
    "&",
    "& a",
    "> b",
    "+ a",
    "~ .x",
    ".x",
    "a &",
    "&.x",
    "& + &",
    ":is(&, .y)",
    ":not(&)",
    "&::before",
    "> &",
    ":where(&) b",
    "b",
    "#i",
    "~ &",
    ":unknown(&)",
    ">",
    "& &",
];
const NAMES: &[&str] = &[
    "color",
    "COLOR",
    "margin",
    "margin-top",
    "--x",
    "--X",
    "width",
    "display",
    "all",
    "ALL",
    "direction",
    "border",
    "border-top-color",
    "font-stretch",
    "font-width",
    "white-space",
    "text-wrap-mode",
    "nope",
    "-webkit-nope",
];
const VALUES: &[&str] = &[
    "red",
    "1px",
    "\"blue\"",
    "f(b)",
    "{a}",
    "x y",
    "initial",
    "INHERIT",
    "unset",
    "revert",
    "revert-layer",
    "revert-rule",
    "Revert-Layer",
    "red initial",
    "var(--x)",
    "var(--x) inherit",
    "f(var(--y))",
    "--f(1)",
    "default",
    // Valid by the value grammars of some of NAMES, so plain values reach
    // the cascade too.
    "auto",
    "block",
    "rtl",
    "condensed",
    "nowrap",
    "1px 2px",
    "1px solid red",
];
const LAYERS: &[&str] = &[
    "a", "b", "a.b", "b.a", "c", "A", "a.b.c", "initial", "a .b", "B.c",
];
const NAMESPACES: &[&str] = &[
    "@namespace svg url(http://www.w3.org/2000/svg);",
    "@namespace \"http://www.w3.org/1999/xhtml\";",
    "@namespace url(urn:x);",
    "@namespace svg;",
    "@namespace svg \"http://www.w3.org/2000/svg\";",
    "@NAMESPACE x \"urn:x\";",
];
const IMPORTS: &[&str] = &[
    "@import url(x.css);",
    "@import \"y.css\" layer;",
    "@import url(z.css) layer(a.b);",
    "@import \"w.css\" layer(c) screen;",
    "@import url(v.css) layer(initial);",
    "@import nope;",
];
const OTHERS: &[&str] = &[
    "@media screen { a { color: blue } }",
    "@font-face { font-family: x }",
    "@unknown x;",
    "@charset \"utf-8\";",
    "@supports (color: red) { .x { width: 1px } }",
    "@keyframes k { from { color: red } }",
];

fn decl(g: &mut Gen) -> String {
    format!(
        "{}: {}{}",
        g.pick(NAMES),
        g.pick(VALUES),
        if g.chance(25) { " !important" } else { "" }
    )
}

fn selector_list(g: &mut Gen, pool: &[&str]) -> String {
    let n = 1 + g.below(2);
    (0..n).map(|_| g.pick(pool)).collect::<Vec<_>>().join(", ")
}

fn layer_prelude(g: &mut Gen) -> String {
    if g.chance(25) {
        String::new()
    } else {
        g.pick(LAYERS).to_string()
    }
}

/// The contents of a style rule's block.
fn body(g: &mut Gen, depth: usize) -> String {
    let mut out = Vec::new();
    for _ in 0..1 + g.below(6) {
        let item = match g.below(10) {
            0..=4 => decl(g),
            5 | 6 if depth < 3 => format!(
                "{} {{ {} }}",
                selector_list(g, NESTED_SELECTORS),
                body(g, depth + 1)
            ),
            7 if depth < 3 => format!("@layer {} {{ {} }}", layer_prelude(g), body(g, depth + 1)),
            8 => format!("@layer {}, {}", g.pick(LAYERS), g.pick(LAYERS)),
            9 => format!("@media screen {{ {} }}", decl(g)),
            _ => decl(g),
        };
        out.push(item);
    }
    out.join("; ")
}

/// Rules inside a top-level `@layer` (not in a style rule).
fn rule_list(g: &mut Gen, depth: usize) -> String {
    let mut out = Vec::new();
    for _ in 0..g.below(4) {
        out.push(top_rule(g, depth));
    }
    out.join("\n")
}

fn top_rule(g: &mut Gen, depth: usize) -> String {
    match g.below(12) {
        0..=4 => format!("{} {{ {} }}", selector_list(g, TOP_SELECTORS), body(g, 0)),
        5 | 6 if depth < 2 => format!(
            "@layer {} {{ {} }}",
            layer_prelude(g),
            rule_list(g, depth + 1)
        ),
        7 => format!("@layer {};", g.pick(LAYERS)),
        8 => g.pick(NAMESPACES).to_string(),
        9 => g.pick(IMPORTS).to_string(),
        10 => g.pick(OTHERS).to_string(),
        // Declarations where none are allowed.
        _ => format!("{} {{ }} {}", g.pick(TOP_SELECTORS), decl(g)),
    }
}

fn sheet(g: &mut Gen) -> String {
    let mut out = Vec::new();
    for _ in 0..1 + g.below(10) {
        out.push(top_rule(g, 0));
    }
    out.join("\n")
}

const TAGS: &[&str] = &["a", "b", "div", "DIV", "p"];
const CLASSES: &[&str] = &["x", "x y", "X", "y x x", "y"];

fn tree(g: &mut Gen) -> Tree {
    let mut t = Tree::new(g.chance(80), "html");
    for _ in 0..g.below(12) {
        let parent = g.below(t.nodes.len());
        let ns = if g.chance(15) { SVG } else { HTML };
        let tag = g.pick(TAGS);
        let mut attrs: Vec<(&str, String)> = Vec::new();
        if g.chance(50) {
            attrs.push(("class", g.pick(CLASSES).into()));
        }
        if g.chance(25) {
            attrs.push(("id", if g.chance(50) { "i" } else { "I" }.into()));
        }
        if g.chance(20) {
            attrs.push(("title", "t".into()));
        }
        if g.chance(40) {
            let n = g.below(4);
            let s: Vec<String> = (0..n).map(|_| decl(g)).collect();
            attrs.push(("style", s.join("; ")));
        }
        let attrs: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        t.add(parent, ns, tag, &attrs);
    }
    t
}

fn origin(g: &mut Gen) -> (Origin, reference::Origin) {
    match g.below(4) {
        0 => (Origin::UserAgent, reference::Origin::UserAgent),
        1 => (Origin::User, reference::Origin::User),
        _ => (Origin::Author, reference::Origin::Author),
    }
}

/// The production output with declarations named as the model names them.
fn ids(set: &StyleSet<'_>, c: &Cascaded<'_>) -> Vec<(String, Vec<Id>)> {
    c.properties
        .iter()
        .map(|(n, es)| {
            let ids = es
                .iter()
                .map(|e| match e.source {
                    Source::Rule(d) => {
                        let d = &set.decls()[d as usize];
                        Id::Sheet(d.sheet as usize, d.decl.name)
                    }
                    Source::Attribute(i) => Id::Attribute(i as usize),
                })
                .collect();
            (n.to_string(), ids)
        })
        .collect()
}

fn id(set: &StyleSet<'_>, s: Source) -> Id {
    match s {
        Source::Rule(d) => {
            let d = &set.decls()[d as usize];
            Id::Sheet(d.sheet as usize, d.decl.name)
        }
        Source::Attribute(i) => Id::Attribute(i as usize),
    }
}

/// The production specified values as the model writes them.
fn mstyle(set: &StyleSet<'_>, s: &Style<'_>) -> Vec<(String, MValue)> {
    let v = |v: Value| match v {
        Value::Initial => MValue::Initial,
        Value::Declared { source, element } => MValue::Declared(id(set, source), element),
    };
    let mut out: Vec<(String, MValue)> = LONGHANDS
        .iter()
        .zip(s.longhands.iter())
        .map(|(l, &x)| (l.name.to_string(), v(x)))
        .chain(s.custom.iter().map(|&(n, x)| (n.to_string(), v(x))))
        .collect();
    out.sort();
    out
}

fn run(seed: u64) {
    let mut g = Gen(seed | 1);
    let n = 1 + g.below(3);
    let sheets: Vec<(String, (Origin, reference::Origin))> =
        (0..n).map(|_| (sheet(&mut g), origin(&mut g))).collect();
    let t = tree(&mut g);
    let quirks = g.chance(30);

    let mut set = StyleSet::new();
    let mut model = Model::new();
    let parses: Vec<_> = sheets.iter().map(|(s, _)| reference::sheet(s)).collect();
    for (i, ((text, (o, mo)), (parsed, top))) in sheets.iter().zip(&parses).enumerate() {
        set.add_sheet(Wtf8::from_str(text), *o);
        model.add_sheet(parsed, top, i, *mo);
    }
    let cascade = set.prepare();
    let styles: Vec<Option<StyleAttribute<'_>>> = (0..t.nodes.len())
        .map(|i| {
            t.attr(i, "style")
                .map(|s| StyleAttribute::parse(Wtf8::from_str(s)))
        })
        .collect();
    let model_styles: Vec<_> = (0..t.nodes.len())
        .map(|i| t.attr(i, "style").map(reference::style_attribute))
        .collect();
    let elements: Vec<_> = (0..t.nodes.len())
        .map(|i| (t.el(i), t.nodes[i].parent))
        .collect();
    let model_attrs: Vec<_> = (0..t.nodes.len())
        .map(|i| t.attr(i, "style").map(reference::style_attribute))
        .collect();
    let model_specified = model.styles(&elements, quirks, &model_attrs);
    cascade.with_session(quirks, |s| {
        let mut specified: Vec<Style<'_>> = Vec::new();
        for i in 0..t.nodes.len() {
            let initial = Style::initial();
            let parent = t.nodes[i].parent.map_or(&initial, |p| &specified[p]);
            let got = s.style(&t.el(i), i as u32, styles[i].as_ref(), parent);
            assert_eq!(
                mstyle(&set, &got),
                model_specified[i],
                "specified values: seed {seed}, element {i}\nsheets: {sheets:#?}\ntree: {t:#?}\nquirks {quirks}"
            );
            specified.push(got);
        }
        for i in 0..t.nodes.len() {
            let e = t.el(i);
            let got = s.cascade(&e, styles[i].as_ref());
            let (mp, md) = match &model_styles[i] {
                Some((p, d)) => (Some(p), d.as_slice()),
                None => (None, &[][..]),
            };
            let want = model.cascade(&e, quirks, mp, md);
            if std::env::var("CASCADE_STATS").is_ok() {
                let sheet = want
                    .iter()
                    .flat_map(|w| &w.1)
                    .filter(|i| matches!(i, Id::Sheet(..)))
                    .count();
                eprintln!(
                    "STAT {} {} {sheet}",
                    want.len(),
                    want.iter().map(|w| w.1.len()).sum::<usize>()
                );
            }
            assert_eq!(
                ids(&set, &got),
                want,
                "seed {seed}, element {i}\nsheets: {sheets:#?}\ntree: {t:#?}\nquirks {quirks}"
            );
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(2000)
    ))]
    #[test]
    fn cascade_matches_the_literal_model(seed in any::<u64>()) {
        run(seed);
    }
}
