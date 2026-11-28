//! Hand-worked cascade cases, most of them the spec's own examples, each
//! citing its section (css-cascade-5, css-nesting-1 and css-namespaces-3
//! at csswg-drafts 7027ab6). Each case gives the sheets and the tree, and
//! checks the cascaded value's text; the working is in the comments.

mod common;

use common::tree::{HTML, SVG, Tree};
use optimal_style::{MAX_RULE_NESTING, Origin, Source, StyleAttribute, StyleSet};
use optimal_wtf8::Wtf8;

/// The cascaded value (its source text) of `prop` on each element of `t`,
/// with `sheets` in order.
fn cascaded(sheets: &[(&str, Origin)], t: &Tree, prop: &str) -> Vec<Option<String>> {
    let mut set = StyleSet::new();
    for (s, o) in sheets {
        set.add_sheet(Wtf8::from_str(s), *o);
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
                out.winner(prop).map(|e| match e.source {
                    Source::Rule(d) => {
                        let d = &set.decls()[d as usize];
                        source(&set.sheets()[d.sheet as usize].parsed, set.values(d))
                    }
                    Source::Attribute(a) => {
                        let st = styles[i].as_ref().unwrap();
                        source(&st.parsed, st.values(a as usize))
                    }
                })
            })
            .collect()
    })
}

/// The source text of a value.
fn source(p: &optimal_css::Parsed<'_>, v: optimal_css::Values<'_, '_>) -> String {
    let spans: Vec<_> = v.map(|c| c.span()).collect();
    match (spans.first(), spans.last()) {
        (Some(f), Some(l)) => p.source(f.start..l.end).to_string_lossy(),
        _ => String::new(),
    }
}

/// The winner's text on element `i`.
fn ident(sheets: &[(&str, Origin)], t: &Tree, prop: &str, i: usize) -> Option<String> {
    cascaded(sheets, t, prop)[i].clone()
}

fn author(s: &str) -> Vec<(&str, Origin)> {
    vec![(s, Origin::Author)]
}

/// html > one child element.
fn one(local: &str, attrs: &[(&str, &str)]) -> Tree {
    let mut t = Tree::new(true, "html");
    t.add(0, HTML, local, attrs);
    t
}

#[test]
fn unlayered_beats_layered_whatever_the_specificity() {
    // css-cascade-5 6.4, first example: audio (0,0,1) unlayered wins over
    // audio[controls] (0,1,1) in layer "reset".
    let css = "audio { display: flex; }
        @layer reset { audio[controls] { display: block; } }";
    let t = one("audio", &[("controls", "")]);
    assert_eq!(
        ident(&author(css), &t, "display", 1).as_deref(),
        Some("flex")
    );
}

#[test]
fn layer_order_example() {
    // 6.4.3: the order is reset.type, reset, framework.theme, framework,
    // then the outer layer.
    let css = "h1 { color: darkslateblue; }
        @layer reset.type { strong { font-weight: bold; } }
        @layer framework {
          .title { font-weight: 100; }
          @layer theme { h1, h2 { color: maroon; } }
        }
        @layer reset { [hidden] { display: none; } }";
    let mut t = Tree::new(true, "html");
    t.add(0, HTML, "h1", &[]);
    t.add(0, HTML, "strong", &[("class", "title")]);
    let s = author(css);
    // Unlayered h1 beats framework.theme.
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("darkslateblue"));
    // framework (implicit sub-layer) comes after reset.type: 100 wins over
    // bold. font-weight: 100 is a number.
    assert_eq!(ident(&s, &t, "font-weight", 2).as_deref(), Some("100"));
}

#[test]
fn nested_names_are_distinct_layers() {
    // 6.4.2, second example: framework.base is not base. "base" is
    // declared first, so framework.base (inside framework, declared later)
    // comes after it and wins.
    let css = "@layer base { p { z-index: 1; } }
        @layer framework { @layer base { p { z-index: 2; } } }";
    let t = one("p", &[]);
    assert_eq!(
        ident(&author(css), &t, "z-index", 1).as_deref(),
        Some("2")
    );
    // And framework.theme in a later block joins the same layer (third
    // example): the later block's rule is in framework.theme, which comes
    // after framework.default.
    let css = "@layer framework {
          @layer default { p { color: red; } }
          @layer theme { p { color: maroon; } }
        }
        @layer framework.theme { p { color: rebeccapurple; } }
        @layer framework.default { p { color: blue; } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("rebeccapurple")
    );
}

