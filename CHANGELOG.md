# Changelog

All notable changes to `styloria` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
styloria is pre-1.0, so new features and breaking changes both land as
minor-version bumps (`0.x.0`), per [Cargo's SemVer compatibility
rules](https://doc.rust-lang.org/cargo/reference/semver.html).

## [Unreleased]

CSS Syntax Level 3 as of the **1 October 2026 Candidate Recommendation
Draft**, and one parse tree instead of two. This is a breaking release: the
per-level entry points are gone, and a nested rule in a style rule's block is
now a rule rather than an error.

### Changed

- **One call parses everything.** `parse_stylesheet(css)` returns the whole
  tree and every `SyntaxError` in it, sorted by position; `parse_block_contents(css)`
  does the same for a `style="…"` attribute. Every node carries its span, as
  the `spanned` tree did. Before, parsing stopped at each `{ … }` and a caller
  descended one level per call, because the 2021 text left a block's meaning
  to each at-rule's own spec. The 2026 CRD reads *every* rule's block with the
  same §5.5.5 "consume a block's contents", so that reason is gone.
- **A block is a list of `BlockItem`s** — `Declaration` or `Rule` — in source
  order. The spec groups consecutive declarations; the flat list loses
  nothing and is the order a validator reports in. `QualifiedRule::block` and
  `AtRule::block` are `Spanned<Vec<BlockItem>>` (alias `Block`).
- **Each block item is a declaration or a nested rule, decided as §5.5.5
  says.** What that changes, from 0.11:

  | input | 0.11 | 0.12 |
  |---|---|---|
  | `p { color: red; a { color: blue } }` | `MalformedDeclaration` at `a` | a nested rule, no error |
  | `p { . a { } }` | `MalformedDeclaration` | a nested rule + `InvalidSelector` |
  | `p { a { } color: red; }` | one malformed declaration up to `;` | a rule, then a declaration |
  | `@media x { color: red }` | `UnterminatedRule` | a declaration, no error |
  | a rule inside `@font-face { … }` | skipped in silence | a rule, visible |
  | `p { color: red {} }` | a declaration | a rule + `InvalidSelector` (§5.5.6: a `{}` block may only be a whole value) |
  | `@media print { p { } i` (trailing fragment) | `UnterminatedRule` | `MalformedDeclaration` |
  | `--foo:hover { … }` at top level | a qualified rule | dropped, reported as `DroppedCustomPropertyRule` |

  `color red;` and `1px: red;` are still one `MalformedDeclaration` /
  `UnexpectedToken` each. Which kinds can occur at the top level and which
  inside a block is now a table on `SyntaxErrorKind`.
- **`!important` follows the spec's rule** everywhere: the last two
  non-whitespace values, stripped from the value. The value-based path used
  to accept any `!` and any `important`, and kept both in the value.
- **The at-rule table no longer decides how a block is parsed**, only whether
  nested preludes are checked as selectors (`@media`, `@supports`,
  `@container`, `@layer`, `@scope`, `@document`, `@starting-style`; not
  `@keyframes`, not at-rules this crate does not know).
- **Non-ASCII ident code points are the CRD's narrower set**
  (w3c/csswg-drafts#7129): letters of every script still are, but U+00A0
  NO-BREAK SPACE, the other Unicode spaces and the C1 controls now tokenize
  as delims. No stylesheet on the 852-sheet test shelf changed.
- **`validate` walks the tree** instead of re-parsing `@media` bodies, and
  so reaches declarations it could not before: in nested style rules, in
  `@keyframes` blocks, directly inside a conditional group rule, and in a
  `style` attribute's nested rules.
- **`serialize` writes the tree.** `serialize_declaration_list` is now
  `serialize_block_contents`.

### Added

- **`Token::BadUrl` carries its raw text**, from `url(` to the `)` that ended
  recovery, so a caller can quote it without re-tokenizing.
- **`Token::UnicodeRange { start, end }`.** §5.5.6 re-reads the value of a
  `unicode-range` declaration with unicode ranges allowed, so
  `U+0-7F, U+4??` comes back as two ranges instead of an ident, numbers and
  dimensions. Nowhere else: `u+a { }` is still a selector.
- **`SyntaxErrorKind::DroppedCustomPropertyRule`** for a top-level rule that
  starts like a custom property. §5.5.3 drops it without naming a parse
  error; it is reported so the dropped content leaves a trace. The span
  covers the whole construct.
- **`validate_relative_selector_list`.** A style rule nested in a style
  rule has a relative selector list for a prelude (CSS Nesting §2.1), so
  `p { > a { } }` is valid and is not reported; `> a { }` at the top level
  still is.
- `validate_parsed_stylesheet` and `validate_parsed_block`, for a caller that
  already holds the tree; `Rule::prelude`, `Rule::block` and
  `BlockItem::span`.

### Removed

- The position-less `Parser` and its types (`styloria::Stylesheet` and
  friends at the crate root are now the spanned ones), the `spanned`
  module, `parse_rule_list`, `parse_declaration_list_from_values`,
  `parse_at_rule_block`, `BlockContents`, `DeclarationListItem`,
  `parse_declaration_list(_with_errors)` and `parse_stylesheet_with_errors`.
  `SPAN_PROTOTYPE.md`, which described the two-tree design.

### Fixed

- **`validate_stylesheet` was quadratic in `@media` nesting**, and then
  overflowed the stack: it re-parsed each conditional group rule's body
  from text, one level at a time. 100,000 nested `@media` ran for minutes and
  then aborted the process. It is now one walk over the tree; the same input
  parses and validates in about 10 ms.
- **A backslash at the end of input is an escape** (U+FFFD), per §4.3.8,
  which only disqualifies a newline (WPT `escaped-eof.html`). It was a delim.
- **A NUL is U+FFFD**, an ident code point (§3.3), inside names and strings
  as well as escapes. It was a delim.
- The serializer escapes a newline in a `url()` value by code point (a
  backslash-newline is not an escape), escapes non-ASCII code points that are
  not ident code points, and writes a bad string so it re-reads as one
  rather than swallowing what follows.

### Performance

The input is tokenized once and every bracket paired with its closer in the
same pass, so "try a declaration, then re-read as a rule" is decided by
looking at one level of tokens and building the item once — linear however
deeply the input nests. Property names are looked up in a hash table built
at compile time rather than by binary search over a lower-cased copy.

On the 852 stylesheets of the test shelf, `parse_stylesheet` plus
`validate_stylesheet` went from 125 ms to 50 ms per pass, with the same 34
errors at the same positions; validating the tree already parsed
(`validate_parsed_stylesheet`) brings it to 21 ms. Parsing alone runs at
about 130 MB/s. Peak memory on the 3–4.5 MB hostile inputs measured is below
0.11's.

## [0.11.0] - 2026-08-18

### Added

- **`parse_at_rule_block`** — read an at-rule's `{ … }` block as whatever
  that at-rule holds, returning `BlockContents::Declarations` or
  `BlockContents::Rules` ([#4](https://github.com/veripublica/styloria/issues/4)).

  0.10.0 gave a caller both readings of a block; it did not say **which** one
  a given at-rule wants, so a consumer kept that table itself. That table is
  a fact about CSS, not about the consumer, and the copy in epubveri was
  incomplete in the way such a copy always becomes: it knew the
  conditional-group rules and not `@keyframes`, so a keyframe block was read
  as declarations and `0% { opacity: 0 }` came back as one malformed
  declaration — an error on valid CSS, measured against epubcheck, which
  reports nothing there. `@-webkit-keyframes`, `@starting-style` and any
  unregistered at-rule holding rules failed the same way.

  Three readings, and the middle one is why the table cannot live in a
  caller's list of names:

  - **Rules with selectors** — `@media`, `@supports`, `@container`,
    `@layer`, `@scope`, `@document`, `@starting-style`. Preludes go through
    `validate_selector_list` as before.
  - **Rules whose preludes are not selectors** — `@keyframes`. A keyframe
    selector (`from`, `to`, `0%`) is correct under CSS Animations 1 §3 and
    malformed under Selectors 4, so simply adding `keyframes` to a grouping
    list turns one invented error into two. Preludes come back unexamined;
    this crate carries no keyframe-selector grammar, and inventing one would
    be a restrictive check nobody asked for.
  - **Declarations** — everything else, including at-rules this crate has
    never heard of. There a chunk shaped like a nested rule is skipped in
    silence rather than blamed, which is the direction the unknown case has
    to fail in: CSS keeps gaining at-rules, so the table is permanently one
    release behind the language, and its ignorance must not become an error
    on a valid stylesheet. A malformed *declaration* is still reported —
    that is malformed whatever the block turns out to hold.

  A vendor prefix is stripped before the lookup, which is what carries
  `@-webkit-keyframes` and `@-moz-document`.

  This does not take back the deferral `parse_rule_list` and
  `parse_declaration_list_from_values` were built on. **When** to descend
  into a block is still entirely the caller's decision, and
  `parse_stylesheet_with_errors` still reports about rules and says nothing
  about what is inside them. What moved here is **what** a block holds.

### Fixed

- **An at-rule in a declaration list is returned rather than dropped.**
  `parse_declaration_list_from_values` skipped an `@…` chunk in silence while
  the text-based `parse_declaration_list_with_errors` returned it as a
  `DeclarationListItem::AtRule`. Neither is a parse error — §5.4.2 consumes
  an at-rule in a declaration list quite happily — but a caller that wants to
  judge whether one is *misplaced* (a nested at-rule in a style rule's block
  is CSS Nesting, which the CSS Snapshot puts outside the official definition
  of CSS) could not see the construct at all, and a validator built on this
  had a false negative it could not have found from here.

  The "both entry points agree" test could not see it either: its ten inputs
  were all declarations. It now compares the returned items and not only the
  error kinds, over at-rule inputs as well — checked by re-dropping them and
  watching it fail.

### Changed

- **The docs now say which entry point reports what.** `MalformedDeclaration`
  was documented as though it were a property of the CSS rather than of the
  call you made — it cannot come from `parse_stylesheet_with_errors`, which
  does not descend into blocks. That silence is what #4 was opened about, and
  it cost a consumer a hand-rolled declaration walk. The `spanned` module
  header now carries the table of which entry point to reach for given what
  you hold.

## [0.10.0] - 2026-08-17

### Added

- **`parse_declaration_list_from_values`** — read already-parsed component
  values as a declaration list, returning the declarations and their
  `SyntaxError`s ([#4](https://github.com/veripublica/styloria/issues/4)).

  The twin of `parse_rule_list`, and needed for the same reason. A `{ … }`
  block holds either rules or declarations; CSS Syntax Level 3 does not say
  which, and only the caller knows. `parse_rule_list` already offered that
  interpretation for component values — the declaration side accepted source
  text only, which a caller holding a block no longer has. So the one
  remaining way to read a block as declarations was to write the walk
  yourself, and a consumer did: "is this a well-formed declaration" had ended
  up outside the CSS crate purely because of that asymmetry.

  Input is component values rather than text for the same reason
  `parse_rule_list` takes them: **spans stay absolute**, so a caller can point
  at a line and column in the original stylesheet rather than into a
  re-tokenized fragment.

  An empty chunk (`{;}`, `a;;b`) is not an error — §5.4.4 discards a stray
  `<semicolon-token>`, so it is valid CSS and this stays silent about it.

### Fixed

- **A malformed declaration is one error, not one per token.** §5.4.2 says a
  parse error in a declaration list discards component values up to the next
  `<semicolon-token>` or EOF. The ident branch did that; the fallback branch
  discarded a single component value and looped, so `1px: red` produced
  `UnexpectedToken` twice and then `MalformedDeclaration` — three errors for
  one broken declaration, which a consumer mapping every kind to one message
  id reports three times. Recovery still resumes at the next declaration:
  `1px: red; color: blue` is one error and one declaration.

  Same shape as the selector fix in 0.9.1 (#3), arriving by a different
  route. Both entry points are now asserted to agree over a set of inputs, so
  the text-based and value-based paths cannot drift.

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
