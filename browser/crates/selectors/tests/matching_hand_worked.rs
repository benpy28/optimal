//! Hand-worked matching cases, each answer worked out from the spec text
//! cited beside it.

mod common;

use common::tree::{El, HTML, Kind, Tree, XML};
use optimal_selectors::{
    MatchContext, Namespaces, Scope, lang_matches, language_tag_well_formed, matches,
    parse_selector, query_all,
};

fn sel(s: &str) -> optimal_selectors::SelectorList {
    let mut ns = Namespaces::new();
    ns.declare("x", "urn:x");
    ns.declare("h", HTML);
    parse_selector(s, &ns).unwrap_or_else(|e| panic!("{s}: {e:?}"))
}

/// IDs of the elements `s` matches under `cx`, in tree order.
fn ids(t: &Tree, s: &str, cx: &MatchContext<'_, El<'_>>) -> Vec<String> {
    let list = sel(s);
    t.subtree(0)
        .into_iter()
        .filter(|&n| t.is_element(n) && matches(&list, &El::new(t, n), cx))
        .map(|n| t.attr(n, "", "id").unwrap_or("?").to_string())
        .collect()
}

fn ids_plain(t: &Tree, s: &str) -> Vec<String> {
    ids(t, s, &MatchContext::new())
}

/// html#r > (body#b > (p#p1, p#p2 > span#s1), svg|g#g)
fn sample(html: bool) -> Tree {
    let mut t = Tree::new(html);
    let r = t.element(HTML, "html", &[("id", "r"), ("lang", "en-GB")]);
    t.append(0, r);
    let b = t.element(HTML, "body", &[("id", "b")]);
    t.append(r, b);
    let p1 = t.element(HTML, "p", &[("id", "p1"), ("class", "A b")]);
    t.append(b, p1);
    let p2 = t.element(
        HTML,
        "p",
        &[("id", "p2"), ("type", "Text"), ("lang", "fr-CA")],
    );
    t.append(b, p2);
    let s1 = t.element(HTML, "span", &[("id", "s1"), ("data-X", "v")]);
    t.append(p2, s1);
    let g = t.element(
        "http://www.w3.org/2000/svg",
        "foreignObject",
        &[("id", "g"), ("viewBox", "0"), ("type", "Text")],
    );
    t.append(r, g);
    t
}

#[test]
fn html_case_rules() {
    // HTML 4.16.2: type selectors are lowercased for HTML elements in
    // HTML documents, compared as written otherwise.
    let t = sample(true);
    assert_eq!(ids_plain(&t, "P"), ["p1", "p2"]);
    assert_eq!(ids_plain(&t, "foreignObject"), ["g"]);
    assert!(ids_plain(&t, "foreignobject").is_empty());
    assert!(ids_plain(&t, "FOREIGNOBJECT").is_empty());
    // Attribute names likewise: `data-X` was set with an uppercase letter
    // (as setAttributeNS can), so the lowercased `[data-X]` never finds it
    // on an HTML element; on the SVG element, names compare as written.
    assert!(ids_plain(&t, "[data-X]").is_empty());
    assert_eq!(ids_plain(&t, "[viewBox]"), ["g"]);
    assert!(ids_plain(&t, "[viewbox]").is_empty());
    // Listed attribute values are case-insensitive on HTML elements only,
    // and `s` turns that off.
    assert_eq!(ids_plain(&t, "[type=text]"), ["p2"]);
    assert!(ids_plain(&t, "[type=text s]").is_empty());
    assert_eq!(ids_plain(&t, "[type=text i]"), ["p2", "g"]);
    // `lang` is listed too; `id` and `class` are not.
    assert_eq!(ids_plain(&t, "[lang|=FR]"), ["p2"]);
    assert!(ids_plain(&t, "[id=P1]").is_empty());
    assert!(ids_plain(&t, "[class~=a]").is_empty());
    // In an XML document none of this applies.
    let x = sample(false);
    assert!(ids_plain(&x, "P").is_empty());
    assert_eq!(ids_plain(&x, "p"), ["p1", "p2"]);
    assert!(ids_plain(&x, "[type=text]").is_empty());
}

