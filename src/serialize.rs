//! Serialization back to CSS text (CSS Syntax Level 3, "Serialization":
//! <https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/#serialization>).
//!
//! The acceptance bar is round-tripping to *equivalent* CSS, not
//! byte-identical output: whitespace/comments aren't preserved
//! (tokenization discards them), but escaping must always be correct — a
//! serialized identifier/string/url must always re-tokenize to the same
//! value it started from.

use crate::parser::{
    BlockItem, BlockKind, ComponentValue, Declaration, Rule, SimpleBlock, Stylesheet,
};
use crate::references::url_references;
use crate::span::{Span, Spanned};
use crate::token::Token;
use crate::tokenizer::is_name;

fn needs_control_escape(c: char) -> bool {
    matches!(c, '\u{1}'..='\u{1f}' | '\u{7f}')
}

fn escape_code_point(c: char, out: &mut String) {
    use std::fmt::Write;
    let _ = write!(out, "\\{:x} ", c as u32);
}

/// Whether, in XML-safe output, `c` cannot be written as itself after `out`:
/// `<` and `&` start markup, `>` would close a `]]>`, and U+FFFE and U+FFFF
/// are not XML characters. (The controls XML forbids are escaped in every
/// mode already.)
fn xml_unsafe(c: char, out: &str) -> bool {
    match c {
        '<' | '&' | '\u{FFFE}' | '\u{FFFF}' => true,
        '>' => out.ends_with("]]"),
        _ => false,
    }
}

/// Serialize a "name" body (an identifier's or hash's textual content),
/// escaping whatever isn't a plain name code point. `check_leading_digit`
/// enables the ident-only rule that a name can't start (or start with `-`
/// then) a digit without escaping it — hash-token values don't need this
/// (`#123` is already unambiguous as a hash).
// Two of the branches below escape the code point the same way, which clippy
// reads as a duplicated block. They stay separate on purpose: one is the
// control-character rule, the other the leading-digit rule, and they are
// independent parts of the serialization spec that happen to share an action.
#[allow(clippy::if_same_then_else)]
fn serialize_name(s: &str, out: &mut String, check_leading_digit: bool, xml: bool) {
    if s == "-" {
        out.push_str("\\-");
        return;
    }
    let starts_with_dash = s.starts_with('-');
    for (i, c) in s.chars().enumerate() {
        if c == '\0' {
            out.push('\u{FFFD}');
        } else if needs_control_escape(c) || (xml && xml_unsafe(c, out)) {
            escape_code_point(c, out);
        } else if check_leading_digit
            && c.is_ascii_digit()
            && (i == 0 || (i == 1 && starts_with_dash))
        {
            escape_code_point(c, out);
        } else if is_name(c) {
            out.push(c);
        } else if c.is_ascii() {
            out.push('\\');
            out.push(c);
        } else {
            // A non-ASCII code point that is not an ident code point (a
            // no-break space, say) has to be escaped by value: `\` followed
            // by the character itself would read back as a valid escape too,
            // but only if the character is not a newline or hex digit, and
            // writing it as hex sidesteps both.
            escape_code_point(c, out);
        }
    }
}

/// Append `s` as an identifier, escaped so it re-tokenizes as the same
/// ident.
pub fn serialize_ident(s: &str, out: &mut String) {
    serialize_name(s, out, true, false);
}

/// How a string or URL is being written.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Css,
    Xml,
    /// XML-safe, and the text is a URL: an XML-unsafe character cannot be
    /// escaped, since epubcheck does not decode CSS escapes in a URL
    /// (w3c/epubcheck#1746) and would report a correct book.
    XmlUrl,
}

/// Write `c` as itself, or `Err` when XML-safe output cannot hold it.
/// `Ok(false)` leaves it to the caller.
fn push_xml(c: char, out: &mut String, mode: Mode) -> Result<bool, ()> {
    if mode == Mode::Css || !xml_unsafe(c, out) {
        return Ok(false);
    }
    if mode == Mode::XmlUrl {
        return Err(());
    }
    escape_code_point(c, out);
    Ok(true)
}

/// Append `s` as a double-quoted string, escaped so it re-tokenizes as the
/// same string.
pub fn serialize_string(s: &str, out: &mut String) {
    let _ = write_string(s, out, Mode::Css);
}

