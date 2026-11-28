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

## CSS tokenizer

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| romainmenke/css-tokenizer-tests | 0db1d12 | 287 / 287 | token type, raw text, start and end index, and the structured value (unescaped value, numeric value, type flag, sign, unit); each test run whole, one code point at a time, and in random chunks, which must all give the same tokens and errors |

The suite reports comments as tokens, as our tokenizer does, so they are
compared too. [`css-tokenizer-errata.json`](css-tokenizer-errata.json)
corrects 2 tests where the suite contradicts css-syntax-3 4.3.4: after
`url(`, all whitespace but the last code point belongs to the function
token. Types and values are unchanged; the harness fails if an erratum
stops differing from the suite.

The suite has no expected parse errors, so error kinds and offsets are
checked against a literal model of the spec (`crates/css/tests/common/reference.rs`)
on random inputs, together with every token, value and offset.

## CSS parser

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| SimonSapin/css-parsing-tests | 203ce36 | 149 / 149 | component_value_list, one_component_value, declaration_list, one_declaration, rule_list, one_rule, stylesheet and blocks_contents; stylesheet_bytes, An+B and color files are for later stages |

The suite follows the 2021 Candidate Recommendation; we follow the 2026
text of css-syntax-3 section 5 (nested rules, nested declarations,
speculative declaration parsing, custom properties keep their original
text). Two kinds of difference are handled:

- Mechanical conversions of the expected output: the suite's
  `["error", "eof-in-string"]`, `["error", "eof-in-url"]` and
  `["error", "invalid"]` markers, which its reference implementation puts
  in the tree where a parse error happened, are dropped (the spec reports
  parse errors apart, and ours are checked against the model below), and
  the 2021 match tokens (`~=`, `|=`, `^=`, `$=`, `*=`, `||`) are split into
  the two delims the 2026 tokenizer gives.
- [`css-parsing-errata.json`](css-parsing-errata.json) replaces the
  expected output of 16 tests where the 2026 text gives a different tree,
  each with the section it follows. The harness fails if an erratum stops
  differing from the suite.

Parse error kinds and offsets are checked against a literal recursive
model of section 5 (`crates/css/tests/common/parse_reference.rs`) on
random token streams for all seven entry points. A hostile-input test
times 15 input shapes (deep nesting, unclosed blocks, long speculative
declarations) at three sizes and fails on worse than linear growth.

## CSS An+B

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| SimonSapin/css-parsing-tests `An+B.json` | 203ce36 | 128 / 128 | A and B, or no match |
| web-platform-tests `css/css-syntax/anb-parsing.html` | 6852422 | 67 / 67 | each `testANB` call read from the file: parsed and serialized |
| web-platform-tests `css/css-syntax/anb-serialization.html` | 6852422 | 20 / 20 | as above |

No errata. A and B are clamped to `i32` (css-values-4 5.1 lets a UA clamp
integers to the range it supports), with the negation in clauses such as
`-n- <integer>` done before clamping. Random token streams are checked
against a literal model that writes out each clause of the grammar in 6.2
as a sequence of token matchers, for whole values and for the longest
matching prefix.

## Selectors

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| web-platform-tests `css/selectors/parsing` | 6852422 | 352 / 352 | every `test_valid_selector`, `test_invalid_selector` and related call, read from the files' JavaScript; each run through "parse a selector" and as a style rule's prelude (the two must agree), and each valid serialization parsed again (it must give itself back) |
| web-platform-tests `dom/nodes` (Selectors API) | 6852422 | 941 / 941 | `selectors.js` against `ParentNode-querySelector-All-content.html` parsed with `optimal-html`, set up as `ParentNode-querySelector-All.js` does (namespaced elements, clones, appended divs), in the document, element, detached element and fragment contexts: every `querySelectorAll` case, the invalid selectors, `null`/`undefined` tag names and `*` in tree order |

Four files are left out because their features are Selectors Level 5 or
tentative: `parse-heading.html` (`:heading`), the two
`*class-prefix.tentative.html` files, and `parse-has-slotted.tentative.html`
(functional `:has-slotted()`; css-shadow-1 has only `:has-slotted`).

[`wpt-selectors-errata.json`](wpt-selectors-errata.json) records 5 cases
where the 2026 spec text gives another answer than the test: `:lang()`
arguments serialize as strings (cssom-1 5.2), `:has()` is syntactically
allowed after the element-backed `::part()` (css-pseudo-4 section 5), and
user action pseudo-classes are allowed after `::slotted()` (selectors-4
3.6.3). The harness fails if an erratum stops being needed.

`crates/selectors/tests/common/reference.rs` is a literal model written
separately: it builds its own component value tree, parses the section 16
grammar production by production with no restrictions, and then checks
every validity rule in a second pass, with its own cssom-1 serializer.
Random selectors (from fragments, and from the grammar's shapes with
nested functional pseudo-classes) must get the same validity and
serialization from both, with and without a default namespace. Valid
serializations must parse back to themselves with the same specificity.
`crates/selectors/tests/scaling.rs` times 17 hostile shapes (long
compounds and lists, deep blocks in forgiving arguments, nesting past the
limit) at three sizes with `CSS_SCALING=1`.

For matching, the XHTML variant of the Selectors API tests
(`ParentNode-querySelector-All-xht.xht`) waits for an XML parser, and the
`matches()` and scoped cases of `selectors.js` (`scopedSelectors`) and the
other `dom/nodes` querySelector tests, which build their trees from script,
wait for the script engine. [`wpt-query-errata.json`](wpt-query-errata.json)
records 8 cases (2 selectors in 4 contexts): selectors-4 13.2 now lets an
`:empty` element hold document white space, so `<p> </p>` is empty.
`crates/selectors/tests/common/match_reference.rs` is a literal model of
matching (every candidate tried, `:has()` searching the whole tree), run
against random trees (any namespace, text, comments, a fragment) and random
selectors for every element, with no scoping root, an element, the
document and a fragment as the scoping root, with and without quirks mode
and a nesting parent; `query_all` must equal filtering the subtree. The
scaling test also times matching: linear on wide trees for 12 hostile
selectors, and at worst quadratic (pinned) for deep ancestor chains and long
sibling runs, which each element may walk in full. `:lang()` checks that
tags and ranges are well-formed (RFC 5646) but not yet that they are valid
or canonical, which needs the IANA subtag registry.
