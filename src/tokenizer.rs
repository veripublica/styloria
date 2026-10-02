//! The CSS tokenization algorithm (CSS Syntax Level 3, §4:
//! <https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/#tokenization>).
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
//! The scanner works on **bytes**. Every code point that steers tokenization
//! is ASCII, and in UTF-8 no byte of a multi-byte character can be mistaken
//! for one, so a character is only decoded where the spec asks about a
//! non-ASCII code point (is it an ident code point?) or where its value is
//! copied into a token.
//!
//! Tokenization never fails: every malformed construct has spec-defined
//! recovery (`BadString`, `BadUrl`, or a `Delim` for a stray character), so
//! `next_token` returns `Option<Token<'a>>` only to signal end-of-input,
//! never an error.

use std::borrow::Cow;

use crate::span::{Span, Spanned};
use crate::token::{NumericType, Token};

pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
    /// Where the token being produced began, for `BadUrl`'s raw text.
    tok_start: usize,
    /// §4.3.1's "unicode ranges allowed" flag. Only the parser sets it, to
    /// re-read a `unicode-range` declaration's value (§5.5.6).
    unicode_ranges: bool,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        // A leading U+FEFF is a byte-order mark, not content. CSS Syntax
        // §3.2 consumes it while *determining the encoding*, so by the time
        // the tokenizer runs it should already be gone — but a caller that
        // decoded the bytes itself (the common case: `from_utf8_lossy` over a
        // file that happens to start with a UTF-8 BOM) hands it straight
        // through. Left in place it tokenizes as a delim, which turns the
        // `@charset` rule after it into a qualified rule's prelude and
        // cascades into spurious errors for the rest of the stylesheet.
        let pos = usize::from(input.starts_with('\u{FEFF}')) * '\u{FEFF}'.len_utf8();
        Tokenizer {
            input,
            pos,
            tok_start: pos,
            unicode_ranges: false,
        }
    }

    /// A tokenizer over `input[start..end]` with unicode ranges allowed, whose
    /// spans stay absolute offsets into `input`. Used to re-read the value of
    /// a `unicode-range` declaration.
    pub(crate) fn unicode_range_value(input: &'a str, start: usize, end: usize) -> Self {
        Tokenizer {
            input: &input[..end],
            pos: start,
            tok_start: start,
            unicode_ranges: true,
        }
    }

    #[inline]
    fn byte(&self, i: usize) -> Option<u8> {
        self.input.as_bytes().get(i).copied()
    }

    /// The code point starting at byte offset `i`.
    #[inline]
    fn char_at(&self, i: usize) -> Option<char> {
        match self.byte(i)? {
            b if b < 0x80 => Some(b as char),
            _ => self.input[i..].chars().next(),
        }
    }

    /// The byte length of the ident-start code point at `i`, if there is one.
    #[inline]
    fn ident_start_len(&self, i: usize) -> Option<usize> {
        match self.byte(i)? {
            b if b.is_ascii_alphabetic() || b == b'_' => Some(1),
            // A NUL is replaced with U+FFFD by §3.3, and U+FFFD is a
            // non-ASCII ident code point.
            0 => Some(1),
            b if b >= 0x80 => {
                let c = self.char_at(i)?;
                is_non_ascii_ident(c).then(|| c.len_utf8())
            }
            _ => None,
        }
    }

    #[inline]
    fn ident_char_len(&self, i: usize) -> Option<usize> {
        match self.byte(i)? {
            b if b.is_ascii_digit() || b == b'-' => Some(1),
            _ => self.ident_start_len(i),
        }
    }

    /// §4.3.8 "Check if two code points are a valid escape", at `i`.
    ///
    /// A `\` at the very end of the input *is* one: only a newline after it
    /// disqualifies it, and §4.3.7 turns the missing code point into U+FFFD
    /// (WPT `css/css-syntax/escaped-eof.html`).
    #[inline]
    fn is_valid_escape_at(&self, i: usize) -> bool {
        self.byte(i) == Some(b'\\') && !matches!(self.byte(i + 1), Some(b'\n' | b'\r' | 0x0c))
    }

    /// §4.3.9 "Check if three code points would start an ident sequence".
    fn would_start_ident_at(&self, i: usize) -> bool {
        match self.byte(i) {
            Some(b'-') => {
                self.byte(i + 1) == Some(b'-')
                    || self.ident_start_len(i + 1).is_some()
                    || self.is_valid_escape_at(i + 1)
            }
            Some(b'\\') => self.is_valid_escape_at(i),
            _ => self.ident_start_len(i).is_some(),
        }
    }

    /// §4.3.10 "Check if three code points would start a number".
    fn starts_number_at(&self, i: usize) -> bool {
        let digit = |j: usize| matches!(self.byte(j), Some(b) if b.is_ascii_digit());
        match self.byte(i) {
            Some(b'+' | b'-') => digit(i + 1) || (self.byte(i + 1) == Some(b'.') && digit(i + 2)),
            Some(b'.') => digit(i + 1),
            Some(b) => b.is_ascii_digit(),
            None => false,
        }
    }

    /// §4.3.11 "Check if three code points would start a unicode-range".
    fn would_start_unicode_range_at(&self, i: usize) -> bool {
        matches!(self.byte(i), Some(b'u' | b'U'))
            && self.byte(i + 1) == Some(b'+')
            && matches!(self.byte(i + 2), Some(b) if b == b'?' || b.is_ascii_hexdigit())
    }

    fn consume_comments(&mut self) {
        while self.byte(self.pos) == Some(b'/') && self.byte(self.pos + 1) == Some(b'*') {
            self.pos += 2;
            match self.input[self.pos..].find("*/") {
                Some(rel) => self.pos += rel + 2,
                None => self.pos = self.input.len(),
            }
        }
    }

    /// §4.3.11 "Consume a newline": `\r\n` counts as one newline.
    fn consume_newline(&mut self) {
        if self.byte(self.pos) == Some(b'\r') && self.byte(self.pos + 1) == Some(b'\n') {
            self.pos += 2;
        } else {
            self.pos += 1;
        }
    }

    /// §4.3.7 "Consume an escaped code point". Assumes the leading `\` has
    /// already been consumed.
    fn consume_escaped_code_point(&mut self) -> char {
        match self.byte(self.pos) {
            Some(b) if b.is_ascii_hexdigit() => {
                let mut code = 0u32;
                let mut n = 0;
                while n < 6 {
                    match self.byte(self.pos) {
                        Some(h) if h.is_ascii_hexdigit() => {
                            code = code * 16 + (h as char).to_digit(16).unwrap_or(0);
                            self.pos += 1;
                            n += 1;
                        }
                        _ => break,
                    }
                }
                match self.byte(self.pos) {
                    Some(b'\n' | b'\r' | 0x0c) => self.consume_newline(),
                    Some(b' ' | b'\t') => self.pos += 1,
                    _ => {}
                }
                if code == 0 || code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code) {
                    '\u{FFFD}'
                } else {
                    char::from_u32(code).unwrap_or('\u{FFFD}')
                }
            }
            Some(_) => {
                let c = self.char_at(self.pos).unwrap_or('\u{FFFD}');
                self.pos += c.len_utf8();
                if c == '\0' { '\u{FFFD}' } else { c }
            }
            None => '\u{FFFD}',
        }
    }

    /// §4.3.12 "Consume an ident sequence".
    fn consume_name(&mut self) -> Cow<'a, str> {
        let start = self.pos;
        loop {
            match self.byte(self.pos) {
                Some(b) if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' => self.pos += 1,
                Some(0 | b'\\') => break,
                Some(b) if b >= 0x80 => match self.ident_start_len(self.pos) {
                    Some(n) => self.pos += n,
                    None => return Cow::Borrowed(&self.input[start..self.pos]),
                },
                _ => return Cow::Borrowed(&self.input[start..self.pos]),
            }
        }
        // Slow path: an escape or a NUL means the value is not a slice of
        // the input any more.
        let mut owned = self.input[start..self.pos].to_string();
        loop {
            match self.byte(self.pos) {
                Some(0) => {
                    owned.push('\u{FFFD}');
                    self.pos += 1;
                }
                Some(b'\\') if self.is_valid_escape_at(self.pos) => {
                    self.pos += 1;
                    owned.push(self.consume_escaped_code_point());
                }
                _ => match self.ident_char_len(self.pos) {
                    Some(n) => {
                        owned.push_str(&self.input[self.pos..self.pos + n]);
                        self.pos += n;
                    }
                    None => return Cow::Owned(owned),
                },
            }
        }
    }

    /// §4.3.5 "Consume a string token". `quote` is the opening delimiter,
    /// already consumed by the caller.
    fn consume_string(&mut self, quote: u8) -> Token<'a> {
        let start = self.pos;
        loop {
            match self.byte(self.pos) {
                None => return Token::String(Cow::Borrowed(&self.input[start..self.pos])),
                Some(b) if b == quote => {
                    let s = &self.input[start..self.pos];
                    self.pos += 1;
                    return Token::String(Cow::Borrowed(s));
                }
                Some(b'\n' | b'\r' | 0x0c) => return Token::BadString,
                Some(b'\\' | 0) => break,
                Some(_) => self.pos += 1,
            }
        }
        let mut owned = self.input[start..self.pos].to_string();
        loop {
            match self.byte(self.pos) {
                None => return Token::String(Cow::Owned(owned)),
                Some(b) if b == quote => {
                    self.pos += 1;
                    return Token::String(Cow::Owned(owned));
                }
                Some(b'\n' | b'\r' | 0x0c) => return Token::BadString,
                Some(b'\\') => {
                    self.pos += 1;
                    match self.byte(self.pos) {
                        None => {}
                        Some(b'\n' | b'\r' | 0x0c) => self.consume_newline(),
                        Some(_) => owned.push(self.consume_escaped_code_point()),
                    }
                }
                Some(0) => {
                    owned.push('\u{FFFD}');
                    self.pos += 1;
                }
                Some(_) => {
                    let c = self.char_at(self.pos).unwrap_or('\u{FFFD}');
                    owned.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// §4.3.15 "Consume the remnants of a bad url", then the bad-url token
    /// itself, carrying everything from `url(` to here.
    fn bad_url(&mut self) -> Token<'a> {
        loop {
            match self.byte(self.pos) {
                None => break,
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') if self.is_valid_escape_at(self.pos) => {
                    self.pos += 1;
                    self.consume_escaped_code_point();
                }
                Some(_) => self.pos += 1,
            }
        }
        Token::BadUrl(Cow::Borrowed(&self.input[self.tok_start..self.pos]))
    }

    /// §4.3.6 "Consume a url token". Called right after `url(`.
    fn consume_url(&mut self) -> Token<'a> {
        self.skip_whitespace();
        let start = self.pos;
        loop {
            match self.byte(self.pos) {
                None => return Token::Url(Cow::Borrowed(&self.input[start..self.pos])),
                Some(b')') => {
                    let s = &self.input[start..self.pos];
                    self.pos += 1;
                    return Token::Url(Cow::Borrowed(s));
                }
                Some(b) if is_whitespace(b) => {
                    let s = Cow::Borrowed(&self.input[start..self.pos]);
                    return self.finish_url_after_whitespace(s);
                }
                Some(b'"' | b'\'' | b'(') => return self.bad_url(),
                Some(b) if is_non_printable(b) => return self.bad_url(),
                Some(b'\\') => {
                    if !self.is_valid_escape_at(self.pos) {
                        return self.bad_url();
                    }
                    break;
                }
                Some(0) => break,
                Some(_) => self.pos += 1,
            }
        }
        let mut owned = self.input[start..self.pos].to_string();
        loop {
            match self.byte(self.pos) {
                None => return Token::Url(Cow::Owned(owned)),
                Some(b')') => {
                    self.pos += 1;
                    return Token::Url(Cow::Owned(owned));
                }
                Some(b) if is_whitespace(b) => {
                    return self.finish_url_after_whitespace(Cow::Owned(owned));
                }
                Some(b'"' | b'\'' | b'(') => return self.bad_url(),
                Some(b) if is_non_printable(b) => return self.bad_url(),
                Some(b'\\') => {
                    if !self.is_valid_escape_at(self.pos) {
                        return self.bad_url();
                    }
                    self.pos += 1;
                    owned.push(self.consume_escaped_code_point());
                }
                Some(0) => {
                    owned.push('\u{FFFD}');
                    self.pos += 1;
                }
                Some(_) => {
                    let c = self.char_at(self.pos).unwrap_or('\u{FFFD}');
                    owned.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn finish_url_after_whitespace(&mut self, value: Cow<'a, str>) -> Token<'a> {
        self.skip_whitespace();
        match self.byte(self.pos) {
            None => Token::Url(value),
            Some(b')') => {
                self.pos += 1;
                Token::Url(value)
            }
            _ => self.bad_url(),
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.byte(self.pos), Some(b) if is_whitespace(b)) {
            self.pos += 1;
        }
    }

    fn skip_digits(&mut self) {
        while matches!(self.byte(self.pos), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    /// §4.3.13 "Consume a number". Advances past the number's characters
    /// and reports whether it looked like an integer or used a decimal
    /// point / exponent.
    fn consume_number(&mut self) -> NumericType {
        let mut num_type = NumericType::Integer;
        if matches!(self.byte(self.pos), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        self.skip_digits();
        if self.byte(self.pos) == Some(b'.')
            && matches!(self.byte(self.pos + 1), Some(b) if b.is_ascii_digit())
        {
            self.pos += 1;
            num_type = NumericType::Number;
            self.skip_digits();
        }
        if matches!(self.byte(self.pos), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(self.byte(self.pos + 1), Some(b'+' | b'-')));
            if matches!(self.byte(self.pos + 1 + sign), Some(b) if b.is_ascii_digit()) {
                num_type = NumericType::Number;
                self.pos += 1 + sign;
                self.skip_digits();
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
        if self.would_start_ident_at(self.pos) {
            let unit = self.consume_name();
            Token::Dimension {
                value,
                num_type,
                unit,
                repr,
            }
        } else if self.byte(self.pos) == Some(b'%') {
            self.pos += 1;
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
        if self.byte(self.pos) != Some(b'(') {
            return Token::Ident(name);
        }
        self.pos += 1;
        if !name.eq_ignore_ascii_case("url") {
            return Token::Function(name);
        }
        // `url(` followed (after whitespace) by a quote is an ordinary
        // function whose argument is a string; only the unquoted form is a
        // url token. Whitespace before the quote is left for a whitespace
        // token, as the spec's "while the next two code points are
        // whitespace" leaves the last one.
        let after_paren = self.pos;
        self.skip_whitespace();
        if matches!(self.byte(self.pos), Some(b'"' | b'\'')) {
            self.pos = after_paren;
            return Token::Function(name);
        }
        self.consume_url()
    }

    /// §4.3.14 "Consume a unicode-range token".
    fn consume_unicode_range(&mut self) -> Token<'a> {
        self.pos += 2;
        let hex = |t: &mut Self, max: usize| {
            let (mut v, mut n) = (0u32, 0);
            while n < max {
                match t.byte(t.pos) {
                    Some(h) if h.is_ascii_hexdigit() => {
                        v = v * 16 + (h as char).to_digit(16).unwrap_or(0);
                        t.pos += 1;
                        n += 1;
                    }
                    _ => break,
                }
            }
            (v, n)
        };
        let (mut start, digits) = hex(self, 6);
        let mut end = start;
        let mut wild = 0;
        while digits + wild < 6 && self.byte(self.pos) == Some(b'?') {
            self.pos += 1;
            wild += 1;
            start <<= 4;
            end = (end << 4) | 0xF;
        }
        if wild == 0
            && self.byte(self.pos) == Some(b'-')
            && matches!(self.byte(self.pos + 1), Some(h) if h.is_ascii_hexdigit())
        {
            self.pos += 1;
            end = hex(self, 6).0;
        }
        Token::UnicodeRange { start, end }
    }

    /// §4.3.1 "Consume a token".
    pub fn next_token(&mut self) -> Option<Token<'a>> {
        self.consume_comments();
        self.tok_start = self.pos;
        let b = self.byte(self.pos)?;
        Some(match b {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c => {
                self.skip_whitespace();
                Token::Whitespace
            }
            b'"' | b'\'' => {
                self.pos += 1;
                self.consume_string(b)
            }
            b'#' => {
                self.pos += 1;
                if self.ident_char_len(self.pos).is_some() || self.is_valid_escape_at(self.pos) {
                    let is_id = self.would_start_ident_at(self.pos);
                    let value = self.consume_name();
                    Token::Hash { value, is_id }
                } else {
                    Token::Delim('#')
                }
            }
            b'(' => self.punct(Token::LeftParen),
            b')' => self.punct(Token::RightParen),
            b',' => self.punct(Token::Comma),
            b':' => self.punct(Token::Colon),
            b';' => self.punct(Token::Semicolon),
            b'[' => self.punct(Token::LeftSquare),
            b']' => self.punct(Token::RightSquare),
            b'{' => self.punct(Token::LeftCurly),
            b'}' => self.punct(Token::RightCurly),
            b'+' | b'.' => {
                if self.starts_number_at(self.pos) {
                    self.consume_numeric()
                } else {
                    self.punct(Token::Delim(b as char))
                }
            }
            b'-' => {
                if self.starts_number_at(self.pos) {
                    self.consume_numeric()
                } else if self.byte(self.pos + 1) == Some(b'-')
                    && self.byte(self.pos + 2) == Some(b'>')
                {
                    self.pos += 3;
                    Token::Cdc
                } else if self.would_start_ident_at(self.pos) {
                    self.consume_ident_like()
                } else {
                    self.punct(Token::Delim('-'))
                }
            }
            b'<' => {
                if self.input.as_bytes()[self.pos + 1..].starts_with(b"!--") {
                    self.pos += 4;
                    Token::Cdo
                } else {
                    self.punct(Token::Delim('<'))
                }
            }
            b'@' => {
                self.pos += 1;
                if self.would_start_ident_at(self.pos) {
                    Token::AtKeyword(self.consume_name())
                } else {
                    Token::Delim('@')
                }
            }
            b'\\' => {
                if self.is_valid_escape_at(self.pos) {
                    self.consume_ident_like()
                } else {
                    // Parse error: a lone backslash (before EOF or a newline).
                    self.punct(Token::Delim('\\'))
                }
            }
            b'0'..=b'9' => self.consume_numeric(),
            b'u' | b'U' if self.unicode_ranges && self.would_start_unicode_range_at(self.pos) => {
                self.consume_unicode_range()
            }
            _ if self.ident_start_len(self.pos).is_some() => self.consume_ident_like(),
            _ => {
                let c = self.char_at(self.pos).unwrap_or('\u{FFFD}');
                self.pos += c.len_utf8();
                Token::Delim(c)
            }
        })
    }

    #[inline]
    fn punct(&mut self, t: Token<'a>) -> Token<'a> {
        self.pos += 1;
        t
    }

    /// Like [`next_token`](Self::next_token), but also returns the token's
    /// source [`Span`] (its byte range in the input).
    ///
    /// Comments preceding the token are consumed first, so the returned span
    /// covers only the token itself, never a leading `/* … */`. The reported
    /// range is exactly the bytes the tokenizer advanced over to produce this
    /// token, so `span.slice(input)` round-trips to the token's source text.
    pub fn next_token_spanned(&mut self) -> Option<Spanned<Token<'a>>> {
        let node = self.next_token()?;
        Some(Spanned {
            node,
            span: Span {
                start: self.tok_start,
                end: self.pos,
            },
        })
    }

    /// Adapt this tokenizer into an iterator of [`Spanned`] tokens.
    pub fn spanned(self) -> SpannedTokens<'a> {
        SpannedTokens { inner: self }
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Token<'a>> {
        self.next_token()
    }
}

/// An iterator of [`Spanned`] tokens, produced by
/// [`Tokenizer::spanned`]. Yields the same tokens as iterating the
/// [`Tokenizer`] directly, each paired with its source [`Span`].
pub struct SpannedTokens<'a> {
    inner: Tokenizer<'a>,
}

impl<'a> Iterator for SpannedTokens<'a> {
    type Item = Spanned<Token<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next_token_spanned()
    }
}

/// §4.2 "non-ASCII ident code point". The 2026 CRD narrowed this from
/// "anything at or above U+0080" to the code points HTML allows in custom
/// element names (w3c/csswg-drafts#7129), so U+00A0 NO-BREAK SPACE, the
/// other Unicode spaces and the C1 controls are no longer name characters.
fn is_non_ascii_ident(c: char) -> bool {
    matches!(c as u32,
        0xB7
        | 0xC0..=0xD6
        | 0xD8..=0xF6
        | 0xF8..=0x37D
        | 0x37F..=0x1FFF
        | 0x200C
        | 0x200D
        | 0x203F
        | 0x2040
        | 0x2070..=0x218F
        | 0x2C00..=0x2FEF
        | 0x3001..=0xD7FF
        | 0xF900..=0xFDCF
        | 0xFDF0..=0xFFFD
        | 0x10000..)
}

/// An ident code point (§4.2), for the serializer's escaping decisions.
/// NUL is not one here: the serializer substitutes it before asking.
pub(crate) fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || is_non_ascii_ident(c)
}

fn is_non_printable(b: u8) -> bool {
    matches!(b, 0x01..=0x08 | 0x0b | 0x0e..=0x1f | 0x7f)
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token<'_>> {
        Tokenizer::new(input).collect()
    }

    fn spanned(input: &str) -> Vec<Spanned<Token<'_>>> {
        Tokenizer::new(input).spanned().collect()
    }

    #[test]
    fn spans_round_trip_to_source() {
        let src = "body { color: red }";
        for st in spanned(src) {
            // Every token's span slices back to exactly the text that token
            // was tokenized from (for tokens whose text is a literal slice).
            match &st.node {
                Token::Ident(name) => assert_eq!(st.span.slice(src), name.as_ref()),
                Token::Colon => assert_eq!(st.span.slice(src), ":"),
                Token::LeftCurly => assert_eq!(st.span.slice(src), "{"),
                Token::RightCurly => assert_eq!(st.span.slice(src), "}"),
                _ => {}
            }
        }
    }

    /// Spans tile the input: each token starts where the previous one (or
    /// the comment before it) ended, so no byte is double-counted or lost.
    #[test]
    fn spans_are_contiguous_apart_from_comments() {
        let src = "a/*x*/b { c: url( d ) \"e\\\"f\" 1.5em }";
        let toks = spanned(src);
        let joined: String = toks.iter().map(|t| t.span.slice(src)).collect();
        assert_eq!(joined, src.replace("/*x*/", ""));
    }

    #[test]
    fn leading_comment_excluded_from_span() {
        let src = "/* generated */ body";
        let toks = spanned(src);
        // The only non-whitespace token is `body`; its span must start at
        // the 'b', not at the comment.
        let ident = toks
            .iter()
            .find(|s| matches!(s.node, Token::Ident(_)))
            .unwrap();
        assert_eq!(ident.span.slice(src), "body");
        assert_eq!(ident.span.start, src.find("body").unwrap());
    }

    #[test]
    fn span_gives_line_and_column() {
        // The end goal: locate a token by line:column, the way a validator
        // would surface a CSS finding's exact position.
        let src = "body {\n  color: red;\n}";
        let (line, col) = spanned(src)
            .into_iter()
            .find(|s| matches!(&s.node, Token::Ident(n) if n.as_ref() == "color"))
            .expect("expected the `color` ident")
            .span
            .start_line_col(src);
        assert_eq!((line, col), (2, 3));
    }

    #[test]
    fn bad_url_token_is_locatable() {
        // A tokenizer-level error (a BadUrl) still carries a span, so a
        // consumer can point at exactly where it is rather than at the whole
        // file — the CSS analogue of what epubveri already does elsewhere.
        let src = "a {\n  background: url(foo bar);\n}";
        let bad = spanned(src)
            .into_iter()
            .find(|s| matches!(s.node, Token::BadUrl(_)))
            .expect("expected a BadUrl token");
        assert_eq!(bad.span.start_line_col(src).0, 2);
        assert_eq!(bad.span.slice(src), "url(foo bar)");
    }

    /// The raw text rides on the token itself, so a consumer can quote the
    /// bad url without slicing the source or re-tokenizing.
    #[test]
    fn bad_url_carries_its_raw_text() {
        for (src, raw) in [
            ("url(foo bar)", "url(foo bar)"),
            ("url(a\"b)c) x", "url(a\"b)"),
            ("URL( a'b )", "URL( a'b )"),
            ("url(a\\)b c)", "url(a\\)b c)"),
            ("url(a b", "url(a b"),
        ] {
            assert_eq!(
                tokens(src)[0],
                Token::BadUrl(raw.into()),
                "raw text of {src:?}"
            );
        }
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
        assert_eq!(tokens("'ünï'"), vec![Token::String("ünï".into())]);
    }

    #[test]
    fn string_escape() {
        assert_eq!(tokens(r#""a\62 c""#), vec![Token::String("abc".into())]);
        // line continuation: backslash-newline inside a string is elided
        assert_eq!(tokens("\"a\\\nb\""), vec![Token::String("ab".into())]);
        assert_eq!(tokens("\"a\\\r\nb\""), vec![Token::String("ab".into())]);
    }

    #[test]
    fn bad_string_unescaped_newline() {
        let toks = tokens("\"abc\ndef\"");
        assert_eq!(toks[0], Token::BadString);
        // the newline itself is NOT consumed, so tokenization continues after it
        assert_eq!(toks[1], Token::Whitespace);
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
        assert_eq!(
            tokens("<!-"),
            vec![Token::Delim('<'), Token::Delim('!'), Token::Delim('-')]
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
        assert_eq!(tokens("url(a\\ b)"), vec![Token::Url("a b".into())]);
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
        assert_eq!(
            tokens(r#"url(  'x')"#),
            vec![
                Token::Function("url".into()),
                Token::Whitespace,
                Token::String("x".into()),
                Token::RightParen,
            ]
        );
    }

    #[test]
    fn bad_url_recovery() {
        let toks = tokens("url(a\"b)c) foo");
        assert!(matches!(toks[0], Token::BadUrl(_)));
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
        assert_eq!(tokens("--"), vec![Token::Ident("--".into())]);
    }

    #[test]
    fn escaped_ident() {
        // \41 is a hex escape for 'A'
        assert_eq!(tokens(r"\41 nchor"), vec![Token::Ident("Anchor".into())]);
        assert_eq!(tokens(r"a\:b"), vec![Token::Ident("a:b".into())]);
    }

    /// §3.3 replaces NUL with U+FFFD before tokenizing, and U+FFFD is an
    /// ident code point - so a NUL is part of a name, not a delim.
    #[test]
    fn nul_becomes_a_replacement_character() {
        assert_eq!(tokens("a\0b"), vec![Token::Ident("a\u{FFFD}b".into())]);
        assert_eq!(tokens("\"a\0\""), vec![Token::String("a\u{FFFD}".into())]);
        assert_eq!(tokens(r"\0 x"), vec![Token::Ident("\u{FFFD}x".into())]);
    }

    /// The 2026 CRD's narrower non-ASCII ident set (csswg-drafts#7129):
    /// letters in any script stay name characters, a no-break space does not.
    #[test]
    fn non_ascii_ident_code_points() {
        assert_eq!(tokens("çiğdem"), vec![Token::Ident("çiğdem".into())]);
        assert_eq!(tokens("日本"), vec![Token::Ident("日本".into())]);
        assert_eq!(
            tokens(".😀"),
            vec![Token::Delim('.'), Token::Ident("😀".into())]
        );
        assert_eq!(
            tokens("a\u{A0}b"),
            vec![
                Token::Ident("a".into()),
                Token::Delim('\u{A0}'),
                Token::Ident("b".into())
            ]
        );
        assert_eq!(tokens("\u{D7}"), vec![Token::Delim('\u{D7}')]);
    }

    #[test]
    fn unicode_ranges_only_when_allowed() {
        // Normal tokenization: an ident and numbers, as since CSS 2.1.
        assert_eq!(tokens("u+a")[0], Token::Ident("u".into()));
        fn range(s: &str) -> Vec<Token<'_>> {
            Tokenizer::unicode_range_value(s, 0, s.len())
                .filter(|t| *t != Token::Whitespace && *t != Token::Comma)
                .collect()
        }
        assert_eq!(
            range("U+0-7F, u+4??, U+10FFFF, U+0025-00FF"),
            vec![
                Token::UnicodeRange {
                    start: 0,
                    end: 0x7F
                },
                Token::UnicodeRange {
                    start: 0x400,
                    end: 0x4FF
                },
                Token::UnicodeRange {
                    start: 0x10FFFF,
                    end: 0x10FFFF
                },
                Token::UnicodeRange {
                    start: 0x25,
                    end: 0xFF
                },
            ]
        );
        assert_eq!(
            range("U+??????"),
            vec![Token::UnicodeRange {
                start: 0,
                end: 0xFFFFFF
            }]
        );
    }

    /// A trailing backslash is an escape of the end of input: U+FFFD, inside
    /// whatever token it ends.
    #[test]
    fn a_backslash_at_the_end_of_input_is_a_replacement_character() {
        assert_eq!(tokens("\\"), vec![Token::Ident("\u{FFFD}".into())]);
        assert_eq!(tokens("a\\"), vec![Token::Ident("a\u{FFFD}".into())]);
        assert_eq!(tokens("@a\\"), vec![Token::AtKeyword("a\u{FFFD}".into())]);
        assert_eq!(
            tokens("#a\\"),
            vec![Token::Hash {
                value: "a\u{FFFD}".into(),
                is_id: true
            }]
        );
        assert_eq!(tokens("url(a\\"), vec![Token::Url("a\u{FFFD}".into())]);
        // In a string it is dropped instead (§4.3.5).
        assert_eq!(tokens("\"a\\"), vec![Token::String("a".into())]);
        // Before a newline it is not an escape at all.
        assert_eq!(tokens("\\\n")[0], Token::Delim('\\'));
    }

    #[test]
    fn crlf_is_one_newline_after_an_escape() {
        assert_eq!(tokens("\\41\r\nb"), vec![Token::Ident("Ab".into())]);
    }
}
