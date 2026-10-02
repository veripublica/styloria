//! CSS token types (CSS Syntax Level 3, §4 "Tokenization":
//! <https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/#tokenization>).
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
    ///
    /// Carries the token's raw source text, from `url(` up to and including
    /// the `)` that ended the recovery (or to the end of input), so a caller
    /// can quote what the author wrote without re-tokenizing.
    BadUrl(Cow<'a, str>),
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
    /// `U+0-7F`, `U+4??`: an inclusive range of code points (§4.3.14).
    ///
    /// Never produced while tokenizing a stylesheet. The parser re-reads the
    /// value of a `unicode-range` declaration with unicode ranges allowed
    /// (§5.5.6), and that is the only place this token appears — elsewhere
    /// `u+a` is an ident and a number, which is what keeps `u+a { }` a
    /// selector.
    UnicodeRange {
        start: u32,
        end: u32,
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
    /// True for the three bracket-opening tokens a "simple block" can start
    /// with (CSS Syntax Level 3 §5.5.9).
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