fn write_string(s: &str, out: &mut String, mode: Mode) -> Result<(), ()> {
    out.push('"');
    for c in s.chars() {
        if push_xml(c, out, mode)? {
            continue;
        }
        match c {
            '\0' => out.push('\u{FFFD}'),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if needs_control_escape(c) => escape_code_point(c, out),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

/// Serializes a bare (unquoted) `url(...)` token's contents, escaping
/// whitespace/quotes/parens/backslash so the result is always valid to
/// re-tokenize as a `url-token` rather than accidentally becoming a
/// `bad-url-token`.
pub fn serialize_url(s: &str, out: &mut String) {
    let _ = write_url(s, out, Mode::Css);
}

fn write_url(s: &str, out: &mut String, mode: Mode) -> Result<(), ()> {
    out.push_str("url(");
    for c in s.chars() {
        if push_xml(c, out, mode)? {
            continue;
        }
        match c {
            '\0' => out.push('\u{FFFD}'),
            '"' | '\'' | '(' | ')' | '\\' | ' ' | '\t' | '\n' | '\r' | '\x0c' => {
                escape_code_point(c, out);
            }
            c if needs_control_escape(c) => escape_code_point(c, out),
            c => out.push(c),
        }
    }
    out.push(')');
    Ok(())
}

fn serialize_dimension_unit(unit: &str, out: &mut String, xml: bool) {
    // Guard against re-parse ambiguity: a unit starting with e/E followed by
    // a digit (or +/-) would otherwise look like it continues the number's
    // exponent (e.g. dimension `3` + unit `e2` must not re-serialize as the
    // number `3e2`).
    let mut chars = unit.chars();
    if let Some(first) = chars.next()
        && matches!(first, 'e' | 'E')
        && matches!(chars.clone().next(), Some(c) if c.is_ascii_digit() || c == '+' || c == '-')
    {
        escape_code_point(first, out);
        serialize_name(chars.as_str(), out, false, xml);
        return;
    }
    serialize_name(unit, out, false, xml);
}

/// Why [`serialize_stylesheet_for_xml`] could not write a stylesheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlSafeError {
    /// The component value that cannot be written, in the parsed source.
    pub span: Span,
    pub kind: XmlSafeErrorKind,
}

/// What [`XmlSafeError`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlSafeErrorKind {
    /// A `<` or `&` delimiter (a media-query range, the nesting selector), or
    /// one XML does not allow. A CSS escape would turn it into an
    /// identifier.
    Delimiter(char),
    /// A `<!--`.
    Cdo,
    /// A URL holding `<`, `&`, a `>` after `]]`, or a character XML does not
    /// allow. CSS Syntax decodes an escape in a URL, but epubcheck does not
    /// (w3c/epubcheck#1746), so the URL is refused rather than escaped. A
    /// `<bad-url-token>` holding one is refused too.
    Url,
    /// A `>` delimiter right after two `]`, which would close a CDATA
    /// section.
    CdataEnd,
}

impl std::fmt::Display for XmlSafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            XmlSafeErrorKind::Delimiter(c) => write!(f, "a {c:?} delimiter")?,
            XmlSafeErrorKind::Cdo => f.write_str("a `<!--`")?,
            XmlSafeErrorKind::Url => f.write_str("a URL holding a character XML cannot take")?,
            XmlSafeErrorKind::CdataEnd => f.write_str("a `]]>`")?,
        }
        write!(
            f,
            " at bytes {}..{} cannot be written XML-safe",
            self.span.start, self.span.end
        )
    }
}

impl std::error::Error for XmlSafeError {}

/// What XML-safe output needs while it walks the tree: the spans of the
/// strings that are URLs (`@import "a.css"`, `url("a.png")`), in source
/// order.
struct Xml {
    urls: Vec<Span>,
}

impl Xml {
    fn is_url(&self, span: Span) -> bool {
        let i = self.urls.partition_point(|u| u.start <= span.start);
        i > 0 && span.end <= self.urls[i - 1].end
    }
}

type Written = Result<(), XmlSafeError>;

/// Append one token. A function token writes its name and `(`.
pub fn serialize_token(t: &Token, out: &mut String) {
    let _ = write_token(t, Span::new(0, 0), out, None);
}

