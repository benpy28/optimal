//! Hand-worked cases, each with the spec rule it checks.

use optimal_selectors::*;

fn ser(src: &str) -> String {
    match parse_selector(src, &Namespaces::new()) {
        Ok(l) => l.to_string(),
        Err(e) => panic!("{src:?} should be valid, got {e:?}"),
    }
}

fn err(src: &str) -> SelectorError {
    match parse_selector(src, &Namespaces::new()) {
        Ok(l) => panic!("{src:?} should be invalid, serialized as {l}"),
        Err(e) => e,
    }
}

fn spec(src: &str) -> (u32, u32, u32) {
    let l = parse_selector(src, &Namespaces::new()).unwrap();
    assert_eq!(l.0.len(), 1);
    let s = l.0[0].specificity(Specificity::ZERO);
    (s.a, s.b, s.c)
}

#[test]
fn specificity_examples_of_section_15() {
    // The example list at the end of section 15.
    assert_eq!(spec("*"), (0, 0, 0));
    assert_eq!(spec("LI"), (0, 0, 1));
    assert_eq!(spec("UL LI"), (0, 0, 2));
    assert_eq!(spec("UL OL+LI"), (0, 0, 3));
    assert_eq!(spec("H1 + *[REL=up]"), (0, 1, 1));
    assert_eq!(spec("UL OL LI.red"), (0, 1, 3));
    assert_eq!(spec("LI.red.level"), (0, 2, 1));
    assert_eq!(spec("#x34y"), (1, 0, 0));
    assert_eq!(spec("#s12:not(FOO)"), (1, 0, 1));
    assert_eq!(spec(".foo :is(.bar, #baz)"), (1, 1, 0));
    // The examples of the evaluation-context pseudo-classes.
    assert_eq!(spec(":is(em, #foo)"), (1, 0, 0));
    assert_eq!(spec(".qux:where(em, #foo#bar#baz)"), (0, 1, 0));
    assert_eq!(spec(":nth-child(even of li, .item)"), (0, 2, 0));
    assert_eq!(spec(":not(em, strong#foo)"), (1, 0, 1));
}

#[test]
fn specificity_of_other_selectors() {
    // :has() takes its most specific argument (15).
    assert_eq!(spec(":has(> a, #b .c)"), (1, 1, 0));
    // A relative selector's specificity is its selector's.
    let rel = parse_relative_selector("> #a .b", &Namespaces::new()).unwrap();
    assert_eq!(
        rel[0].specificity(Specificity::ZERO),
        Specificity { a: 1, b: 1, c: 0 }
    );
    // :nth-child() without `of` is one pseudo-class.
    assert_eq!(spec(":nth-child(2n)"), (0, 1, 0));
    // Invalid items of a forgiving list count as zero (16.1).
    assert_eq!(spec(":is(.a, 123)"), (0, 1, 0));
    assert_eq!(spec(":is()"), (0, 0, 0));
    // Pseudo-elements count like type selectors; ::slotted() adds its
    // argument, :host() is a pseudo-class plus its argument (css-shadow-1).
    assert_eq!(spec("a::before"), (0, 0, 2));
    assert_eq!(spec("::slotted(.a)"), (0, 1, 1));
    assert_eq!(spec(":host(.a)"), (0, 2, 0));
    assert_eq!(spec(":host"), (0, 1, 0));
    assert_eq!(spec(":host-context(#a)"), (1, 1, 0));
    assert_eq!(spec("::part(a b)"), (0, 0, 1));
    // ::highlight(*) is zero; with a name, like a type (css-pseudo-4 3.1).
    assert_eq!(spec("::highlight(*)"), (0, 0, 0));
    assert_eq!(spec("::highlight(x)"), (0, 0, 1));
    // Pseudo-classes after a pseudo-element count too.
    assert_eq!(spec("::before:hover"), (0, 1, 1));
    // The universal selector and namespace prefixes add nothing.
    assert_eq!(spec("*|*"), (0, 0, 0));
    // :where() is zero whatever it holds.
    assert_eq!(spec(":where(#a#b)"), (0, 0, 0));
}

