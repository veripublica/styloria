//! Source spans — byte ranges into the original CSS text, plus a
//! [`Spanned<T>`] wrapper that attaches one to any value.
//!
//! # Prototype status
//!
//! This is the first slice of position support (see `SPAN_PROTOTYPE.md`).
//! Today the **tokenizer** emits spans, via
//! [`Tokenizer::next_token_spanned`](crate::tokenizer::Tokenizer::next_token_spanned)
//! and [`Tokenizer::spanned`](crate::tokenizer::Tokenizer::spanned). The
//! intended next step is to thread spans up through the parser so that
//! [`Declaration`](crate::Declaration), [`Rule`](crate::Rule), and
//! [`ComponentValue`](crate::ComponentValue) each carry the range covering
//! their first..last token. That is what lets a validator (e.g. epubveri)
//! report the exact `line:column` of a CSS finding instead of only the file
//! name — the reason this exists.

/// A half-open byte range `[start, end)` into the original source string.
///
/// Offsets are **byte** offsets (not char indices) so a span can slice the
/// source directly; they always land on UTF-8 char boundaries because every
/// token begins and ends on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// The slice of `source` this span covers. `source` must be the same
    /// string the span was produced from.
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start.min(source.len())..self.end.min(source.len())]
    }

    /// The 1-based line and column of this span's start within `source`.
    /// The column is counted in **characters** (not bytes), matching how
    /// editors report positions.
    pub fn start_line_col(&self, source: &str) -> (u32, u32) {
        line_col(source, self.start)
    }

    /// The smallest span covering both `self` and `other` (their union,
    /// including any gap between them). Used when a parser builds a node's
    /// span from its first and last child tokens.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

fn line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset.min(source.len())];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let column = match before.rfind('\n') {
        Some(nl) => before[nl + 1..].chars().count() as u32 + 1,
        None => before.chars().count() as u32 + 1,
    };
    (line, column)
}

/// A value paired with the source [`Span`] it was produced from.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Spanned<T> {
        Spanned { node, span }
    }

    /// Transform the inner value, keeping the same span.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            node: f(self.node),
            span: self.span,
        }
    }

    /// A borrow of the inner value with the same span.
    pub fn as_ref(&self) -> Spanned<&T> {
        Spanned {
            node: &self.node,
            span: self.span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_single_line() {
        let src = "body { color: red }";
        // offset 7 is the 'c' of "color"
        assert_eq!(Span::new(7, 12).start_line_col(src), (1, 8));
    }

    #[test]
    fn line_col_multiline() {
        let src = "a {\n  b: c;\n}";
        // offset of 'b' on line 2: "a {\n  " = 6 bytes, so 'b' is at 6
        let off = src.find('b').unwrap();
        assert_eq!(Span::new(off, off + 1).start_line_col(src), (2, 3));
    }

    #[test]
    fn slice_round_trips() {
        let src = "body { color: red }";
        let span = Span::new(7, 12);
        assert_eq!(span.slice(src), "color");
    }

    #[test]
    fn union_covers_both() {
        assert_eq!(Span::new(2, 5).to(Span::new(9, 12)), Span::new(2, 12));
    }
}