fn write_token(t: &Token, span: Span, out: &mut String, xml: Option<&Xml>) -> Written {
    let refuse = |kind| Err(XmlSafeError { span, kind });
    let x = xml.is_some();
    match t {
        Token::Ident(s) => serialize_name(s, out, true, x),
        Token::Function(s) => {
            serialize_name(s, out, true, x);
            out.push('(');
        }
        Token::AtKeyword(s) => {
            out.push('@');
            serialize_name(s, out, true, x);
        }
        Token::Hash { value, is_id } => {
            out.push('#');
            serialize_name(value, out, *is_id, x);
        }
        Token::String(s) => {
            let mode = match xml {
                None => Mode::Css,
                Some(xml) if xml.is_url(span) => Mode::XmlUrl,
                Some(_) => Mode::Xml,
            };
            if write_string(s, out, mode).is_err() {
                return refuse(XmlSafeErrorKind::Url);
            }
        }
        // An unterminated string: a quote and the newline that ended it,
        // which re-tokenizes as a bad string again rather than swallowing
        // the text after it.
        Token::BadString => out.push_str("\"\n"),
        Token::Url(s) => {
            let mode = if x { Mode::XmlUrl } else { Mode::Css };
            if write_url(s, out, mode).is_err() {
                return refuse(XmlSafeErrorKind::Url);
            }
        }
        Token::BadUrl(raw) => {
            if x {
                // Only the last two characters written can join a `]]>`.
                let mut tail: Vec<char> = out.chars().rev().take(2).collect();
                tail.reverse();
                let mut probe: String = tail.into_iter().collect();
                for c in raw.chars() {
                    let xml_control = needs_control_escape(c) && !matches!(c, '\t' | '\n' | '\r');
                    if xml_control || xml_unsafe(c, &probe) {
                        return refuse(XmlSafeErrorKind::Url);
                    }
                    probe.push(c);
                }
            }
            out.push_str(raw);
            if !raw.ends_with(')') {
                out.push(')');
            }
        }
        Token::Delim('>') if x && out.ends_with("]]") => {
            return refuse(XmlSafeErrorKind::CdataEnd);
        }
        Token::Delim(c) if x && xml_unsafe(*c, out) => {
            return refuse(XmlSafeErrorKind::Delimiter(*c));
        }
        Token::Delim(c) => out.push(*c),
        Token::Number { repr, .. } => out.push_str(repr),
        Token::Percentage { repr, .. } => {
            out.push_str(repr);
            out.push('%');
        }
        Token::Dimension { repr, unit, .. } => {
            out.push_str(repr);
            serialize_dimension_unit(unit, out, x);
        }
        Token::UnicodeRange { start, end } => {
            use std::fmt::Write;
            let _ = write!(out, "U+{start:X}");
            if end != start {
                let _ = write!(out, "-{end:X}");
            }
        }
        Token::Whitespace => out.push(' '),
        Token::Cdo if x => return refuse(XmlSafeErrorKind::Cdo),
        Token::Cdo => out.push_str("<!--"),
        Token::Cdc => out.push_str("-->"),
        Token::Colon => out.push(':'),
        Token::Semicolon => out.push(';'),
        Token::Comma => out.push(','),
        Token::LeftSquare => out.push('['),
        Token::RightSquare => out.push(']'),
        Token::LeftParen => out.push('('),
        Token::RightParen => out.push(')'),
        Token::LeftCurly => out.push('{'),
        Token::RightCurly => out.push('}'),
    }
    Ok(())
}

/// Append one component value: a token, a function with its arguments and
/// `)`, or a simple block with its brackets.
pub fn serialize_component_value(v: &ComponentValue, out: &mut String) {
    let _ = write_component_value(v, Span::new(0, 0), out, None);
}

fn write_component_value(
    v: &ComponentValue,
    span: Span,
    out: &mut String,
    xml: Option<&Xml>,
) -> Written {
    match v {
        ComponentValue::Token(t) => write_token(t, span, out, xml)?,
        ComponentValue::Function { name, args } => {
            serialize_name(name, out, true, xml.is_some());
            out.push('(');
            write_values(args, out, xml)?;
            out.push(')');
        }
        ComponentValue::Block(b) => write_simple_block(b, out, xml)?,
    }
    Ok(())
}

/// A list of component values: a prelude, a declaration's value, a block's
/// or function's contents.
pub fn serialize_values(values: &[Spanned<ComponentValue>], out: &mut String) {
    let _ = write_values(values, out, None);
}

fn write_values(
    values: &[Spanned<ComponentValue>],
    out: &mut String,
    xml: Option<&Xml>,
) -> Written {
    for v in values {
        write_component_value(&v.node, v.span, out, xml)?;
    }
    Ok(())
}