#[test]
fn nesting_selector_takes_the_parents_specificity() {
    let l = parse_selector("& .a, &&", &Namespaces::new()).unwrap();
    let parent = Specificity { a: 1, b: 0, c: 1 };
    assert_eq!(l.0[0].specificity(parent), Specificity { a: 1, b: 1, c: 1 });
    assert_eq!(l.0[1].specificity(parent), Specificity { a: 2, b: 0, c: 2 });
    // No parent rule: zero.
    assert_eq!(
        l.0[0].specificity(Specificity::ZERO),
        Specificity { a: 0, b: 1, c: 0 }
    );
}

#[test]
fn specificity_saturates() {
    // Section 15: values past the limit clamp instead of overflowing.
    let s = Specificity {
        a: u32::MAX,
        b: 0,
        c: 0,
    };
    let l = parse_selector("& #a", &Namespaces::new()).unwrap();
    assert_eq!(l.0[0].specificity(s).a, u32::MAX);
}

#[test]
fn serialization_of_combinators_and_lists() {
    // cssom-1 5.2: one space either side of a combinator; ", " between
    // selectors.
    assert_eq!(ser("a>b+c~d e"), "a > b + c ~ d e");
    assert_eq!(ser("a  \n\t b"), "a b");
    assert_eq!(ser("a , b,c"), "a, b, c");
    // Whitespace before and after the whole list is not part of it.
    assert_eq!(ser("  a  "), "a");
}

#[test]
fn universal_selector_is_dropped_only_beside_others() {
    // cssom-1 5.2 steps 1 and 2.
    assert_eq!(ser("*"), "*");
    assert_eq!(ser("*.a"), ".a");
    assert_eq!(ser("*::before"), "::before");
    assert_eq!(ser("*|*"), "*");
    assert_eq!(ser("*|*.a"), ".a");
}

#[test]
fn namespaces() {
    let mut ns = Namespaces::new();
    ns.declare("svg", "http://www.w3.org/2000/svg");
    ns.declare("x", "urn:x");
    let s = |src: &str| parse_selector(src, &ns).unwrap().to_string();
    // No default namespace: `*|` adds nothing, named prefixes are kept.
    assert_eq!(s("svg|a"), "svg|a");
    assert_eq!(s("svg|*.a"), "svg|*.a");
    assert_eq!(s("|a"), "|a");
    assert_eq!(s("|*"), "|*");
    assert_eq!(s("*|a"), "a");
    // Attributes: no prefix is no namespace, `|` too (cssom-1 5.2).
    assert_eq!(s("[svg|href]"), "[svg|href]");
    assert_eq!(s("[*|href]"), "[*|href]");
    assert_eq!(s("[|href]"), "[href]");
    // An undeclared prefix makes the selector invalid (3.9).
    assert_eq!(
        parse_selector("y|a", &ns),
        Err(SelectorError::UndeclaredPrefix)
    );
    assert_eq!(
        parse_selector("[y|a]", &ns),
        Err(SelectorError::UndeclaredPrefix)
    );
    // Prefixes are case-sensitive (css-namespaces-3 section 3).
    assert_eq!(
        parse_selector("SVG|a", &ns),
        Err(SelectorError::UndeclaredPrefix)
    );

    // With a default namespace: no prefix means the default, `*|` must be
    // kept to mean any namespace, and a prefix naming the default is left
    // out.
    ns.set_default("http://www.w3.org/2000/svg");
    let s = |src: &str| parse_selector(src, &ns).unwrap().to_string();
    assert_eq!(s("a"), "a");
    assert_eq!(s("svg|a"), "a");
    assert_eq!(s("*|a"), "*|a");
    assert_eq!(s("*|*"), "*|*");
    assert_eq!(s("*|*.a"), "*|*.a");
    assert_eq!(s("x|a"), "x|a");
    let l = parse_selector("a, *|a, |a", &ns).unwrap();
    let ns_of = |i: usize| match &l.0[i].first.compound[0] {
        Simple::Type { ns, .. } => ns.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        ns_of(0),
        Namespace::Url("http://www.w3.org/2000/svg".into())
    );
    assert_eq!(ns_of(1), Namespace::Any);
    assert_eq!(ns_of(2), Namespace::None);
    // The default namespace doesn't apply to attributes (6.4).
    let l = parse_selector("[a]", &ns).unwrap();
    let Simple::Attribute(a) = &l.0[0].first.compound[0] else {
        panic!()
    };
    assert_eq!(a.ns, Namespace::None);
    // A later declaration of a prefix replaces the earlier one.
    ns.declare("x", "urn:y");
    let l = parse_selector("x|a", &ns).unwrap();
    assert!(
        matches!(&l.0[0].first.compound[0], Simple::Type { ns: Namespace::Url(u), .. } if &**u == "urn:y")
    );
}