#[test]
fn anonymous_layers_are_each_distinct() {
    // 6.4.2.1: two unnamed layers are separate, the second later.
    let t = one("p", &[]);
    let css = "@layer { p { color: green; } } @layer { p { color: red; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("red"));
    // Within one unnamed layer, foo twice is the same layer, which comes
    // before the anonymous layer's own rules.
    let css = "@layer { p { color: green; } @layer foo { p { color: red; } } @layer foo { p { color: blue; } } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
    // In separate unnamed layers, foo is two layers: the second wins.
    let css = "@layer { @layer foo { p { color: red; } } } @layer { @layer foo { p { color: blue; } } } @layer { @layer foo { } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("blue"));
}

#[test]
fn important_reverses_the_layer_order() {
    // 6.1 Layers: for important declarations the earliest layer wins, and
    // a layer beats the unlayered (outer) declarations.
    let t = one("p", &[]);
    let css = "@layer a { p { color: green !important; } }
        @layer b { p { color: red !important; } }
        p { color: blue !important; }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
    // Normal: the later layer, and unlayered over both.
    let css = "@layer a { p { color: red; } } @layer b { p { color: green; } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
}

#[test]
fn layer_statement_sets_the_order_up_front() {
    // 6.4.4.2: "@layer b, a;" makes b come first, whatever the blocks'
    // order; a's rule wins.
    let t = one("p", &[]);
    let css = "@layer b, a; @layer a { p { color: green; } } @layer b { p { color: red; } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
}

#[test]
fn invalid_layer_rules_are_ignored_with_their_contents() {
    let t = one("p", &[]);
    // CSS-wide keywords can't be layer names (6.4.2); a block takes one
    // name; a statement needs one.
    for css in [
        "@layer initial { p { color: red; } }",
        "@layer a.REVERT-LAYER { p { color: red; } }",
        "@layer a, b { p { color: red; } }",
        "@layer a .b { p { color: red; } }",
    ] {
        assert_eq!(ident(&author(css), &t, "color", 1), None, "{css}");
    }
    // An invalid statement declares nothing, not even its valid names: a
    // is declared by its block, after b, and wins. (Had the statement
    // declared a, b would come later and win.)
    let css = "@layer a, initial; @layer b { p { color: red; } } @layer a { p { color: green; } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
}

#[test]
fn origins_and_importance() {
    // 6.1 Origin and Importance.
    let t = one("p", &[]);
    let ua = (
        "p { color: ua; } p { width: ua !important; }",
        Origin::UserAgent,
    );
    let user = (
        "p { color: user; } p { margin: user !important; width: user; }",
        Origin::User,
    );
    let auth = (
        "p { color: author; margin: author !important; width: author !important; } p { padding: author }",
        Origin::Author,
    );
    // The author sheet is first; the order of sheets doesn't matter
    // across origins.
    let sheets = [auth, user, ua];
    // Normal: author over user over user agent.
    assert_eq!(ident(&sheets, &t, "color", 1).as_deref(), Some("author"));
    // Important: user over author.
    assert_eq!(ident(&sheets, &t, "margin-top", 1).as_deref(), Some("user"));
    // Important user agent over everything.
    assert_eq!(ident(&sheets, &t, "width", 1).as_deref(), Some("ua"));
    // Unopposed: the only one.
    assert_eq!(ident(&sheets, &t, "padding-left", 1).as_deref(), Some("author"));
    // Normal user over normal user agent.
    let sheets = [
        ("p { color: ua; }", Origin::UserAgent),
        ("p { color: user; }", Origin::User),
    ];
    assert_eq!(ident(&sheets, &t, "color", 1).as_deref(), Some("user"));
}

