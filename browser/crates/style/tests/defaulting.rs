//! Hand-worked cases for declaration validity and defaulting
//! (css-cascade-5 #shorthand, #all-shorthand, #defaulting), each with the
//! working in a comment. Values are checked as specified-value text (see
//! `common/specified.rs`).

mod common;

use common::specified::specified;
use common::tree::{HTML, Tree};
use optimal_style::{Origin, StyleSet, Value};
use optimal_wtf8::Wtf8;

/// html > body > p#x (with an optional style attribute) > span.
fn tree(style: Option<&str>) -> Tree {
    let mut t = Tree::new(true, "html");
    let body = t.add(0, HTML, "body", &[]);
    let mut attrs = vec![("id", "x")];
    if let Some(s) = style {
        attrs.push(("style", s));
    }
    let p = t.add(body, HTML, "p", &attrs);
    t.add(p, HTML, "span", &[]);
    t
}

const P: usize = 2;
const SPAN: usize = 3;

fn author(css: &str) -> Vec<(&str, Origin)> {
    vec![(css, Origin::Author)]
}

fn on(sheets: &[(&str, Origin)], t: &Tree, el: usize, prop: &str) -> Option<String> {
    specified(sheets, t, prop)[el].clone()
}

#[test]
fn css_wide_keywords() {
    let t = tree(None);
    // initial: the table's value, whatever the parent has.
    let css = "body { color: red; display: flex } p { color: initial; display: initial }";
    assert_eq!(
        on(&author(css), &t, P, "color").as_deref(),
        Some("CanvasText")
    );
    assert_eq!(
        on(&author(css), &t, P, "display").as_deref(),
        Some("inline")
    );
    // inherit: the parent's value, inherited property or not.
    let css = "body { color: red; display: flex } p { color: inherit; display: inherit }";
    assert_eq!(on(&author(css), &t, P, "color").as_deref(), Some("red"));
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // unset: inherit for color (inherited), initial for display.
    let css = "body { color: red; display: flex } p { color: unset; display: unset }";
    assert_eq!(on(&author(css), &t, P, "color").as_deref(), Some("red"));
    assert_eq!(
        on(&author(css), &t, P, "display").as_deref(),
        Some("inline")
    );
    // No declaration: the same as unset. The span inherits p's color
    // through p, which has none of its own.
    let css = "body { color: red; display: flex }";
    assert_eq!(on(&author(css), &t, SPAN, "color").as_deref(), Some("red"));
    assert_eq!(
        on(&author(css), &t, SPAN, "display").as_deref(),
        Some("inline")
    );
    // The root inherits initial values.
    let css = "html { color: inherit; z-index: inherit }";
    assert_eq!(
        on(&author(css), &t, 0, "color").as_deref(),
        Some("CanvasText")
    );
    assert_eq!(on(&author(css), &t, 0, "z-index").as_deref(), Some("auto"));
    // Keywords are ASCII case-insensitive.
    let css = "body { color: red } p { COLOR: INHERIT; color: Initial }";
    assert_eq!(
        on(&author(css), &t, P, "color").as_deref(),
        Some("CanvasText")
    );
}

#[test]
fn inherited_values_name_their_element() {
    // body declares color; p inherits it, and span through p: each names
    // body (key 1).
    let mut set = StyleSet::new();
    set.add_sheet(Wtf8::from_str("body { color: red }"), Origin::Author);
    let c = set.prepare();
    let t = tree(None);
    c.with_session(false, |s| {
        let initial = optimal_style::Style::initial();
        let html = s.style(&t.el(0), 0, None, &initial);
        let body = s.style(&t.el(1), 1, None, &html);
        let p = s.style(&t.el(P), 2, None, &body);
        let span = s.style(&t.el(SPAN), 3, None, &p);
        let el = |st: &optimal_style::Style<'_>| match st.get("color") {
            Some(Value::Declared { element, .. }) => Some(element),
            _ => None,
        };
        assert_eq!(html.get("color"), Some(Value::Initial));
        assert_eq!([el(&body), el(&p), el(&span)], [Some(1), Some(1), Some(1)]);
        // A non-inherited property doesn't pass down.
        assert_eq!(span.get("display"), Some(Value::Initial));
        assert_eq!(span.get("nope"), None);
    });
}

