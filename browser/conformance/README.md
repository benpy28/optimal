# Conformance

Pass rates on the official suites. CI prints the current numbers in each
run's summary and uploads them as the `browser-conformance` artifact.

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| html5lib-tests `tokenizer/` | c5495c5 | 7046 / 7046 | tokens, error codes and error positions; each test run whole, one code point at a time, and in random chunks |

`xmlViolation.test` is not counted: it tests the XML-infoset coercion an XML
serializer applies, not the HTML tokenizer.

## Errata

[`html5lib-tokenizer-errata.json`](html5lib-tokenizer-errata.json) lists
expectations in the suite that contradict the standard, each with the reason
and the spec-derived correction we test against instead. The harness fails if
an erratum stops matching the suite (for example after an upstream fix), so
the list can't go stale silently.
