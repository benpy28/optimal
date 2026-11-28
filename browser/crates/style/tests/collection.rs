//! Collection and bucketing details that random sheets reach too rarely
//! to rely on: placement of `@import` and `@namespace` after each kind of
//! rule, `@import` layer syntax, refusal counts, the order given to
//! element-attached declarations, and which selectors each element tries.

mod common;

use common::tree::{HTML, SVG, Tree};
use optimal_style::{MAX_RULE_NESTING, Origin, Source, StyleAttribute, StyleSet};
use optimal_wtf8::Wtf8;

fn set(css: &str) -> StyleSet<'_> {
    let mut s = StyleSet::new();
    s.add_sheet(Wtf8::from_str(css), Origin::Author);
    s
}

/// Whether `svg|a` (with the prefix declared by `before` and the rule
/// after) matched an SVG `a`: whether the @namespace counted.
fn namespace_counts(before: &str) -> bool {
    let css = format!(
        "{before} @namespace svg url(http://www.w3.org/2000/svg); svg|a {{ font-family: x }}"
    );
    let s = set(&css);
    let mut t = Tree::new(true, "html");
    t.add(0, SVG, "a", &[]);
    let c = s.prepare();
    c.with_session(false, |ss| {
        ss.cascade(&t.el(1), None).winner("font-family").is_some()
    })
}

#[test]
fn namespace_placement_after_each_kind_of_rule() {
    // Ignored rules don't count; valid ones end the place for @namespace.
    for before in [
        "",
        "@layer a;",
        "@layer a, b; @import url(x);",
        "@import url(x); @import \"y\";",
        "@namespace z url(urn:z);",
        "@charset \"utf-8\";",
        "@unknown {}",
        "@unknown;",
        "!!bad {}",
        "@layer initial {}",
        "@media;",
        "@namespace;",
    ] {
        assert!(namespace_counts(before), "after {before:?}");
    }
    for before in [
        "p {}",
        "@layer {}",
        "@media screen {}",
        "@supports (a: b) {}",
        "@font-face {}",
        "@keyframes k {}",
        "@page {}",
        "@import url(x); @layer a;",
        "@namespace z url(urn:z); @layer a;",
    ] {
        assert!(!namespace_counts(before), "after {before:?}");
    }
}

#[test]
fn namespace_syntax() {
    for (rule, counts) in [
        ("@namespace svg \"http://www.w3.org/2000/svg\";", true),
        ("@namespace svg url(\"http://www.w3.org/2000/svg\");", true),
        (
            "@namespace svg url( \"http://www.w3.org/2000/svg\" );",
            true,
        ),
        ("@NameSpace svg url(http://www.w3.org/2000/svg);", true),
        ("@namespace svg;", false),
        ("@namespace svg url(http://www.w3.org/2000/svg) x;", false),
        ("@namespace svg url(a b);", false),
        ("@namespace svg f(\"http://www.w3.org/2000/svg\");", false),
        ("@namespace svg url(http://www.w3.org/2000/svg) {}", false),
        // A later declaration of the prefix replaces the earlier one.
        (
            "@namespace svg url(urn:x); @namespace svg url(http://www.w3.org/2000/svg);",
            true,
        ),
        (
            "@namespace svg url(http://www.w3.org/2000/svg); @namespace svg url(urn:x);",
            false,
        ),
    ] {
        let css = format!("{rule} svg|a {{ font-family: x }}");
        let s = set(&css);
        let mut t = Tree::new(true, "html");
        t.add(0, SVG, "a", &[]);
        let c = s.prepare();
        let got = c.with_session(false, |ss| {
            ss.cascade(&t.el(1), None).winner("font-family").is_some()
        });
        assert_eq!(got, counts, "{rule}");
    }
    // A default namespace applies to type selectors.
    let s = set("@namespace url(http://www.w3.org/2000/svg); a { font-family: x }");
    let mut t = Tree::new(true, "html");
    t.add(0, SVG, "a", &[]);
    t.add(0, HTML, "a", &[]);
    let c = s.prepare();
    c.with_session(false, |ss| {
        assert!(ss.cascade(&t.el(1), None).winner("font-family").is_some());
        assert!(ss.cascade(&t.el(2), None).winner("font-family").is_none());
    });
}

