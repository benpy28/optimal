//! Hand-worked parser cases. Each names the css-syntax-3 section 5 step it
//! checks and works the expected result out in a comment. Results are
//! compared in the compact JSON shape of `common::parse_json`:
//! `["qr", prelude, declarations, children]`, `["at", name, prelude, block]`,
//! `["nd", declarations]`, `["d", name, value, important, original text]`,
//! with values written by [`v`] as their source text.

mod common;

use common::parse_reference::Entry;
use optimal_css::{AcceptAll, Parsed, Rule, SyntaxError, Token, Validity};
use optimal_wtf8::Wtf8;
use serde_json::{Value, json};

/// The source text of each top-level component value in a JSON value list,
/// so expectations read as CSS.
fn texts(src: &str, values: &Value) -> Vec<String> {
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|v| span_text(src, v))
        .collect()
}

fn span_text(src: &str, v: &Value) -> String {
    match v[0].as_str().unwrap() {
        "t" => src[v[2].as_u64().unwrap() as usize..v[3].as_u64().unwrap() as usize].to_owned(),
        "f" => format!(
            "{}({}{}",
            v[1].as_str().unwrap(),
            texts(src, &v[2]).concat(),
            if v[3].as_bool().unwrap() { ")" } else { "" }
        ),
        "b" => {
            let (o, c) = match v[1].as_str().unwrap() {
                "OpenCurly" => ("{", "}"),
                "OpenSquare" => ("[", "]"),
                _ => ("(", ")"),
            };
            format!(
                "{o}{}{}",
                texts(src, &v[2]).concat(),
                if v[3].as_bool().unwrap() { c } else { "" }
            )
        }
        other => panic!("{other}"),
    }
}

/// Rewrite every value list in `j` as one string of source text.
fn readable(src: &str, j: &Value) -> Value {
    let Some(a) = j.as_array() else {
        return j.clone();
    };
    match a.first().and_then(Value::as_str) {
        Some("d") => json!(["d", a[1], texts(src, &a[2]).concat(), a[3]]),
        Some("qr") => json!([
            "qr",
            texts(src, &a[1]).concat(),
            a[2].as_array()
                .unwrap()
                .iter()
                .map(|d| readable(src, d))
                .collect::<Vec<_>>(),
            a[3].as_array()
                .unwrap()
                .iter()
                .map(|r| readable(src, r))
                .collect::<Vec<_>>()
        ]),
        Some("at") => json!([
            "at",
            a[1],
            texts(src, &a[2]).concat(),
            a[3].as_array()
                .map(|b| b.iter().map(|i| readable(src, i)).collect::<Vec<_>>())
        ]),
        Some("nd") | Some("decls") => {
            json!([
                a[0],
                a[1].as_array()
                    .unwrap()
                    .iter()
                    .map(|d| readable(src, d))
                    .collect::<Vec<_>>()
            ])
        }
        _ => Value::Array(a.iter().map(|x| readable(src, x)).collect()),
    }
}

/// Parse `src` as a stylesheet, in readable form, and the parser errors.
fn sheet(src: &str) -> (Value, Value) {
    let (r, e) = common::parse_json(Wtf8::from_str(src), Entry::Stylesheet);
    (readable(src, &r), e)
}

#[test]
fn nested_rules_and_declarations_5_5_5() {
    // "color:red" is a declaration; "b{c:d}" isn't (no colon after "b"), so
    // it is re-read as a nested qualified rule; "e:f" after it is a second
    // run of declarations, which 5.5.3 turns into a nested declarations rule.
    let (r, e) = sheet("a{color:red; b{c:d} e:f}");
    assert_eq!(
        r,
        json!([[
            "qr",
            "a",
            [["d", "color", "red", false]],
            [
                ["qr", "b", [["d", "c", "d", false]], []],
                ["nd", [["d", "e", "f", false]]]
            ]
        ]])
    );
    assert_eq!(e, json!([]));
}

