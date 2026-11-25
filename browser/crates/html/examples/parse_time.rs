//! Times parsing of a few pathological inputs: `cargo run --release --example parse_time`.

use optimal_html::tree_builder::{ParseOptions, parse_document};
use std::time::Instant;

fn time(name: &str, input: &str) {
    let t = Instant::now();
    let p = parse_document(input, ParseOptions::default());
    println!(
        "{name:40} {:>8} bytes {:>10.2?} ({} nodes)",
        input.len(),
        t.elapsed(),
        p.document.node_count()
    );
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    time("nested div", &"<div>".repeat(n));
    time("nested div then text", &format!("{}x", "<div>".repeat(n)));
    time("nested div with p each", &"<div><p>".repeat(n));
    time("nested b (formatting)", &"<b>".repeat(n));
    time(
        "b then p (reconstruct)",
        &format!("{}{}", "<b>".repeat(n), "<p>x".repeat(n)),
    );
    time("unclosed a", &"<a>x".repeat(n));
    time("nested table", &"<table><tr><td>".repeat(n));
    time("table text", &format!("<table>{}", "x<b>".repeat(n)));
    time("nested svg", &format!("<svg>{}", "<g>".repeat(n)));
    time(
        "misnested </b>",
        &format!("{}{}", "<b><div>".repeat(n / 10), "</b>".repeat(n / 10)),
    );
    time(
        "adoption with many divs",
        &format!("<b>{}</b>", "<div>".repeat(n)),
    );
    time("end tags of nothing", &"</div>".repeat(n));
    time(
        "select options",
        &format!("<select>{}", "<option>x".repeat(n)),
    );
    time("li siblings", &format!("<ul>{}", "<li>x".repeat(n)));
    time("dd/dt", &format!("<dl>{}", "<dd><div>".repeat(n)));
    time("headings", &"<h1><div>".repeat(n));
    time(
        "distinct formatting (Noah's Ark)",
        &(0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
    );
    time(
        "distinct then same",
        &format!(
            "{}{}",
            (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
            "<i>".repeat(n)
        ),
    );
    time(
        "formatting then many end tags",
        &format!(
            "{}{}",
            (0..n).map(|i| format!("<b id={i}>")).collect::<String>(),
            "</b>".repeat(n)
        ),
    );
    time("long text", &"x".repeat(n * 50));
    time(
        "many attributes",
        &format!(
            "<div {}>",
            (0..n)
                .map(|i| format!("a{i}=v"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );
}
