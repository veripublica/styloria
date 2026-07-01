//! CSS token types (CSS Syntax Level 3, §4 "Tokenization":
//! <https://www.w3.org/TR/css-syntax-3/#tokenization>).
//!
//! Tokens borrow from the tokenizer's input (`&'a str`) wherever possible;
//! they only own a `String` when their content required un-escaping (a
//! `\`-escape or a literal NUL, which is substituted per spec) or otherwise
//! can't be a contiguous slice of the original input.

use std::borrow::Cow;

/// The `type` flag CSS Syntax Level 3 attaches to numeric tokens: whether
/// the token's original representation looked like an integer or used a
/// decimal point / exponent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericType {
    Integer,
    Number,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Ident(Cow<'a, str>),
    Function(Cow<'a, str>),
    AtKeyword(Cow<'a, str>),
    /// `#foo` — `is_id` is true when the hash's name would itself be a
    /// valid identifier (the "id" type flag vs. "unrestricted").
    Hash {
        value: Cow<'a, str>,
        is_id: bool,
    },
    String(Cow<'a, str>),
    /// An unterminated string (an unescaped newline appeared before the
    /// closing quote). Per spec this is still a valid, non-fatal token.
    BadString,
    /// `url(...)` in its bare/unquoted form (`url(foo.png)`). Note that
    /// `url("foo.png")` tokenizes differently — as a `Function("url")`
    /// token followed by a `String` token, handled at the parser level like
    /// any other function call, per spec §4.3.4.
    Url(Cow<'a, str>),
    /// A `url(...)` whose contents couldn't be tokenized (unescaped quote/
    /// paren/whitespace-then-non-close inside). Still non-fatal.
    BadUrl,
    /// A single code point that didn't start any other token.
    Delim(char),
    Number {
        value: f64,
        num_type: NumericType,
        /// The token's original textual representation, kept for
        /// round-trip-faithful serialization (e.g. preserving "1.50" or
        /// "1e2" rather than reformatting the parsed value).
        repr: &'a str,
    },
    Percentage {
        value: f64,
        repr: &'a str,
    },
    Dimension {
        value: f64,
        num_type: NumericType,
        unit: Cow<'a, str>,
        repr: &'a str,
    },
    Whitespace,
    /// `<!--`
    Cdo,
    /// `-->`
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LeftSquare,
    RightSquare,
    LeftParen,
    RightParen,
    LeftCurly,
    RightCurly,
}

impl<'a> Token<'a> {
    /// True for the four bracket-opening tokens a "simple block" can start
    /// with (CSS Syntax Level 3 §5.4.7).
    pub fn is_block_open(&self) -> bool {
        matches!(
            self,
            Token::LeftCurly | Token::LeftSquare | Token::LeftParen
        )
    }

    pub fn matching_close(&self) -> Option<Token<'static>> {
        match self {
            Token::LeftCurly => Some(Token::RightCurly),
            Token::LeftSquare => Some(Token::RightSquare),
            Token::LeftParen => Some(Token::RightParen),
            _ => None,
        }
    }
}