#[test]
fn curly_block_values_5_5_6_step_8() {
    // A {}-block is allowed as the whole value of a non-custom property...
    assert_eq!(
        sheet("a{b:{c}}").0,
        json!([["qr", "a", [["d", "b", "{c}", false]], []]])
    );
    // ...and with !important after it (step 6 removes that first).
    assert_eq!(
        sheet("a{b:{c} !important}").0,
        json!([["qr", "a", [["d", "b", "{c}", true]], []]])
    );
    // With anything else it is no declaration: "b:{c} d" is re-read as a
    // rule "b:" with block "{c}" (whose "c" is a rule that runs into the
    // "}": a parse error, nothing); then " d" runs into the outer "}".
    let (r, e) = sheet("a{b:{c} d}");
    assert_eq!(r, json!([["qr", "a", [], [["qr", "b:", [], []]]]]));
    assert_eq!(e, json!([["CloseCurlyInRule", 6], ["CloseCurlyInRule", 9]]));
    // A custom property takes anything, and keeps its original text.
    let src = "a{--x:{y} z /*c*/ ;}";
    let (r, _) = common::parse_json(Wtf8::from_str(src), Entry::Stylesheet);
    assert_eq!(
        readable(src, &r),
        json!([["qr", "a", [["d", "--x", "{y} z", false]], []]])
    );
    assert_eq!(r[0][2][0][4], json!([6, 11]), "original text {{y}} z");
}

#[test]
fn important_5_5_6_steps_6_and_7() {
    let d = |s: &str| {
        readable(
            s,
            &common::parse_json(Wtf8::from_str(s), Entry::Declaration).0,
        )
    };
    assert_eq!(d("a: b ! IMPORTANT  "), json!(["d", "a", "b", true]));
    assert_eq!(d("a: ! important"), json!(["d", "a", "", true]));
    assert_eq!(
        d("a: b !important x"),
        json!(["d", "a", "b !important x", false])
    );
    // "!" inside a block doesn't count: only top-level values do.
    assert_eq!(
        d("a: (!important)"),
        json!(["d", "a", "(!important)", false])
    );
    // The value ends at ";" (not consumed), so parse a declaration sees
    // more input.
    let (_, r) = optimal_css::parse_declaration(Wtf8::from_str("a:b;"), &mut AcceptAll);
    assert_eq!(r.err(), Some(SyntaxError::ExtraInput));
}

#[test]
fn at_rules_5_5_2() {
    let (r, e) = sheet("@media x{a{b:c}} @import y;");
    assert_eq!(
        r,
        json!([
            [
                "at",
                "media",
                " x",
                [["qr", "a", [["d", "b", "c", false]], []]]
            ],
            ["at", "import", " y", null]
        ])
    );
    assert_eq!(e, json!([]));
    // In a block, an at-rule ends a run of declarations.
    let (r, _) = sheet("@p{a:b;@x;c:d}");
    assert_eq!(
        r,
        json!([[
            "at",
            "p",
            "",
            [
                ["decls", [["d", "a", "b", false]]],
                ["at", "x", "", null],
                ["decls", [["d", "c", "d", false]]]
            ]
        ]])
    );
    // A nested at-rule without block ends at the block's "}".
    assert_eq!(
        sheet("a{@x y}").0,
        json!([["qr", "a", [], [["at", "x", " y", null]]]])
    );
}

#[test]
fn custom_property_look_alikes_5_5_3() {
    // At the top level "--foo:hover{...}" is dropped whole.
    let (r, _) = sheet("--foo:hover{color:red} b{}");
    assert_eq!(r, json!([["qr", "b", [], []]]));
    // Nested, a custom property is a declaration up to ";".
    let (r, _) = sheet("a{--foo:hover{x} y; z:w}");
    assert_eq!(
        r,
        json!([[
            "qr",
            "a",
            [["d", "--foo", "hover{x} y", false], ["d", "z", "w", false]],
            []
        ]])
    );
}