#[test]
fn import_layer_syntax() {
    // Three roots, plus each declared layer.
    for (css, layers) in [
        ("@import url(x);", 3),
        ("@import url(x) screen;", 3),
        ("@import url(x) layer;", 4),
        ("@import \"x\" LAYER;", 4),
        ("@import url(x) layer(a);", 4),
        ("@import url(x) Layer(a.b);", 5),
        ("@import url(x) layer( a );", 4),
        // Conditions: not evaluated yet, so the layer waits.
        ("@import url(x) layer(a) screen;", 3),
        ("@import url(x) layer supports(display: block);", 3),
        // Only layer() is the layer clause, whatever the function holds.
        ("@import url(x) supports(a);", 3),
        // layer() without a valid name is a condition (WPT
        // layer-import-parsing): the import stays, its layer isn't
        // declared. Invalid: a bad URL.
        ("@import url(x) layer(initial);", 3),
        ("@import url(x) layer();", 3),
        ("@import x layer(a);", 3),
        ("@import layer(a);", 3),
        ("@import url(x) layer(a) {}", 3),
        // Two imports, both counted.
        ("@import url(x) layer(a); @import url(y) layer(b);", 5),
        // supports() is a condition, not a layer() function.
        ("@import url(x) supports(a: b); @import url(y) layer(b);", 4),
        // Not at the top: ignored.
        ("p {} @import url(x) layer(a);", 3),
        ("@layer { @import url(x) layer(a); }", 4),
    ] {
        assert_eq!(set(css).layers().len(), layers, "{css}");
    }
    // An invalid @import doesn't end the place for others: the second one
    // is still valid.
    assert_eq!(
        set("@import url(x) layer(1); @import url(y) layer(b);")
            .layers()
            .len(),
        4
    );
}

#[test]
fn refusal_counts() {
    assert_eq!(set("p { & { font-family: x } }").refused_too_deep(), 0);
    let deep = format!(
        "p {{ {}{} }}",
        "& { ".repeat(MAX_RULE_NESTING as usize + 1),
        "} ".repeat(MAX_RULE_NESTING as usize + 1)
    );
    let two = format!("{deep} {deep}");
    assert_eq!(set(&two).refused_too_deep(), 2);
}

#[test]
fn element_attached_declarations_come_after_every_sheet() {
    // Their order of appearance is after all sheets' declarations, in
    // their own order (6.1).
    let s = set("p { font-family: a; counter-reset: b } p { font-family: c }");
    let t = {
        let mut t = Tree::new(true, "html");
        t.add(0, HTML, "p", &[]);
        t
    };
    let style = StyleAttribute::parse(Wtf8::from_str("font-family: d; font-family: e"));
    let c = s.prepare();
    c.with_session(false, |ss| {
        let out = ss.cascade(&t.el(1), Some(&style));
        let color = out.get("font-family").unwrap();
        let orders: Vec<u32> = color.iter().map(|e| e.priority.order).collect();
        assert_eq!(orders, vec![4, 3, 2, 0]);
        assert_eq!(color[0].source, Source::Attribute(1));
        assert_eq!(color[3].source, Source::Rule(0));
    });
}

#[test]
fn each_element_tries_only_its_buckets() {
    // Each selector is in one bucket: its rightmost compound's ID, else its
    // first class, else its type, else the universal bucket.
    let css = "#i { order: 1 } .x { order: 1 } p { order: 1 } * { order: 1 } #i.x { order: 1 } p.x { order: 1 }
        .x.y { order: 1 } .y.x { order: 1 } .x.z { order: 1 } p#i { order: 1 } :is(.x) { order: 1 } div .x { order: 1 }
        a::before { order: 1 } p, .x, #i { order: 1 }";
    let s = set(css);
    let mut t = Tree::new(true, "html");
    let full = t.add(0, HTML, "p", &[("id", "i"), ("class", "x X x")]);
    let plain = t.add(0, HTML, "p", &[]);
    let y = t.add(0, HTML, "b", &[("class", "y")]);
    let c = s.prepare();
    c.with_session(false, |ss| {
        let mut tried = |e: usize| {
            let before = ss.selectors_tried();
            ss.cascade(&t.el(e), None);
            ss.selectors_tried() - before
        };
        // id i: #i, #i.x, p#i, #i (list) = 4; class x (once, though
        // written twice, and X lowercased to x): .x, p.x, .x.y, .x.z,
        // div .x, .x (list) = 6; type p: p, p (list) = 2; universal: *,
        // :is(.x) = 2.
        assert_eq!(tried(full), 14);
        // type p and universal.
        assert_eq!(tried(plain), 4);
        // class y: .y.x; universal 2.
        assert_eq!(tried(y), 3);
        // The root html: universal only.
        assert_eq!(tried(0), 2);
    });
}