/// Append a `{ … }`, `[ … ]` or `( … )` block.
pub fn serialize_simple_block(b: &SimpleBlock, out: &mut String) {
    let _ = write_simple_block(b, out, None);
}

fn write_simple_block(b: &SimpleBlock, out: &mut String, xml: Option<&Xml>) -> Written {
    let (open, close) = match b.kind {
        BlockKind::Curly => ('{', '}'),
        BlockKind::Square => ('[', ']'),
        BlockKind::Paren => ('(', ')'),
    };
    out.push(open);
    write_values(&b.values, out, xml)?;
    out.push(close);
    Ok(())
}

/// Append `name:value`, with `!important` when set and no trailing `;`.
pub fn serialize_declaration(d: &Declaration, out: &mut String) {
    let _ = write_declaration(d, out, None);
}

fn write_declaration(d: &Declaration, out: &mut String, xml: Option<&Xml>) -> Written {
    serialize_name(&d.name, out, true, xml.is_some());
    out.push(':');
    write_values(&d.value, out, xml)?;
    if d.important {
        out.push_str("!important");
    }
    Ok(())
}

fn write_items(items: &[BlockItem], out: &mut String, xml: Option<&Xml>) -> Written {
    for item in items {
        match item {
            BlockItem::Declaration(d) => {
                write_declaration(&d.node, out, xml)?;
                out.push(';');
            }
            BlockItem::Rule(r) => write_rule(&r.node, out, xml)?,
        }
    }
    Ok(())
}

/// Append a rule with its block, or with `;` for a statement at-rule.
pub fn serialize_rule(r: &Rule, out: &mut String) {
    let _ = write_rule(r, out, None);
}

fn write_rule(r: &Rule, out: &mut String, xml: Option<&Xml>) -> Written {
    match r {
        Rule::Qualified(q) => write_values(&q.prelude, out, xml)?,
        Rule::At(a) => {
            out.push('@');
            serialize_name(&a.name, out, true, xml.is_some());
            write_values(&a.prelude, out, xml)?;
        }
    }
    match r.block() {
        Some(b) => {
            out.push('{');
            write_items(&b.node, out, xml)?;
            out.push('}');
        }
        None => out.push(';'),
    }
    Ok(())
}

/// The whole stylesheet as equivalent CSS: comments and whitespace are not
/// kept, so it reads back to the same rules and values, at other spans.
pub fn serialize_stylesheet(sheet: &Stylesheet) -> String {
    let mut out = String::new();
    for r in &sheet.rules {
        let _ = write_rule(&r.node, &mut out, None);
    }
    out
}

/// The stylesheet as [`serialize_stylesheet`] writes it, but fit to stand as
/// the text of an XHTML `<style>` element without XML escaping: no `<`, no
/// `&`, no `]]>`, and only characters XML 1.0 allows. Where one of those is
/// inside a string, an identifier or a hash it is written as a CSS escape
/// (`\3c `, `\26 `, `\3e `), which an XML and an HTML parser both pass
/// through and CSS reads back as the same text.
///
/// The error names the first component value no escape can save: a `<` or
/// `&` delimiter (`@media (width < 600px)`, the nesting selector `&`), a
/// `<!--`, a URL holding such a character, or a `]]>` the tokens themselves
/// spell.
pub fn serialize_stylesheet_for_xml(sheet: &Stylesheet) -> Result<String, XmlSafeError> {
    let xml = Xml {
        urls: url_references(sheet).into_iter().map(|u| u.span).collect(),
    };
    let mut out = String::new();
    for r in &sheet.rules {
        write_rule(&r.node, &mut out, Some(&xml))?;
    }
    Ok(out)
}