/// Rejects declarations named `x` and rules whose prelude starts with `bad`,
/// like the model's validity rules.
#[test]
fn invalid_rule_error_splits_declarations_5_5_5() {
    // "bad{}" is a valid-looking rule the context rejects: an invalid rule
    // error, which still ends the run of declarations, so "y:2" starts a
    // nested declarations rule.
    let (r, _) = common::parse_json(Wtf8::from_str("a{w:1; bad{} y:2}"), Entry::Stylesheet);
    let src = "a{w:1; bad{} y:2}";
    assert_eq!(
        readable(src, &r),
        json!([[
            "qr",
            "a",
            [["d", "w", "1", false]],
            [["nd", [["d", "y", "2", false]]]]
        ]])
    );
    // A rejected declaration is re-read as a rule: "x:1;" is a rule prelude
    // ended by ";", a parse error, and nothing.
    let src = "a{x:1; y:2}";
    let (r, e) = common::parse_json(Wtf8::from_str(src), Entry::Stylesheet);
    assert_eq!(
        readable(src, &r),
        json!([["qr", "a", [["d", "y", "2", false]], []]])
    );
    assert_eq!(e, json!([["SemicolonInRule", 5]]));
}

#[test]
fn unicode_range_descriptor_5_5_13() {
    let src = "@font-face{unicode-range:U+0-7F, u+4??}";
    let (p, rules) = optimal_css::parse_stylesheet(Wtf8::from_str(src), &mut AcceptAll);
    let Rule::At {
        block: Some(items), ..
    } = &p.rules[rules[0] as usize]
    else {
        panic!()
    };
    let optimal_css::BlockItem::Declarations(d) = &items[0] else {
        panic!()
    };
    let toks: Vec<_> = p
        .values(d[0].value.clone())
        .map(|v| v.token().clone())
        .collect();
    assert_eq!(
        toks,
        [
            Token::UnicodeRange {
                start: 0,
                end: 0x7F
            },
            Token::Comma,
            Token::Whitespace,
            Token::UnicodeRange {
                start: 0x400,
                end: 0x4FF
            }
        ]
    );
    // Any other property tokenizes normally.
    let (p, rules) = optimal_css::parse_stylesheet(Wtf8::from_str("a{b:U+1}"), &mut AcceptAll);
    let Rule::Qualified { declarations, .. } = &p.rules[rules[0] as usize] else {
        panic!()
    };
    assert!(matches!(
        p.values(declarations[0].value.clone())
            .next()
            .unwrap()
            .token(),
        Token::Ident(_)
    ));
}

#[test]
fn top_level_5_5_1() {
    // CDO and CDC are dropped between rules, but not inside a prelude.
    assert_eq!(sheet("<!-- a{} -->").0, json!([["qr", "a", [], []]]));
    assert_eq!(sheet("a<!--{}").0, json!([["qr", "a<!--", [], []]]));
    // A stray "}" is a parse error and part of the next rule's prelude.
    let (r, e) = sheet("} a{}");
    assert_eq!(r, json!([["qr", "} a", [], []]]));
    assert_eq!(e, json!([["CloseCurlyInRule", 0]]));
    // A prelude that hits the end of input: parse error, no rule.
    let (r, e) = sheet("a b");
    assert_eq!(r, json!([]));
    assert_eq!(e, json!([["EofInRule", 3]]));
    // An unclosed block still makes the rule.
    assert_eq!(
        sheet("a{b:c").0,
        json!([["qr", "a", [["d", "b", "c", false]], []]])
    );
}

