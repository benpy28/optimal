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

- html5lib-tests (MIT), fetched by the script, not vendored.
- `crates/html/data/entities.json`: the WHATWG named character reference
  table, CC BY 4.0, Copyright WHATWG (Apple, Google, Mozilla, Microsoft).
