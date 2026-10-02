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
- **Validation** (`validate` module) checks declaration *names* against the
  CSS vocabulary: unknown properties in style rules and unknown descriptors
  in at-rules (`@font-face`, `@counter-style`, `@page`, …), each with the span
  to underline. Custom (`--*`) and vendor-prefixed names are exempt, and the
  check errs toward silence. Selector lists are checked for syntax.
  Value-level validation is a later layer.
- **Serialization** (`serialize` module) writes the tree back as equivalent
  CSS.
- Nesting is bounded (`MAX_NESTING_DEPTH`), and parsing stays linear on
  hostile input, so no stylesheet can exhaust the stack or the clock.

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
