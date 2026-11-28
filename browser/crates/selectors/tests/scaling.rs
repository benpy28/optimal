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
/// nested logical pseudo-classes, :has(), `of S`); so does matching along a
/// deep chain of ancestors or a long run of siblings, where each element
/// could look at all of its ancestors or earlier siblings without the
/// caches (see `sibling_runs_and_ancestor_chains_cost_linear_time`).
/// `:has()` over a deep chain is pinned at quadratic: it has no cache yet.
/// Release builds with `CSS_SCALING=1` only.
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
    let (a, b, c) = (
        time(&deep(d), "div:has(x)"),
        time(&deep(4 * d), "div:has(x)"),
        time(&deep(16 * d), "div:has(x)"),
    );
    println!("deep chain, :has     div:has(x) n={d}: {a:?}, 4n: {b:?}, 16n: {c:?}");
    assert!(
        b <= a * 32 + slack || c <= b * 32 + slack,
        "deep chain, :has: {a:?} -> {b:?} -> {c:?} is worse than quadratic"
    );
}

/// Hostile shapes for the caches: one run of n siblings, or one chain of n
/// ancestors, matched with selectors that make each element look at all
/// of its earlier siblings or ancestors unless the earlier answers are
/// reused (a match that is never found, so no search stops early). Each is
/// timed through `query_all` (tree order) and through a `MatchSession` in
/// reverse tree order (the caches are then filled from the other end), and
/// must grow linearly: at most 8x for 4x the size, at one of two steps
/// (16x for 4x is what the uncached matcher shows). Release builds with
/// `CSS_SCALING=1` only.
#[test]
fn sibling_runs_and_ancestor_chains_cost_linear_time() {
    use common::tree::{El, HTML, Tree};
    use optimal_selectors::{MatchContext, MatchSession, Scope, query_all};
    if std::env::var_os("CSS_SCALING").is_none() {
        return;
    }
    // n children of one element: all `p`, or each of its own type except
    // a `p` at both ends (so of-type runs are long and short at once).
    let siblings = |n: usize, mixed: bool| {
        let mut t = Tree::new(true);
        let r = t.element(HTML, "html", &[]);
        t.append(0, r);
        for k in 0..n {
            let name = if mixed && k != 0 && k != n - 1 {
                format!("e{k}")
            } else {
                "p".to_string()
            };
            let e = t.element(HTML, &name, &[]);
            t.append(r, e);
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
    let time = |t: &Tree, s: &str| {
        let list = parse_selector(s, &Namespaces::new()).unwrap();
        let cx = MatchContext {
            scope: Scope::Root {
                first_child: t.first_element_child(0).map(|i| El::new(t, i)),
            },
            ..MatchContext::new()
        };
        let all = query_all(&list, &cx);
        let best = |f: &dyn Fn() -> usize| {
            (0..3)
                .map(|_| {
                    let start = std::time::Instant::now();
                    std::hint::black_box(f());
                    start.elapsed()
                })
                .min()
                .unwrap()
        };
        let forward = best(&|| query_all(&list, &cx).len());
        let reverse = best(&|| {
            let session = MatchSession::new(&cx);
            all.iter()
                .rev()
                .filter(|e| session.matches(&list, e))
                .count()
                + (0..t.nodes.len())
                    .rev()
                    .filter(|&i| t.is_element(i) && session.matches(&list, &El::new(t, i)))
                    .count()
        });
        (forward, reverse)
    };
    let slack = std::time::Duration::from_millis(20);
    let n = 20_000;
    type Make<'a> = &'a dyn Fn(usize) -> Tree;
    let runs: [(&str, Make<'_>, &str); 14] = [
        ("siblings", &|n| siblings(n, false), "x ~ p"),
        ("siblings", &|n| siblings(n, false), "x ~ p ~ p"),
        ("siblings", &|n| siblings(n, false), "x + p ~ p"),
        ("siblings", &|n| siblings(n, false), "x ~ p:nth-child(2n)"),
        ("siblings", &|n| siblings(n, false), "p:nth-child(2n of p)"),
        (
            "siblings",
            &|n| siblings(n, false),
            "p:nth-last-child(2n of :not(x))",
        ),
        (
            "siblings",
            &|n| siblings(n, false),
            "p:nth-last-child(3n+1)",
        ),
        (
            "siblings",
            &|n| siblings(n, false),
            "p:nth-of-type(odd):last-of-type",
        ),
        (
            "mixed siblings",
            &|n| siblings(n, true),
            "p:last-of-type, :nth-last-of-type(1)",
        ),
        (
            "mixed siblings",
            &|n| siblings(n, true),
            ":first-of-type:only-of-type",
        ),
        (
            "mixed siblings",
            &|n| siblings(n, true),
            "html > :nth-child(n+2 of :not(p)) ~ p",
        ),
        ("deep chain", &deep, "x div div"),
        ("deep chain", &deep, "x div > div ~ div"),
        ("deep chain", &deep, ":is(x div) div"),
    ];
    for (name, make, s) in runs {
        let (a, b, c) = (
            time(&make(n), s),
            time(&make(4 * n), s),
            time(&make(16 * n), s),
        );
        println!("{name:16} {s:40} n={n}: {a:?}, 4n: {b:?}, 16n: {c:?}");
        for (k, (a, b, c)) in [(a.0, b.0, c.0), (a.1, b.1, c.1)].into_iter().enumerate() {
            let order = ["tree order", "reverse order"][k];
            assert!(
                b <= a * 8 + slack || c <= b * 8 + slack,
                "{name}, {s}, {order}: {a:?} -> {b:?} -> {c:?} is not linear"
            );
        }
    }
}

/// The same shapes as above, counted instead of timed, so the check runs in
/// every build (and catches a cache that is never filled or never read):
/// the number of steps to a parent or sibling, over matching every element
/// of a run of n = 2,000 siblings or a chain of n ancestors, stays under
/// 12n. Without the caches the first case takes about n * n / 2 steps.
#[test]
fn sibling_runs_and_ancestor_chains_take_linear_steps() {
    use common::tree::{El, HTML, Tree};
    use optimal_selectors::{
        Direction, Element, MatchContext, MatchSession, PseudoClassKeyword, Scope, query_all,
    };
    use std::cell::Cell;

    /// An element that counts the tree steps taken from it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Counted<'t> {
        el: El<'t>,
    }
    thread_local! {
        static STEPS: Cell<usize> = const { Cell::new(0) };
    }
    fn step<'t>(e: Option<El<'t>>) -> Option<Counted<'t>> {
        STEPS.with(|s| s.set(s.get() + 1));
        e.map(|el| Counted { el })
    }
    impl Element for Counted<'_> {
        fn parent_element(&self) -> Option<Self> {
            step(self.el.parent_element())
        }
        fn is_root(&self) -> bool {
            self.el.is_root()
        }
        fn prev_sibling_element(&self) -> Option<Self> {
            step(self.el.prev_sibling_element())
        }
        fn next_sibling_element(&self) -> Option<Self> {
            step(self.el.next_sibling_element())
        }
        fn first_element_child(&self) -> Option<Self> {
            step(self.el.first_element_child())
        }
        fn same_parent(&self, other: &Self) -> bool {
            self.el.same_parent(&other.el)
        }
        fn any_non_element_child(&self, f: impl FnMut(Option<&[u8]>) -> bool) -> bool {
            self.el.any_non_element_child(f)
        }
        fn is_html_element_in_html_document(&self) -> bool {
            self.el.is_html_element_in_html_document()
        }
        fn local_name(&self) -> &[u8] {
            self.el.local_name()
        }
        fn namespace(&self) -> &[u8] {
            self.el.namespace()
        }
        fn attribute(&self, ns: &[u8], local: &[u8]) -> Option<&[u8]> {
            self.el.attribute(ns, local)
        }
        fn any_attribute(&self, f: impl FnMut(&[u8], &[u8], &[u8]) -> bool) -> bool {
            self.el.any_attribute(f)
        }
        fn state(&self, pc: PseudoClassKeyword) -> bool {
            self.el.state(pc)
        }
        fn custom_state(&self, name: &str) -> bool {
            self.el.custom_state(name)
        }
        fn with_language<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
            self.el.with_language(f)
        }
        fn directionality(&self) -> Direction {
            self.el.directionality()
        }
    }

    let n: usize = 2000;
    let siblings = |mixed: bool| {
        let mut t = Tree::new(true);
        let r = t.element(HTML, "html", &[]);
        t.append(0, r);
        for k in 0..n {
            let name = if mixed && k != 0 && k != n - 1 {
                format!("e{k}")
            } else {
                "p".to_string()
            };
            let e = t.element(HTML, &name, &[]);
            t.append(r, e);
        }
        t
    };
    let deep = || {
        let mut t = Tree::new(true);
        let mut p = 0;
        for _ in 0..n {
            let e = t.element(HTML, "div", &[]);
            t.append(p, e);
            p = e;
        }
        t
    };
    let (same, mixed, chain) = (siblings(false), siblings(true), deep());
    for (t, s, hits) in [
        (&same, "x ~ p", 0),
        (&same, "x + p ~ p", 0),
        (&same, "p:nth-child(2n of p)", n / 2),
        (&same, "p:nth-last-child(2n of :not(x))", n / 2),
        (&same, "p:nth-last-child(3n+1)", n.div_ceil(3)),
        // The other types, the last p and the root.
        (&mixed, "p:last-of-type, :nth-last-of-type(1)", n),
        // The other types and the root.
        (&mixed, ":first-of-type:only-of-type", n - 1),
        (&chain, "x div div", 0),
        (&chain, ":is(x div) div", 0),
    ] {
        let list = parse_selector(s, &Namespaces::new()).unwrap();
        let cx = MatchContext {
            scope: Scope::Root {
                first_child: t
                    .first_element_child(0)
                    .map(|i| Counted { el: El::new(t, i) }),
            },
            ..MatchContext::new()
        };
        STEPS.with(|c| c.set(0));
        let all = query_all(&list, &cx);
        let forward = STEPS.with(Cell::get);
        assert_eq!(all.len(), hits, "{s}");
        let els: Vec<_> = (0..t.nodes.len())
            .filter(|&i| t.is_element(i))
            .map(|i| Counted { el: El::new(t, i) })
            .collect();
        STEPS.with(|c| c.set(0));
        let session = MatchSession::new(&cx);
        let back = els
            .iter()
            .rev()
            .filter(|e| session.matches(&list, e))
            .count();
        let reverse = STEPS.with(Cell::get);
        assert_eq!(back, hits, "{s}");
        println!("{s:40} steps: {forward} in tree order, {reverse} in reverse");
        assert!(
            forward <= 12 * n && reverse <= 12 * n,
            "{s}: {forward}, {reverse} steps for n = {n}"
        );
    }
}