#[test]
fn attribute_selectors() {
    // 6.1 to 6.3. Values serialize as strings; whitespace inside the
    // brackets is allowed around the parts.
    assert_eq!(ser("[a]"), "[a]");
    assert_eq!(ser("[ a ]"), "[a]");
    assert_eq!(ser("[a=b]"), "[a=\"b\"]");
    assert_eq!(ser("[ a = 'b' i ]"), "[a=\"b\" i]");
    assert_eq!(ser("[a=\"b\"s]"), "[a=\"b\" s]");
    assert_eq!(ser("[a=b I]"), "[a=\"b\" i]");
    assert_eq!(
        ser("[a~=b][a|=b][a^=b][a$=b][a*=b]"),
        "[a~=\"b\"][a|=\"b\"][a^=\"b\"][a$=\"b\"][a*=\"b\"]"
    );
    // `|=` after a name is the dash matcher, not a namespace prefix.
    assert_eq!(ser("[a|=b]"), "[a|=\"b\"]");
    // No whitespace inside a matcher, nothing unknown after the value.
    assert_eq!(err("[a| =b]"), SelectorError::Syntax);
    assert_eq!(err("[a ~ = b]"), SelectorError::Syntax);
    assert_eq!(err("[a=b x]"), SelectorError::Syntax);
    assert_eq!(err("[a=b i i]"), SelectorError::Syntax);
    assert_eq!(err("[a=1]"), SelectorError::Syntax);
    assert_eq!(err("[a=]"), SelectorError::Syntax);
    assert_eq!(err("[]"), SelectorError::Syntax);
    assert_eq!(err("[1]"), SelectorError::Syntax);
    assert_eq!(err("[*]"), SelectorError::Syntax);
    assert_eq!(err("[a b]"), SelectorError::Syntax);
    // A matcher's two characters are adjacent (16, white-space rules).
    assert_eq!(err("[a~ b]"), SelectorError::Syntax);
    assert_eq!(err("[a^ b i]"), SelectorError::Syntax);
    // `*` is a namespace prefix only with its `|`.
    assert_eq!(err("[*.b]"), SelectorError::Syntax);
    assert_eq!(err("[*|=b]"), SelectorError::Syntax);
    assert_eq!(err("[|=b]"), SelectorError::Syntax);
}

#[test]
fn compound_selector_order() {
    // 3.1: a type selector comes first; css-nesting-1: `&` anywhere else.
    assert_eq!(ser("div&.a"), "div&.a");
    assert_eq!(ser(".a&"), ".a&");
    assert_eq!(err("&div"), SelectorError::Syntax);
    assert_eq!(ser(".a div"), ".a div");
    assert_eq!(err(".a*"), SelectorError::Syntax);
    assert_eq!(err("a*"), SelectorError::Syntax);
    assert_eq!(err("a|"), SelectorError::Syntax);
    // A hash must be an identifier to be an ID selector (16).
    assert_eq!(err("#1a"), SelectorError::Syntax);
    assert_eq!(ser("#-a"), "#-a");
    // No whitespace inside a compound or a class selector.
    assert_eq!(err(". a"), SelectorError::Syntax);
    assert_eq!(err(": hover"), SelectorError::Syntax);
    // Comments are not whitespace: they join, which here is an error.
    assert_eq!(err("a/**/b"), SelectorError::Syntax);
    assert_eq!(ser("a/**/ b"), "a b");
    assert_eq!(ser("a/**/.b"), "a.b");
}

