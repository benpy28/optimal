//! Collecting, cascading and defaulting grow linearly with the input on hostile style
//! sheets and trees: many rules, long selector lists, many layers, deep
//! and wide trees, deep nesting, long attribute values. Timing is only
//! meaningful in release builds, so it runs when `CSS_SCALING=1`.

mod common;

use common::tree::{HTML, Tree};
use optimal_style::{Origin, Style, StyleAttribute, StyleSet};
use optimal_wtf8::Wtf8;
use std::time::{Duration, Instant};

/// A case at size n: the sheet, the tree, and each element's style
/// attribute.
type Case = fn(usize) -> (String, Tree);

fn flat(n: usize, attrs: impl Fn(usize) -> Vec<(&'static str, String)>) -> Tree {
    let mut t = Tree::new(true, "html");
    for i in 0..n {
        let a = attrs(i);
        let a: Vec<(&str, &str)> = a.iter().map(|(k, v)| (*k, v.as_str())).collect();
        t.add(0, HTML, "p", &a);
    }
    t
}

fn deep(n: usize, local: &str) -> Tree {
    let mut t = Tree::new(true, "html");
    let mut p = 0;
    for _ in 0..n {
        p = t.add(p, HTML, local, &[]);
    }
    t
}

fn run(css: &str, t: &Tree) -> usize {
    let mut set = StyleSet::new();
    set.add_sheet(Wtf8::from_str(css), Origin::Author);
    let styles: Vec<_> = (0..t.nodes.len())
        .map(|i| {
            t.attr(i, "style")
                .map(|s| StyleAttribute::parse(Wtf8::from_str(s)))
        })
        .collect();
    let c = set.prepare();
    c.with_session(false, |s| {
        let mut out: Vec<Style<'_>> = Vec::new();
        let mut sum = 0;
        #[allow(clippy::needless_range_loop)]
        for i in 0..t.nodes.len() {
            sum += s
                .cascade(&t.el(i), styles[i].as_ref())
                .properties
                .iter()
                .map(|p| p.1.len())
                .sum::<usize>();
            let initial = Style::initial();
            let parent = t.nodes[i].parent.map_or(&initial, |p| &out[p]);
            let st = s.style(&t.el(i), i as u32, styles[i].as_ref(), parent);
            sum += st.custom.len();
            out.push(st);
        }
        sum
    })
}

#[test]
fn hostile_cascades_run_in_linear_time() {
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    let cases: &[(&str, Case)] = &[
        ("many class rules", |n| {
            let css: String = (0..n)
                .map(|i| format!(".c{i} {{ color: red }}\n"))
                .collect();
            (
                css,
                flat(n / 10, |i| vec![("class", format!("c{}", i * 7 % n))]),
            )
        }),
        ("many id rules", |n| {
            let css: String = (0..n)
                .map(|i| format!("#i{i} {{ color: red }}\n"))
                .collect();
            (
                css,
                flat(n / 10, |i| vec![("id", format!("i{}", i * 3 % n))]),
            )
        }),
        ("long selector list", |n| {
            let list: Vec<String> = (0..n).map(|i| format!(".c{i}")).collect();
            let css = format!("{} {{ color: red }}", list.join(","));
            (css, flat(n / 10, |i| vec![("class", format!("c{i}"))]))
        }),
        ("many classes per element", |n| {
            let css: String = (0..100)
                .map(|i| format!(".c{i} {{ color: red }}\n"))
                .collect();
            let classes: Vec<String> = (0..n).map(|i| format!("c{}", i % 200)).collect();
            (css, flat(1, |_| vec![("class", classes.join(" "))]))
        }),
        ("one class repeated", |n| {
            (
                ".x { color: red }".into(),
                flat(1, |_| vec![("class", "x ".repeat(n))]),
            )
        }),
        ("deep tree, descendant rules", |n| {
            (
                "a b { color: x } a > a { width: y } :is(a) a a { margin: z }".into(),
                deep(n / 10, "a"),
            )
        }),
        ("wide tree, sibling rules", |n| {
            (
                "p + p { color: x } p ~ p { width: y } p:nth-child(2n+1) { margin: z }".into(),
                flat(n / 10, |_| vec![]),
            )
        }),
        ("many universal rules", |n| {
            let css: String = (0..n).map(|i| format!("* {{ --v{i}: 1 }}\n")).collect();
            (css, flat(20, |_| vec![]))
        }),
        ("many declarations, one property", |n| {
            (
                format!("p {{ {} }}", "color: red;".repeat(n)),
                flat(10, |_| vec![]),
            )
        }),
        ("many layers", |n| {
            let names: Vec<String> = (0..n / 10).map(|i| format!("l{i}")).collect();
            let blocks: String = (0..n / 10)
                .rev()
                .map(|i| format!("@layer l{i} {{ p {{ color: c{i} }} }}\n"))
                .collect();
            (
                format!("@layer {};\n{blocks}", names.join(", ")),
                flat(10, |_| vec![]),
            )
        }),
        ("deep layers", |n| {
            (
                format!(
                    "{}p {{ color: red }}{}",
                    "@layer {".repeat(n),
                    "}".repeat(n)
                ),
                flat(10, |_| vec![]),
            )
        }),
        ("deep nesting, many leaves", |n| {
            let leaves: String = (0..n / 20).map(|_| "& { color: red } ").collect();
            (
                format!(
                    "p {{ {}{}{} }}",
                    "& { ".repeat(200),
                    leaves,
                    "} ".repeat(200)
                ),
                flat(10, |_| vec![]),
            )
        }),
        ("nested rule lists", |n| {
            let inner: Vec<String> = (0..n / 10).map(|i| format!("> .c{i}")).collect();
            (
                format!("div {{ {} {{ color: red }} }}", inner.join(", ")),
                {
                    let mut t = Tree::new(true, "html");
                    let d = t.add(0, HTML, "div", &[]);
                    for i in 0..n / 10 {
                        t.add(d, HTML, "p", &[("class", &format!("c{i}"))]);
                    }
                    t
                },
            )
        }),
        ("revert-rule chain", |n| {
            let css = "p { z-index: 1 }\n".to_string() + &"p { z-index: revert-rule }\n".repeat(n);
            (css, flat(10, |_| vec![]))
        }),
        ("revert-layer chain", |n| {
            let css: String = (0..n / 10)
                .map(|i| format!("@layer l{i} {{ p {{ color: red; color: revert-layer }} }}\n"))
                .collect();
            (css, flat(10, |_| vec![]))
        }),
        ("inherited custom properties", |n| {
            let decls: String = (0..n / 10).map(|i| format!("--v{i}: 1;")).collect();
            (format!("html {{ {decls} }}"), deep(n, "div"))
        }),
        ("all: inherit, deep tree", |n| {
            ("* { all: inherit }".to_string(), deep(n / 10, "div"))
        }),
        ("long style attributes", |n| {
            (
                String::new(),
                flat(10, |_| vec![("style", "color: red;".repeat(n / 10))]),
            )
        }),
    ];
    let time = |make: Case, n: usize| {
        let (css, t) = make(n);
        (0..3)
            .map(|_| {
                let start = Instant::now();
                std::hint::black_box(run(&css, &t));
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    // Each case at n, 4n and 16n must be linear (at most 8x for 4x the
    // input, against 16x for quadratic) across at least one of the two
    // steps, so that one cache-size step can't fail it.
    let n = 20_000;
    for (name, make) in cases {
        let t1 = time(*make, n);
        let t4 = time(*make, 4 * n);
        let t16 = time(*make, 16 * n);
        println!("{name:32} n={n}: {t1:?}, 4n: {t4:?}, 16n: {t16:?}");
        let slack = Duration::from_millis(20);
        assert!(
            t4 <= t1 * 8 + slack || t16 <= t4 * 8 + slack,
            "{name}: growth {t1:?} -> {t4:?} -> {t16:?} is not linear"
        );
    }
}

/// A value shape at size n.
type ValueCase = fn(usize) -> String;

/// Matching values against every property's grammar grows linearly with
/// the value: long lists, long runs of one component, long sums inside
/// calc(), many functions, nesting up to the cap and past it.
#[test]
fn hostile_values_match_and_parse_in_linear_time() {
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    use optimal_css::parse_component_values;
    use optimal_style::property::{LONGHANDS, SHORTHANDS, classify, target};
    use optimal_style::values::parse_value;
    let cases: &[(&str, ValueCase)] = &[
        ("idents", |n| "a ".repeat(n)),
        ("lengths", |n| "1px ".repeat(n)),
        ("comma list of idents", |n| "a, ".repeat(n) + "a"),
        ("comma list of colors", |n| "red, ".repeat(n) + "red"),
        ("commas", |n| ",".repeat(n)),
        ("family names", |n| "a b c, ".repeat(n) + "serif"),
        ("shadows", |n| "1px 2px 3px red, ".repeat(n) + "1px 1px red"),
        ("gradient stops", |n| {
            format!(
                "linear-gradient(to right, {} blue)",
                "red 1%, 2%, ".repeat(n)
            )
        }),
        ("calc sum", |n| {
            format!("calc({} 1px)", "1px + 2% - ".repeat(n))
        }),
        ("calc product", |n| {
            format!("calc(1px{})", " * 2 / 3".repeat(n))
        }),
        ("min arguments", |n| {
            format!("min({} 1px)", "1em, ".repeat(n))
        }),
        ("many functions", |n| "calc(1px) ".repeat(n)),
        ("many rgb()", |n| "rgb(1 2 3) ".repeat(n)),
        ("nested parens, capped", |n| {
            let d = 60;
            format!("{}{}", "calc(".repeat(d), ")".repeat(d)).repeat(n / 60 + 1)
        }),
        ("nested past the cap", |n| {
            format!("{}1px{}", "calc(".repeat(n), ")".repeat(n))
        }),
        ("transform functions", |n| {
            "translate(1px, 2px) rotate(3deg) ".repeat(n)
        }),
        ("grid-like slashes", |n| "1px / ".repeat(n) + "1px"),
    ];
    let names: Vec<&str> = LONGHANDS
        .iter()
        .map(|l| l.name)
        .chain(SHORTHANDS.iter().map(|s| s.name))
        .collect();
    let time = |make: ValueCase, n: usize| {
        let text = make(n);
        let (parsed, range) = parse_component_values(Wtf8::from_str(&text));
        (0..3)
            .map(|_| {
                let start = Instant::now();
                for name in &names {
                    std::hint::black_box(classify(name, parsed.values(range.clone())));
                    // And the parse tree, for values that match.
                    let t = target(name).expect("a property");
                    std::hint::black_box(parse_value(t, parsed.values(range.clone())));
                }
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let n = 400;
    for (name, make) in cases {
        let t1 = time(*make, n);
        let t4 = time(*make, 4 * n);
        let t16 = time(*make, 16 * n);
        println!("{name:32} n={n}: {t1:?}, 4n: {t4:?}, 16n: {t16:?}");
        let slack = Duration::from_millis(20);
        assert!(
            t4 <= t1 * 8 + slack || t16 <= t4 * 8 + slack,
            "{name}: growth {t1:?} -> {t4:?} -> {t16:?} is not linear"
        );
    }
}
