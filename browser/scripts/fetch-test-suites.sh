#!/usr/bin/env bash
# Fetch the conformance suites the browser engine is tested against, at pinned
# commits, into browser/build/. They are test data under their own licences
# (html5lib-tests: MIT; web-platform-tests: BSD-3-Clause) and are not vendored.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p build

fetch() { # name url commit
  local dir="build/$1" url="$2" commit="$3"
  if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD)" = "$commit" ]; then
    echo "$1 already at $commit"
    return
  fi
  rm -rf "$dir"
  git init -q "$dir"
  git -C "$dir" remote add origin "$url"
  for attempt in 1 2 3 4; do
    if git -C "$dir" fetch -q --depth 1 origin "$commit"; then break; fi
    [ "$attempt" = 4 ] && { echo "failed to fetch $1" >&2; exit 1; }
    sleep $((attempt * 5))
  done
  git -C "$dir" checkout -q FETCH_HEAD
  echo "$1 at $(git -C "$dir" rev-parse HEAD)"
}

# html5lib-tests: tokenizer suite. (Tree construction tests moved to WPT.)
fetch html5lib-tests https://github.com/html5lib/html5lib-tests.git c5495c539ca8a79c3c5107f5f14010bcff9a35d5

fetch_sparse() { # name url commit path... (directories or files)
  local name="$1" dir="build/$1" url="$2" commit="$3"
  shift 3
  local paths=("$@") path="$*" patterns=()
  for p in "${paths[@]}"; do patterns+=("/$p"); done
  if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD)" = "$commit" ] \
    && [ "$(git -C "$dir" sparse-checkout list | tr '\n' ' ')" = "${patterns[*]} " ]; then
    echo "$name already at $commit"
    return
  fi
  rm -rf "$dir"
  git init -q "$dir"
  git -C "$dir" remote add origin "$url"
  git -C "$dir" sparse-checkout set --no-cone "${patterns[@]}"
  for attempt in 1 2 3 4; do
    if git -C "$dir" fetch -q --depth 1 --filter=blob:none origin "$commit"; then break; fi
    [ "$attempt" = 4 ] && { echo "failed to fetch $name" >&2; exit 1; }
    sleep $((attempt * 5))
  done
  git -C "$dir" checkout -q FETCH_HEAD
  echo "$name at $(git -C "$dir" rev-parse HEAD) ($path only)"
}

# web-platform-tests: the tree construction tests (html/syntax/parsing).
fetch_sparse wpt https://github.com/web-platform-tests/wpt.git 6852422b49e7dc8037d7f130ee07cf991fb73f05 html/syntax/parsing/resources

# web-platform-tests again, at the same commit: the CSS syntax tests
# (css/css-syntax), whose An+B cases the css crate runs, and the selector
# parsing tests (css/selectors/parsing).
fetch_sparse wpt-css https://github.com/web-platform-tests/wpt.git 6852422b49e7dc8037d7f130ee07cf991fb73f05 css/css-syntax css/selectors/parsing css/css-cascade css/css-nesting

# web-platform-tests again, at the same commit: the Selectors API tests
# (dom/nodes), whose selectors.js cases and fixture the selectors crate's
# matching runs.
fetch_sparse wpt-dom https://github.com/web-platform-tests/wpt.git 6852422b49e7dc8037d7f130ee07cf991fb73f05 dom/nodes/selectors.js dom/nodes/ParentNode-querySelector-All.js dom/nodes/ParentNode-querySelector-All-content.html dom/nodes/ParentNode-querySelector-All-content.xht

# romainmenke/css-tokenizer-tests (MIT): the CSS tokenizer suite.
fetch css-tokenizer-tests https://github.com/romainmenke/css-tokenizer-tests.git 0db1d128d02f0e3e38c9271f377eb8d70f280813

# SimonSapin/css-parsing-tests (CC0): CSS parser tests (component values,
# rules, declarations, An+B, colours). Some predate the 2026 css-syntax-3
# text; corrections are in conformance/css-parsing-errata.json.
fetch css-parsing-tests https://github.com/SimonSapin/css-parsing-tests.git 203ce36bffd617db7f118c551e32794561fb273d

# csswg-drafts again, at the same commit: the specs whose property definition
# tables the style crate's property table copies. The table is checked against
# them (crates/style/tests/property_table.rs), so they are fetched every time.
fetch_sparse csswg-propdefs https://github.com/w3c/csswg-drafts.git 7027ab6fdf3c98d9c44666820041cb075b0e28dd css-align-3 css-backgrounds-3 css-box-4 css-break-3 css-color-4 css-content-3 css-display-4 css-flexbox-1 css-fonts-4 css-inline-3 css-lists-3 css-overflow-3 css-position-3 css-sizing-3 css-tables-3 css-text-4 css-transforms-1 css-ui-4 css-writing-modes-4 css2 css-cascade-5

# The specs the CSS crate is written from, at the commit it follows. Not test
# data, so only fetched on request: ./scripts/fetch-test-suites.sh --specs
if [ "${1:-}" = "--specs" ]; then
  fetch_sparse csswg-drafts https://github.com/w3c/csswg-drafts.git 7027ab6fdf3c98d9c44666820041cb075b0e28dd css-syntax-3 selectors-4 cssom-1 css-pseudo-4 css-shadow-1 css-nesting-1 css-namespaces-3 css-text-3 css-cascade-5 css-values-4
  # WHATWG HTML (the selector case-sensitivity rules, and the pseudo-classes
  # HTML defines) and DOM (scope-match a selectors string).
  fetch_sparse whatwg-html https://github.com/whatwg/html.git 30f7046075a1c19999caef3a52388f64d96acad7 source
  fetch_sparse whatwg-dom https://github.com/whatwg/dom.git ddfb52fde44069e2f77b839bdf114eeefc87e3f7 dom.bs
fi
