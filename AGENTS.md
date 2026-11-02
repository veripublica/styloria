# styloria: detail for code agents and maintainers

`README.md` is the short introduction. This file holds the detail, and is
kept true at every release. API docs: <https://docs.rs/styloria>. Past changes:
`CHANGELOG.md`.

## Scope

A pure-Rust, general-purpose CSS parser and serializer, following
[CSS Syntax Level 3](https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/) as
of the 1 October 2026 Candidate Recommendation Draft. It has no dependencies,
and nothing in it is specific to EPUB, although
[epubveri](https://github.com/veripublica/epubveri) is its main consumer.

Rules the code follows:

- **The spec decides.** Where a specification and another tool disagree, the
  code follows the specification.
- **Three-valued answers.** Matching, the cascade and value checks answer
  Yes, No or Unknown. What cannot be settled from the input is Unknown, with
  a reason where the API has one, never a guess.
- **Errs toward silence.** A value is invalid only when even a lenient reading
  fails. Custom properties (`--*`), vendor-prefixed names and `var()` are
  left alone.
- **Bounded on hostile input.** See [Limits](#limits).

## What it declines to do

- **No browser engine.** No computed values, layout, inheritance, or
  user-agent and user styles. The cascade covers author declarations only.
- **No environment.** `@media` conditions are parsed, not evaluated, because
  there is no device to evaluate them against. A rule under a condition is
  Unknown. `@media all` and an empty list count as always holding.
- **No value normalization in comparisons.** `compare_to` reads values as
  written.
- **No document tree.** The caller supplies one through the `Element` trait.
- **No fetching.** `@import` is followed only when the caller hands over the
  imported sheet (`Cascade::with_imports`).
- **No formatting preservation in the serializer.** It writes equivalent CSS:
  comments and original whitespace are gone. To change one rule and keep the
  rest, edit the source text using spans, and use `decode_stylesheet` and
  `encode_stylesheet` for the bytes.
- **No guessing an encoding.** Without the `legacy-cjk` feature, a multi-byte
  legacy encoding is `DecodeError::Unsupported`.
- **No judgement on whether something is allowed where it stands.** Blocks are
  lists of declarations and nested rules in source order, whatever rule holds
  them. Context rules belong to the caller, apart from the checks `validate`
  lists below.

## API

`src/lib.rs` re-exports the public API. Spans are byte offsets into the input,
and tree nodes borrow from it.

### Parsing

- `parse_stylesheet(css)` returns `(Stylesheet, Vec<SyntaxError>)`: the whole
  tree, a span on every node, and every recovered error sorted by position.
- `parse_block_contents(css)` does the same for the inside of a block, such
  as a `style="…"` attribute.
- `syntax_errors(css)` is a convenience for callers that want only the errors.
- `declaration_removal_span(source, span)` gives what to delete to remove a
  declaration, with its `;` and the whitespace up to the next token.
- `Tokenizer` exposes the tokens (`next_token`, `next_token_spanned`,
  `spanned`).

`SyntaxErrorKind`: `BadString`, `BadUrl`, `MalformedDeclaration`,
`UnterminatedRule`, `UnterminatedBlock`, `UnexpectedToken`,
`InvalidUnicodeRange`, `InvalidSelector`, `InvalidSelectorStructure`,
`UnknownPseudo`, `NarrowedIdent`, `NestingTooDeep`,
`DroppedCustomPropertyRule`.

### Decoding

- `decode_stylesheet(bytes, protocol, environment)` follows CSS Syntax's
  order: byte order mark, protocol label, `@charset`, the referring document's
  encoding, UTF-8.
- `encode_stylesheet(text, encoding, bom)` is its inverse. A decode whose
  `Decoded::lossless` is true encodes back to the original bytes.
- `charset_rule(bytes)` reads the `@charset` label.

Supported: UTF-8, UTF-16, every single-byte encoding of the Encoding Standard,
and `replacement` (decode only).

### Selectors and matching

- `parse_selector_list`, `parse_relative_selector_list`: a `SelectorList` tree,
  with `specificity()` on its nodes, `An+B` and `resolve_nesting`.
- `SelectorList::matches(&element, &namespaces)` returns `Match`: `Yes`, `No`
  or `Unknown`. Unknown means the answer depends on something a static tree
  does not say (`:hover`, `:has()`). Answers are about the element itself, so
  a pseudo-element answers `No`.
- `Element` is the caller's tree: nine required methods (`parent`,
  `prev_sibling`, `next_sibling`, `has_children`, `local_name`, `namespace`,
  `attribute`, `attribute_ns`, `is_html_document`) and three with defaults
  (`id`, `has_class`, `each_class`). Overriding `has_class` requires overriding
  `each_class` too, because the cascade finds rules through it.
- `Namespaces::from_stylesheet` collects `@namespace` prefixes.

### Cascade

- `Cascade::new(&[Sheet])` or `Cascade::with_imports(&[Sheet], import)`, built
  once per document, in document order.
- `cascade.style_for(&element, inline)` returns an `ElementStyle`: every
  declaration that could apply, highest precedence first.
- `style.property(name)` returns a `PropertyAnswer`, with `status`,
  `decided_by()` and `decided_by_certain()`.
- `cascade.pseudo_elements_for(&element)` names the pseudo-elements rules could
  style on an element, and `cascade.style_for_pseudo(&element, name)` gives an
  `ElementStyle` for one. `style_for` covers the element itself only.
- `before.compare_to(&after)` returns a `StyleComparer`.
  `comparer.element(&a, a_inline, &b, b_inline)` returns a `StyleComparison`:
  a `status` and a list of `StyleDifference` (`pseudo`, `property`, `status`,
  `reasons`). The caller says which element corresponds to which.

It handles `!important`, the `style` attribute, `@layer`, specificity, order,
CSS Nesting, `@import` and shorthands with their longhands (`longhands(name)`).
A declaration whose value is certainly invalid is dropped, as a browser drops
it. `@supports` conditions go through `evaluate_supports`, where `Yes` means
"can hold" and `No` holds everywhere.

What a comparison answers:

- `Yes`: the possible winners are the same for every property, on the element
  and its pseudo-elements. `Yes` on every element of a document means the same
  computed values, as far as the limits below allow.
- `No`: two certain winners, or a certain winner and none, differ as written.
  `red` and `#f00` differ, since values are not normalized. So do
  `url(missing.png)` and `none`: whether that draws the same is the caller's to
  know.
- `Unknown`: a possible winner is uncertain and not the same in both states;
  or the winners are written under different names (`margin: 0` and
  `margin-left: 0`, `all`); or one side has a CSS-wide keyword and the other
  nothing; or `differing_at_rules` is not empty.

`!important` alone does not count when the winner stays the same. A property no
data source knows is ignored. Limits: a `url()` is compared as text, so moving a
sheet is not seen, and the condition behind `Sheet::conditional` is the
caller's and is not compared.

`UnknownReason`: `Selector`, `Conditional`, `ConditionalSheet`, `Nested`,
`Logical`, `Prefixed`, `Revert`, `NarrowedIdent`.

### At-rule preludes

`parse_namespace_rule`, `parse_import_rule`, `parse_layer_rule` and
`parse_media_query_list` return structure. `MediaQueryList::always_holds` says
whether a list is empty, `all` or `only all`.

### Validation

- `validate_stylesheet(css)`, `validate_parsed_stylesheet(&sheet)`,
  `validate_declaration_list(css)`, `validate_parsed_block(&items)` return
  `Diagnostic`s, each with `span`, `kind` and `name`. For `InvalidValue`, the
  span covers the value and `name` is the property.
- `check_value`, `check_declaration`, `check_descriptor` return `ValueCheck`:
  `Valid`, `Invalid` or `Unknown(UnknownValue)`.

`DiagnosticKind`: `UnknownProperty`, `UnknownDescriptor`, `InvalidValue`,
`UnknownAtRule`, `MisplacedAtRule`, `UnknownMediaFeature`,
`ImportantInKeyframe`.

`UnknownValue`: `UnknownProperty`, `PrefixedProperty`, `NoGrammar`, `Empty`,
`Substitution`, `BadToken`, `OtherSpecification`, `PrefixedValue`, `Unread`,
`TooComplex`, `NarrowedIdent`.

Grammars come from mdn-data (`MDN_DATA_VERSION`, now 2.37.2), read in CSS Values
4's value definition syntax, with cited corrections where the specifications
are ahead of it. A value in a specification's grammar is Valid even if no engine
has it yet. The CSS-wide keywords fit properties, and descriptors only in
`@page`.

### References

`url_references(&sheet)` and `block_url_references(&items)` give every URL with
its context and form. `selector_references(&sheet)` gives every element name,
class, id and attribute the selectors name, with escapes resolved.

### Serialization

- `serialize_stylesheet(&sheet)`, `serialize_block_contents(&items)`, and
  functions for the smaller pieces (`serialize::serialize_rule` and others).
- `serialize_stylesheet_for_xml(&sheet)` writes CSS that can stand inside an
  XHTML `<style>` as it is: no `<`, `&` or `]]>`, with CSS escapes where a
  string or name holds one. It returns `XmlSafeError` with the span where
  nothing can stand in (a `<` delimiter in a media query, the nesting selector
  `&`).

### Features

`legacy-cjk`, off by default, adds Shift_JIS, EUC-JP, ISO-2022-JP, GBK, gb18030,
Big5 and EUC-KR, decode and encode, from the Encoding Standard's indexes dated
2024-09-18. It adds about 310 KiB to a WebAssembly build.

### Stability

styloria is pre-1.0, so a breaking change lands in a `0.x.0`. The error and
diagnostic enums are exhaustive on purpose: a new class of error is a new
variant, and an exhaustive `match` has to take it. The `NarrowedIdent` variants
exist because w3c/csswg-drafts#7129 is open, and may change when it is decided.

## Limits

| Constant | Value | What happens past it |
| --- | --- | --- |
| `MAX_NESTING_DEPTH` | 256 | The parser skips to the matching close bracket and reports `NestingTooDeep`; the rest of the sheet still parses. |
| `MAX_SELECTOR_DEPTH` | 32 | Deeper pseudo-class arguments are kept unread and not reported as errors; `resolve_nesting` returns `None`. |
| `MAX_RESOLVED_SIMPLES` | 4096 | `resolve_nesting` returns `None`; the cascade gives `UnknownReason::Nested`. |
| `MAX_MATCH_STEPS` | 4096 | The match answers `Unknown`. |
| `MAX_VALUE_STEPS` | 65536 | The value check answers `Unknown(TooComplex)`. |
| `MAX_IMPORT_DEPTH` | 32 | Imports are followed no deeper. |

Value verdicts are cached per thread, at most 8,192 of them. Parsing is linear:
the input is tokenized once and brackets are paired up front.

## Measurements

Measured on 0.18.0, on an Apple M2 Pro, best of 10 passes, over the stylesheets
of one shelf of 562 real EPUB books: 878 sheets, 2.6 MB, and 64,404 `style`
attributes. They are not benchmarks of your input.

| Stage, whole shelf | Time |
| --- | --- |
| Parse (about 140 MB/s) | 19 ms |
| Validate the parsed sheets | 8 ms |
| Build each sheet's cascade | 16 ms |
| Parse and validate the `style` attributes | 25 ms |

100,000 nested `@media` rules parse and validate in about 10 ms.

`compare_to` was checked against Firefox 158's computed styles, on the element and
its `::before`, `::after`, `::first-line`, `::first-letter` and `::marker`, in 1,506
sampled documents with images and fonts left out. In the 1,001 where every element
compared `Yes`, no computed style differed; the edit reached an element in 113 of
those. It is one browser and a horizontal, left-to-right shelf.

## Layout

No `tests/` directory: tests sit beside the code, in `#[cfg(test)]` modules and
`src/values/tests.rs`, plus doc tests.

| Path | Holds |
| --- | --- |
| `src/lib.rs` | Public re-exports |
| `src/tokenizer.rs`, `src/token.rs` | CSS Syntax §4; byte-based, tokens borrow from the input |
| `src/parser.rs` | CSS Syntax §5, property-blind; tree and errors |
| `src/span.rs` | `Span`, `Spanned` |
| `src/decode.rs`, `src/cjk.rs` | Bytes to text and back; multi-byte encodings |
| `src/selector.rs`, `src/matching.rs` | Selector tree and specificity; matching |
| `src/narrowed.rs` | The reading from before csswg-drafts#7129 |
| `src/cascade.rs` | The per-element query |
| `src/at_rule.rs` | At-rule preludes, `evaluate_supports` |
| `src/validate.rs`, `src/values/` | Diagnostics; value grammars |
| `src/properties.rs` | Shorthands and logical properties |
| `src/references.rs` | What a sheet points at |
| `src/serialize.rs` | The tree back to CSS |

`#[allow(clippy::if_same_then_else)]` on `serialize_name` is deliberate; the
comment above it says why.

## Build and test

CI runs these on every push to `main` and on pull requests:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --all-features
cargo build --lib --target wasm32-unknown-unknown
cargo build --lib --target wasm32-unknown-unknown --all-features
cargo +1.88 check --lib
```

The minimum supported Rust version is 1.88. The wasm32 builds matter because
epubveri ships WebAssembly. `Cargo.lock` is not committed, since this is a
library. A dependency, even a dev one, is not added casually: there are none
today.

Any change to the tokenizer or the parser's hot paths should be measured on
real stylesheets, not only on synthetic tests.

## Contributing and security

External pull requests are not accepted until a CLA is in place, because the
dual licence requires one copyright holder; see `CONTRIBUTING.md`. Report
security problems as `SECURITY.md` says, not in a public issue.

## Licence

`AGPL-3.0-only OR LicenseRef-veripublica-Commercial`: see `LICENSE` and
`LICENSE-COMMERCIAL.md`.
