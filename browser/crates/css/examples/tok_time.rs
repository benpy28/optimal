//! Times the tokenizer alone: `cargo run --release -p optimal-css --example tok_time`.
fn main() {
    for n in [10_000, 40_000, 160_000, 640_000, 2_560_000] {
        let input = "a ".repeat(n);
        let t = std::time::Instant::now();
        let mut tk = optimal_css::Tokenizer::new(optimal_wtf8::Wtf8::from_str(&input));
        let v: Vec<_> = std::iter::from_fn(|| tk.next_token()).collect();
        let e = t.elapsed();
        println!(
            "n={n:8} {e:?} {:.1} ns/byte, {} tokens",
            e.as_nanos() as f64 / input.len() as f64,
            v.len()
        );
    }
}