#[test]
fn element_attached_declarations() {
    // 6.1 Element-Attached Styles: a style attribute beats any rule of the
    // same importance; an important rule beats a normal attribute; an
    // important attribute beats important rules in any layer.
    let t = one(
        "p",
        &[
            ("id", "x"),
            ("style", "color: attr; margin: attr; width: attr !important"),
        ],
    );
    let css = "#x#x { color: rule; }
        p { margin: rule !important; }
        @layer a { p { width: rule !important; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("attr"));
    assert_eq!(ident(&s, &t, "margin-top", 1).as_deref(), Some("rule"));
    assert_eq!(ident(&s, &t, "width", 1).as_deref(), Some("attr"));
    // User important still beats author important attributes.
    let sheets = [("p { width: user !important }", Origin::User)];
    assert_eq!(ident(&sheets, &t, "width", 1).as_deref(), Some("user"));
    // Later declarations in the attribute win.
    let t = one("p", &[("style", "color: a; color: b")]);
    assert_eq!(ident(&[], &t, "color", 1).as_deref(), Some("b"));
}

#[test]
fn specificity_of_the_most_specific_matching_selector() {
    // 6.1 Specificity and selectors-4 15: a list's specificity for an
    // element is that of its most specific selector that matches it.
    let css = "#x, p { color: red; } .a { color: green; }";
    let mut t = Tree::new(true, "html");
    t.add(0, HTML, "p", &[("class", "a")]);
    t.add(0, HTML, "p", &[("class", "a"), ("id", "x")]);
    let s = author(css);
    // p (0,0,1) loses to .a (0,1,0); #x (1,0,0) wins.
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("green"));
    assert_eq!(ident(&s, &t, "color", 2).as_deref(), Some("red"));
}

#[test]
fn order_of_appearance_across_sheets() {
    // 6.1 Order of Appearance: sheets in order; the last wins.
    let t = one("p", &[]);
    let sheets = [
        ("p { color: first }", Origin::Author),
        ("p { color: second }", Origin::Author),
    ];
    assert_eq!(ident(&sheets, &t, "color", 1).as_deref(), Some("second"));
}

#[test]
fn nested_rules_come_after_their_parent() {
    // css-nesting-1 3.2: both (0,0,1); the nested rule comes after.
    let t = one("article", &[]);
    let css = "article { color: blue; & { color: red; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("red"));
    // :where(&) has specificity zero and loses before order counts.
    let css = "article { color: blue; :where(&) { color: red; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("blue"));
    // 3.2 mixing: the trailing declaration is a nested declarations rule
    // after the & rule, so red wins, not blue.
    let css = "article { color: green; & { color: blue; } color: red; }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("red"));
}

#[test]
fn nesting_selector_specificity_is_the_parents_largest() {
    // css-nesting-1 4: & counts as the largest specificity in "#a, b"
    // (1,0,0), so "& c" is (1,0,1) and beats ".foo c" (0,1,1), though only
    // "b" matched.
    let css = "#a, b { & c { color: blue; } } .foo c { color: red; }";
    let mut t = Tree::new(true, "html");
    let b = t.add(0, HTML, "b", &[("class", "foo")]);
    t.add(b, HTML, "c", &[]);
    assert_eq!(ident(&author(css), &t, "color", 2).as_deref(), Some("blue"));
}

#[test]
fn relative_nested_selectors() {
    // css-nesting-1 3.1: "> .bar" is "& > .bar"; ".parent &" stands as
    // written; ".x" without & is "& .x".
    let mut t = Tree::new(true, "html");
    let foo = t.add(0, HTML, "div", &[("class", "foo parent")]);
    let bar = t.add(foo, HTML, "p", &[("class", "bar")]);
    let deep = t.add(bar, HTML, "p", &[("class", "bar")]);
    let css = ".foo { > .bar { color: child; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", bar).as_deref(), Some("child"));
    assert_eq!(ident(&s, &t, "color", deep), None);
    let css = ".bar { .parent & { color: inside; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", bar).as_deref(), Some("inside"));
    assert_eq!(ident(&s, &t, "color", deep).as_deref(), Some("inside"));
    let css = ".foo { .bar { color: desc; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", deep).as_deref(), Some("desc"));
    assert_eq!(ident(&s, &t, "color", foo), None);
    // Same specificity with and without the written &: the later wins.
    let css = ".foo { & > .bar { color: a; } } .foo { > .bar { color: b; } }";
    assert_eq!(ident(&author(css), &t, "color", bar).as_deref(), Some("b"));
    // A nested rule with an invalid selector is dropped, but the parent
    // stays (css-nesting-1 3.1).
    let css = ".foo { color: kept; !!x { color: dropped; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", foo).as_deref(), Some("kept"));
}