#[test]
fn ids_and_classes_in_quirks_mode() {
    // selectors-4 6.6, 6.7: ASCII case-insensitive in quirks mode only.
    let t = sample(true);
    let quirks = MatchContext {
        quirks: true,
        ..MatchContext::new()
    };
    assert!(ids_plain(&t, ".a").is_empty());
    assert_eq!(ids(&t, ".a", &quirks), ["p1"]);
    assert_eq!(ids(&t, "#P1", &quirks), ["p1"]);
    assert!(ids_plain(&t, "#P1").is_empty());
    // HTML: attribute selectors on id and class stay case-sensitive.
    assert!(ids(&t, "[class~=a]", &quirks).is_empty());
}

#[test]
fn attribute_value_matchers() {
    // selectors-4 6.1, 6.2: empty values and values with white space
    // never match ~=; empty values never match ^= $= *=.
    let mut t = Tree::new(true);
    let r = t.element(
        HTML,
        "html",
        &[("id", "r"), ("title", "a-b c\td"), ("data-e", "")],
    );
    t.append(0, r);
    for (s, hit) in [
        ("[title~=c]", true),
        ("[title~=d]", true),
        ("[title~='c d']", false),
        ("[title~='']", false),
        ("[title|=a]", true),
        ("[title|=a-b]", false),
        ("[title|='a-b c\td']", true),
        ("[title^='']", false),
        ("[title$='']", false),
        ("[title*='']", false),
        ("[title^=a-]", true),
        ("[title$='\td']", true),
        ("[title*='b c']", true),
        ("[data-e='']", true),
        ("[data-e]", true),
        ("[data-e^='']", false),
        ("[title=A-B\\ c\\9 d i]", true),
    ] {
        assert_eq!(!ids_plain(&t, s).is_empty(), hit, "{s}");
    }
}

#[test]
fn namespaces() {
    let mut t = Tree::new(true);
    let r = t.element(HTML, "html", &[("id", "r")]);
    t.append(0, r);
    let a = t.element("urn:x", "a", &[("id", "xa")]);
    t.append(r, a);
    t.set_attribute(a, "urn:x", "k", "1");
    let b = t.element("", "a", &[("id", "na"), ("k", "2")]);
    t.append(r, b);
    assert_eq!(ids_plain(&t, "x|a"), ["xa"]);
    assert_eq!(ids_plain(&t, "|a"), ["na"]);
    assert_eq!(ids_plain(&t, "*|a"), ["xa", "na"]);
    assert_eq!(ids_plain(&t, "a"), ["xa", "na"]);
    assert_eq!(ids_plain(&t, "h|*"), ["r"]);
    // An attribute selector with no prefix wants no namespace (6.4).
    assert_eq!(ids_plain(&t, "[k]"), ["na"]);
    assert_eq!(ids_plain(&t, "[x|k]"), ["xa"]);
    assert_eq!(ids_plain(&t, "[*|k]"), ["xa", "na"]);
    assert_eq!(ids_plain(&t, "[|k='2']"), ["na"]);
    // With a default namespace, type and universal selectors want it.
    let mut ns = Namespaces::new();
    ns.set_default("urn:x");
    let list = parse_selector("a", &ns).unwrap();
    let hits: Vec<usize> = (0..t.nodes.len())
        .filter(|&n| t.is_element(n) && matches(&list, &El::new(&t, n), &MatchContext::new()))
        .collect();
    assert_eq!(hits, [a]);
}

