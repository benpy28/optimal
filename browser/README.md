# Optimal browser engine

Optimal's own web engine, written from scratch in Rust from the public web
standards. It is developed as ordinary Rust crates that build and test on any
host, and moves onto Optimal when the desktop phase lands. Until it passes its
gates, Optimal ships a borrowed browser.

- Design, security model and stage plan: [docs/DESIGN.md](docs/DESIGN.md)
- Conformance results and test-data errata: [conformance/](conformance/)

| Crate | What it is | Status |
|---|---|---|
| `crates/wtf8` | Web strings: UTF-8 that can also hold lone surrogates | done |
| `crates/html` | WHATWG HTML parser | tokenizer done (html5lib-tests 7046/7046); tree builder next |
| `crates/css` | CSS Syntax Level 3 | tokenizer and parser done (css-tokenizer-tests 287/287, 2 with errata; css-parsing-tests 149/149, 16 with errata), An+B (An+B.json 128/128, WPT 87/87) |
| `crates/selectors` | Selectors Level 4 | parsing, serialization, specificity and matching done (WPT css/selectors/parsing 352/352, 5 with errata; dom/nodes querySelectorAll 941/941, 8 with errata) |
| `crates/style` | CSS Cascading and Inheritance Level 5 | style sheet collection (nesting, cascade layers, `@namespace`, style attributes) and the cascade sort done (59 runnable WPT css-cascade and css-nesting cases, 59 passed); defaulting and property values next |

## Building and testing

```sh
cd browser
./scripts/fetch-test-suites.sh   # conformance suites at pinned commits, into build/
cargo test --workspace
```

`PROPTEST_CASES` and `TOKENIZER_RANDOM_ROUNDS` turn up the number of random
cases and random input chunkings. CI runs them much higher, and nightly
higher still.

## Licences of test data

- html5lib-tests (MIT), web-platform-tests (BSD-3-Clause),
  romainmenke/css-tokenizer-tests (MIT) and SimonSapin/css-parsing-tests
  (CC0), fetched by the script, not vendored.
- `crates/html/data/entities.json`: the WHATWG named character reference
  table, CC BY 4.0, Copyright WHATWG (Apple, Google, Mozilla, Microsoft).