#[test]
fn ampersand_at_top_level_is_scope() {
    // css-nesting-1 4: outside nesting, & is :scope, which in a style sheet
    // is :root.
    let mut t = Tree::new(true, "html");
    t.add(0, HTML, "p", &[]);
    let css = "& { color: root; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 0).as_deref(), Some("root"));
    assert_eq!(ident(&s, &t, "color", 1), None);
}

#[test]
fn nested_group_rules() {
    // css-nesting-1 3.3: declarations directly in a nested @layer act as a
    // nested declarations rule, in that layer: here layer "a", which loses
    // to the unlayered "green" though it comes later.
    let t = one("p", &[]);
    let css = "p { color: green; @layer a { color: red; } }";
    assert_eq!(
        ident(&author(css), &t, "color", 1).as_deref(),
        Some("green")
    );
    // Style rules nested in it take & from the style rule.
    let css = "@layer a { p { @layer b { & { color: deep; } } } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("deep"));
    // An at-rule that is not allowed in a style rule is ignored; the
    // declarations around it stay in the rule.
    let css = "p { color: before; @font-face { color: x; } width: after; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("before"));
    assert_eq!(ident(&s, &t, "width", 1).as_deref(), Some("after"));
}

#[test]
fn declarations_outside_style_rules_are_dropped() {
    let t = one("p", &[]);
    let css = "@layer a { color: red; p { width: ok; } }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1), None);
    assert_eq!(ident(&s, &t, "width", 1).as_deref(), Some("ok"));
}

#[test]
fn namespaces() {
    // css-namespaces-3: a declared prefix, the default namespace (type
    // selectors only), and placement.
    let mut t = Tree::new(true, "html");
    t.add(0, HTML, "a", &[]);
    t.add(0, SVG, "a", &[]);
    let css = "@namespace svg url(http://www.w3.org/2000/svg); svg|a { color: svg; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1), None);
    assert_eq!(ident(&s, &t, "color", 2).as_deref(), Some("svg"));
    let css = "@namespace \"http://www.w3.org/1999/xhtml\"; a { color: html; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("html"));
    assert_eq!(ident(&s, &t, "color", 2), None);
    // After a style rule, @namespace is ignored, so svg|a has an
    // undeclared prefix and its rule is invalid.
    let css = "p {} @namespace svg url(http://www.w3.org/2000/svg); svg|a { color: svg; }";
    assert_eq!(ident(&author(css), &t, "color", 2), None);
    // After @import and @layer statements it is fine; an invalid rule
    // before it doesn't count.
    let css = "@layer x; @import url(a.css); !!bad {} @charset \"x\"; @namespace svg url(http://www.w3.org/2000/svg); svg|a { color: svg; }";
    assert_eq!(ident(&author(css), &t, "color", 2).as_deref(), Some("svg"));
    // A @layer statement after @import ends the place for @namespace
    // (css-cascade-5 6.4.4.2 note).
    let css = "@import url(a.css); @layer x; @namespace svg url(http://www.w3.org/2000/svg); svg|a { color: svg; }";
    assert_eq!(ident(&author(css), &t, "color", 2), None);
    // Prefixes are per sheet.
    let sheets = [
        (
            "@namespace svg url(http://www.w3.org/2000/svg);",
            Origin::Author,
        ),
        ("svg|a { color: svg; }", Origin::Author),
    ];
    assert_eq!(ident(&sheets, &t, "color", 2), None);
}