#[test]
fn revert_by_origin() {
    let t = tree(None);
    let ua = ("p { display: block; color: blue }", Origin::UserAgent);
    let user = ("p { display: flex }", Origin::User);
    // Author revert: back to the user origin's flex.
    let sheets = [
        ua,
        user,
        ("p { display: grid; display: revert }", Origin::Author),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("flex"));
    // An important author revert also rolls back every author declaration:
    // the normal grid too.
    let sheets = [
        ua,
        user,
        (
            "p { display: grid } p { display: revert !important }",
            Origin::Author,
        ),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("flex"));
    // User revert: past the author grid to the UA's block (the author
    // declaration lost to the important user one and is gone with it).
    let sheets = [
        ua,
        ("p { display: revert !important }", Origin::User),
        ("p { display: grid }", Origin::Author),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("block"));
    // UA revert is unset: initial for display, inherited for color.
    let sheets = [
        ("p { display: revert; color: revert }", Origin::UserAgent),
        ("body { color: green; display: flex }", Origin::Author),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("inline"));
    assert_eq!(on(&sheets, &t, P, "color").as_deref(), Some("green"));
    // Nothing to roll back to: unset.
    let sheets = [("p { display: revert }", Origin::Author)];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("inline"));
}

#[test]
fn revert_layer() {
    let t = tree(None);
    // Normal: past its own layer (b's grid and the revert-layer) to a.
    let css = "@layer a, b; @layer a { p { display: flex } }
        @layer b { p { display: grid } p { display: revert-layer } }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // From the implicit outer layer to the last layer.
    let css = "@layer a { p { display: flex } } p { display: grid; display: revert-layer }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // Important, in layer b of a, b: the list from the top is b's
    // important revert-layer, then (normal) the unlayered table, b's grid,
    // a's flex. Between b's normal and important levels lie the unlayered
    // normal declarations (after b), so they are rolled back too: flex.
    let css = "@layer a, b; @layer a { p { display: flex } }
        @layer b { p { display: grid; display: revert-layer !important } }
        p { display: table }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // But important declarations in earlier layers win over it (a's
    // important beats b's important), and an earlier layer's normal ones
    // stay: here a's important block.
    let css = "@layer a, b; @layer a { p { display: block !important } }
        @layer b { p { display: revert-layer !important } }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("block"));
    // Important in the user origin: every author declaration lies between
    // the user's normal and important levels, so none survive; the user
    // layer's own normal one goes too; the UA's is what is left.
    let sheets = [
        ("p { display: block }", Origin::UserAgent),
        (
            "p { display: flex; display: revert-layer !important }",
            Origin::User,
        ),
        (
            "p { display: grid } p { display: table !important }",
            Origin::Author,
        ),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("block"));
    // No lower layer in the origin: on to the previous origin.
    let sheets = [
        ("p { display: block }", Origin::UserAgent),
        ("@layer x { p { display: revert-layer } }", Origin::Author),
    ];
    assert_eq!(on(&sheets, &t, P, "display").as_deref(), Some("block"));
}

#[test]
fn revert_layer_in_a_style_attribute() {
    // Normal: the attribute's declarations are one layer, above all
    // normal rules: back to the rule.
    let t = tree(Some("display: grid; display: revert-layer"));
    let css = "@layer a { p { display: flex } }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // Important: only element-attached declarations roll back (the normal
    // grid in the attribute too), not the important rule in between.
    let t = tree(Some("display: grid; display: revert-layer !important"));
    let css = "p { display: flex } #x { display: table !important }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("table"));
    let css = "p { display: flex }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
}

