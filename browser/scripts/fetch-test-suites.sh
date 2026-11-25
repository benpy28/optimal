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