#[test]
fn import_layers_are_declared_and_placement_is_checked() {
    // css-cascade-5 2: an @import's layer joins the layer order even though
    // the sheet isn't loaded. b is declared by the import, so a comes later
    // and wins.
    let t = one("p", &[]);
    let css =
        "@import url(x.css) layer(b); @layer a { p { color: a; } } @layer b { p { color: b; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("a"));
    // After "@layer b;" a second @import is invalid (6.4.4.2 note), so its
    // layer a is declared by the block after c: order b, c, a, and a wins.
    // (Had the import counted, a would come before c, and c would win.)
    let css = "@import \"x.css\"; @layer b; @import \"y.css\" layer(a);
        @layer c { p { color: c; } } @layer a { p { color: a; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("a"));
    // With import conditions the layer isn't declared yet (the conditions
    // aren't evaluated): a is declared by its block, after c.
    let css = "@import url(x.css) layer(a) print; @layer c { p { color: c; } } @layer a { p { color: a; } }";
    assert_eq!(ident(&author(css), &t, "color", 1).as_deref(), Some("a"));
}

#[test]
fn property_names() {
    // Property names are ASCII case-insensitive; custom property names are
    // case-sensitive.
    let t = one("p", &[]);
    let css = "p { COLOR: upper; color: lower; --X: big; --x: small; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1).as_deref(), Some("lower"));
    assert_eq!(ident(&s, &t, "--X", 1).as_deref(), Some("big"));
    assert_eq!(ident(&s, &t, "--x", 1).as_deref(), Some("small"));
}

#[test]
fn pseudo_element_selectors_never_match_elements() {
    let t = one("p", &[]);
    let css = "p::before { color: red; } p, p::after { width: ok; }";
    let s = author(css);
    assert_eq!(ident(&s, &t, "color", 1), None);
    assert_eq!(ident(&s, &t, "width", 1).as_deref(), Some("ok"));
}

#[test]
fn dropped_at_rules() {
    // Known gap: conditional group rules are dropped for now. Unknown
    // at-rules are ignored.
    let t = one("p", &[]);
    let css = "@media all { p { color: red; } } @unknown { p { color: red; } } @supports (a: b) { p { color: red; } }";
    assert_eq!(ident(&author(css), &t, "color", 1), None);
}

#[test]
fn deep_nesting_is_refused_not_overflowed() {
    // Style rules nested past MAX_RULE_NESTING are refused with their
    // contents; the rules above stay. The top rule has depth 0 and each
    // "&" level adds one, so depths 0 to MAX_RULE_NESTING stay.
    let n = MAX_RULE_NESTING as usize + 50;
    let css = format!("p {{ {}{}", "color: c; & { ".repeat(n), "} ".repeat(n + 1));
    let mut set = StyleSet::new();
    set.add_sheet(Wtf8::from_str(&css), Origin::Author);
    assert_eq!(set.rules().len(), MAX_RULE_NESTING as usize + 1);
    assert_eq!(set.refused_too_deep(), 1);
    // The deepest rule that stays still matches.
    let t = one("p", &[]);
    let c = set.prepare();
    let count = c.with_session(false, |s| {
        s.cascade(&t.el(1), None).get("color").map(|e| e.len())
    });
    assert_eq!(count, Some(MAX_RULE_NESTING as usize + 1));
    // Each functional pseudo-class level counts too.
    let css = format!(
        "p {{ {}&{} {{ color: x }} }}",
        ":is(".repeat(MAX_RULE_NESTING as usize),
        ")".repeat(MAX_RULE_NESTING as usize)
    );
    let mut set = StyleSet::new();
    set.add_sheet(Wtf8::from_str(&css), Origin::Author);
    assert_eq!(set.refused_too_deep(), 1);
    // Layers nest as deep as the input does.
    let css = format!(
        "{}p {{ color: deep }}{}",
        "@layer {".repeat(100_000),
        "}".repeat(100_000)
    );
    let mut set = StyleSet::new();
    set.add_sheet(Wtf8::from_str(&css), Origin::Author);
    assert_eq!(set.layers().len(), 100_003);
}
