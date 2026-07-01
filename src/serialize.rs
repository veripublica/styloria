//! Serialization back to CSS text (CSS Syntax Level 3, "Serialization":
//! <https://www.w3.org/TR/css-syntax-3/#serialization>).
//!
//! The acceptance bar for this increment is round-tripping to *equivalent*
//! CSS, not byte-identical output: whitespace/comments aren't preserved
//! (tokenization discards them), but escaping must always be correct — a
//! serialized identifier/string/url must always re-tokenize to the same
//! value it started from.

use crate::parser::{
    ComponentValue, Declaration, DeclarationListItem, Rule, SimpleBlock, Stylesheet,
};
use crate::token::Token;
use crate::tokenizer::is_name;

fn needs_control_escape(c: char) -> bool {
    matches!(c, '\u{1}'..='\u{1f}' | '\u{7f}')
}

fn escape_code_point(c: char, out: &mut String) {
    out.push('\\');
    out.push_str(&format!("{:x}", c as u32));
    out.push(' ');
}

/// Serialize a "name" body (an identifier's or hash's textual content),
/// escaping whatever isn't a plain name code point. `check_leading_digit`
/// enables the ident-only rule that a name can't start (or start with `-`
/// then) a digit without escaping it — hash-token values don't need this
/// (`#123` is already unambiguous as a hash).
fn serialize_name(s: &str, out: &mut String, check_leading_digit: bool) {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() == 1 && chars[0] == '-' {
        out.push('\\');
        out.push('-');
        return;
    }
    for (i, &c) in chars.iter().enumerate() {
        if c == '\0' {
            out.push('\u{FFFD}');
        } else if needs_control_escape(c) {
            escape_code_point(c, out);
        } else if check_leading_digit
            && c.is_ascii_digit()
            && (i == 0 || (i == 1 && chars[0] == '-'))
        {
            escape_code_point(c, out);
        } else if is_name(c) {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
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
                out.push('\\');
                out.push(c);
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
    if let Some(first) = chars.next() {
        if matches!(first, 'e' | 'E')
            && matches!(chars.clone().next(), Some(c) if c.is_ascii_digit() || c == '+' || c == '-')
        {
            escape_code_point(first, out);
            let rest: String = chars.collect();
            serialize_name(&rest, out, false);
            return;
        }
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
        Token::BadString => out.push('"'),
        Token::Url(s) => serialize_url(s, out),
        Token::BadUrl => out.push_str("url()"),
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
            for a in args {
                serialize_component_value(a, out);
            }
            out.push(')');
        }
        ComponentValue::Block(b) => serialize_simple_block(b, out),
    }
}

pub fn serialize_simple_block(b: &SimpleBlock, out: &mut String) {
    let (open, close) = match b.kind {
        crate::parser::BlockKind::Curly => ('{', '}'),
        crate::parser::BlockKind::Square => ('[', ']'),
        crate::parser::BlockKind::Paren => ('(', ')'),
    };
    out.push(open);
    for v in &b.values {
        serialize_component_value(v, out);
    }
    out.push(close);
}

pub fn serialize_declaration(d: &Declaration, out: &mut String) {
    serialize_ident(&d.name, out);
    out.push(':');
    for v in &d.value {
        serialize_component_value(v, out);
    }
    if d.important {
        out.push_str("!important");
    }
}

fn serialize_prelude(prelude: &[ComponentValue], out: &mut String) {
    for v in prelude {
        serialize_component_value(v, out);
    }
}

pub fn serialize_rule(r: &Rule, out: &mut String) {
    match r {
        Rule::Qualified(q) => {
            serialize_prelude(&q.prelude, out);
            serialize_simple_block(&q.block, out);
        }
        Rule::At(a) => {
            out.push('@');
            serialize_ident(&a.name, out);
            serialize_prelude(&a.prelude, out);
            match &a.block {
                Some(b) => serialize_simple_block(b, out),
                None => out.push(';'),
            }
        }
    }
}

pub fn serialize_stylesheet(sheet: &Stylesheet) -> String {
    let mut out = String::new();
    for r in &sheet.rules {
        serialize_rule(r, &mut out);
    }
    out
}

pub fn serialize_declaration_list(items: &[DeclarationListItem]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        match item {
            DeclarationListItem::Declaration(d) => serialize_declaration(d, &mut out),
            DeclarationListItem::AtRule(a) => serialize_rule(&Rule::At(a.clone()), &mut out),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
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
    fn string_roundtrip_with_quote_and_backslash() {
        let mut out = String::new();
        serialize_string("a\"b\\c", &mut out);
        let toks: Vec<_> = Tokenizer::new(&out).collect();
        assert_eq!(toks, vec![Token::String("a\"b\\c".into())]);
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

    #[test]
    fn stylesheet_roundtrip() {
        let css = r#"
            @media screen and (min-width: 10px) {
                a.foo::before { content: "he said \"hi\""; color: red !important; }
            }
            --custom: { a b c };
            p { margin: 0 auto; }
        "#;
        let sheet = Parser::parse_stylesheet(css);
        let out = serialize_stylesheet(&sheet);
        let sheet2 = Parser::parse_stylesheet(&out);
        assert_eq!(
            sheet, sheet2,
            "round-tripped stylesheet parsed differently:\n{out}"
        );
    }

    #[test]
    fn declaration_list_roundtrip() {
        let css = "color: red; --x: 1px solid blue; margin:0 10px !important";
        let items = Parser::parse_declaration_list(css);
        let out = serialize_declaration_list(&items);
        let items2 = Parser::parse_declaration_list(&out);
        assert_eq!(
            items, items2,
            "round-tripped declaration list parsed differently:\n{out}"
        );
    }
}
