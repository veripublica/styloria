# styloria

A pure-Rust CSS3 parser and serializer — a standalone, general-purpose
library, not tied to any single consumer project.

**Status: early (0.x).** The tokenizer, parser and serializer follow
[CSS Syntax Level 3](https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/)
as of the **1 October 2026 Candidate Recommendation Draft**, including its
nesting-aware block parsing, and are used in production by
[`epubveri`](https://github.com/veripublica/epubveri). The public API may
still change before 1.0.

```rust
let css = "p { color: red; a:hover { color: blue } }";
let (sheet, errors) = styloria::parse_stylesheet(css);
assert!(errors.is_empty());
// One tree: rules, the declarations and nested rules in their blocks, and
// the component values inside those — every node with its byte span.
let rule = &sheet.rules[0];
assert_eq!(rule.span.start_line_col(css), (1, 1));
```

- **`parse_stylesheet`** returns the whole tree and every `SyntaxError` the
  parser recovered from, sorted by position. **`parse_block_contents`** does
  the same for the inside of a block, such as an HTML `style="…"` attribute.
- Every block is read with the spec's "consume a block's contents", so a
  block is a list of declarations and nested rules in source order, whatever
  rule holds it. Which of them are *allowed* in a given context is left to
  the caller.
- **Decoding** (`decode` module) turns a stylesheet's bytes into text in CSS
  Syntax's order (byte order mark, protocol label, `@charset`, the
  referring document's encoding, UTF-8), and back: a lossless decode
  encodes to the original bytes, so a tool can edit one rule and leave the
  rest untouched. UTF-8, UTF-16 and every single-byte encoding of the
  Encoding Standard; the others are an error, never a guess.
- **Selectors** (`selector` module) are read into a tree with specificity
  (a pseudo-class or pseudo-element no specification defines is reported),
  and **matched** (`matching` module) against the caller's own document
  tree through a small `Element` trait. The answer is three-valued: `Yes`,
  `No`, or `Unknown` when it depends on something a static tree does not
  say (`:hover`, `:has()`).
- **The cascade** (`cascade` module) answers which declarations could set a
  property on an element, highest precedence first, and which one decides:
  `!important`, the `style` attribute, `@layer`, specificity, order, CSS
  Nesting, `@import`, shorthands and their longhands. What cannot be
  settled (a rule inside `@media`, an Unknown selector) is `Unknown`, with
  the reason.
- **Validation** (`validate` and `values` modules) reports unknown
  properties, descriptors and at-rules, misplaced `@import` and
  `@namespace` rules, and values that cannot fit their property's
  or descriptor's grammar (`color: 12px`, `@font-face { font-display: fast }`),
  each with the span to underline. The grammars
  are mdn-data's, corrected against the specifications where mdn lags
  behind them. It errs toward silence: a value is invalid only when even
  a lenient reading fails, and custom properties, vendor prefixes and
  `var()` are left alone.
- **References** (`references` module): every URL a stylesheet names, with
  its context, and every element name, class, id and attribute its
  selectors name, escapes resolved.
- **Serialization** (`serialize` module) writes the tree back as equivalent
  CSS. `serialize_stylesheet_for_xml` writes it so it can stand inside an
  XHTML `<style>` as it is: no `<`, `&` or `]]>`, CSS escapes where a
  string or name holds one, and an error with the span where nothing can
  (a `<` delimiter in a media query, the nesting selector `&`).
- **Bounded on hostile input.** Nesting has a depth limit, matching and
  value checks have step budgets, and parsing stays linear, so no
  stylesheet can exhaust the stack or the clock. No dependencies.

## Why

Most CSS parsing in the Rust ecosystem lives inside larger, non-standalone
projects (browser engines, bundlers) or comes with licensing that doesn't fit
every use case. `styloria` aims to be:

- **Pure Rust** — no C dependencies.
- **Standalone** — usable by any Rust project that needs to parse, validate,
  or serialize CSS, not coupled to a particular consumer.
- **Spec-driven** — starts from the [CSS Syntax Level 3](https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/)
  tokenizer and core grammar (the well-specified, property-agnostic layer),
  then builds structural/semantic validation on top.

`styloria` is developed alongside [`epubveri`](https://github.com/veripublica/epubveri)
(a pure-Rust EPUB validator), which depends on it for EPUB content
documents' embedded/linked CSS — but `styloria` itself is not EPUB-specific,
and is meant to be independently useful.

## License

`styloria` is dual-licensed:

- **AGPL-3.0-only** ([`LICENSE`](./LICENSE)) — free for any use, including
  commercial products, as long as your product also complies with the AGPL
  (including the network-use / source-disclosure clause).
- **Commercial license** (`LicenseRef-veripublica-Commercial`,
  see [`LICENSE-COMMERCIAL.md`](./LICENSE-COMMERCIAL.md)) — for embedding
  `styloria` in closed-source or proprietary products without AGPL's
  copyleft obligations. Contact baris@kayadelen.com.

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). In short: not accepting external
contributions yet — a CLA is required first (see that file for why).
