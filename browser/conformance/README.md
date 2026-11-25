# Conformance

Pass rates on the official suites. CI prints the current numbers in each
run's summary and uploads them as the `browser-conformance` artifact.

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| html5lib-tests `tokenizer/` | c5495c5 | 7046 / 7046 | tokens, error codes and error positions; each test run whole, one code point at a time, and in random chunks |
| WPT `html/syntax/parsing` tree construction (`*.dat`) | 6852422 | 3890 / 3890 runnable | the tree, in documents and fragments, scripting on and off; whole, per code point (same parse errors too) and random chunks. 14 more are blocked, see below |

`xmlViolation.test` is not counted: it tests the XML-infoset coercion an XML
serializer applies, not the HTML tokenizer.

## Errata

[`html5lib-tokenizer-errata.json`](html5lib-tokenizer-errata.json) lists
expectations in the suite that contradict the standard, each with the reason
and the spec-derived correction we test against instead. The harness fails if
an erratum stops matching the suite (for example after an upstream fix), so
the list can't go stale silently.

## Tree construction: blocked tests

Blocked tests are counted apart, never as passes:

- **6 need the JS engine**: the `scripted_*.dat` files expect `<script>` to
  run and call `document.write` during parsing.
- **8 need DOM element behaviour**: listed in
  [`wpt-tree-blocked.json`](wpt-tree-blocked.json) with the reason. They
  expect `<selectedcontent>` to mirror the selected `<option>`, which is the
  select element's DOM behaviour, not tree construction. The harness fails
  if a listed test starts passing.

## Tree construction: parse error counts

The suite's own README says only the number of parse errors matters, and
its counts come from an older implementation's choices (for example one
error per character of foster-parented text). The standard itself doesn't
fix a count for runs of text. So counts are gated against the suite where
they match (currently 2931 of 3890), and every difference elsewhere is pinned
in [`wpt-tree-error-counts.json`](wpt-tree-error-counts.json): a change to
which errors the tree builder reports fails CI until that file is regenerated
(`TREE_ERROR_COUNTS_OUT=<path>`) and the change reviewed.
Our errors are deterministic: chunking never changes their number or order,
which the harness and the property tests check.

[`wpt-tree-errata.json`](wpt-tree-errata.json) holds corrections where a
tree expectation contradicts the standard. It is empty: no tree expectation
needed correcting.
