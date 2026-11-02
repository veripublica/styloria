# styloria

styloria is a CSS parser and serializer in pure Rust, with no dependencies. It
is for anyone who needs to read, check or rewrite stylesheets in a Rust program
without bringing in a browser engine.

I wrote it for [epubveri](https://github.com/veripublica/epubveri), my EPUB
validator, but nothing in it knows about EPUB. I kept it as its own crate so
you can use it without the rest.

It follows CSS Syntax Level 3 and gives you the whole stylesheet as a tree, with
the byte position of every node and every syntax error it recovered from. On
top of that it can tell you which properties and values are wrong, match
selectors against your own document tree, and say which declaration sets a
property on an element. When the stylesheet alone can't settle something, such
as whether `:hover` matches, it answers Unknown instead of guessing.

```sh
cargo add styloria
```

```rust
let css = "p { color: 12px; colr: blue }";
let (sheet, errors) = styloria::parse_stylesheet(css);
assert!(errors.is_empty());

for d in styloria::validate_parsed_stylesheet(&sheet) {
    let (line, column) = d.span.start_line_col(css);
    println!("{line}:{column} {:?} {}", d.kind, d.name);
}
// 1:12 InvalidValue { at_rule: None, valid_under: None } color
// 1:18 UnknownProperty colr
```

The API can still change before 1.0. The full picture, including what it
declines to do, is in [`AGENTS.md`](./AGENTS.md).

## Licence

AGPL-3.0-only, or a [commercial licence](./LICENSE-COMMERCIAL.md) if you need
to use it in closed-source software: write to me at baris@kayadelen.com. I can't
accept pull requests yet, and [`CONTRIBUTING.md`](./CONTRIBUTING.md) says why.