#[test]
fn single_item_entry_points_5_3() {
    let r = |s: &str| optimal_css::parse_rule(Wtf8::from_str(s), &mut AcceptAll).1;
    assert_eq!(r("  ").err(), Some(SyntaxError::Empty));
    assert_eq!(r("a").err(), Some(SyntaxError::Invalid));
    assert_eq!(r("a{} b{}").err(), Some(SyntaxError::ExtraInput));
    assert!(r(" @x ; ").is_ok());
    let c = |s: &str| optimal_css::parse_component_value(Wtf8::from_str(s)).1;
    assert_eq!(c("").err(), Some(SyntaxError::Empty));
    assert_eq!(c(" f(a b) ").ok(), Some(1));
    assert_eq!(c("a b").err(), Some(SyntaxError::ExtraInput));
    // Comma-separated lists: one group per top-level comma.
    let (p, groups) =
        optimal_css::parse_comma_separated_component_values(Wtf8::from_str("a,(b,c), d"));
    assert_eq!(groups.len(), 3);
    assert_eq!(p.values(groups[1].clone()).count(), 1);
}

/// A validity hook sees each construct once it is complete, and what it
/// rejects is dropped.
#[test]
fn validity_hook_sees_complete_constructs() {
    struct NoImport(Vec<String>);
    impl Validity for NoImport {
        fn at_rule(&mut self, p: &Parsed<'_>, rule: &Rule, nested: bool) -> bool {
            let Rule::At { name, .. } = rule else {
                unreachable!()
            };
            self.0.push(format!(
                "{}{}",
                p.name(*name),
                if nested { " nested" } else { "" }
            ));
            p.name(*name) != "import"
        }
    }
    let mut v = NoImport(Vec::new());
    let (_, rules) = optimal_css::parse_stylesheet(Wtf8::from_str("@import a; @m{@n;}"), &mut v);
    assert_eq!(rules.len(), 1);
    assert_eq!(v.0, ["import", "n nested", "m"]);
}

/// Deep nesting is handled without recursion: no stack overflow, in
/// parsing or in dropping the result.
#[test]
fn deep_nesting_does_not_recurse() {
    for src in [
        "a{".repeat(200_000),
        "(".repeat(200_000),
        "@m{".repeat(200_000),
        "a:b{".repeat(200_000),
    ] {
        let (p, rules) = optimal_css::parse_stylesheet(Wtf8::from_str(&src), &mut AcceptAll);
        assert!(rules.len() <= 1);
        drop(p);
    }
}

#[test]
fn views_report_lengths_and_spans() {
    use optimal_css::parse_component_values;
    let (p, range) = parse_component_values(Wtf8::from_str(""));
    assert!(p.tokens.is_empty());
    assert_eq!(p.tokens.len(), 0);
    assert!(p.values(range).is_empty());

    // "a (b c) d": a, ws, block, ws, d.
    let src = "a (b /**/c) d";
    let (p, range) = parse_component_values(Wtf8::from_str(src));
    assert!(!p.tokens.is_empty());
    assert_eq!(p.tokens.len(), 9, "comments are not in the list");
    let values = p.values(range);
    assert!(!values.is_empty());
    let spans: Vec<&str> = values.map(|v| &src[v.span()]).collect();
    assert_eq!(spans, ["a", " ", "(b /**/c)", " ", "d"]);
    let block = { values }.nth(2).unwrap();
    let optimal_css::Kind::Block(_, inner) = block.kind() else {
        panic!("a block")
    };
    assert!(!inner.is_empty());
    let (p2, r2) = parse_component_values(Wtf8::from_str("()"));
    let optimal_css::Kind::Block(_, empty) = p2.values(r2).next().unwrap().kind() else {
        panic!("a block")
    };
    assert!(empty.is_empty());
}

#[test]
fn at_rule_ends_a_run_of_declarations_even_if_rejected_5_5_5() {
    // 5.5.5's <at-keyword-token> branch moves the declarations so far into
    // the rules before it consumes the at-rule, so a rejected "@bad" still
    // splits "w:1" from "y:2".
    let src = "a{w:1; @bad; y:2}";
    assert_eq!(
        sheet(src).0,
        json!([[
            "qr",
            "a",
            [["d", "w", "1", false]],
            [["nd", [["d", "y", "2", false]]]]
        ]])
    );
    // With nothing before it there is nothing to move, and the rejected
    // rule adds nothing, so "y:2" is the first run: the rule's own
    // declarations.
    assert_eq!(
        sheet("a{@bad; y:2}").0,
        json!([["qr", "a", [["d", "y", "2", false]], []]])
    );
}

