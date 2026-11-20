//! styloria's own invariants on the stylesheets of epub-test-books
//! (<https://github.com/veripublica/epub-test-books>).
//!
//! CI checks out a pinned tag of that repository, unzips each book's `.css`
//! files into one directory and names it in `STYLORIA_TEST_BOOKS_CSS`. Without
//! the variable this test says it skipped and passes, so `cargo test` needs
//! no download.
//!
//! The sets' expected results are verifier findings, not ours, so they are
//! not compared. What is checked is what styloria promises about any input:
//!
//! - decoding never fails on these bytes (a multi-byte legacy encoding may,
//!   without the `legacy-cjk` feature), and a lossless decode encodes back to
//!   them;
//! - parsing, validating, reading selectors and asking which declarations
//!   the parsed sheet keeps do not panic;
//! - that answer covers every declaration of the tree once, in source order,
//!   and its spans lie in the text too;
//! - every span of the tree, of a syntax error and of a diagnostic lies in
//!   the text, on character boundaries;
//! - serializing is a fixed point: the text serialized once parses to a tree
//!   that serializes to the same text.

use std::path::{Path, PathBuf};

use styloria::{
    BlockItem, ComponentValue, DecodeError, Kept, Rule, Span, Spanned, cssom_declarations,
    decode_stylesheet, encode_stylesheet, parse_selector_list, parse_stylesheet,
    serialize_stylesheet, validate_parsed_stylesheet,
};

const VAR: &str = "STYLORIA_TEST_BOOKS_CSS";

#[test]
fn test_books_keep_the_invariants() {
    let Some(dir) = std::env::var_os(VAR) else {
        eprintln!("skipped: {VAR} is not set");
        return;
    };
    let mut files = Vec::new();
    collect(Path::new(&dir), &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "{VAR} names a directory with no .css file"
    );
    let mut failures = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        if let Err(e) = check(&bytes) {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    eprintln!("{} stylesheets checked", files.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("css"))
        {
            out.push(path);
        }
    }
}

fn check(bytes: &[u8]) -> Result<(), String> {
    let decoded = match decode_stylesheet(bytes, None, None) {
        Ok(d) => d,
        // Without `legacy-cjk` the multi-byte encodings are refused, as
        // documented; with it they decode.
        Err(DecodeError::Unsupported { .. }) if !cfg!(feature = "legacy-cjk") => return Ok(()),
        Err(e) => return Err(format!("decode: {e}")),
    };
    if decoded.lossless {
        let back = encode_stylesheet(&decoded.text, decoded.encoding, decoded.had_bom)
            .map_err(|e| format!("encode: {e:?}"))?;
        if back != bytes {
            return Err("a lossless decode does not encode back to its bytes".into());
        }
    }
    let css = &*decoded.text;
    let (sheet, errors) = parse_stylesheet(css);
    let mut spans = Vec::new();
    for rule in &sheet.rules {
        rule_spans(rule, &mut spans);
    }
    spans.extend(errors.iter().map(|e| e.span));
    spans.extend(validate_parsed_stylesheet(&sheet).iter().map(|d| d.span));
    for rule in &sheet.rules {
        if let Rule::Qualified(q) = &rule.node
            && let Err(errs) = parse_selector_list(&q.prelude)
        {
            spans.extend(errs.iter().map(|e| e.span));
        }
    }
    let declarations: Vec<Span> = rule_declarations(&sheet);
    let kept = cssom_declarations(&sheet);
    if kept
        .iter()
        .map(|d| d.declaration.span)
        .ne(declarations.iter().copied())
    {
        return Err("cssom_declarations does not give every declaration once, in order".into());
    }
    spans.extend(kept.iter().filter_map(|d| match d.kept {
        Kept::Yes => None,
        Kept::No { span, .. } | Kept::Unknown { span, .. } => Some(span),
    }));
    if let Some(s) = spans.iter().find(|s| !in_text(**s, css)) {
        return Err(format!("span {}..{} outside the text", s.start, s.end));
    }
    let once = serialize_stylesheet(&sheet);
    let twice = serialize_stylesheet(&parse_stylesheet(&once).0);
    if once != twice {
        return Err("serializing is not a fixed point".into());
    }
    Ok(())
}

fn rule_declarations(sheet: &styloria::Stylesheet<'_>) -> Vec<Span> {
    fn walk(items: &[BlockItem<'_>], out: &mut Vec<Span>) {
        for item in items {
            match item {
                BlockItem::Declaration(d) => out.push(d.span),
                BlockItem::Rule(r) => {
                    if let Some(b) = r.node.block() {
                        walk(&b.node, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for rule in &sheet.rules {
        if let Some(b) = rule.node.block() {
            walk(&b.node, &mut out);
        }
    }
    out
}

fn in_text(s: Span, css: &str) -> bool {
    s.start <= s.end
        && s.end <= css.len()
        && css.is_char_boundary(s.start)
        && css.is_char_boundary(s.end)
}

fn rule_spans(rule: &Spanned<Rule<'_>>, out: &mut Vec<Span>) {
    out.push(rule.span);
    if let Rule::At(a) = &rule.node {
        out.push(a.name_span);
    }
    values_spans(rule.node.prelude(), out);
    if let Some(block) = rule.node.block() {
        out.push(block.span);
        for item in &block.node {
            match item {
                BlockItem::Declaration(d) => {
                    out.push(d.span);
                    out.push(d.node.name_span);
                    values_spans(&d.node.value, out);
                }
                BlockItem::Rule(r) => rule_spans(r, out),
            }
        }
    }
}

fn values_spans(values: &[Spanned<ComponentValue<'_>>], out: &mut Vec<Span>) {
    for v in values {
        out.push(v.span);
        match &v.node {
            ComponentValue::Function { args, .. } => values_spans(args, out),
            ComponentValue::Block(b) => values_spans(&b.values, out),
            ComponentValue::Token(_) => {}
        }
    }
}
