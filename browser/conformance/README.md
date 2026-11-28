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
and a nesting parent; `query_all` must equal filtering the subtree, and
matching every element through one `MatchSession` in reverse and in
rotated order (caches filled from elsewhere first) must agree too. The
scaling test also times matching: linear on wide trees for 12 hostile
selectors, and linear on one run of up to 320,000 siblings and one chain of
up to 320,000 ancestors for 14 selectors that make each element look at all
of its earlier siblings or ancestors unless earlier answers are reused
(`~`, `:nth-*()` with and without `of S`, `:*-of-type`, descendant chains),
in tree order and in reverse. Without the caches the first of them does not
finish in 5 minutes. `:has()` over a deep chain is still pinned at
quadratic: it has no cache yet. `:lang()` checks that
tags and ranges are well-formed (RFC 5646) but not yet that they are valid
or canonical, which needs the IANA subtag registry.

## Cascade

| Suite | Pinned commit | Passed | Notes |
|---|---|---|---|
| web-platform-tests `css/css-cascade/layer-basic.html`, `layer-important.html` | 6852422 | 43 / 43 | each case's `style` read from the script's `testCases` array; both `target` elements must cascade `color` to `green` |
| web-platform-tests `css/css-cascade/layer-vs-inline-style.html` | 6852422 | 4 / 4 | the case's sheet and inline style; `#target`'s `background-color` must be `#reference`'s (`green`) |
| web-platform-tests `css/css-cascade/layer-rule-invalid-name.html` | 6852422 | 4 / 4 | each `<style>`'s rule count |
| web-platform-tests `css/css-cascade/parsing/layer.html` | 6852422 | 11 / 11 | `test_valid_rule` / `test_invalid_rule`: a valid `@layer` declares a layer |
| web-platform-tests `css/css-cascade/parsing/layer-import-parsing.html` | 6852422 | 13 / 13 | a valid layered `@import` declares its layer; with an invalid `layer()` it stays a valid import (the `@import` after it still counts) |
| web-platform-tests static pages in `css/css-cascade` and `css/css-nesting` | 6852422 | 49 / 49 | the page's `<style>` elements and markup (parsed by `optimal-html`), and each `getComputedStyle` assert written out as (element, property, value) in `crates/style/tests/wpt.rs`: `layers-basic`, `important-vs-inline-001` (first assert), `nesting-layer`, `top-level-parent-pseudo-specificity`, `implicit-nesting-ident`, `nested-error-recovery`, `nested-declarations-matching`, `revert-rule-basic`, `revert-rule-important`, `revert-rule-layer`, `revert-rule-custom-property`, `revert-rule-revert-layer`, `nesting-revert-rule` (`#test1` to `#test3`), `inherit-initial` |
| web-platform-tests reftests of the defaulting keywords in `css/css-cascade` | 6852422 | 36 / 36 (20 pages) | each page "passes if there is a filled green square" (or names its condition, or matches a reference page); the specified values that make it so are written out in `crates/style/tests/wpt.rs` with the reasoning: `revert-layer-001` to `-003`, `-005` to `-007`, `-009`, `-012`, `revert-val-001`, `-002`, `unset-val-001`, `-002`, `initial-color-background-001`, `initial-background-color`, `all-prop-001`, `-002`, `all-prop-{inherit,initial,revert,unset}-color`. Pages whose result depends on the user agent sheet get its `display: block` rule for `div` and `p`, copied from the HTML standard's rendering section |
| web-platform-tests `*crash*.html` in both directories | 6852422 | 14 / 14 | every page collected and cascaded without a panic |

There are no computed values yet, so a value is checked as the specified
value's text (a declaration's value as written, or the property table's
initial value), and an expected `rgb(0, 128, 0)` is the computed value of
the keyword `green` (likewise red, yellow, black, and `rgba(0, 0, 0, 0)`
for `transparent`).

The property table (`crates/style/src/property.rs`, 154 longhands, 29
shorthands, 2 legacy aliases) is checked against the property definition
tables of the specs it cites, at csswg-drafts 7027ab6
(`crates/style/tests/property_table.rs`): every longhand's initial value
and inherited flag, every shorthand's definition and longhands.

