# Source spans — prototype & roadmap

**Goal.** Let a consumer of styloria (notably the `epubveri` EPUB validator)
report the exact `line:column` of a CSS finding, instead of only the file
name. Today CSS is the *only* finding family in epubveri that cannot carry a
position, purely because styloria's tokens and parse nodes don't expose one.

This document describes the first slice (landed as a prototype) and the plan
to extend it up through the parser.

## What exists now

Byte-accurate spans at both the **tokenizer** and the **parser** level —
additive, with no breaking change to any existing API.

### Primitives (`span` module)

- `Span { start, end }` — a half-open byte range into the source. Helpers:
  `slice(src)`, `start_line_col(src)` (1-based line, char column),
  `to(other)` (union, for building a node span from its first/last child).
- `Spanned<T> { node, span }` — any value paired with its span (`map`,
  `as_ref`).

### Tokenizer spans

- `Tokenizer::next_token_spanned() -> Option<Spanned<Token>>` and
  `Tokenizer::spanned() -> SpannedTokens` (an iterator). Comments preceding a
  token are skipped first, so a span covers only the token itself; every
  span round-trips (`span.slice(src)` == the token's source text).

### Parser-node spans (`spanned` module)

A full mirror of the parse tree where every node — and every nested value —
carries a span. Same grammar as the plain parser, module-qualified names
(`styloria::spanned::Rule`, `…::ComponentValue`, `…::SimpleBlock`,
`…::QualifiedRule`, `…::AtRule`). A block's span covers its brackets, a
rule's span covers its whole text, a token value's span is the token's, and
an `AtRule` also exposes `name_span` (just the `@name` keyword).

```rust
let src = "@media screen {\n  div.box { padding: 0; }\n}";
let sheet = styloria::spanned::parse_stylesheet(src);
if let styloria::spanned::Rule::At(at) = &sheet.rules[0].node {
    // at.name_span → "@media"; the nested `div.box { … }` block and the
    // `padding` property inside it each carry their own span, so a
    // validator can report `padding` at (line 2, col 13).
}
```

The plain [`Parser`] and its position-less types are untouched — a consumer
that doesn't need positions keeps using them.

## How epubveri will consume it

`epubveri/src/css.rs` currently emits `CSS-008` (and `CSS-001`/`CSS-019`)
with no position, because it walks the plain `ComponentValue` tree. Switching
its CSS pass to `spanned::parse_stylesheet` lets every `report.push_at*` call
pass a `Position` derived from `span.start_line_col(css_source)` — closing
epubveri's last position gap and lifting overall position coverage from ~82%
toward ~95%. The walk is structurally identical (same rule/block/value
shape), so it's a mechanical swap plus threading the span into the finding.

The spanned surface now mirrors the plain parser's two entry points:
`spanned::parse_stylesheet` and `spanned::parse_declaration_list` (the
latter with a spanned `Declaration` carrying a `name_span` for the property,
for `style="…"` attributes and `@font-face`/`@page` bodies).

## Possible follow-ups

- If a future `styloria` major version wants a single tree, the position-less
  types could be dropped in favour of the spanned ones (a `0.2` break); until
  then the two coexist and consumers opt in.
