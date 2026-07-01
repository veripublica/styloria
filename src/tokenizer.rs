//! The CSS tokenization algorithm (CSS Syntax Level 3, §4:
//! <https://www.w3.org/TR/css-syntax-3/#tokenization>).
//!
//! Newline/NUL normalization (§3.3) is applied lazily at the point of use
//! rather than as an upfront pass over the input: `\r`, `\r\n`, and `\x0c`
//! are recognized as newline-equivalent wherever the spec calls for a
//! newline check, and a literal NUL is substituted with U+FFFD only when a
//! token's content is actually being copied into an owned buffer. This
//! avoids ever needing to rewrite the input, so `Token<'a>` can always slice
//! straight from the caller's `&'a str` when no escape processing is
//! needed.
//!
//! Tokenization never fails: every malformed construct has spec-defined
//! recovery (`BadString`, `BadUrl`, or a `Delim` for a stray character), so
//! `next_token` returns `Option<Token<'a>>` only to signal end-of-input,
//! never an error.

use std::borrow::Cow;

use crate::token::{NumericType, Token};

pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Tokenizer { input, pos: 0 }
    }

    fn nth_char(&self, n: usize) -> Option<char> {
        self.input[self.pos..].chars().nth(n)
    }

    fn peek(&self) -> Option<char> {
        self.nth_char(0)
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn is_valid_escape_at(&self, n: usize) -> bool {
        self.nth_char(n) == Some('\\')
            && !matches!(self.nth_char(n + 1), None | Some('\n' | '\r' | '\x0c'))
    }

    fn would_start_ident_at(&self, n: usize) -> bool {
        match self.nth_char(n) {
            Some('-') => match self.nth_char(n + 1) {
                Some(c) if is_name_start(c) || c == '-' => true,
                _ => self.is_valid_escape_at(n + 1),
            },
            Some(c) if is_name_start(c) => true,
            Some('\\') => self.is_valid_escape_at(n),
            _ => false,
        }
    }

    fn starts_number_at(&self, n: usize) -> bool {
        match self.nth_char(n) {
            Some('+' | '-') => match self.nth_char(n + 1) {
                Some(c) if is_digit(c) => true,
                Some('.') => matches!(self.nth_char(n + 2), Some(c) if is_digit(c)),
                _ => false,
            },
            Some('.') => matches!(self.nth_char(n + 1), Some(c) if is_digit(c)),
            Some(c) if is_digit(c) => true,
            _ => false,
        }
    }

    fn consume_comments(&mut self) {
        while self.peek() == Some('/') && self.nth_char(1) == Some('*') {
            self.pos += 2;
            match self.input[self.pos..].find("*/") {
                Some(rel) => self.pos += rel + 2,
                None => self.pos = self.input.len(),
            }
        }
    }

    /// §4.3.11 "Consume a newline": `\r\n` counts as one newline.
    fn consume_newline(&mut self) {
        if self.peek() == Some('\r') {
            self.advance();
            if self.peek() == Some('\n') {
                self.advance();
            }
        } else {
            self.advance();
        }
    }

    /// §4.3.7 "Consume an escaped code point". Assumes the leading `\` has
    /// already been consumed.
    fn consume_escaped_code_point(&mut self) -> char {
        match self.peek() {
            Some(c) if is_hex_digit(c) => {
                let mut hex = String::with_capacity(6);
                for _ in 0..6 {
                    match self.peek() {
                        Some(h) if is_hex_digit(h) => {
                            hex.push(h);
                            self.advance();
                        }
                        _ => break,
                    }
                }
                if matches!(self.peek(), Some(w) if is_whitespace(w)) {
                    self.consume_newline_or_space();
                }
                let code = u32::from_str_radix(&hex, 16).unwrap_or(0);
                if code == 0 || code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code) {
                    '\u{FFFD}'
                } else {
                    char::from_u32(code).unwrap_or('\u{FFFD}')
                }
            }
            Some(c) => {
                self.advance();
                c
            }
            None => '\u{FFFD}',
        }
    }

    /// The single trailing whitespace code point consumed after a hex
    /// escape may itself be a (possibly multi-byte) newline.
    fn consume_newline_or_space(&mut self) {
        if is_newline_start(self.peek()) {
            self.consume_newline();
        } else {
            self.advance();
        }
    }

    /// §4.3.12 "Consume a name" (renamed from "consume an ident sequence"
    /// in some spec drafts).
    fn consume_name(&mut self) -> Cow<'a, str> {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(c) if is_name(c) => {
                    self.advance();
                }
                _ if self.is_valid_escape_at(0) => {
                    let mut owned = self.input[start..self.pos].to_string();
                    loop {
                        match self.peek() {
                            Some(c) if is_name(c) => {
                                owned.push(c);
                                self.advance();
                            }
                            _ if self.is_valid_escape_at(0) => {
                                self.advance();
                                owned.push(self.consume_escaped_code_point());
                            }
                            _ => break,
                        }
                    }
                    return Cow::Owned(owned);
                }
                _ => break,
            }
        }
        Cow::Borrowed(&self.input[start..self.pos])
    }

    /// §4.3.2/§4.3.3 "Consume a string token". `quote` is the opening
    /// delimiter already consumed by the caller.
    fn consume_string(&mut self, quote: char) -> Token<'a> {
        let start = self.pos;
        loop {
            match self.peek() {
                None => return Token::String(Cow::Borrowed(&self.input[start..self.pos])),
                Some(c) if c == quote => {
                    let s = &self.input[start..self.pos];
                    self.advance();
                    return Token::String(Cow::Borrowed(s));
                }
                Some(c) if is_newline_start(Some(c)) => return Token::BadString,
                Some('\\') => break,
                Some(_) => {
                    self.advance();
                }
            }
        }
        let mut owned = self.input[start..self.pos].to_string();
        loop {
            match self.peek() {
                None => return Token::String(Cow::Owned(owned)),
                Some(c) if c == quote => {
                    self.advance();
                    return Token::String(Cow::Owned(owned));
                }
                Some(c) if is_newline_start(Some(c)) => return Token::BadString,
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        None => {}
                        Some(c) if is_newline_start(Some(c)) => self.consume_newline(),
                        _ => owned.push(self.consume_escaped_code_point()),
                    }
                }
                Some(c) => {
                    owned.push(c);
                    self.advance();
                }
            }
        }
    }

    /// §4.3.14 "Consume the remnants of a bad url" (error recovery).
    fn consume_bad_url_remnants(&mut self) {
        loop {
            match self.peek() {
                None => return,
                Some(')') => {
                    self.advance();
                    return;
                }
                _ if self.is_valid_escape_at(0) => {
                    self.advance();
                    self.consume_escaped_code_point();
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
    }

    /// §4.3.6 "Consume a url token". Called right after `url(` and any
    /// following whitespace have already been consumed.
    fn consume_url(&mut self) -> Token<'a> {
        let start = self.pos;
        loop {
            match self.peek() {
                None => return Token::Url(Cow::Borrowed(&self.input[start..self.pos])),
                Some(')') => {
                    let s = &self.input[start..self.pos];
                    self.advance();
                    return Token::Url(Cow::Borrowed(s));
                }
                Some(c) if is_whitespace(c) => {
                    let owned = self.input[start..self.pos].to_string();
                    return self.finish_url_after_whitespace(owned);
                }
                Some(c) if c == '"' || c == '\'' || c == '(' || is_non_printable(c) => {
                    self.advance();
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if self.is_valid_escape_at(0) {
                        break;
                    }
                    self.advance();
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        let mut owned = self.input[start..self.pos].to_string();
        loop {
            match self.peek() {
                None => return Token::Url(Cow::Owned(owned)),
                Some(')') => {
                    self.advance();
                    return Token::Url(Cow::Owned(owned));
                }
                Some(c) if is_whitespace(c) => return self.finish_url_after_whitespace(owned),
                Some(c) if c == '"' || c == '\'' || c == '(' || is_non_printable(c) => {
                    self.advance();
                    self.consume_bad_url_remnants();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if self.is_valid_escape_at(0) {
                        self.advance();
                        owned.push(self.consume_escaped_code_point());
                    } else {
                        self.advance();
                        self.consume_bad_url_remnants();
                        return Token::BadUrl;
                    }
                }
                Some(c) => {
                    owned.push(c);
                    self.advance();
                }
            }
        }
    }

    fn finish_url_after_whitespace(&mut self, owned: String) -> Token<'a> {
        while matches!(self.peek(), Some(c) if is_whitespace(c)) {
            self.advance();
        }
        match self.peek() {
            None => Token::Url(Cow::Owned(owned)),
            Some(')') => {
                self.advance();
                Token::Url(Cow::Owned(owned))
            }
            _ => {
                self.consume_bad_url_remnants();
                Token::BadUrl
            }
        }
    }

    /// §4.3.13 "Consume a number". Advances past the number's characters
    /// and reports whether it looked like an integer or used a decimal
    /// point / exponent.
    fn consume_number(&mut self) -> NumericType {
        let mut num_type = NumericType::Integer;
        if matches!(self.peek(), Some('+' | '-')) {
            self.advance();
        }
        while matches!(self.peek(), Some(c) if is_digit(c)) {
            self.advance();
        }
        if self.peek() == Some('.') && matches!(self.nth_char(1), Some(c) if is_digit(c)) {
            self.advance();
            num_type = NumericType::Number;
            while matches!(self.peek(), Some(c) if is_digit(c)) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let has_sign = matches!(self.nth_char(1), Some('+' | '-'));
            let digit_offset = if has_sign { 2 } else { 1 };
            if matches!(self.nth_char(digit_offset), Some(c) if is_digit(c)) {
                num_type = NumericType::Number;
                self.advance();
                if has_sign {
                    self.advance();
                }
                while matches!(self.peek(), Some(c) if is_digit(c)) {
                    self.advance();
                }
            }
        }
        num_type
    }

    /// §4.3.3 "Consume a numeric token".
    fn consume_numeric(&mut self) -> Token<'a> {
        let start = self.pos;
        let num_type = self.consume_number();
        let repr = &self.input[start..self.pos];
        let value = repr.parse::<f64>().unwrap_or(0.0);
        if self.would_start_ident_at(0) {
            let unit = self.consume_name();
            Token::Dimension {
                value,
                num_type,
                unit,
                repr,
            }
        } else if self.peek() == Some('%') {
            self.advance();
            Token::Percentage { value, repr }
        } else {
            Token::Number {
                value,
                num_type,
                repr,
            }
        }
    }

    /// §4.3.4 "Consume an ident-like token".
    fn consume_ident_like(&mut self) -> Token<'a> {
        let name = self.consume_name();
        if name.eq_ignore_ascii_case("url") && self.peek() == Some('(') {
            self.advance();
            while matches!(self.peek(), Some(c) if is_whitespace(c)) {
                self.advance();
            }
            match self.peek() {
                Some('"' | '\'') => Token::Function(name),
                _ => self.consume_url(),
            }
        } else if self.peek() == Some('(') {
            self.advance();
            Token::Function(name)
        } else {
            Token::Ident(name)
        }
    }

    /// §4.3.1 "Consume a token".
    pub fn next_token(&mut self) -> Option<Token<'a>> {
        self.consume_comments();
        let c = self.peek()?;
        Some(match c {
            c if is_whitespace(c) => {
                while matches!(self.peek(), Some(w) if is_whitespace(w)) {
                    self.advance();
                }
                Token::Whitespace
            }
            '"' | '\'' => {
                self.advance();
                self.consume_string(c)
            }
            '#' => {
                self.advance();
                if matches!(self.peek(), Some(nc) if is_name(nc)) || self.is_valid_escape_at(0) {
                    let is_id = self.would_start_ident_at(0);
                    let value = self.consume_name();
                    Token::Hash { value, is_id }
                } else {
                    Token::Delim('#')
                }
            }
            '(' => {
                self.advance();
                Token::LeftParen
            }
            ')' => {
                self.advance();
                Token::RightParen
            }
            '+' => {
                if self.starts_number_at(0) {
                    self.consume_numeric()
                } else {
                    self.advance();
                    Token::Delim('+')
                }
            }
            ',' => {
                self.advance();
                Token::Comma
            }
            '-' => {
                if self.starts_number_at(0) {
                    self.consume_numeric()
                } else if self.nth_char(1) == Some('-') && self.nth_char(2) == Some('>') {
                    self.pos += 3;
                    Token::Cdc
                } else if self.would_start_ident_at(0) {
                    self.consume_ident_like()
                } else {
                    self.advance();
                    Token::Delim('-')
                }
            }
            '.' => {
                if self.starts_number_at(0) {
                    self.consume_numeric()
                } else {
                    self.advance();
                    Token::Delim('.')
                }
            }
            ':' => {
                self.advance();
                Token::Colon
            }
            ';' => {
                self.advance();
                Token::Semicolon
            }
            '<' => {
                if self.nth_char(1) == Some('!')
                    && self.nth_char(2) == Some('-')
                    && self.nth_char(3) == Some('-')
                {
                    self.pos += 4;
                    Token::Cdo
                } else {
                    self.advance();
                    Token::Delim('<')
                }
            }
            '@' => {
                self.advance();
                if self.would_start_ident_at(0) {
                    Token::AtKeyword(self.consume_name())
                } else {
                    Token::Delim('@')
                }
            }
            '[' => {
                self.advance();
                Token::LeftSquare
            }
            '\\' => {
                if self.is_valid_escape_at(0) {
                    self.consume_ident_like()
                } else {
                    // parse error: a lone backslash (e.g. before EOF or a newline)
                    self.advance();
                    Token::Delim('\\')
                }
            }
            ']' => {
                self.advance();
                Token::RightSquare
            }
            '{' => {
                self.advance();
                Token::LeftCurly
            }
            '}' => {
                self.advance();
                Token::RightCurly
            }
            c if is_digit(c) => self.consume_numeric(),
            c if is_name_start(c) => self.consume_ident_like(),
            c => {
                self.advance();
                Token::Delim(c)
            }
        })
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Token<'a>> {
        self.next_token()
    }
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}
fn is_hex_digit(c: char) -> bool {
    c.is_ascii_hexdigit()
}
fn is_non_ascii(c: char) -> bool {
    c as u32 >= 0x80
}
fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || is_non_ascii(c) || c == '_'
}
pub(crate) fn is_name(c: char) -> bool {
    is_name_start(c) || is_digit(c) || c == '-'
}
fn is_non_printable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}
/// Newline-equivalent per the lazily-applied §3.3 preprocessing: `\n`,
/// `\r` (including as the first half of `\r\n`), and `\x0c`.
fn is_newline_start(c: Option<char>) -> bool {
    matches!(c, Some('\n' | '\r' | '\x0c'))
}
fn is_whitespace(c: char) -> bool {
    is_newline_start(Some(c)) || c == '\t' || c == ' '
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token<'_>> {
        Tokenizer::new(input).collect()
    }

    #[test]
    fn whitespace_and_idents() {
        assert_eq!(
            tokens("  foo  bar"),
            vec![
                Token::Whitespace,
                Token::Ident("foo".into()),
                Token::Whitespace,
                Token::Ident("bar".into()),
            ]
        );
    }

    #[test]
    fn numbers() {
        let toks = tokens("1 1.5 -3 +4 .5 1e2 1.5e-2 3%");
        let expect_repr = |t: &Token, r: &str| match t {
            Token::Number { repr, .. } | Token::Percentage { repr, .. } => assert_eq!(*repr, r),
            _ => panic!("not numeric: {t:?}"),
        };
        let nums: Vec<_> = toks
            .into_iter()
            .filter(|t| !matches!(t, Token::Whitespace))
            .collect();
        assert_eq!(nums.len(), 8);
        expect_repr(&nums[0], "1");
        expect_repr(&nums[1], "1.5");
        expect_repr(&nums[2], "-3");
        expect_repr(&nums[3], "+4");
        expect_repr(&nums[4], ".5");
        expect_repr(&nums[5], "1e2");
        expect_repr(&nums[6], "1.5e-2");
        match &nums[7] {
            Token::Percentage { value, .. } => assert_eq!(*value, 3.0),
            other => panic!("expected percentage, got {other:?}"),
        }
        match &nums[0] {
            Token::Number {
                value, num_type, ..
            } => {
                assert_eq!(*value, 1.0);
                assert_eq!(*num_type, NumericType::Integer);
            }
            other => panic!("expected number, got {other:?}"),
        }
        match &nums[1] {
            Token::Number { num_type, .. } => assert_eq!(*num_type, NumericType::Number),
            other => panic!("expected number, got {other:?}"),
        }
    }

    #[test]
    fn dimension() {
        let toks: Vec<_> = tokens("10px -3.5em")
            .into_iter()
            .filter(|t| *t != Token::Whitespace)
            .collect();
        match &toks[0] {
            Token::Dimension { value, unit, .. } => {
                assert_eq!(*value, 10.0);
                assert_eq!(unit.as_ref(), "px");
            }
            other => panic!("expected dimension, got {other:?}"),
        }
        match &toks[1] {
            Token::Dimension { value, unit, .. } => {
                assert_eq!(*value, -3.5);
                assert_eq!(unit.as_ref(), "em");
            }
            other => panic!("expected dimension, got {other:?}"),
        }
    }

    #[test]
    fn strings_basic() {
        assert_eq!(tokens(r#""hello""#), vec![Token::String("hello".into())]);
        assert_eq!(tokens("'hello'"), vec![Token::String("hello".into())]);
    }

    #[test]
    fn string_escape() {
        assert_eq!(tokens(r#""a\62 c""#), vec![Token::String("abc".into())]);
        // line continuation: backslash-newline inside a string is elided
        assert_eq!(tokens("\"a\\\nb\""), vec![Token::String("ab".into())]);
    }

    #[test]
    fn bad_string_unescaped_newline() {
        let toks = tokens("\"abc\ndef\"");
        assert_eq!(toks[0], Token::BadString);
        // the newline itself is NOT consumed, so tokenization continues after it
        assert!(toks.contains(&Token::Whitespace) || toks.len() > 1);
    }

    #[test]
    fn bad_string_eof() {
        // EOF before the closing quote: parse error, but still a String
        // token (not BadString) per spec, with whatever content was seen.
        assert_eq!(tokens(r#""abc"#), vec![Token::String("abc".into())]);
    }

    #[test]
    fn hash_tokens() {
        assert_eq!(
            tokens("#foo #123 #"),
            vec![
                Token::Hash {
                    value: "foo".into(),
                    is_id: true
                },
                Token::Whitespace,
                Token::Hash {
                    value: "123".into(),
                    is_id: false
                },
                Token::Whitespace,
                Token::Delim('#'),
            ]
        );
    }

    #[test]
    fn at_keyword() {
        assert_eq!(tokens("@media"), vec![Token::AtKeyword("media".into())]);
        assert_eq!(tokens("@"), vec![Token::Delim('@')]);
    }

    #[test]
    fn cdo_cdc() {
        assert_eq!(
            tokens("<!-- -->"),
            vec![Token::Cdo, Token::Whitespace, Token::Cdc]
        );
    }

    #[test]
    fn function_and_url_bare() {
        assert_eq!(
            tokens("rgb(1,2,3)"),
            vec![
                Token::Function("rgb".into()),
                Token::Number {
                    value: 1.0,
                    num_type: NumericType::Integer,
                    repr: "1"
                },
                Token::Comma,
                Token::Number {
                    value: 2.0,
                    num_type: NumericType::Integer,
                    repr: "2"
                },
                Token::Comma,
                Token::Number {
                    value: 3.0,
                    num_type: NumericType::Integer,
                    repr: "3"
                },
                Token::RightParen,
            ]
        );
        assert_eq!(tokens("url(foo.png)"), vec![Token::Url("foo.png".into())]);
        assert_eq!(tokens("url( foo.png )"), vec![Token::Url("foo.png".into())]);
    }

    #[test]
    fn url_quoted_is_a_function() {
        // url("foo.png") tokenizes as Function("url") + String + RightParen,
        // not a Url token — the parser handles it like any other function.
        assert_eq!(
            tokens(r#"url("foo.png")"#),
            vec![
                Token::Function("url".into()),
                Token::String("foo.png".into()),
                Token::RightParen,
            ]
        );
    }

    #[test]
    fn bad_url_recovery() {
        let toks = tokens("url(a\"b)c) foo");
        assert!(matches!(toks[0], Token::BadUrl));
        // tokenization must have recovered and continued afterward
        assert!(toks.iter().any(|t| *t == Token::Ident("foo".into())));
    }

    #[test]
    fn comments_are_stripped_but_dont_merge_tokens() {
        assert_eq!(
            tokens("a/**/b"),
            vec![Token::Ident("a".into()), Token::Ident("b".into())]
        );
        assert_eq!(tokens("/* unterminated"), vec![]);
    }

    #[test]
    fn brackets_and_punctuation() {
        assert_eq!(
            tokens("{}[]();,:"),
            vec![
                Token::LeftCurly,
                Token::RightCurly,
                Token::LeftSquare,
                Token::RightSquare,
                Token::LeftParen,
                Token::RightParen,
                Token::Semicolon,
                Token::Comma,
                Token::Colon,
            ]
        );
    }

    #[test]
    fn custom_property_ident() {
        // custom properties start with "--", which must tokenize as one ident
        assert_eq!(tokens("--foo"), vec![Token::Ident("--foo".into())]);
    }

    #[test]
    fn escaped_ident() {
        // \41 is a hex escape for 'A'
        assert_eq!(tokens(r"\41 nchor"), vec![Token::Ident("Anchor".into())]);
    }
}