#[test]
fn empty_and_dangling() {
    // 3.9: an empty selector is invalid, and so is a list with one.
    assert_eq!(err(""), SelectorError::Empty);
    assert_eq!(err("  "), SelectorError::Empty);
    assert_eq!(err("a,"), SelectorError::Empty);
    assert_eq!(err(",a"), SelectorError::Empty);
    assert_eq!(err("a,,b"), SelectorError::Empty);
    assert_eq!(err("a >"), SelectorError::Syntax);
    assert_eq!(err("> a"), SelectorError::Syntax);
    assert_eq!(err("a > > b"), SelectorError::Syntax);
    assert_eq!(err("a ! b"), SelectorError::Syntax);
    assert_eq!(err("a{}"), SelectorError::Syntax);
    assert_eq!(err("a;"), SelectorError::Syntax);
}

#[test]
fn pseudo_class_names() {
    // 3.5: ASCII case-insensitive, serialized in their own case.
    assert_eq!(ser(":HOVER"), ":hover");
    assert_eq!(ser(":Nth-Child(ODD)"), ":nth-child(2n+1)");
    assert_eq!(ser(":NOT(A)"), ":not(A)");
    // Appendix B: :-webkit-autofill is a legacy alias of :autofill.
    assert_eq!(ser(":-WebKit-Autofill"), ":autofill");
    // Unknown, or with the wrong form.
    assert_eq!(err(":unknown"), SelectorError::UnknownPseudoClass);
    assert_eq!(err(":hover()"), SelectorError::UnknownPseudoClass);
    assert_eq!(err(":not"), SelectorError::UnknownPseudoClass);
    assert_eq!(err(":matches(a)"), SelectorError::UnknownPseudoClass);
    // Every keyword round-trips.
    for k in PseudoClassKeyword::ALL {
        assert_eq!(
            ser(&format!(":{}", k.name().to_uppercase())),
            format!(":{}", k.name())
        );
    }
}

#[test]
fn functional_pseudo_classes() {
    assert_eq!(
        ser(":nth-child( 2n + 1 of .a , b )"),
        ":nth-child(2n+1 of .a, b)"
    );
    assert_eq!(ser(":nth-last-child(-n+3)"), ":nth-last-child(-n+3)");
    assert_eq!(ser(":nth-of-type(even)"), ":nth-of-type(2n)");
    assert_eq!(ser(":nth-last-of-type(5)"), ":nth-last-of-type(5)");
    assert_eq!(err(":nth-of-type(2n of a)"), SelectorError::Syntax);
    assert_eq!(err(":nth-child(2n of)"), SelectorError::Empty);
    assert_eq!(err(":nth-child(2n or a)"), SelectorError::Syntax);
    assert_eq!(err(":nth-child(of a)"), SelectorError::Syntax);
    // `of S` is unforgiving and takes no pseudo-elements.
    assert_eq!(err(":nth-child(2 of a, 1)"), SelectorError::Syntax);
    assert_eq!(
        err(":nth-child(2 of ::before)"),
        SelectorError::NotAllowedHere
    );
    // :lang(): identifiers or strings, comma-separated (7.2).
    assert_eq!(
        ser(":lang(en, \"*-Latn\", \\*-x)"),
        ":lang(\"en\", \"*-Latn\", \"*-x\")"
    );
    assert_eq!(err(":lang()"), SelectorError::Syntax);
    assert_eq!(err(":lang(en,)"), SelectorError::Syntax);
    assert_eq!(err(":lang(en fr)"), SelectorError::Syntax);
    assert_eq!(err(":lang(1)"), SelectorError::Syntax);
    // :dir(): one identifier; unknown ones are valid (7.1).
    assert_eq!(ser(":dir( rtl )"), ":dir(rtl)");
    assert_eq!(ser(":dir(up)"), ":dir(up)");
    assert_eq!(err(":dir(\"ltr\")"), SelectorError::Syntax);
    assert_eq!(err(":dir(ltr rtl)"), SelectorError::Syntax);
    assert_eq!(err(":dir()"), SelectorError::Syntax);
    // :host() and :host-context() take one compound selector.
    assert_eq!(ser(":host( .a:hover )"), ":host(.a:hover)");
    assert_eq!(ser(":host(*)"), ":host(*)");
    assert_eq!(err(":host(a b)"), SelectorError::NotAllowedHere);
    assert_eq!(err(":host(a > b)"), SelectorError::NotAllowedHere);
    assert_eq!(err(":host(a, b)"), SelectorError::Syntax);
    assert_eq!(err(":host(::before)"), SelectorError::NotAllowedHere);
    assert_eq!(err(":host()"), SelectorError::Empty);
    assert_eq!(ser(":host-context(.dark)"), ":host-context(.dark)");
}