#[test]
fn structural_pseudo_classes() {
    let mut t = Tree::new(true);
    let r = t.element(HTML, "html", &[("id", "r")]);
    t.append(0, r);
    // r > (c1 "", c2 comment, c3 " \t\n\r", c4 "\u{c}", c5 nbsp, c6 > text "x")
    let texts: [Option<&str>; 6] = [
        Some(""),
        None,
        Some(" \t\n\r"),
        Some("\u{c}"),
        Some("\u{a0}"),
        Some("x"),
    ];
    for (k, text) in texts.into_iter().enumerate() {
        let e = t.element(
            HTML,
            if k % 2 == 0 { "p" } else { "q" },
            &[("id", &format!("c{}", k + 1))],
        );
        t.append(r, e);
        let n = match text {
            Some(s) => t.create(Kind::Text(s.into())),
            None => t.create(Kind::Comment),
        };
        t.append(e, n);
    }
    // 13.2: only document white space (space, tab, segment breaks) may
    // remain; form feed and no-break space are not document white space.
    assert_eq!(ids_plain(&t, ":empty"), ["c1", "c2", "c3"]);
    assert_eq!(ids_plain(&t, ":root"), ["r"]);
    assert_eq!(ids_plain(&t, ":first-child"), ["r", "c1"]);
    assert_eq!(ids_plain(&t, ":last-child"), ["r", "c6"]);
    assert_eq!(ids_plain(&t, ":only-child"), ["r"]);
    assert_eq!(
        ids_plain(&t, "q:first-of-type, p:last-of-type"),
        ["c2", "c5"]
    );
    assert_eq!(ids_plain(&t, ":nth-child(2n+3)"), ["c3", "c5"]);
    assert_eq!(ids_plain(&t, ":nth-last-child(-n+2)"), ["r", "c5", "c6"]);
    assert_eq!(
        ids_plain(&t, "p:nth-of-type(2), q:nth-last-of-type(3)"),
        ["c2", "c3"]
    );
    assert_eq!(ids_plain(&t, ":only-of-type"), ["r"]);
    // `of S` counts only the siblings matching S, and needs S itself
    // (13.3.1): c2, c4, c6 are the q siblings.
    assert_eq!(ids_plain(&t, ":nth-child(2 of q)"), ["c4"]);
    // :has() with sibling combinators: `~` looks past the next sibling,
    // `+` does not.
    assert_eq!(ids_plain(&t, ":has(~ #c3)"), ["c1", "c2"]);
    assert_eq!(ids_plain(&t, ":has(+ #c3)"), ["c2"]);
    assert_eq!(ids_plain(&t, ":nth-last-child(1 of p)"), ["c5"]);
    assert_eq!(
        ids_plain(&t, ":nth-child(odd of :not(:empty))"),
        ["r", "c4", "c6"]
    );
    // The root counts as a first, last and only child (13.3).
    assert_eq!(
        ids_plain(
            &t,
            ":root:first-child:last-child:nth-child(1):nth-last-of-type(1)"
        ),
        ["r"]
    );
}

#[test]
fn logical_combinations() {
    let t = sample(true);
    assert_eq!(ids_plain(&t, ":is(#p1, span, nope)"), ["p1", "s1"]);
    assert_eq!(ids_plain(&t, ":where(body > p)"), ["p1", "p2"]);
    assert_eq!(ids_plain(&t, "p:not(.A)"), ["p2"]);
    assert_eq!(ids_plain(&t, ":not(html, body, p, span)"), ["g"]);
    // Invalid items of a forgiving list are dropped, not fatal (16.1).
    assert_eq!(ids_plain(&t, ":is(:unknown, #s1, ::before)"), ["s1"]);
    // :is() puts a whole complex selector at the compound's position.
    assert_eq!(ids_plain(&t, "span:is(body > p *)"), ["s1"]);
    assert_eq!(ids_plain(&t, ":is(html > body) > p"), ["p1", "p2"]);
}

#[test]
fn has() {
    let t = sample(true);
    // 4.5: relative selectors anchored at the element.
    assert_eq!(ids_plain(&t, ":has(span)"), ["r", "b", "p2"]);
    assert_eq!(ids_plain(&t, ":has(> span)"), ["p2"]);
    assert_eq!(ids_plain(&t, ":has(+ #p2)"), ["p1"]);
    assert_eq!(ids_plain(&t, ":has(~ foreignObject)"), ["b"]);
    assert_eq!(ids_plain(&t, ":has(+ p > span)"), ["p1"]);
    assert_eq!(ids_plain(&t, ":has(> p span)"), ["b"]);
    assert_eq!(ids_plain(&t, ":has(> p + p)"), ["b"]);
    assert_eq!(
        ids_plain(&t, ":has(~ * foreignObject)"),
        Vec::<String>::new()
    );
    // The anchor is not a descendant of itself.
    assert_eq!(ids_plain(&t, "p:has(p)"), Vec::<String>::new());
    // Inside :is() and :not().
    assert_eq!(ids_plain(&t, "p:not(:has(*))"), ["p1"]);
    assert_eq!(ids_plain(&t, ":is(:has(> #p1), #s1)"), ["b", "s1"]);
    // The first compound must be reached from the anchor by the leading
    // combinator: span's parent p2 is a child of body, not of html.
    assert_eq!(ids_plain(&t, ":has(> p > span)"), ["b"]);
    assert_eq!(ids_plain(&t, ":has(> body span)"), ["r"]);
    // ...and for the descendant combinator too: body is no descendant of p2.
    assert!(ids_plain(&t, "p:has(body span)").is_empty());
}