/// The contents of a block, as [`parse_block_contents`](crate::parse_block_contents)
/// returns them — a `style="…"` attribute's value, say.
pub fn serialize_block_contents(items: &[BlockItem]) -> String {
    let mut out = String::new();
    let _ = write_items(items, &mut out, None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_block_contents, parse_stylesheet};
    use crate::tokenizer::Tokenizer;

    #[test]
    fn ident_roundtrip_plain() {
        let mut out = String::new();
        serialize_ident("foo-bar", &mut out);
        assert_eq!(out, "foo-bar");
    }

    #[test]
    fn ident_roundtrip_leading_digit_needs_escape() {
        let mut out = String::new();
        serialize_ident("1foo", &mut out);
        let toks: Vec<_> = Tokenizer::new(&out).collect();
        assert_eq!(toks, vec![Token::Ident("1foo".into())]);
    }

    #[test]
    fn ident_roundtrip_solitary_hyphen() {
        let mut out = String::new();
        serialize_ident("-", &mut out);
        let toks: Vec<_> = Tokenizer::new(&out).collect();
        assert_eq!(toks, vec![Token::Ident("-".into())]);
    }

    #[test]
    fn ident_roundtrip_non_ident_code_points() {
        for s in ["a\u{A0}b", "a b", "a:b", "x\u{D7}", "ç\u{2000}"] {
            let mut out = String::new();
            serialize_ident(s, &mut out);
            let toks: Vec<_> = Tokenizer::new(&out).collect();
            assert_eq!(toks, vec![Token::Ident(s.into())], "{s:?} → {out:?}");
        }
    }

    #[test]
    fn string_roundtrip_with_quote_and_backslash() {
        let mut out = String::new();
        serialize_string("a\"b\\c", &mut out);
        let toks: Vec<_> = Tokenizer::new(&out).collect();
        assert_eq!(toks, vec![Token::String("a\"b\\c".into())]);
    }

    #[test]
    fn url_roundtrip_with_specials() {
        let mut out = String::new();
        serialize_url("a b(c)'d\"e\\f", &mut out);
        let toks: Vec<_> = Tokenizer::new(&out).collect();
        assert_eq!(toks, vec![Token::Url("a b(c)'d\"e\\f".into())], "{out}");
    }

    #[test]
    fn dimension_exponent_ambiguity_guard() {
        // A dimension with value "3" and unit "e2" must not re-serialize
        // and re-tokenize as the number 300 (i.e. as "3e2" bare).
        let mut out = String::new();
        serialize_dimension_unit("e2", &mut out, false);
        let full = format!("3{out}");
        let toks: Vec<_> = Tokenizer::new(&full).collect();
        match &toks[0] {
            Token::Dimension { unit, .. } => assert_eq!(unit.as_ref(), "e2"),
            other => panic!(
                "expected a dimension token, got {other:?} (ambiguity guard failed: {full:?})"
            ),
        }
    }

    /// Serializing, parsing and serializing again is a fixed point: the
    /// second parse saw the same structure as the first. (Trees can't be
    /// compared directly, since every span moves.)
    fn assert_stable(css: &str) {
        let (sheet, _) = parse_stylesheet(css);
        let once = serialize_stylesheet(&sheet);
        let (again, _) = parse_stylesheet(&once);
        let twice = serialize_stylesheet(&again);
        assert_eq!(once, twice, "not stable for {css:?}");
    }

    #[test]
    fn stylesheet_roundtrip() {
        assert_stable(
            r#"
            @media screen and (min-width: 10px) {
                a.foo::before { content: "he said \"hi\""; color: red !important; }
            }
            p { margin: 0 auto; --x: { a b c }; }
            @import url(x.css);
            "#,
        );
        assert_stable("p { color: red; a:hover { color: blue } & b { c: d } }");
        assert_stable("@font-face { unicode-range: U+0-7F, U+4??; src: url(a b) }");
        assert_stable("a { b: \"c\nd: e }");
        assert_stable("@page { margin: 1cm; @top-center { content: \"x\" } }");
    }

    #[test]
    fn serialized_text_is_what_the_tree_says() {
        let (sheet, _) =
            parse_stylesheet("p{color:red;a{color:blue}--x:{y};margin:0!important}@import 'a';");
        assert_eq!(
            serialize_stylesheet(&sheet),
            "p{color:red;a{color:blue;}--x:{y};margin:0!important;}@import \"a\";"
        );
    }

    #[test]
    fn block_contents_roundtrip() {
        let css = "color: red; --x: 1px solid blue; margin:0 10px !important";
        let (items, _) = parse_block_contents(css);
        let once = serialize_block_contents(&items);
        let (items2, _) = parse_block_contents(&once);
        assert_eq!(once, serialize_block_contents(&items2));
        assert_eq!(
            once,
            "color:red;--x:1px solid blue;margin:0 10px!important;"
        );
    }

    /// The XML-safe bar: either an error, or text with no `<`, `&`, `]]>`
    /// or non-XML character that reads back to the same CSS as the input.
    fn xml_safe(css: &str) -> Result<String, XmlSafeError> {
        let (sheet, _) = parse_stylesheet(css);
        let written = serialize_stylesheet_for_xml(&sheet)?;
        assert!(
            !written.contains(['<', '&', '\u{FFFE}', '\u{FFFF}']) && !written.contains("]]>"),
            "not XML-safe: {written:?}"
        );
        assert!(
            !written
                .chars()
                .any(|c| needs_control_escape(c) && !matches!(c, '\t' | '\n' | '\r')),
            "control character in {written:?}"
        );
        // Compared after one plain round trip: two whitespace tokens a
        // comment kept apart are written as two spaces and read back as one.
        let reread = |text: &str| serialize_stylesheet(&parse_stylesheet(text).0);
        assert_eq!(
            reread(&written),
            reread(&serialize_stylesheet(&sheet)),
            "{css:?} → {written:?} reads back differently"
        );
        Ok(written)
    }

    fn xml_refused(css: &str) -> (XmlSafeErrorKind, &str) {
        let e = xml_safe(css).expect_err(css);
        (e.kind, e.span.slice(css))
    }

    #[test]
    fn xml_safe_matches_plain_output_when_nothing_needs_escaping() {
        let css = "@media screen{a>b.c#d::before{content:\"x > y\";margin:0 1px!important}}";
        let (sheet, _) = parse_stylesheet(css);
        assert_eq!(xml_safe(css).unwrap(), serialize_stylesheet(&sheet));
    }

    #[test]
    fn xml_safe_escapes_inside_strings_names_and_hashes() {
        assert_eq!(
            xml_safe("p::before{content:\"a<b&c>d\"}").unwrap(),
            "p::before{content:\"a\\3c b\\26 c>d\";}"
        );
        assert_eq!(
            xml_safe(".a\\<b, #x\\&y, [t=\"]]>\"] { font-family: A\\&B; width: 1x\\<y }").unwrap(),
            ".a\\3c b, #x\\26 y, [t=\"]]\\3e \"] {font-family:A\\26 B;width:1x\\3c y;}"
        );
        // A non-XML character, escaped like a control.
        assert_eq!(
            xml_safe("p{content:\"\u{FFFF}\"}").unwrap(),
            "p{content:\"\\ffff \";}"
        );
    }

    #[test]
    fn xml_safe_drops_comments() {
        assert_eq!(xml_safe("p{} /* a < b & c ]]> */").unwrap(), "p{}");
        assert_eq!(xml_safe("a, /* <b> */ b {}").unwrap(), "a,  b {}");
    }

    #[test]
    fn xml_safe_refuses_unescapable_delimiters() {
        assert_eq!(
            xml_refused("@media (width < 600px) { p { } }"),
            (XmlSafeErrorKind::Delimiter('<'), "<")
        );
        assert_eq!(
            xml_refused("p { & b { color: red } }"),
            (XmlSafeErrorKind::Delimiter('&'), "&")
        );
        assert_eq!(
            xml_refused("p { color: red <!-- }"),
            (XmlSafeErrorKind::Cdo, "<!--")
        );
        assert_eq!(
            xml_refused("p { grid-template-columns: [[a]]> }"),
            (XmlSafeErrorKind::CdataEnd, ">")
        );
        // A `<!--` and `-->` at the top level are dropped by the parser.
        assert_eq!(xml_safe("<!-- p { } -->").unwrap(), "p {}");
    }

    #[test]
    fn xml_safe_refuses_urls_rather_than_escaping_them() {
        for (css, url) in [
            ("p { background: url(a&b.png) }", "url(a&b.png)"),
            ("p { background: url(\"a<b.png\") }", "\"a<b.png\""),
            ("@import \"a&b.css\";", "\"a&b.css\""),
            ("@namespace x \"urn:a&b\";", "\"urn:a&b\""),
            ("p { background: url(a]]>b) }", "url(a]]>b)"),
            ("p { background: url(a b&c) }", "url(a b&c)"),
        ] {
            assert_eq!(xml_refused(css), (XmlSafeErrorKind::Url, url), "{css}");
        }
        // `>` alone is XML-safe, and a url without these characters passes.
        assert_eq!(
            xml_safe("p{background:url(a>b.png) url(\"c.png\")}").unwrap(),
            "p{background:url(a>b.png) url(\"c.png\");}"
        );
    }
}