#[test]
fn revert_rule() {
    let t = tree(None);
    // The whole rule goes: its grid too.
    let css = "p { display: flex } #x { display: grid; display: revert-rule }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // An important revert-rule also takes its rule's normal declarations
    // with it, and leaves other rules' normal ones, earlier or later.
    let css = "#x { display: revert-rule !important; display: table } p { display: flex }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // Each nested declarations rule is a rule of its own: the one after
    // `& {}` rolls back to the rule's own declarations.
    let css = "#x { display: flex; & {} display: grid; display: revert-rule }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // A chain: each rolls back one more rule.
    let css =
        "p { z-index: 1 } p { z-index: 2 } p { z-index: revert-rule } p { z-index: revert-rule }";
    assert_eq!(on(&author(css), &t, P, "z-index").as_deref(), Some("2"));
    // The style attribute is one rule.
    let t = tree(Some("display: grid; display: revert-rule"));
    let css = "p { display: flex }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
}

#[test]
fn an_invalid_declaration_with_a_block_splits_the_rule() {
    // css-syntax-3: `nope: {x}` parses as a declaration (a {}-block may be
    // a whole value), but it isn't valid (unknown property), so the parser
    // reads it again as a qualified rule with a block. That rule ends the
    // run of declarations, so what follows is a nested declarations rule,
    // which revert-rule rolls back on its own: flex. (Checking validity
    // only after parsing would keep one run, and give inline.)
    let t = tree(None);
    let css = "#x { display: flex; nope: {x}; display: grid; display: revert-rule }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    // An invalid declaration without a block doesn't split.
    let css = "#x { display: flex; nope: x; display: grid; display: revert-rule }";
    assert_eq!(
        on(&author(css), &t, P, "display").as_deref(),
        Some("inline")
    );
}

#[test]
fn invalid_declarations_are_dropped() {
    let t = tree(None);
    for (css, want) in [
        // A keyword combined with another value.
        ("p { display: flex; display: grid initial }", "flex"),
        ("p { display: flex; display: inherit inherit }", "flex"),
        // `all` takes only keywords.
        ("p { display: flex; all: grid }", "flex"),
        // Unknown names, and empty values.
        ("p { display: flex; nope: inherit }", "flex"),
        ("p { display: flex; display: }", "flex"),
        // `default` is reserved, not a keyword, and not in the grammar.
        ("p { display: flex; display: default }", "flex"),
        // Not in the grammar (css-display-4: one inside value).
        ("p { display: flex; display: grid flex }", "flex"),
        // A substitution function makes any value valid at parse time,
        // and it isn't a keyword then.
        ("p { display: var(--a) initial }", "var(--a) initial"),
        ("p { display: --f(x) inherit }", "--f(x) inherit"),
        (
            "p { display: calc(var(--a)) initial }",
            "calc(var(--a)) initial",
        ),
        // Function names are ASCII case-insensitive.
        ("p { display: VAR(--a) initial }", "VAR(--a) initial"),
        // A keyword inside a function is not at the top level, so the
        // value is checked against the grammar, which has no f().
        ("p { display: flex; display: f(initial) }", "flex"),
    ] {
        assert_eq!(
            on(&author(css), &t, P, "display").as_deref(),
            Some(want),
            "{css}"
        );
    }
    // Custom properties take any value, keywords included.
    let css = "p { --a: initial x; --b: ; --c: inherit }";
    assert_eq!(on(&author(css), &t, P, "--a").as_deref(), Some("initial x"));
    assert_eq!(on(&author(css), &t, P, "--b").as_deref(), Some(""));
    assert_eq!(on(&author(css), &t, P, "--c"), None);
}