#[test]
fn lang() {
    let t = sample(true);
    // 7.2: implicit wildcard matching on later subtags; content language
    // from the nearest lang attribute (HTML).
    assert_eq!(ids_plain(&t, ":lang(en)"), ["r", "b", "p1", "g"]);
    assert_eq!(ids_plain(&t, ":lang(fr)"), ["p2", "s1"]);
    assert_eq!(ids_plain(&t, ":lang(\\*-CA)"), ["p2", "s1"]);
    assert_eq!(ids_plain(&t, ":lang(EN-gb)"), ["r", "b", "p1", "g"]);
    assert_eq!(ids_plain(&t, ":lang(de, fr-CA)"), ["p2", "s1"]);
    assert!(ids_plain(&t, ":lang(\"\")").is_empty());
    let mut x = Tree::new(true);
    let r = x.element(HTML, "html", &[("id", "r")]);
    x.append(0, r);
    let a = x.element(HTML, "a", &[("id", "a"), ("lang", "")]);
    x.append(r, a);
    let b = x.element(HTML, "b", &[("id", "b"), ("lang", "und")]);
    x.append(r, b);
    let c = x.element("urn:x", "c", &[("id", "c"), ("lang", "de")]);
    x.append(r, c);
    x.set_attribute(c, XML, "lang", "nl");
    let d = x.element("urn:x", "d", &[("id", "d"), ("lang", "de")]);
    x.append(r, d);
    // `*` matches tagged elements (und included), "" only untagged ones;
    // xml:lang wins, and lang counts only on HTML and SVG elements.
    assert_eq!(ids_plain(&x, ":lang('*')"), ["b", "c"]);
    assert_eq!(ids_plain(&x, ":lang('')"), ["r", "a", "d"]);
    assert_eq!(ids_plain(&x, ":lang(nl)"), ["c"]);
}

#[test]
fn language_ranges() {
    // RFC 4647 3.3.2's own examples: de-*-DE (here as written by
    // selectors-4, which only allows an initial wildcard, so de-DE).
    for tag in [
        "de-DE",
        "de-de",
        "de-Latn-DE",
        "de-Latf-DE",
        "de-DE-x-goethe",
        "de-Latn-DE-1996",
        "de-Deva-DE",
    ] {
        assert!(lang_matches(tag.as_bytes(), b"de-DE"), "{tag}");
    }
    for tag in ["de", "de-x-DE", "de-Deva"] {
        assert!(!lang_matches(tag.as_bytes(), b"de-DE"), "{tag}");
    }
    assert!(lang_matches(b"de-CH", b"*-CH"));
    assert!(lang_matches(b"it-CH", b"*-ch"));
    assert!(!lang_matches(b"de", b"*-CH"));
    // selectors-4 7.2: a wildcard after the first subtag makes the range
    // ill-formed; ill-formed ranges and tags match nothing.
    assert!(!lang_matches(b"de-DE", b"de-*-DE"));
    assert!(!lang_matches(b"en_US", b"en"));
    assert!(!lang_matches("åå".as_bytes(), "åå".as_bytes()));
    assert!(lang_matches(b"qq", b"qq"));
    // A singleton ends the search for a later range subtag.
    assert!(!lang_matches(b"en-a-bbb-US", b"en-US"));
    assert!(lang_matches(b"en-US-a-bbb", b"en-US"));
    // Grandfathered tags are well-formed.
    assert!(lang_matches(b"i-klingon", b"i-klingon"));
    assert!(lang_matches(b"zh-min-nan", b"zh"));
    // An ill-formed tag is not "tagged" for `*` either.
    assert!(!lang_matches(b"en_US", b"*"));
    assert!(lang_matches(b"en-US", b"*"));
}

