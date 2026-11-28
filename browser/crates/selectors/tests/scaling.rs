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
