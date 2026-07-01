# styloria

A pure-Rust CSS3 parser and serializer — a standalone, general-purpose
library, not tied to any single consumer project.

**Status: early / pre-alpha.** Just started; there is no working parser yet.

## Why

Most CSS parsing in the Rust ecosystem lives inside larger, non-standalone
projects (browser engines, bundlers) or comes with licensing that doesn't fit
every use case. `styloria` aims to be:

- **Pure Rust** — no C dependencies.
- **Standalone** — usable by any Rust project that needs to parse, validate,
  or serialize CSS, not coupled to a particular consumer.
- **Spec-driven** — starts from the [CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/)
  tokenizer and core grammar (the well-specified, property-agnostic layer),
  then builds structural/semantic validation on top.

`styloria` is developed alongside [`epubveri`](https://github.com/veripublica/epubveri)
(a pure-Rust EPUB validator), which will depend on it for EPUB content
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