#[test]
fn nesting_selector_detection_per_item() {
    // A nested selector without & is relative to & (css-nesting-1 3.1);
    // one that contains & at any depth stands as written. Each
    // comma-separated item is decided alone.
    let mut t = Tree::new(true, "html");
    let p = t.add(0, HTML, "div", &[("class", "p")]);
    let inside = t.add(p, HTML, "b", &[("class", "c")]);
    let outside = t.add(0, HTML, "b", &[("class", "c")]);
    let x = t.add(0, HTML, "div", &[("class", "x")]);
    let p_in_x = t.add(x, HTML, "div", &[("class", "p")]);
    let titled = t.add(p, HTML, "i", &[("title", "&")]);
    let titled_outside = t.add(0, HTML, "i", &[("title", "&")]);
    let colored = |css: &str| -> Vec<usize> {
        let s = set(css);
        let c = s.prepare();
        c.with_session(false, |ss| {
            (0..t.nodes.len())
                .filter(|&i| ss.cascade(&t.el(i), None).winner("font-family").is_some())
                .collect()
        })
    };
    // ":is(.c)" is relative even though the next item has &.
    assert_eq!(
        colored(".p { :is(.c), &.q { font-family: x } }"),
        vec![inside]
    );
    // & inside a function: ".x :is(&)" stands as written.
    assert_eq!(colored(".p { .x :is(&) { font-family: x } }"), vec![p_in_x]);
    assert_eq!(
        colored(".p { .x :is(.y, :not(&)) { font-family: x } }"),
        vec![]
    );
    // & in a string is not the nesting selector.
    assert_eq!(
        colored(".p { [title=\"&\"] { font-family: x } }"),
        vec![titled]
    );
    let _ = (outside, titled_outside);
}

/// How many nested rules a sheet refuses.
fn refused(css: &str) -> usize {
    set(css).refused_too_deep()
}

#[test]
fn nesting_depth_counts_every_functional_level() {
    // A nested rule's depth is its parent's plus one plus its selector's
    // depth; MAX_RULE_NESTING passes, one more is refused. Each kind of
    // functional pseudo-class or pseudo-element counts, in any compound.
    let m = MAX_RULE_NESTING as usize;
    let wrap =
        |open: &str, inner: &str, n: usize| format!("{}{}{}", open.repeat(n), inner, ")".repeat(n));
    // (selector at selector depth n, with n = m - 1 levels passing)
    type Shape = fn(usize) -> String;
    let shapes: &[(&str, Shape)] = &[
        (":is", |n| format!("{}&{}", ":is(".repeat(n), ")".repeat(n))),
        (":where", |n| {
            format!("{}&{}", ":where(".repeat(n), ")".repeat(n))
        }),
        (":not", |n| {
            format!("{}&{}", ":not(".repeat(n), ")".repeat(n))
        }),
        (":has", |n| {
            format!("{}:has(.a){}", ":is(".repeat(n - 1), ")".repeat(n - 1))
        }),
        (":nth-child of", |n| {
            format!(
                "{}:nth-child(1 of .a){}",
                ":is(".repeat(n - 1),
                ")".repeat(n - 1)
            )
        }),
        (":nth-last-child of", |n| {
            format!(
                "{}:nth-last-child(1 of .a){}",
                ":is(".repeat(n - 1),
                ")".repeat(n - 1)
            )
        }),
        (":host", |n| {
            format!("{}:host(.a){}", ":is(".repeat(n - 1), ")".repeat(n - 1))
        }),
        ("::slotted", |n| {
            format!("::slotted({}.a{})", ":is(".repeat(n - 1), ")".repeat(n - 1))
        }),
        ("second compound", |n| {
            format!(".b {}&{}", ":is(".repeat(n), ")".repeat(n))
        }),
        ("after a pseudo-element", |n| {
            format!("::part(x){}.a{}", ":is(".repeat(n), ")".repeat(n))
        }),
    ];
    for (name, shape) in shapes {
        let ok = format!("p {{ {} {{ font-family: x }} }}", shape(m - 1));
        let deep = format!("p {{ {} {{ font-family: x }} }}", shape(m));
        let s = set(&ok);
        assert_eq!(
            (s.rules().len(), s.refused_too_deep()),
            (2, 0),
            "{name}: {ok}"
        );
        let s = set(&deep);
        assert_eq!((s.rules().len(), s.refused_too_deep()), (1, 1), "{name}");
    }
    // The parent's depth carries down: two levels of m / 2.
    let half = |n: usize| wrap(":is(", "&", n);
    assert_eq!(
        refused(&format!("{} {{ {} {{ }} }}", half(m / 2), half(m / 2 - 1))),
        0
    );
    assert_eq!(
        refused(&format!("{} {{ {} {{ }} }}", half(m / 2), half(m / 2))),
        1
    );
}