#[test]
fn well_formed_tags() {
    // RFC 5646 2.1 and its appendix A examples.
    for t in [
        "de",
        "fr",
        "ja",
        "i-enochian",
        "zh-Hant",
        "zh-Hans",
        "sr-Cyrl",
        "sr-Latn",
        "zh-cmn-Hans-CN",
        "cmn-Hans-CN",
        "zh-yue-HK",
        "yue-HK",
        "zh-Hans-CN",
        "sr-Latn-RS",
        "sl-rozaj",
        "sl-rozaj-biske",
        "sl-nedis",
        "de-CH-1901",
        "sl-IT-nedis",
        "hy-Latn-IT-arevela",
        "de-DE",
        "en-US",
        "es-419",
        "de-CH-x-phonebk",
        "az-Arab-x-AZE-derbend",
        "x-whatever",
        "qaa-Qaaa-QM-x-southern",
        "de-Qaaa",
        "sr-Latn-QM",
        "sr-Qaaa-RS",
        "en-US-u-islamcal",
        "zh-CN-a-myext-x-private",
        "en-a-myext-b-another",
        "abcde",
        "abcdefgh",
        "en-GB-oed",
        "und",
    ] {
        assert!(language_tag_well_formed(t.as_bytes()), "{t}");
    }
    for t in [
        "",
        "de-419-DE",
        "a-DE",
        "ar-a-aaa-b-bbb-a",
        "abcdefghi",
        "en-",
        "-en",
        "en--US",
        "x",
        "en-x",
        "en-a",
        "en-a-x-y",
        "en-US-abc",
        "abcd-abc",
        "en-Latn-Latn",
        "en-US-Latn",
        "en_US",
        "e1",
        "1234",
        "en-abc-def-ghi-jkl",
    ] {
        assert!(!language_tag_well_formed(t.as_bytes()), "{t}");
    }
}

#[test]
fn scope_and_nesting() {
    let t = sample(true);
    let p2 = t.by_id(0, "p2").unwrap();
    let at = |scope| MatchContext {
        scope,
        ..MatchContext::new()
    };
    // 8.4: no scoping root means :root.
    assert_eq!(ids_plain(&t, ":scope"), ["r"]);
    let cx = at(Scope::Element(El::new(&t, p2)));
    assert_eq!(ids(&t, ":scope", &cx), ["p2"]);
    assert_eq!(ids(&t, ":scope > *", &cx), ["s1"]);
    assert_eq!(ids(&t, "body :scope", &cx), ["p2"]);
    // A virtual root (the document) is the parent of the root element, and
    // featureless: it matches :scope, never the subject.
    let doc = at(Scope::Root {
        first_child: Some(El::new(&t, 1)),
    });
    assert_eq!(ids(&t, ":scope > *", &doc), ["r"]);
    assert_eq!(ids(&t, ":scope > body", &doc), Vec::<String>::new());
    assert!(ids(&t, ":scope", &doc).is_empty());
    assert_eq!(ids(&t, ":is(:scope, x) > html", &doc), ["r"]);
    assert_eq!(ids(&t, "* > html", &doc), Vec::<String>::new());
    assert_eq!(ids(&t, ":not(.z) > html", &doc), Vec::<String>::new());
    assert_eq!(ids(&t, ":scope:has(> html) > html", &doc), ["r"]);
    assert_eq!(ids(&t, ":has(> html) > html", &doc), Vec::<String>::new());
    assert_eq!(ids(&t, "& > html", &doc), ["r"]);
    assert_eq!(ids(&t, ":scope:has(html) > *", &doc), ["r"]);
    // css-nesting-1: & is :scope outside nesting, the parent rule inside.
    assert_eq!(ids(&t, "& > *", &cx), ["s1"]);
    let parent = sel("p");
    let nested = MatchContext {
        nesting: &[&parent],
        ..MatchContext::new()
    };
    assert_eq!(ids(&t, "& > span, :not(&)", &nested), ["r", "b", "s1", "g"]);
    let grand = sel("#b");
    let inner = sel("& > p");
    let twice = MatchContext {
        nesting: &[&grand, &inner],
        ..MatchContext::new()
    };
    assert_eq!(ids(&t, "& span", &twice), ["s1"]);
    assert_eq!(ids(&t, "&.A", &twice), ["p1"]);
    // With a parent rule, & on the virtual root is that rule's selector,
    // which no element-only selector matches there.
    let nested_doc = MatchContext {
        scope: Scope::Root {
            first_child: Some(El::new(&t, 1)),
        },
        nesting: &[&parent],
        ..MatchContext::new()
    };
    assert!(ids(&t, "& > html", &nested_doc).is_empty());
    let scope_rule = sel(":scope");
    let scoped_doc = MatchContext {
        nesting: &[&scope_rule],
        ..nested_doc.clone()
    };
    assert_eq!(ids(&t, "& > html", &scoped_doc), ["r"]);
}

