//! Serialization back to CSS text (CSS Syntax Level 3, "Serialization":
//! <https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/#serialization>).
//!
//! The acceptance bar is round-tripping to *equivalent* CSS, not
//! byte-identical output: whitespace/comments aren't preserved
//! (tokenization discards them), but escaping must always be correct — a
//! serialized identifier/string/url must always re-tokenize to the same
//! value it started from.
//!
//! One tree has no text: a nested rule whose prelude is only a name and a
//! colon (`a:{…}` after junk made it fail as a declaration) reads back as a
//! declaration. Its selector is invalid, so a browser drops it either way.

use crate::parser::{
    BlockItem, BlockKind, ComponentValue, Declaration, Rule, SimpleBlock, Stylesheet,
};
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

/// In XML-safe output, write `c` as a CSS escape if XML cannot hold it as
/// itself. Whether it did is the answer.
fn escape_for_xml(c: char, out: &mut String, xml: bool) -> bool {
    if xml && xml_unsafe(c, out) {
        escape_code_point(c, out);
        return true;
    }
    false
}

/// Append `s` as a double-quoted string, escaped so it re-tokenizes as the
/// same string.
pub fn serialize_string(s: &str, out: &mut String) {
    write_string(s, out, false);
}

fn write_string(s: &str, out: &mut String, xml: bool) {
    out.push('"');
    for c in s.chars() {
        if escape_for_xml(c, out, xml) {
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
}

/// Serializes a bare (unquoted) `url(...)` token's contents, escaping
/// whitespace/quotes/parens/backslash so the result is always valid to
/// re-tokenize as a `url-token` rather than accidentally becoming a
/// `bad-url-token`.
pub fn serialize_url(s: &str, out: &mut String) {
    write_url(s, out, false);
}

fn write_url(s: &str, out: &mut String, xml: bool) {
    out.push_str("url(");
    for c in s.chars() {
        if escape_for_xml(c, out, xml) {
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
    /// A `<bad-url-token>` holding `<`, `&`, `]]>` or a character XML does
    /// not allow: it is written as its source text, which has no escapes.
    BadUrl,
    /// A `>` delimiter right after two `]`, which would close a CDATA
    /// section.
    CdataEnd,
}

impl std::fmt::Display for XmlSafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            XmlSafeErrorKind::Delimiter(c) => write!(f, "a {c:?} delimiter")?,
            XmlSafeErrorKind::Cdo => f.write_str("a `<!--`")?,
            XmlSafeErrorKind::BadUrl => {
                f.write_str("a bad URL holding a character XML cannot take")?
            }
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

type Written = Result<(), XmlSafeError>;

/// The last token written, as far as the next one could run into it once
/// the comments between them are gone.
#[derive(Clone, Copy, PartialEq)]
enum Prev {
    Other,
    Whitespace,
    Ident,
    /// An at-keyword, a hash or a dimension: a name the next token can
    /// extend.
    AtHashDimension,
    Number,
    /// A `#` or `-` delimiter.
    HashOrMinus,
    /// An `@` delimiter.
    At,
    /// A `.` or `+` delimiter.
    DotOrPlus,
    /// A `/` delimiter.
    Slash,
    /// A `<` delimiter.
    Less,
    UnicodeRange,
}

/// The token about to be written, in the same terms.
#[derive(Clone, Copy, PartialEq)]
enum Next {
    Other,
    Ident,
    Function,
    Url,
    BadUrl,
    Minus,
    Number,
    Percentage,
    Dimension,
    Cdc,
    LeftParen,
    Star,
    Percent,
    Question,
    Bang,
}

/// Whether `prev` and `next` written side by side would read back as other
/// tokens, so an empty comment has to go between them: the table in CSS
/// Syntax's "Serialization", plus a number before `-` (`1` `-` `-x` would
/// read as the dimension `1--x`), a `<` before `!` (a CDO), and a unicode
/// range, which a number or name would extend.
fn needs_comment(prev: Prev, next: Next) -> bool {
    use Next::*;
    let name_or_number = matches!(
        next,
        Ident | Function | Url | BadUrl | Minus | Number | Percentage | Dimension
    );
    match prev {
        Prev::Ident => name_or_number || matches!(next, Cdc | LeftParen),
        Prev::AtHashDimension => name_or_number || next == Cdc,
        Prev::HashOrMinus => name_or_number,
        Prev::Number => name_or_number || next == Percent,
        Prev::At => matches!(next, Ident | Function | Url | BadUrl | Minus),
        Prev::DotOrPlus => matches!(next, Number | Percentage | Dimension),
        Prev::Slash => next == Star,
        Prev::Less => next == Bang,
        Prev::UnicodeRange => name_or_number || next == Question,
        Prev::Other | Prev::Whitespace => false,
    }
}

fn next_of(t: &Token) -> Next {
    match t {
        Token::Ident(_) => Next::Ident,
        Token::Function(_) => Next::Function,
        Token::Url(_) => Next::Url,
        Token::BadUrl(_) => Next::BadUrl,
        Token::Number { .. } => Next::Number,
        Token::Percentage { .. } => Next::Percentage,
        Token::Dimension { .. } => Next::Dimension,
        Token::Cdc => Next::Cdc,
        Token::LeftParen => Next::LeftParen,
        Token::Delim('-') => Next::Minus,
        Token::Delim('*') => Next::Star,
        Token::Delim('%') => Next::Percent,
        Token::Delim('?') => Next::Question,
        Token::Delim('!') => Next::Bang,
        _ => Next::Other,
    }
}

fn prev_of(t: &Token) -> Prev {
    match t {
        Token::Ident(_) => Prev::Ident,
        Token::AtKeyword(_) | Token::Hash { .. } | Token::Dimension { .. } => Prev::AtHashDimension,
        Token::Number { .. } => Prev::Number,
        Token::Delim('#' | '-') => Prev::HashOrMinus,
        Token::Delim('@') => Prev::At,
        Token::Delim('.' | '+') => Prev::DotOrPlus,
        Token::Delim('/') => Prev::Slash,
        Token::Delim('<') => Prev::Less,
        Token::UnicodeRange { .. } => Prev::UnicodeRange,
        Token::Whitespace => Prev::Whitespace,
        _ => Prev::Other,
    }
}

/// Writes the tree, keeping track of the last token so that two tokens a
/// comment kept apart are not run together.
struct Writer {
    out: String,
    /// XML-safe output: see [`serialize_stylesheet_for_xml`].
    xml: bool,
    prev: Prev,
}

impl Writer {
    fn plain(out: &mut String) -> Writer {
        Writer {
            out: std::mem::take(out),
            xml: false,
            prev: Prev::Other,
        }
    }

    /// Start a token of kind `next`, separating it from the last one if
    /// they would run together.
    fn start(&mut self, next: Next) {
        if needs_comment(self.prev, next) {
            self.out.push_str("/**/");
        }
    }

    /// Punctuation: nothing runs into it, and it runs into nothing.
    fn punct(&mut self, s: &str) {
        self.out.push_str(s);
        self.prev = Prev::Other;
    }

    fn name(&mut self, s: &str, check_leading_digit: bool) {
        serialize_name(s, &mut self.out, check_leading_digit, self.xml);
    }

    fn token(&mut self, t: &Token, span: Span) -> Written {
        let refuse = |kind| Err(XmlSafeError { span, kind });
        let x = self.xml;
        // Two whitespace tokens (a comment once stood between them) are
        // written as one space, as they read back.
        if *t == Token::Whitespace && self.prev == Prev::Whitespace {
            return Ok(());
        }
        match t {
            Token::Delim(c @ ('<' | '&')) if x => {
                return refuse(XmlSafeErrorKind::Delimiter(*c));
            }
            Token::Delim('>') if x && self.out.ends_with("]]") => {
                return refuse(XmlSafeErrorKind::CdataEnd);
            }
            Token::Delim(c) if x && xml_unsafe(*c, &self.out) => {
                return refuse(XmlSafeErrorKind::Delimiter(*c));
            }
            Token::Cdo if x => return refuse(XmlSafeErrorKind::Cdo),
            _ => {}
        }
        self.start(next_of(t));
        let out = &mut self.out;
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
            Token::String(s) => write_string(s, out, x),
            // An unterminated string: a quote and the newline that ended it,
            // which re-tokenizes as a bad string again rather than swallowing
            // the text after it.
            Token::BadString => out.push_str("\"\n"),
            Token::Url(s) => write_url(s, out, x),
            Token::BadUrl(raw) => {
                if x {
                    // Only the last two characters written can join a `]]>`.
                    let mut tail: Vec<char> = out.chars().rev().take(2).collect();
                    tail.reverse();
                    let mut probe: String = tail.into_iter().collect();
                    for c in raw.chars() {
                        let xml_control =
                            needs_control_escape(c) && !matches!(c, '\t' | '\n' | '\r');
                        if xml_control || xml_unsafe(c, &probe) {
                            return refuse(XmlSafeErrorKind::BadUrl);
                        }
                        probe.push(c);
                    }
                }
                out.push_str(raw);
                if !raw.ends_with(')') {
                    out.push(')');
                }
            }
            // A `\` is a delimiter only when a newline follows it, and
            // anything else after it would read as an escape.
            Token::Delim('\\') => out.push_str("\\\n"),
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
        self.prev = prev_of(t);
        Ok(())
    }

    fn component_value(&mut self, v: &ComponentValue, span: Span) -> Written {
        match v {
            ComponentValue::Token(t) => self.token(t, span),
            ComponentValue::Function { name, args } => {
                self.start(Next::Function);
                self.name(name, true);
                self.punct("(");
                self.values(args)?;
                self.punct(")");
                Ok(())
            }
            ComponentValue::Block(b) => self.simple_block(b),
        }
    }

    fn values(&mut self, values: &[Spanned<ComponentValue>]) -> Written {
        for v in values {
            self.component_value(&v.node, v.span)?;
        }
        Ok(())
    }

    fn simple_block(&mut self, b: &SimpleBlock) -> Written {
        let (open, close) = match b.kind {
            BlockKind::Curly => ("{", "}"),
            BlockKind::Square => ("[", "]"),
            BlockKind::Paren => {
                self.start(Next::LeftParen);
                ("(", ")")
            }
        };
        self.punct(open);
        self.values(&b.values)?;
        self.punct(close);
        Ok(())
    }

    fn declaration(&mut self, d: &Declaration) -> Written {
        self.name(&d.name, true);
        self.punct(":");
        self.values(&d.value)?;
        if d.important {
            self.punct("!important");
        }
        Ok(())
    }

    fn items(&mut self, items: &[BlockItem]) -> Written {
        for item in items {
            match item {
                BlockItem::Declaration(d) => {
                    self.declaration(&d.node)?;
                    self.punct(";");
                }
                BlockItem::Rule(r) => self.rule(&r.node)?,
            }
        }
        Ok(())
    }

    fn rule(&mut self, r: &Rule) -> Written {
        match r {
            Rule::Qualified(q) => self.values(&q.prelude)?,
            Rule::At(a) => {
                self.out.push('@');
                self.name(&a.name, true);
                self.prev = Prev::AtHashDimension;
                self.values(&a.prelude)?;
            }
        }
        match r.block() {
            Some(b) => {
                self.punct("{");
                self.items(&b.node)?;
                self.punct("}");
            }
            None => self.punct(";"),
        }
        Ok(())
    }

    fn rules(&mut self, sheet: &Stylesheet) -> Written {
        for r in &sheet.rules {
            self.rule(&r.node)?;
        }
        Ok(())
    }

    /// The finished text. A U+FEFF at its very start would read back as a
    /// byte-order mark and vanish, so a comment goes before it.
    fn finish(mut self) -> String {
        if self.out.starts_with('\u{FEFF}') {
            self.out.insert_str(0, "/**/");
        }
        self.out
    }
}

/// Append one token. A function token writes its name and `(`.
pub fn serialize_token(t: &Token, out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.token(t, Span::new(0, 0));
    *out = w.out;
}

/// Append one component value: a token, a function with its arguments and
/// `)`, or a simple block with its brackets.
pub fn serialize_component_value(v: &ComponentValue, out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.component_value(v, Span::new(0, 0));
    *out = w.out;
}

/// A list of component values: a prelude, a declaration's value, a block's
/// or function's contents.
pub fn serialize_values(values: &[Spanned<ComponentValue>], out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.values(values);
    *out = w.out;
}

/// Append a `{ … }`, `[ … ]` or `( … )` block.
pub fn serialize_simple_block(b: &SimpleBlock, out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.simple_block(b);
    *out = w.out;
}

/// Append `name:value`, with `!important` when set and no trailing `;`.
pub fn serialize_declaration(d: &Declaration, out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.declaration(d);
    *out = w.out;
}

/// Append a rule with its block, or with `;` for a statement at-rule.
pub fn serialize_rule(r: &Rule, out: &mut String) {
    let mut w = Writer::plain(out);
    let _ = w.rule(r);
    *out = w.out;
}

/// The whole stylesheet as equivalent CSS: comments are not kept, and a run
/// of whitespace is one space, so it reads back to the same rules and
/// values, at other spans. Where two tokens would run together without the
/// comment that separated them (`1px/**/2px`), an empty comment stays.
pub fn serialize_stylesheet(sheet: &Stylesheet) -> String {
    let mut out = String::new();
    let mut w = Writer::plain(&mut out);
    let _ = w.rules(sheet);
    w.finish()
}

/// The stylesheet as [`serialize_stylesheet`] writes it, but fit to stand as
/// the text of an XHTML `<style>` element without XML escaping: no `<`, no
/// `&`, no `]]>`, and only characters XML 1.0 allows. Where one of those is
/// inside a string, an identifier, a hash or a URL it is written as a CSS
/// escape
/// (`\3c `, `\26 `, `\3e `), which an XML and an HTML parser both pass
/// through and CSS reads back as the same text (CSS Syntax decodes escapes
/// in URLs too, §4.3.6).
///
/// The error names the first component value no escape can save: a `<` or
/// `&` delimiter (`@media (width < 600px)`, the nesting selector `&`), a
/// `<!--`, a `<bad-url-token>` holding such a character, or a `]]>` the
/// tokens themselves spell.
pub fn serialize_stylesheet_for_xml(sheet: &Stylesheet) -> Result<String, XmlSafeError> {
    let mut w = Writer {
        out: String::new(),
        xml: true,
        prev: Prev::Other,
    };
    w.rules(sheet)?;
    Ok(w.finish())
}

/// The contents of a block, as [`parse_block_contents`](crate::parse_block_contents)
/// returns them — a `style="…"` attribute's value, say.
pub fn serialize_block_contents(items: &[BlockItem]) -> String {
    let mut out = String::new();
    let mut w = Writer::plain(&mut out);
    let _ = w.items(items);
    w.finish()
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

    /// Tokens a comment kept apart stay apart: the output reads back to the
    /// same tokens as the input, comments and whitespace runs aside.
    #[test]
    fn tokens_a_comment_separated_stay_separate() {
        let tokens = |css: &str| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for t in Tokenizer::new(css) {
                if t == Token::Whitespace && out.last().is_some_and(|l| l == "Whitespace") {
                    continue;
                }
                out.push(format!("{t:?}"));
            }
            out
        };
        for css in [
            "p{margin:1px/**/2px}",
            "a/**/b{}",
            "p{a:b/**/c;d:#x/**/y;e:1/**/%;f:-/**/-x;g:x/**/(y)}",
            "p{a:1/**/e2;b:a/**/-;c:./**/5;d:+/**/1;e:1/**/.5;f:1/**/-/**/-x}",
            "@media/**/screen{}",
            "p{a:@/**/x;b:#/**/x;c:a/**/url(x);d:a/**/-->}",
            "p{a:</**/!/**/-/**/-}",
            "p{a:x /* */ /* */ y}",
            "p{a:b \\\n;c:d}",
            "<!--\u{FEFF} p{}",
        ] {
            let (sheet, _) = parse_stylesheet(css);
            let once = serialize_stylesheet(&sheet);
            let again = serialize_stylesheet(&parse_stylesheet(&once).0);
            assert_eq!(once, again, "not a fixed point: {css:?} → {once:?}");
            let strip = |v: Vec<String>| -> Vec<String> {
                v.into_iter().filter(|t| t != "Whitespace").collect()
            };
            // The rules' own tokens, without the ones the parser adds or
            // drops (`;` after the last declaration, a top-level `<!--`).
            let mut a = strip(tokens(&once));
            let mut b = strip(tokens(css));
            a.retain(|t| t != "Semicolon" && t != "Cdo");
            b.retain(|t| t != "Semicolon" && t != "Cdo");
            assert_eq!(a, b, "{css:?} → {once:?}");
        }
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
        let (again, _) = parse_stylesheet(&written);
        assert_eq!(
            serialize_stylesheet(&again),
            serialize_stylesheet(&sheet),
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
        assert_eq!(xml_safe("a, /* <b> */ b {}").unwrap(), "a, b {}");
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
    fn xml_safe_escapes_inside_urls() {
        for (css, written) in [
            (
                "p { background: url(a&b.png) }",
                "p {background:url(a\\26 b.png);}",
            ),
            (
                "p { background: url(\"a<b.png\") }",
                "p {background:url(\"a\\3c b.png\");}",
            ),
            ("@import \"a&b.css\";", "@import \"a\\26 b.css\";"),
            (
                "p { background: url(a]]>b) }",
                "p {background:url(a]]\\3e b);}",
            ),
            // `>` alone is XML-safe.
            ("p{background:url(a>b.png)}", "p{background:url(a>b.png);}"),
        ] {
            assert_eq!(xml_safe(css).unwrap(), written, "{css}");
        }
        // A bad URL is its source text: nothing in it can be escaped.
        assert_eq!(
            xml_refused("p { background: url(a b&c) }"),
            (XmlSafeErrorKind::BadUrl, "url(a b&c)")
        );
    }
}
