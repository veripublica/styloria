# Changelog

All notable changes to `styloria` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
styloria is pre-1.0, so new features and breaking changes both land as
minor-version bumps (`0.x.0`), per [Cargo's SemVer compatibility
rules](https://doc.rust-lang.org/cargo/reference/semver.html).

## [0.9.1] - 2026-08-17

### Fixed

- **A broken selector is reported once, not once per bad token**
  ([#3](https://github.com/veripublica/styloria/issues/3)).
  `validate_selector_list` pushed a `SyntaxError` for every component value it
  could not accept, so a badly broken prelude became a pile rather than a
  finding. A stylesheet beginning with a stray declaration is the shape that
  exposed it: per CSS Syntax the whole of `text-indent:1.5em;\n@page` becomes
  one qualified rule's prelude, and the walk blamed the `:`, the `1.5em`, the
  `;` and the `@page` of the *next* rule — four errors for one mistake, the
  last of them pointing at well-formed CSS.

  The reporting unit is now the **comma-separated selector**, which is neither
  of the two wrong answers. Per token is the pile above; per prelude would
  lose the case that motivates the check at all, since
  `. h-100, . y-100 { }` really is two independently broken selectors and a
  caller fixing them needs both named. That case still reports two.

  This is not "stop at the first error": each part is still walked in full,
  because `validate_complex` recurses into attribute selectors and the walk is
  what finds them. Only the reporting is capped, and a test pins the
  difference — two malformed attribute selectors in two parts stay two errors.

  No API change. Callers that count `SyntaxErrorKind::InvalidSelector` will
  see fewer of them on malformed input and no change on valid input; measured
  downstream against a 336-book corpus with epubcheck as the oracle, exactly
  one book moves, from 4 findings to epubcheck's own 1.

## [0.9.0] - 2026-08-09

### Added

- **`parse_rule_list`** — read already-parsed component values as a rule list,
  returning the rules and their `SyntaxError`s. For the body of a
  conditional-group at-rule (`@media`, `@supports`, `@container`, `@layer`),
  which holds *rules* where an `@font-face` or `@page` body holds
  *declarations*.

  Preludes go through `validate_selector_list`, exactly as they do for a
  top-level rule. Before this, a malformed selector was reported at the top
  level and **silently accepted one `@media` deep** — CSS Syntax §5.4.2 hands
  an at-rule's block on as a simple block, and nothing inside it was ever
  re-entered as a rule, so the selector check was never reached (issue #2).

  It takes component values rather than source text so that spans stay
  **absolute**: errors point into the original stylesheet, not into a
  re-tokenized fragment.

  The crate still carries no per-at-rule knowledge — CSS Syntax does not say
  which at-rules hold rule lists and which hold declarations, so the caller
  decides *when* to call this. What the caller no longer has to reimplement is
  where each nested rule's prelude ends.

## [0.8.0] - 2026-08-04

### Added

- **`type_selector_names`** — every element name written bare at the head of a
  compound selector in a qualified rule's prelude, with its span. `h4.note em`
  yields `h4` and `em`; classes, ids, attributes, pseudos and `*` yield
  nothing, and a namespace-qualified `svg|circle` yields the local name.

  It reports what a selector *names*, not whether the name is a real element —
  that needs a vocabulary this crate does not have. It exists so a consumer can
  lint for a type selector that can match nothing: `h4a` is valid CSS and a
  typo for `h4` or `.h4a`, and is otherwise invisible.

## [0.7.1] - 2026-08-03

**No library changes** — the parser, the API and the output are identical to
0.7.0. A patch, so consumers on `0.7` pick it up (or ignore it) without
touching their manifests.

### Internal

- The release guard reads the manifest version with **jq** instead of an
  inline `python3` one-liner, removing the last interpreter the release path
  depended on. `cargo metadata` still supplies the JSON — it is cargo's own
  parse of the manifest, the same source `cargo publish` reads.

  `jq -e` is load-bearing rather than decoration: the python one-liner raised
  `StopIteration` if the package was missing, but a bare jq filter prints
  nothing and exits 0, which would compare the tag against an empty string
  and pass. With `-e` a filter that matches nothing exits 4, and `pipefail`
  carries that out of the pipe.

  Verified in both directions before release — a mismatched tag exits 1 with
  its `::error::` line, a missing package exits 4 — because this guard stands
  in front of an upload that can never be undone. Publishing is also the only
  way to exercise the changed guard in CI: `verify_only` checks out an
  existing tag, and every existing tag predates the change.

## [0.7.0] - 2026-08-03

A bound on parser recursion. Before this release a ~1.2 KB stylesheet could
abort the host process.

### Fixed

- **Deeply nested CSS no longer overflows the stack.** "Consume a component
  value" and "consume a simple block" are mutually recursive in CSS Syntax
  Level 3, and nothing bounded the descent, so nesting cost one stack frame
  per level. Four shapes reach it — `a{color:((((…))))}`,
  `@media all{@media all{…}}`, `rgb(rgb(rgb(…)))` and `:is(:is(:is(…)))` —
  and on 0.6.1 all four abort between 10,000 and 20,000 deep on an 8 MiB
  main thread, proportionally sooner on a 2 MiB worker thread. In Rust a
  stack overflow is `SIGABRT`, not a catchable panic, so no caller could
  defend against this downstream; the bound has to live here.

  Both parsers are bounded — the plain one and `spanned` — since a consumer
  may use either, and fixing one would have left the other open.

### Added

- **`MAX_NESTING_DEPTH`** (256), the new limit, public so consumers can
  reason about it. Sized from data rather than taste: across a 65-book EPUB
  shelf the deepest stylesheet nests **2** (median 2, p95 2), CSS getting
  deep only through `@media`-wrapped rules and nested functions. That leaves
  the limit ~128x above real-world CSS and far below the crash.

- **`SyntaxErrorKind::NestingTooDeep`**, reported by `spanned` when the
  limit is reached. Unlike the other variants this is not a defect in the
  CSS — it says the parser declined to descend and the content below that
  point went unparsed. Reporting it rather than truncating silently is the
  point: a caller cannot otherwise tell a refused stylesheet from a shallow
  one.

  Past the limit the parser stops descending but keeps consuming, so the
  token stream stays balanced and the rest of the stylesheet still parses
  normally.

### Breaking

- `SyntaxErrorKind` gained a variant, so an exhaustive `match` on it needs a
  new arm. That compile error is intended — it is how a consumer finds out
  there is a new condition to classify.

## [0.6.1] - 2026-07-26

**No library changes** — the parser, the API and the output are identical to
0.6.0. This release exists to exercise the new automated pipeline end to end,
and is a patch so that consumers on `0.6` pick it up (or ignore it) without
touching their manifests.

### Internal

- Releases now publish to crates.io from CI, authenticated by **trusted
  publishing (OIDC)**: a `v*` tag creates the GitHub Release *and* uploads the
  crate, with no stored registry token and nothing typed by hand. Guarded by
  tag/manifest version agreement, a no-op skip when the version is already
  published, and the test suite against the tagged commit.
- First CI workflow: `cargo fmt --check`, clippy with `-D warnings`, the
  tests, and a `wasm32-unknown-unknown` build on every push and PR. The wasm
  build is there for downstream consumers that ship WebAssembly — it catches
  a wasm-incompatible change on the commit that causes it.

## [0.6.0] - 2026-07-26

### Added

- **`SyntaxErrorKind::InvalidUnicodeRange`** — a `U+…` unicode-range with more
  than six hex digits in a run, which is more than any code point needs
  (`U+10FFFF` is the maximum), so malformed under any reading. This mirrors
  what epubcheck's CSS scanner reports as `SCANNER_ILLEGAL_URANGE`, and its
  rule really is only that: nothing checks range ordering, whether `?` only
  trails, or whether the endpoints make sense.

  Detection is anchored on the `u` **token**, so a `U+00000000` inside a
  string or a comment cannot be mistaken for a range — scanning the source
  text directly would report both. The character run is then counted in the
  source, because it does not survive tokenization: CSS Syntax Level 3 dropped
  the unicode-range token, so `U+0-7F` arrives as an ident, a number, a delim
  and a dimension.

### Breaking

- `SyntaxErrorKind` gained a variant, so an exhaustive `match` on it needs a
  new arm. (Pre-1.0: breaking changes land as minor bumps.)

## [0.5.0] - 2026-07-25

Selector-list validation. CSS Syntax Level 3 hands a qualified rule's prelude
on without looking at it, so `a > > b { }` parses as happily as valid CSS —
correct per that spec, and useless to a tool reporting malformed CSS.

### Added

- **`selector::validate_selector_list(prelude) -> Vec<SyntaxError>`** (also
  re-exported at the crate root), and the new
  **`SyntaxErrorKind::InvalidSelector`** it reports. `parse_stylesheet_with_errors`
  and `syntax_errors` now run it on every qualified rule, so existing callers
  pick the new errors up with no code change.

  The check is **syntactic only** — it never asks whether an element,
  pseudo-class or attribute *name* is real, so `dvi:hovr[hrefff]` passes. It
  reports the shapes a selector list cannot have: a combinator with nothing on
  one side (`> p`, `a > > b`), an empty side of a comma, `.`/`|`/`:` with no
  name after it, a malformed attribute selector (`[]`, `[=x]`, `[href=]`), and
  tokens that cannot start a simple selector.

  **Deliberately permissive in two named places**, because a false positive
  here lands on somebody's real stylesheet: functional pseudo arguments are
  not inspected at all (`:not()`, `:is()`, `:has()`, `:nth-child()` carry
  grammars of their own that keep growing), and anything merely newer than
  this code is accepted — `&` nesting, `::part()`, an unrecognised pseudo
  name. Only shapes no version of Selectors can produce are reported.

### Fixed

- **A leading UTF-8 BOM (`U+FEFF`) is no longer tokenized as content.** CSS
  Syntax §3.2 consumes it while determining the encoding, but a caller that
  decoded the bytes itself (the common `from_utf8_lossy` case) hands it
  straight through. Left in place it became a delim, which turned a following
  `@charset` into a qualified rule's prelude and cascaded errors through the
  rest of the stylesheet. Latent before this release; surfaced by the new
  selector check on a real-world fixture.

### Breaking

- `SyntaxErrorKind` gained the `InvalidSelector` variant, so an exhaustive
  `match` on it needs a new arm. (Pre-1.0: breaking changes land as minor
  bumps.)

## [0.4.0] - 2026-07-24

Syntax-error reporting: the parser recovers from malformed CSS per the CSS
Syntax spec, and can now hand back *what* it recovered from and *where* —
previously discarded silently. Additive; the tree and existing types are
unchanged.

### Added

- **`spanned::parse_stylesheet_with_errors(css) -> (Stylesheet, Vec<SyntaxError>)`**
  and **`spanned::parse_declaration_list_with_errors(css)`**, plus the
  convenience **`spanned::syntax_errors(css) -> Vec<SyntaxError>`**. The plain
  `parse_stylesheet` / `parse_declaration_list` are unchanged (they now discard
  the collected errors).
- **`SyntaxError { span, kind }`** and **`SyntaxErrorKind`** (`BadString`,
  `BadUrl`, `MalformedDeclaration`, `UnterminatedRule`, `UnterminatedBlock`,
  `UnexpectedToken`), re-exported at the crate root. A syntax error is purely
  positional (span + reason, no name), distinct from a semantic `Diagnostic`.

### Changed

- A malformed declaration in a declaration list (`name` with no `:`) now
  discards up to the next `;`/EOF per CSS Syntax §5.4.2, instead of letting its
  leftover tokens be re-parsed as a second spurious declaration. The item list
  is unchanged (a failed declaration yields no item); only the (new) error
  reporting is affected.
- Moved to Rust **edition 2024** (`rust-version = 1.88`), matching the sibling
  crates. No API or behavior change.

## [0.3.0] - 2026-07-18

A **validation** layer on top of the property-agnostic parser: it knows the
CSS vocabulary and reports names CSS does not define, each pinned to the exact
span a tool can underline. The parser and existing types are unchanged — this
is purely additive.

### Added

- **`validate_stylesheet(css) -> Vec<Diagnostic>`.** Checks each declaration's
  *name*. In style rules the name must be a known CSS property; the check
  descends into conditional group rules (`@media`, `@supports`, `@container`,
  `@layer { … }`, `@scope`) to reach nested rules.
- **Descriptor at-rules are checked against their own vocabularies** —
  `@font-face`, `@counter-style`, `@property`, `@font-palette-values`,
  `@view-transition`. `@page` is validated against the union of its page
  descriptors and ordinary properties, which it legally mixes. At-rules with no
  descriptor list (`@keyframes`, `@font-feature-values`) are left alone.
- **`validate_declaration_list(css) -> Vec<Diagnostic>`** for the contents of
  an inline `style="…"` attribute (a bare declaration list, not a stylesheet).
- **`Diagnostic { span, kind, name }`** with
  **`DiagnosticKind::{ UnknownProperty, UnknownDescriptor { at_rule } }`**.

### Notes

- The known-property table is the union of two authoritative machine-readable
  registries — the W3C "all properties" index and MDN's `css/properties.json`
  (which supplies legacy aliases like `word-wrap` and SVG properties the W3C
  index omits). Descriptor sets come from MDN's `css/at-rules.json`. Both are
  regenerated from source, not hand-maintained.
- Exemptions err toward silence: any leading-dash name (custom `--*` and vendor
  `-webkit-`/`-moz-`/…) is exempt, and lookups are ASCII case-insensitive.
  Failing to flag an unknown name is safe; flagging a real one is not.

## [0.2.0] - 2026-07-04

### Added

- An optional, fully additive **source-span** layer: the tokenizer emits byte
  ranges, and the `spanned` parser threads them up through the stylesheet
  model so a consumer can report the exact `line:column` of anything it finds.
  See the `span` / `spanned` modules and `SPAN_PROTOTYPE.md`. The existing
  position-less parser and types are unchanged.

## [0.1.0] - 2026-07-04

### Added

- Initial release: a pure-Rust [CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/)
  **tokenizer**, **parser** (into a structural stylesheet model of rules,
  qualified rules, at-rules, declarations, and component values), and
  **serializer**. Property-agnostic by design — no C dependencies.