No errata. Two bugs were found by these files and fixed: a `@layer`
statement inside a style rule is invalid (only at-rules that hold style
rules nest; `nesting-layer.html`), and an `@import` whose `layer()` holds
no valid name is still a valid import, with that `layer()` read as a
condition (`layer-import-parsing.html`).

Waiting, with the reason (the rest of both directories):

- **script**: CSSOM edits and serialization (`layer-cssom-*`,
  `layer-rules-cssom`, `layer-replaceSync-*`, `layer-stylesheet-*`,
  `layer-statement-before-import`, `cssom`, `set-selector-text`,
  `nested-declarations-cssom*`, `serialize-group-rules-with-decls`,
  `css-nesting/parsing`, `idlharness`), and the tests that change the page
  and check again (`*invalidation*`, `duplicate-stylesheets`,
  `import-removal`, the later asserts of `important-vs-inline-*` and of
  `nested-declarations-matching`);
- **`@import` loading**: `layer-import`, `import-*`;
- **conditional rules** (`@media`, `@supports`, `@container`):
  `layer-media-*`, `conditional-*`, `supports-*`, `nested-media-in-layer`,
  `mixed-declarations-rules`, `#test1` of `nested-error-recovery` and the
  `@media` cases of `nested-declarations-matching`;
- **`@scope`**: `at-scope-*`, `scope-*`, the `@scope` cases of
  `nested-declarations-matching`;
- **defaulting keywords, needing something else first**: script
  (`revert-val-004` after its class change, `-011`, `all-prop-revert-noop`,
  `all-prop-revert-layer-noop`, `all-prop-revert-layer` (every property of
  a browser), `unset-value-storage`, `inline-style-*`), animations and
  transitions (`revert-layer-008`, `-010`, `-011`, `revert-val-003`, `-006`,
  `-007`), a user agent sheet with `h1` margins (`revert-val-005`, which
  also needs logical properties), shadow trees (`revert-layer-004`, `-013`,
  `-014`, `-015`, `revert-rule-shadow`), `@scope` (`#test4` of
  `nesting-revert-rule`), `:visited` (`all-prop-*-visited`), XML
  documents (`all-prop-initial-xml`);
- **`revert-rule-cycle.tentative.html`** (tentative): it expects a cycle
  between `revert-rule !important` and `revert-layer` to give `unset`.
  The spec text rolls back "as if" the reverted declarations were absent;
  reading that as cumulative, as the other `revert-rule` tests need (a
  chain of `revert-rule` works), there is no cycle and the result is the
  lower layer's value (`red` there). Recorded here, not as an erratum,
  until the test leaves tentative status;
- **shadow trees**: `layer-slotted-rule`, `layer-order-invalidation-shadow-dom`,
  `host-nesting-*`;
- **pseudo-element styles**: the `::after` case of
  `nested-declarations-matching`;
- **other at-rules** (`@font-face`, `@keyframes`, `@counter-style`,
  `@property` in layers): `layer-*-override`;
- **rendering**: the reftests.

`crates/style/tests/common/reference.rs` is a literal model written
separately: placement of `@import` and `@namespace` checked against the
list of every earlier valid rule, layers kept as full names and ordered by
comparing them segment by segment, nested selectors made absolute as text
(`& ` written in front) and parsed again, every declaration tried on every
element with a fresh matcher, and a sort that follows css-cascade-5 6.1
criterion by criterion. Random sheets (nesting, named, nested, anonymous
and invalid layers, layer statements, `@namespace` and `@import` in and
out of place, dropped at-rules, invalid selectors, `!important`, three
origins) on random trees with style attributes, in and out of quirks mode,
must give every element the same declarations in the same order (100,000
cases passed locally). `crates/style/tests/scaling.rs` (with
`CSS_SCALING=1`) times 14 hostile shapes (many rules, long selector lists,
many classes, deep and wide trees, many layers, deep layers, deep nesting
with many rules, long style attributes), which must grow linearly.