#[test]
fn close_curly_in_top_level_at_rule_prelude_5_5_2() {
    // Not nested, a "}" is consumed into the prelude.
    let (r, e) = sheet("@m } x; b{}");
    assert_eq!(r, json!([["at", "m", " } x", null], ["qr", "b", [], []]]));
    assert_eq!(e, json!([]));
}

#[test]
fn declaration_edge_cases_5_5_6() {
    use optimal_css::{parse_declaration, parse_declaration_list};
    let decl = |s: &str| parse_declaration(Wtf8::from_str(s), &mut AcceptAll).1;
    // "--" is reserved, so not a custom property name: step 8's {}-block
    // rule applies to it, but not to "--x".
    assert_eq!(decl("--:{b} c").unwrap_err(), SyntaxError::Invalid);
    assert_eq!(decl("--x:{b} c").unwrap().original_text, Some(4..9));
    assert_eq!(decl("--:b").unwrap().original_text, None);
    // Two {}-blocks are "a {}-block and anything else".
    assert_eq!(decl("a:{x}{y}").unwrap_err(), SyntaxError::Invalid);
    assert_eq!(decl("a:{x} {y}").unwrap_err(), SyntaxError::Invalid);
    assert!(decl("a:{x}").is_ok());
    // An empty custom property value: its original text is empty, at
    // where the value would be.
    assert_eq!(decl("--x:").unwrap().original_text, Some(4..4));
    assert_eq!(decl("--x: ").unwrap().original_text, Some(5..5));
    let (_, list) = parse_declaration_list(Wtf8::from_str("--x:;a:b"), &mut AcceptAll);
    assert_eq!(list[0].original_text, Some(4..4));
    assert_eq!(list.len(), 2);
    // A bad declaration's remnants run to the ";": "b:c" is not read as a
    // declaration of its own.
    let (p, list) = parse_declaration_list(Wtf8::from_str("a b:c;d:e"), &mut AcceptAll);
    let names: Vec<&str> = list.iter().map(|d| p.name(d.name)).collect();
    assert_eq!(names, ["d"]);
    let (p, list) = parse_declaration_list(Wtf8::from_str("a;b:c"), &mut AcceptAll);
    let names: Vec<&str> = list.iter().map(|d| p.name(d.name)).collect();
    assert_eq!(names, ["b"]);
}

#[test]
fn custom_property_runs_to_the_closer_5_5_6() {
    // A custom property's value is consumed up to the block's "}".
    let src = "a{--x: y }";
    let (r, _) = common::parse_json(Wtf8::from_str(src), Entry::Stylesheet);
    assert_eq!(
        readable(src, &r),
        json!([["qr", "a", [["d", "--x", "y", false]], []]])
    );
    assert_eq!(r[0][2][0][4], json!([7, 8]));

    // Rejected by the context, it is re-read as a rule whose prelude looks
    // like a custom property (5.5.3): in a block, its remnants run to the
    // "}" and nothing is made; the "}" still closes "a".
    struct NoCustom;
    impl Validity for NoCustom {
        fn declaration(
            &mut self,
            p: &Parsed<'_>,
            d: &optimal_css::Declaration,
            _nested: bool,
        ) -> bool {
            !p.name(d.name).starts_with("--")
        }
    }
    let (p, rules) =
        optimal_css::parse_stylesheet(Wtf8::from_str("a{--v:b{c} d} e{}"), &mut NoCustom);
    assert_eq!(rules.len(), 2);
    let Rule::Qualified {
        declarations,
        children,
        ..
    } = &p.rules[rules[0] as usize]
    else {
        panic!("a qualified rule")
    };
    assert!(declarations.is_empty() && children.is_empty());
}
