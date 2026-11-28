//! Times the parser on generated inputs at growing sizes, to see how the
//! cost grows: `cargo run --release -p optimal-css --example parse_time`.

fn main() {
    type Gen = fn(usize) -> String;
    let cases: &[(&str, Gen)] = &[
        ("declarations", |n| {
            format!("a{{{}}}", "b:c !important;".repeat(n))
        }),
        ("custom look-alikes", |n| {
            format!("a{{{}}}", "--x:y{}".repeat(n))
        }),
        ("block then junk, nested", |n| {
            format!("{}{}", "a:{".repeat(n), "}x".repeat(n))
        }),
        ("rules", |n| "a{}".repeat(n)),
        ("long prelude", |n| format!("{}{{}}", "a ".repeat(n))),
    ];
    for (name, make) in cases {
        for n in [10_000, 40_000, 160_000, 640_000, 2_560_000] {
            let input = make(n);
            let best = (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let (p, r) = optimal_css::parse_stylesheet(
                        optimal_wtf8::Wtf8::from_str(&input),
                        &mut optimal_css::AcceptAll,
                    );
                    std::hint::black_box((p.rules.len(), r.len()));
                    drop(p);
                    t.elapsed()
                })
                .min()
                .unwrap();
            println!(
                "{name:26} n={n:7}: {best:?} ({:.1} ns/byte)",
                best.as_nanos() as f64 / input.len() as f64
            );
        }
    }
}
