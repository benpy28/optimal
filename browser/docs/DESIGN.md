# Optimal browser engine: design

This is the "built by us" web browser engine from the roadmap. It is a long
track: a borrowed browser ships on Optimal until this engine passes its
gates. It is developed as plain Rust crates that build and test on any host,
so it can move fast before the OS can run it, and it moves onto Optimal when
the desktop phase lands.

Each choice below is logged as: chosen, why, rejected.

## Goals, in pillar order

1. **Performance.** Parse, style, lay out and paint faster than the major
   engines on the same hardware, measured on a published page set. Text
   paths scan bytes, tokens borrow buffers instead of allocating, and every
   stage is designed for incremental work (streaming parse, style and layout
   invalidation) from the start, because retrofitting incrementality is what
   makes engines slow.
2. **Security and privacy.** Nobody can read a user's browsing remotely. The
   engine is memory-safe by construction, every site runs in its own
   sandboxed process with no ambient authority, and there is no telemetry.
3. **Lean.** No background work when no page needs it. Memory per tab is
   tracked and budgeted.
4. **Compatibility.** The standards, exactly: WHATWG HTML and DOM, CSS specs,
   ECMAScript. Pass rates on the official suites gate every stage.

## Rules

- **Clean room.** Written from the public specifications only. Nothing is
  copied or ported from Servo, Blink, WebKit, Gecko, Ladybird or any other
  engine, and no GPL or MPL code is used. Test suites (html5lib-tests, MIT;
  web-platform-tests, BSD-3-Clause) are used as test data under their own
  licences and are fetched, not vendored. Spec data tables (the WHATWG named
  character reference list, CC BY 4.0) are vendored with attribution.
- **Stable Rust**, edition 2024. Every crate has `#![forbid(unsafe_code)]`.
  If a measured hot spot ever needs `unsafe`, it goes into one small crate of
  its own whose every `unsafe` block carries a safety argument and a Kani
  proof or Miri test, and the rest of the engine stays `forbid`.
- **Few dependencies.** Runtime crates depend only on each other. Test-only
  dependencies (serde_json to read test data, proptest) are allowed.
- **Gated by conformance.** A stage is done when its suite passes at the
  gate below, in CI, with the pass rate published.

## Architecture

```
            +----------------------- browser process (UI, tabs, profile) ----+
            |  owns: window surface, user profile store, permission prompts   |
            +--------+----------------------+-----------------------+--------+
                     | IPC                  | IPC                   | IPC
            +--------v--------+   +---------v--------+    +---------v---------+
            | network process |   | renderer: site A |    | renderer: site B  |
            | DNS, TLS, HTTP, |   | HTML, CSS, DOM,  |    | (one process per  |
            | cache, cookies  |   | JS, layout, paint|    |  site, always)    |
            +-----------------+   +------------------+    +-------------------+
                     plus: GPU/compositor process, media/decoder utility processes
```

- **Process per site, always** (site = scheme + registrable domain). Cross-site
  iframes get their own process. Why: a renderer bug then leaks only its own
  site's data (the Spectre lesson). Rejected: process per tab (shares sites
  across a process), single process (one bug reads everything).
- **Renderers hold no authority.** On Optimal a renderer starts with an empty
  capability table plus one portal: its IPC channel to the browser process
  (see `design/capabilities.md`). It has no file view, no sockets, no
  devices, a fixed memory budget, and no debug capability on anything. It
  asks the network process for resources by URL, and the network process
  enforces CORS, CORB/ORB, mixed-content and cookie partitioning, so a
  compromised renderer cannot fetch another site's data with credentials.
  During host development the same boundary is drawn with Linux namespaces
  and seccomp.
- **Decoders run sandboxed too.** Image, font and media decoding happen in
  utility processes (or in the renderer that asked, never in the browser
  process), so a malformed file can only crash its own sandbox.
- **Storage is partitioned** by top-level site (cookies, cache, local storage,
  IndexedDB), so cross-site tracking through storage does not work.
- **Privacy defaults.** No telemetry, no crash upload without asking, no
  remote services built in. Sync, when it exists, is end-to-end encrypted
  with keys only the user's devices hold.

## Crates and stages

| Stage | Crate | Gate |
|---|---|---|
| 1 | `optimal-wtf8`: web strings (UTF-8 plus lone surrogates) | property tests against a UTF-16 model |
| 2 | `optimal-html`: tokenizer | html5lib-tests tokenizer suite, 100%, with error codes and positions, under any input chunking |
| 3 | `optimal-html`: tree builder, `optimal-dom` | WPT `html/syntax/parsing` (the html5lib tree tests), 100% |
| 4 | `optimal-css` (syntax), `optimal-selectors`, `optimal-style` (cascade) | WPT `css/css-syntax`, `css/selectors`, `dom/nodes` (Selectors API), `css/css-cascade` |
| 5 | `optimal-style`, `optimal-layout` (block, inline, text), `optimal-paint` (software rasterizer into a framebuffer) | WPT reftests for CSS 2 box model, inline and block |
| 6 | Flexbox and grid | WPT `css/css-flexbox`, `css/css-grid` |
| 7 | `optimal-js`: ECMAScript engine (interpreter first) | test262 |
| 8 | `optimal-net`: URL, fetch, HTTP/1.1, HTTP/2, TLS 1.3 | WPT `url`, `fetch`; TLS against known-answer tests |
| 9 | Integration: event loop, DOM bindings, process split | Acid3, then the 50 most-visited sites render correctly |

Stages 7 and 8 are planned in detail when stage 5 lands; their main open
questions are written down now so early stages don't paint them into a corner:

- **JS engine.** Interpreter first (register bytecode), precise moving GC
  designed in from day one (a GC retrofitted later is the classic source of
  engine bugs), then a baseline JIT. JIT code needs writable-then-executable
  memory, which conflicts with W^X; the plan is a separate JIT-compilation
  step that maps code read+execute only after it is written (never W+X at
  once), and a "no JIT" mode for the highest security setting.
- **Networking.** URL parsing and fetch follow the WHATWG specs. TLS is the
  one place where a borrowed, audited implementation may be the safer
  choice; the decision (and its reasoning) will be logged before stage 8
  starts.
- **DOM bindings.** The DOM is designed for a JS heap to own wrappers later:
  nodes live in an arena with generation-checked handles, so a stale
  reference is a checked error, never a use-after-free.

## Testing

- Every conformance suite runs in CI on every pull request. Results are
  printed to the run summary and kept in `browser/conformance/`.
- Errors in a suite's own expected data are recorded in an errata file with
  the reason and a spec-derived correction; the test then checks against the
  correction. The harness fails if an erratum stops matching, so fixes
  upstream are noticed. Nothing is skipped silently.
- Property tests check every streaming and chunking invariant against the
  whole-input result; seeds are printed on failure.
- Mutation testing (`cargo mutants`) runs in CI: each surviving mutant is
  either killed by a new test or recorded with the reason it is harmless.
- Hostile inputs (huge attribute counts, deep nesting, giant numbers) have
  tests with time limits, so denial-of-service regressions fail CI.