#[test]
fn logical_combinations() {
    // 4.2: :is() is forgiving, keeps invalid items for serialization only.
    let l = parse_selector(":is(.a, 1, ::before, .b)", &Namespaces::new()).unwrap();
    let Simple::PseudoClass(PseudoClass::Is(f)) = &l.0[0].first.compound[0] else {
        panic!()
    };
    assert!(
        matches!(&f.0[..], [ForgivingItem::Valid(_), ForgivingItem::Invalid(a), ForgivingItem::Invalid(b), ForgivingItem::Valid(_)] if &**a == "1" && &**b == "::before")
    );
    assert_eq!(l.to_string(), ":is(.a, 1, ::before, .b)");
    // But <any-value> excludes bad tokens and unmatched closers (16.1).
    assert_eq!(err(":is(a ])"), SelectorError::Syntax);
    assert_eq!(err(":is(a })"), SelectorError::Syntax);
    assert_eq!(err(":is((a ]))"), SelectorError::Syntax);
    assert_eq!(err(":is(\"a\n)"), SelectorError::Syntax);
    assert_eq!(err(":where(url(a b))"), SelectorError::Syntax);
    assert_eq!(ser(":is([a], (b))"), ":is([a], (b))");
    // :not() is not forgiving (4.3).
    assert_eq!(err(":not(a, 1)"), SelectorError::Syntax);
    assert_eq!(err(":not()"), SelectorError::Empty);
    // :has(): relative selectors, no nesting, no pseudo-elements (4.5).
    assert_eq!(ser(":has(>a,+ b, ~c, d)"), ":has(> a, + b, ~ c, d)");
    assert_eq!(err(":has(:has(a))"), SelectorError::NotAllowedHere);
    assert_eq!(err(":has(:not(:has(a)))"), SelectorError::NotAllowedHere);
    assert_eq!(err(":has(::before)"), SelectorError::NotAllowedHere);
    assert_eq!(err(":has(> )"), SelectorError::Empty);
    assert_eq!(err(":has(> > a)"), SelectorError::Syntax);
    // :has() inside :is() inside :has() is dropped, not an error.
    assert_eq!(ser(":has(:is(:has(a), b))"), ":has(:is(:has(a), b))");
    // :has() in ::slotted()'s compound is fine (only the compound's own
    // shape is restricted).
    assert_eq!(ser("::slotted(:has(a b))"), "::slotted(:has(a b))");
}

