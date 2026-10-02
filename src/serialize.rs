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
use crate::span::Spanned;
use crate::token::Token;
use crate::tokenizer::is_name;

fn needs_control_escape(c: char) -> bool {
    matches!(c, '\u{1}'..='\u{1f}' | '\u{7f}')
}

fn escape_code_point(c: char, out: &mut String) {
    use std::fmt::Write;
    let _ = write!(out, "\\{:x} ", c as u32);
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
fn serialize_name(s: &str, out: &mut String, check_leading_digit: bool) {
    if s == "-" {
        out.push_str("\\-");
        return;
    }
    let starts_with_dash = s.starts_with('-');
    for (i, c) in s.chars().enumerate() {
        if c == '\0' {
            out.push('\u{FFFD}');
        } else if needs_control_escape(c) {
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

pub fn serialize_ident(s: &str, out: &mut String) {
    serialize_name(s, out, true);
}

pub fn serialize_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
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
}

/// Serializes a bare (unquoted) `url(...)` token's contents, escaping
/// whitespace/quotes/parens/backslash so the result is always valid to
/// re-tokenize as a `url-token` rather than accidentally becoming a
/// `bad-url-token`.
pub fn serialize_url(s: &str, out: &mut String) {
    out.push_str("url(");
    for c in s.chars() {
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
}

fn serialize_dimension_unit(unit: &str, out: &mut String) {
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
        serialize_name(chars.as_str(), out, false);
        return;
    }
    serialize_name(unit, out, false);
}

pub fn serialize_token(t: &Token, out: &mut String) {
    match t {
        Token::Ident(s) => serialize_ident(s, out),
        Token::Function(s) => {
            serialize_ident(s, out);
            out.push('(');
        }
        Token::AtKeyword(s) => {
            out.push('@');
            serialize_ident(s, out);
        }
        Token::Hash { value, is_id } => {
            out.push('#');
            serialize_name(value, out, *is_id);
        }
        Token::String(s) => serialize_string(s, out),
        // An unterminated string: a quote and the newline that ended it,
        // which re-tokenizes as a bad string again rather than swallowing
        // the text after it.
        Token::BadString => out.push_str("\"\n"),
        Token::Url(s) => serialize_url(s, out),
        Token::BadUrl(raw) => {
            out.push_str(raw);
            if !raw.ends_with(')') {
                out.push(')');
            }
        }
        Token::Delim(c) => out.push(*c),
        Token::Number { repr, .. } => out.push_str(repr),
        Token::Percentage { repr, .. } => {
            out.push_str(repr);
            out.push('%');
        }
        Token::Dimension { repr, unit, .. } => {
            out.push_str(repr);
            serialize_dimension_unit(unit, out);
        }
        Token::UnicodeRange { start, end } => {
            use std::fmt::Write;
            let _ = write!(out, "U+{start:X}");
            if end != start {
                let _ = write!(out, "-{end:X}");
            }
        }
        Token::Whitespace => out.push(' '),
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
}

pub fn serialize_component_value(v: &ComponentValue, out: &mut String) {
    match v {
        ComponentValue::Token(t) => serialize_token(t, out),
        ComponentValue::Function { name, args } => {
            serialize_ident(name, out);
            out.push('(');
            serialize_values(args, out);
            out.push(')');
        }
        ComponentValue::Block(b) => serialize_simple_block(b, out),
    }
}

/// A list of component values: a prelude, a declaration's value, a block's
/// or function's contents.
pub fn serialize_values(values: &[Spanned<ComponentValue>], out: &mut String) {
    for v in values {
        serialize_component_value(&v.node, out);
    }
}

pub fn serialize_simple_block(b: &SimpleBlock, out: &mut String) {
    let (open, close) = match b.kind {
        BlockKind::Curly => ('{', '}'),
        BlockKind::Square => ('[', ']'),
        BlockKind::Paren => ('(', ')'),
    };
    out.push(open);
    serialize_values(&b.values, out);
    out.push(close);
}

pub fn serialize_declaration(d: &Declaration, out: &mut String) {
    serialize_ident(&d.name, out);
    out.push(':');
    serialize_values(&d.value, out);
    if d.important {
        out.push_str("!important");
    }
}

fn serialize_items(items: &[BlockItem], out: &mut String) {
    for item in items {
        match item {
            BlockItem::Declaration(d) => {
                serialize_declaration(&d.node, out);
                out.push(';');
            }
            BlockItem::Rule(r) => serialize_rule(&r.node, out),
        }
    }
}

pub fn serialize_rule(r: &Rule, out: &mut String) {
    match r {
        Rule::Qualified(q) => serialize_values(&q.prelude, out),
        Rule::At(a) => {
            out.push('@');
            serialize_ident(&a.name, out);
            serialize_values(&a.prelude, out);
        }
    }
    match r.block() {
        Some(b) => {
            out.push('{');
            serialize_items(&b.node, out);
            out.push('}');
        }
        None => out.push(';'),
    }
}

pub fn serialize_stylesheet(sheet: &Stylesheet) -> String {
    let mut out = String::new();
    for r in &sheet.rules {
        serialize_rule(&r.node, &mut out);
    }
    out
}

/// The contents of a block, as [`parse_block_contents`](crate::parse_block_contents)
/// returns them — a `style="…"` attribute's value, say.
pub fn serialize_block_contents(items: &[BlockItem]) -> String {
    let mut out = String::new();
    serialize_items(items, &mut out);
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
        serialize_dimension_unit("e2", &mut out);
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
}