#[test]
fn shorthands_all_and_aliases() {
    let t = tree(None);
    // A shorthand declares each longhand; a later longhand wins over it.
    let css = "p { margin: 1px 2px; margin-left: 3px }";
    assert_eq!(
        on(&author(css), &t, P, "margin-top").as_deref(),
        Some("1px 2px")
    );
    assert_eq!(
        on(&author(css), &t, P, "margin-left").as_deref(),
        Some("3px")
    );
    // A keyword on a shorthand sets every longhand to it, reset-only ones
    // included (border resets border-image).
    let css = "body { border-image-width: 3 } p { border-image-width: 2; border: inherit }";
    assert_eq!(
        on(&author(css), &t, P, "border-image-width").as_deref(),
        Some("3")
    );
    assert_eq!(
        on(&author(css), &t, P, "border-top-style").as_deref(),
        Some("none")
    );
    // font resets line-height; font-palette is cascaded independently.
    let css = "p { line-height: 2; font-palette: dark; font: initial }";
    assert_eq!(
        on(&author(css), &t, P, "line-height").as_deref(),
        Some("normal")
    );
    assert_eq!(
        on(&author(css), &t, P, "font-palette").as_deref(),
        Some("dark")
    );
    // all: everything but direction and unicode-bidi, and no custom
    // properties.
    let css =
        "body { direction: rtl; --v: 1 } p { display: flex; unicode-bidi: isolate; all: initial }";
    assert_eq!(
        on(&author(css), &t, P, "display").as_deref(),
        Some("inline")
    );
    assert_eq!(on(&author(css), &t, P, "direction").as_deref(), Some("rtl"));
    assert_eq!(
        on(&author(css), &t, P, "unicode-bidi").as_deref(),
        Some("isolate")
    );
    assert_eq!(on(&author(css), &t, P, "--v").as_deref(), Some("1"));
    // all: revert-layer rolls each longhand back on its own.
    let css = "@layer a { p { color: green; display: flex } }
        @layer b { p { display: grid } p { all: revert-layer } }";
    assert_eq!(on(&author(css), &t, P, "display").as_deref(), Some("flex"));
    assert_eq!(on(&author(css), &t, P, "color").as_deref(), Some("green"));
    // Legacy name aliases set the new name.
    let css = "p { overflow-wrap: anywhere; word-wrap: break-word }";
    assert_eq!(
        on(&author(css), &t, P, "overflow-wrap").as_deref(),
        Some("break-word")
    );
}

#[test]
fn custom_properties() {
    let t = tree(None);
    // Inherited, so p and span see body's value.
    let css = "body { --a: 1 }";
    assert_eq!(on(&author(css), &t, SPAN, "--a").as_deref(), Some("1"));
    // initial is the guaranteed-invalid value, which children inherit.
    let css = "body { --a: 1 } p { --a: initial }";
    assert_eq!(on(&author(css), &t, P, "--a"), None);
    assert_eq!(on(&author(css), &t, SPAN, "--a"), None);
    // unset and inherit inherit.
    let css = "body { --a: 1 } p { --a: 2; --a: unset }";
    assert_eq!(on(&author(css), &t, P, "--a").as_deref(), Some("1"));
    // Names are case-sensitive.
    let css = "body { --a: 1; --A: 2 } p { --A: inherit }";
    assert_eq!(on(&author(css), &t, P, "--A").as_deref(), Some("2"));
    assert_eq!(on(&author(css), &t, P, "--a").as_deref(), Some("1"));
    // revert-rule, and unset with nothing to inherit.
    let css = "p { --a: x } p { --a: y; --a: revert-rule } p { --b: unset }";
    assert_eq!(on(&author(css), &t, P, "--a").as_deref(), Some("x"));
    assert_eq!(on(&author(css), &t, P, "--b"), None);
    // Custom properties declared on several levels merge by name.
    let css = "html { --a: 1; --c: 3 } body { --b: 2 } p { --a: 4 }";
    assert_eq!(on(&author(css), &t, SPAN, "--a").as_deref(), Some("4"));
    assert_eq!(on(&author(css), &t, SPAN, "--b").as_deref(), Some("2"));
    assert_eq!(on(&author(css), &t, SPAN, "--c").as_deref(), Some("3"));
}