#[test]
fn pseudo_elements_and_what_may_follow_them() {
    // 3.6.1: names are case-insensitive; four have a one-colon form.
    assert_eq!(ser("a::BEFORE"), "a::before");
    assert_eq!(ser("a:before"), "a::before");
    assert_eq!(ser("a:First-Line"), "a::first-line");
    assert_eq!(err("a:marker"), SelectorError::UnknownPseudoClass);
    assert_eq!(err("::unknown"), SelectorError::UnknownPseudoElement);
    assert_eq!(err("::before()"), SelectorError::UnknownPseudoElement);
    assert_eq!(err(": :before"), SelectorError::Syntax);
    assert_eq!(err(":: before"), SelectorError::Syntax);
    for k in PseudoElementKeyword::ALL {
        if !matches!(
            k,
            PseudoElementKeyword::Prefix | PseudoElementKeyword::Suffix
        ) {
            assert_eq!(ser(&format!("::{}", k.name())), format!("::{}", k.name()));
        }
    }
    // 3.6.3: after a pseudo-element, user action and logical combination
    // pseudo-classes only (or anything, after an element-backed one).
    assert_eq!(
        ser("::before:hover:focus-visible"),
        "::before:hover:focus-visible"
    );
    assert_eq!(ser("::before:is(:hover, .a)"), "::before:is(:hover, .a)");
    assert_eq!(ser("::before:not(:focus)"), "::before:not(:focus)");
    assert_eq!(err("::before:not(.a)"), SelectorError::NotAllowedHere);
    assert_eq!(
        err("::before:not(:hover .a)"),
        SelectorError::NotAllowedHere
    );
    // The restriction passes as compound-only too: no combinators even
    // between allowed pseudo-classes.
    assert_eq!(
        err("::before:not(:hover :focus)"),
        SelectorError::NotAllowedHere
    );
    assert_eq!(
        err("::part(a):not(:hover > :focus)"),
        SelectorError::NotAllowedHere
    );
    assert_eq!(err("::before:first-child"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before:has(a)"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before.a"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before[a]"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before&"), SelectorError::NotAllowedHere);
    assert_eq!(
        ser("::part(a):first-child:state(x)"),
        "::part(a):first-child:state(x)"
    );
    assert_eq!(err("::part(a).b"), SelectorError::NotAllowedHere);
    // An argument that isn't a logical combination's starts afresh, even
    // after a pseudo-element.
    assert_eq!(ser("::part(a):host(.b)"), "::part(a):host(.b)");
    assert_eq!(
        ser("::part(a):nth-child(2 of .b c)"),
        "::part(a):nth-child(2 of .b c)"
    );
    assert_eq!(
        ser("::file-selector-button:disabled"),
        "::file-selector-button:disabled"
    );
    // 3.6.4: sub-pseudo-elements only where defined.
    assert_eq!(ser("::before::marker"), "::before::marker");
    assert_eq!(ser("::after::marker:hover"), "::after::marker:hover");
    assert_eq!(
        ser("::first-letter::prefix, ::first-letter::suffix"),
        "::first-letter::prefix, ::first-letter::suffix"
    );
    assert_eq!(err("::prefix"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before::prefix"), SelectorError::NotAllowedHere);
    assert_eq!(err("::marker::marker"), SelectorError::NotAllowedHere);
    assert_eq!(err("::before::before"), SelectorError::NotAllowedHere);
    assert_eq!(
        err("::before:hover::marker:hover::marker"),
        SelectorError::NotAllowedHere
    );
    assert_eq!(
        ser("::part(a)::part(b)::before"),
        "::part(a)::part(b)::before"
    );
    assert_eq!(ser("::slotted(a)::before"), "::slotted(a)::before");
    assert_eq!(ser("::slotted(a)::part(b)"), "::slotted(a)::part(b)");
    assert_eq!(
        err("::slotted(a)::first-line"),
        SelectorError::NotAllowedHere
    );
    assert_eq!(
        err("::slotted(a)::selection"),
        SelectorError::NotAllowedHere
    );
    // 3.6.5: no combinators after a pseudo-element.
    assert_eq!(err("::before a"), SelectorError::NotAllowedHere);
    assert_eq!(err("a::before > b"), SelectorError::NotAllowedHere);
    // Not inside :is(), :not(), :has(), `of S` (4.2, 4.3, 4.5).
    assert_eq!(err(":not(::before)"), SelectorError::NotAllowedHere);
    assert_eq!(ser(":is(::before)"), ":is(::before)");
    // Pseudo-element arguments.
    assert_eq!(ser("::part( a  b )"), "::part(a b)");
    assert_eq!(err("::part()"), SelectorError::Syntax);
    assert_eq!(err("::part(a, b)"), SelectorError::Syntax);
    assert_eq!(err("::part(\"a\")"), SelectorError::Syntax);
    assert_eq!(ser("::slotted( .a )"), "::slotted(.a)");
    assert_eq!(err("::slotted(a b)"), SelectorError::NotAllowedHere);
    assert_eq!(ser("::highlight( x )"), "::highlight(x)");
    assert_eq!(ser("::highlight(*)"), "::highlight(*)");
    for kw in [
        "default",
        "initial",
        "INHERIT",
        "unset",
        "revert",
        "revert-layer",
    ] {
        assert_eq!(err(&format!("::highlight({kw})")), SelectorError::Syntax);
    }
    assert_eq!(err("::highlight(a b)"), SelectorError::Syntax);
    assert_eq!(err("::highlight()"), SelectorError::Syntax);
}

#[test]
fn unknown_webkit_pseudo_elements() {
    // Appendix B: valid at parse time, serialized ASCII-lowercased; not
    // the functional form.
    assert_eq!(ser("a::-WebKit-Thing"), "a::-webkit-thing");
    assert_eq!(
        err("::-webkit-thing()"),
        SelectorError::UnknownPseudoElement
    );
    assert_eq!(err("::-moz-thing"), SelectorError::UnknownPseudoElement);
    assert_eq!(err(":-webkit-thing"), SelectorError::UnknownPseudoClass);
}

#[test]
fn identifier_and_string_serialization() {
    // cssom-1 2.1, "serialize an identifier".
    assert_eq!(ser(".\\31 23"), ".\\31 23");
    assert_eq!(ser(".-\\31"), ".-\\31 ");
    assert_eq!(ser(".\\-"), ".\\-");
    assert_eq!(ser(".--"), ".--");
    assert_eq!(ser(".a\\ b"), ".a\\ b");
    assert_eq!(ser(".a\\.b"), ".a\\.b");
    assert_eq!(ser(".\\7f x"), ".\\7f x");
    assert_eq!(ser(".\\0"), ".\u{FFFD}");
    assert_eq!(ser(".é_-"), ".é_-");
    assert_eq!(ser("#\\#"), "#\\#");
    // "serialize a string".
    assert_eq!(ser("[a='\"\\\\']"), "[a=\"\\\"\\\\\"]");
    assert_eq!(ser("[a='\\1 ']"), "[a=\"\\1 \"]");
    assert_eq!(ser("[a=\"'\"]"), "[a=\"'\"]");
}

#[test]
fn relative_selectors() {
    let ns = Namespaces::new();
    let l = parse_relative_selector("> a, b, + c ~ d", &ns).unwrap();
    let s: Vec<String> = l.iter().map(ToString::to_string).collect();
    assert_eq!(s, ["> a", "b", "+ c ~ d"]);
    assert_eq!(l[1].combinator, Combinator::Descendant);
    assert!(!l[1].explicit);
    assert_eq!(parse_relative_selector(">", &ns), Err(SelectorError::Empty));
    // Pseudo-elements are fine in a relative selector of a nested rule.
    assert_eq!(
        parse_relative_selector("> ::before", &ns).unwrap()[0].to_string(),
        "> ::before"
    );
}

#[test]
fn unclosed_functions_and_blocks_at_the_end() {
    // css-syntax-3 5.5: the input's end closes them.
    assert_eq!(ser(":is(a"), ":is(a)");
    assert_eq!(ser("[a=b"), "[a=\"b\"]");
    assert_eq!(ser(":nth-child(2n"), ":nth-child(2n)");
}

#[test]
fn nesting_depth_limit() {
    let deep = |n: u32| format!("{}a{}", ":is(".repeat(n as usize), ")".repeat(n as usize));
    assert!(parse_selector(&deep(MAX_NESTING), &Namespaces::new()).is_ok());
    // One more is refused, not forgiven, by :is().
    assert_eq!(
        parse_selector(&deep(MAX_NESTING + 1), &Namespaces::new()),
        Err(SelectorError::TooDeep)
    );
    let not = format!("{}a{}", ":not(".repeat(300), ")".repeat(300));
    assert_eq!(
        parse_selector(&not, &Namespaces::new()),
        Err(SelectorError::TooDeep)
    );
    let slotted = format!("::slotted({}a{})", ":host(".repeat(300), ")".repeat(300));
    assert_eq!(
        parse_selector(&slotted, &Namespaces::new()),
        Err(SelectorError::TooDeep)
    );
    // Deep blocks inside a forgiving argument are walked without recursion,
    // and the kept item gets the closers the end of input left out.
    let blocks = format!(":is({}", "[".repeat(200_000));
    assert_eq!(
        ser(&blocks),
        format!(":is({}{})", "[".repeat(200_000), "]".repeat(200_000))
    );
    assert_eq!(ser(":is(a, b(c [d"), ":is(a, b(c [d]))");
    assert_eq!(ser(":is(a, (b) c"), ":is(a, (b) c)");
}