#[test]
fn query_all_returns_descendants_in_tree_order() {
    let t = sample(true);
    let b = t.by_id(0, "b").unwrap();
    let cx = MatchContext {
        scope: Scope::Element(El::new(&t, b)),
        ..MatchContext::new()
    };
    let got: Vec<&str> = query_all(&sel("*, html *, body"), &cx)
        .into_iter()
        .map(|e| t.attr(e.i, "", "id").unwrap())
        .collect();
    // The scoping root itself is not a candidate (17.4).
    assert_eq!(got, ["p1", "p2", "s1"]);
}

#[test]
fn pseudo_elements_and_shadow_selectors_match_no_element() {
    let t = sample(true);
    for s in [
        "p::before",
        "::part(a)",
        "span::slotted(*)",
        ":host",
        ":host(p)",
        ":host-context(html) p",
        ":has-slotted",
        ":is(p::before)",
    ] {
        let Ok(list) = parse_selector(s, &Namespaces::new()) else {
            continue;
        };
        for n in 0..t.nodes.len() {
            if t.is_element(n) {
                assert!(
                    !matches(&list, &El::new(&t, n), &MatchContext::new()),
                    "{s}"
                );
            }
        }
    }
}

#[test]
fn dir() {
    let mut t = Tree::new(true);
    let r = t.element(HTML, "html", &[("id", "r"), ("dir", "RTL")]);
    t.append(0, r);
    let a = t.element(HTML, "a", &[("id", "a"), ("dir", "ltr")]);
    t.append(r, a);
    let b = t.element(HTML, "b", &[("id", "b")]);
    t.append(r, b);
    assert_eq!(ids_plain(&t, ":dir(rtl)"), ["r", "b"]);
    assert_eq!(ids_plain(&t, ":dir(LTR)"), ["a"]);
    // 7.1: other values are valid and match nothing.
    assert!(ids_plain(&t, ":dir(auto)").is_empty());
}

#[test]
fn deepest_accepted_nesting_matches_on_a_small_stack() {
    let run = || {
        let t = sample(true);
        let n = optimal_selectors::MAX_NESTING as usize;
        for f in [
            ":is(",
            ":not(:not(",
            ":where(",
            ":nth-child(1 of ",
            ":has(* ",
        ] {
            let depth = if f.starts_with(":not") { n / 2 } else { n };
            let close = if f.starts_with(":not") { "))" } else { ")" };
            let src = format!("{}span{}", f.repeat(depth), close.repeat(depth));
            let list = parse_selector(&src, &Namespaces::new());
            let Ok(list) = list else { continue };
            for k in 0..t.nodes.len() {
                if t.is_element(k) {
                    let _ = matches(&list, &El::new(&t, k), &MatchContext::new());
                }
            }
        }
    };
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

/// Searches inside :has() depend on the anchor, so a shared session must
/// not reuse one anchor's answer for another. Siblings d1, p, d2, q under
/// the root: `:has(~ p ~ q)` asks for a p after the anchor and a q after
/// that p (4.5, relative selectors anchored at the element). For d1, p and
/// then q follow: match. For d2, only q follows: no p after d2, no match.
/// Matching d1 first (tree order) searches q's earlier siblings for a p
/// that follows d1 and finds one; d2 must not take that answer.
#[test]
fn has_searches_depend_on_the_anchor() {
    use optimal_selectors::MatchSession;
    let mut t = Tree::new(true);
    let r = t.element(HTML, "html", &[("id", "r")]);
    t.append(0, r);
    for (name, id) in [("div", "d1"), ("p", "p"), ("div", "d2"), ("q", "q")] {
        let e = t.element(HTML, name, &[("id", id)]);
        t.append(r, e);
    }
    let list = sel("div:has(~ p ~ q)");
    let cx = MatchContext {
        scope: Scope::Root {
            first_child: Some(El::new(&t, r)),
        },
        ..MatchContext::new()
    };
    let got: Vec<_> = query_all(&list, &cx)
        .into_iter()
        .map(|e| t.attr(e.i, "", "id").unwrap())
        .collect();
    assert_eq!(got, ["d1"]);
    let session = MatchSession::new(&cx);
    let d = |id: &str| {
        let i = t
            .subtree(0)
            .into_iter()
            .find(|&n| t.attr(n, "", "id") == Some(id))
            .unwrap();
        El::new(&t, i)
    };
    assert!(session.matches(&list, &d("d1")));
    assert!(!session.matches(&list, &d("d2")));
    assert_eq!(ids_plain(&t, "div:has(~ p ~ q)"), ["d1"]);
}
