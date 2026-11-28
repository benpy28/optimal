//! Parsing and serializing time grows linearly with input size on hostile
//! selectors, and nesting past the limit is refused, not a stack overflow.
//! Timing is only meaningful in release builds, so the timing test runs
//! when `CSS_SCALING=1`; the depth tests always run.

use optimal_selectors::{MAX_NESTING, Namespaces, SelectorError, parse_selector};

#[test]
fn hostile_selectors_parse_in_linear_time() {
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    type Gen = fn(usize) -> String;
    let cases: &[(&str, Gen)] = &[
        ("long compound", |n| ".a".repeat(n)),
        ("long list", |n| "a,".repeat(n) + "a"),
        ("descendants", |n| "a ".repeat(n) + "a"),
        ("children", |n| "a>".repeat(n) + "a"),
        ("forgiving items", |n| format!(":is({})", "1,".repeat(n))),
        ("forgiving valid items", |n| {
            format!(":is({}a)", "a,".repeat(n))
        }),
        ("deep blocks in :is", |n| format!(":is({}", "[".repeat(n))),
        ("deep parens in :is", |n| format!(":is({}", "(".repeat(n))),
        ("too deep :is", |n| ":is(".repeat(n)),
        ("too deep :not", |n| ":not(".repeat(n)),
        ("many :is at the limit", |n| {
            let deep = format!("{}a{}", ":is(".repeat(200), ")".repeat(200));
            vec![deep; n / 400].join(",")
        }),
        ("attributes", |n| "[a=b i]".repeat(n / 7)),
        ("pseudo-classes", |n| ":hover".repeat(n / 6)),
        ("part names", |n| format!("::part({})", "a ".repeat(n))),
        ("lang ranges", |n| format!(":lang({}a)", "a,".repeat(n))),
        ("has arguments", |n| format!(":has({}a)", "> a,".repeat(n))),
        ("nth of list", |n| {
            format!(":nth-child(2n of {}a)", "a,".repeat(n))
        }),
    ];
    let ns = Namespaces::new();
    let time = |input: &str| {
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let r = parse_selector(input, &ns);
                let len = r.as_ref().map(|l| l.to_string().len()).unwrap_or(0);
                let _ = std::hint::black_box((len, r));
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    // Each case at n, 4n and 16n must be linear (at most 8x for 4x the
    // input, against 16x for quadratic) across at least one of the two
    // steps, so that one cache-size step can't fail it.
    let n = 50_000;
    // Linear growth alone would not catch a cost of nesting depth times
    // input (each nested :is() rescanning everything inside it): that is
    // linear in the input, up to the depth limit. So nested :is() must
    // also cost about what the same nesting of plain parentheses does.
    let nested = time(&":is(".repeat(16 * n));
    let parens = time(&format!(":is({}", "(".repeat(16 * n)));
    println!("nested :is {nested:?} against nested parentheses {parens:?}");
    assert!(
        nested <= parens * 10 + std::time::Duration::from_millis(20),
        "nested :is() costs {nested:?}, parentheses {parens:?}"
    );
    for (name, make) in cases {
        let t1 = time(&make(n));
        let t4 = time(&make(4 * n));
        let t16 = time(&make(16 * n));
        println!("{name:24} n={n}: {t1:?}, 4n: {t4:?}, 16n: {t16:?}");
        let slack = std::time::Duration::from_millis(20);
        assert!(
            t4 <= t1 * 8 + slack || t16 <= t4 * 8 + slack,
            "{name}: growth {t1:?} -> {t4:?} -> {t16:?} is not linear"
        );
    }
}

/// The deepest accepted selector parses, serializes, computes specificity
/// and drops on a thread with a 1 MiB stack (half of Rust's default for new
/// threads) in a test build. About 2 to 4 KiB per level; 512 KiB is not
/// enough in a test build, nor 256 KiB in a release build.
#[test]
fn deepest_accepted_nesting_fits_a_small_stack() {
    let run = || {
        for f in [
            ":is(",
            ":not(",
            ":where(",
            ":has(",
            ":nth-child(1 of ",
            ":host(",
        ] {
            let n = MAX_NESTING as usize;
            let src = format!("{}a{}", f.repeat(n), ")".repeat(n));
            let r = parse_selector(&src, &Namespaces::new());
            match f {
                // :has() can't nest, and :host() can't hold :host() with an
                // argument that isn't a compound... both are still parsed
                // to the bottom before failing.
                ":has(" => assert_eq!(r, Err(SelectorError::NotAllowedHere)),
                _ => {
                    let l = r.unwrap_or_else(|e| panic!("{f}: {e:?}"));
                    assert_eq!(l.to_string(), src);
                    let _ = l.0[0].specificity(Default::default());
                }
            }
            let over = format!("{}a{}", f.repeat(n + 1), ")".repeat(n + 1));
            let r = parse_selector(&over, &Namespaces::new());
            assert!(
                matches!(
                    r,
                    Err(SelectorError::TooDeep | SelectorError::NotAllowedHere)
                ),
                "{f}: {r:?}"
            );
        }
    };
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

mod common;

/// Matching cost: `query_all` over a wide tree grows linearly with its
/// size for hostile selectors (long combinator chains that almost match,
/// nested logical pseudo-classes, :has(), `of S`). Two costs are not
/// linear, by design, and are pinned at quadratic instead: climbing a deep
/// chain of ancestors (each element can look at all of its ancestors) and
/// scanning a long run of siblings (each can look at all earlier ones), as
/// in every engine that matches right to left without ancestor or index
/// caches. Release builds with `CSS_SCALING=1` only.
#[test]
fn matching_cost_grows_linearly_on_wide_trees() {
    use common::tree::{El, HTML, Tree};
    use optimal_selectors::{MatchContext, Scope, query_all};
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    // n elements: rows of 8 under the root, each row three levels deep
    // (div > p > span), with a few classes and attributes.
    let wide = |n: usize| {
        let mut t = Tree::new(true);
        let r = t.element(HTML, "html", &[]);
        t.append(0, r);
        for k in 0..n / 24 {
            let row = t.element(
                HTML,
                "div",
                &[("class", if k % 3 == 0 { "a b" } else { "b" })],
            );
            t.append(r, row);
            for j in 0..8 {
                let p = t.element(HTML, "p", &[("title", "x-y")]);
                t.append(row, p);
                let s = t.element(
                    HTML,
                    "span",
                    &[("data-k", if j % 2 == 0 { "1" } else { "2" })],
                );
                t.append(p, s);
            }
        }
        t
    };
    let deep = |n: usize| {
        let mut t = Tree::new(true);
        let mut p = 0;
        for _ in 0..n {
            let e = t.element(HTML, "div", &[]);
            t.append(p, e);
            p = e;
        }
        t
    };
    let siblings = |n: usize| {
        let mut t = Tree::new(true);
        let r = t.element(HTML, "html", &[]);
        t.append(0, r);
        for _ in 0..n {
            let e = t.element(HTML, "p", &[]);
            t.append(r, e);
        }
        t
    };
    let time = |t: &Tree, s: &str| {
        let list = parse_selector(s, &Namespaces::new()).unwrap();
        let cx = MatchContext {
            scope: Scope::Root {
                first_child: t.first_element_child(0).map(|i| El::new(t, i)),
            },
            ..MatchContext::new()
        };
        (0..3)
            .map(|_| {
                let start = std::time::Instant::now();
                std::hint::black_box(query_all(&list, &cx).len());
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let slack = std::time::Duration::from_millis(20);
    let n = 20_000;
    for s in [
        "x div p span",
        "div p span span",
        "html > div > p > span",
        "div.a > p:not([title^=y]) + p span[data-k='2']",
        "div ~ div > p ~ p span",
        ":is(:is(:is(div p))) span:not(:is(.a *))",
        "div:has(> p > span[data-k='3'])",
        "p:has(+ p span)",
        "span:nth-child(1 of :is(p *, span))",
        "p:nth-of-type(2n+1):nth-last-child(odd)",
        ":not(:has(*)):empty",
        "[title|=x i], [data-k$='1' s]",
    ] {
        let (a, b, c) = (
            time(&wide(n), s),
            time(&wide(4 * n), s),
            time(&wide(16 * n), s),
        );
        println!("{s:48} n={n}: {a:?}, 4n: {b:?}, 16n: {c:?}");
        assert!(
            b <= a * 8 + slack || c <= b * 8 + slack,
            "{s}: {a:?} -> {b:?} -> {c:?} is not linear"
        );
    }
    // Quadratic at worst (16x for 4x), not worse.
    let d = 500;
    for (name, make, s) in [
        ("deep chain", &deep as &dyn Fn(usize) -> Tree, "x div div"),
        ("deep chain, :has", &deep, "div:has(x)"),
        ("siblings", &siblings, "x ~ p"),
        ("siblings, nth", &siblings, "p:nth-child(2n of p)"),
    ] {
        let (a, b, c) = (
            time(&make(d), s),
            time(&make(4 * d), s),
            time(&make(16 * d), s),
        );
        println!("{name:20} {s:24} n={d}: {a:?}, 4n: {b:?}, 16n: {c:?}");
        assert!(
            b <= a * 32 + slack || c <= b * 32 + slack,
            "{name}: {a:?} -> {b:?} -> {c:?} is worse than quadratic"
        );
    }
}
