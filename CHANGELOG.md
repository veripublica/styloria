# Changelog

All notable changes to `styloria` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
styloria is pre-1.0, so new features and breaking changes both land as
minor-version bumps (`0.x.0`), per [Cargo's SemVer compatibility
rules](https://doc.rust-lang.org/cargo/reference/semver.html).

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
