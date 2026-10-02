//! The CSS parser (CSS Syntax Level 3, §5, as of the 1 October 2026
//! Candidate Recommendation Draft:
//! <https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/#parsing>).
//!
//! One pass turns source text into a [`Stylesheet`]: rules, the declarations
//! and rules inside their blocks, and the component values inside those, every
//! node carrying the byte [`Span`] it came from. All the [`SyntaxError`]s the
//! parser recovered from come back with it, sorted by position.
//!
//! The parser is property-blind. It does not know what `color` means or which
//! at-rules exist; it reads every `{ … }` that belongs to a rule with §5.5.5
//! "consume a block's contents", which tries each item as a declaration and
//! re-reads it as a nested rule when that fails. A block therefore comes back
//! as a list of [`BlockItem`]s in source order, whatever rule holds it. Which
//! of those items are *allowed* there — a declaration directly inside a
//! top-level `@media`, a nested style rule in an EPUB — is a question about
//! the context, and the caller's to answer.
//!
//! ```
//! let css = "body {\n  color: red;\n  a { color: blue }\n}";
//! let (sheet, errors) = styloria::parse_stylesheet(css);
//! assert!(errors.is_empty());
//! let rule = &sheet.rules[0];
//! assert_eq!(rule.span.start_line_col(css), (1, 1));
//! let items = &rule.node.block().unwrap().node;
//! assert!(matches!(items[0], styloria::BlockItem::Declaration(_)));
//! assert!(matches!(items[1], styloria::BlockItem::Rule(_)));
//! ```
//!
//! # How the "declaration, then rule" retry stays linear
//!
//! The spec describes the retry as "mark the input, try a declaration, restore
//! the mark on failure", which re-reads the item and, naively, everything
//! nested in it. Here the input is tokenized once, up front, and each opening
//! bracket is paired with its closing one in the same pass. The parser then
//! decides what an item is by looking only at the tokens at the item's own
//! level — it steps over a whole `( … )`, `[ … ]` or `{ … }` in one move —
//! and builds the item once, as whichever it turned out to be. Every token is
//! inspected a bounded number of times however deeply the input nests.

use std::borrow::Cow;

use crate::span::{Span, Spanned};
use crate::token::Token;
use crate::tokenizer::Tokenizer;

/// Which bracket a [`SimpleBlock`] was written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Curly,
    Square,
    Paren,
}

/// A component value (§5.5.8): a token, a function, or a bracketed block.
/// Preludes and declaration values are lists of these.
#[derive(Debug, Clone, PartialEq)]
pub enum ComponentValue<'a> {
    Token(Token<'a>),
    Function {
        name: Cow<'a, str>,
        args: Vec<Spanned<ComponentValue<'a>>>,
    },
    Block(SimpleBlock<'a>),
}

/// A `{}`/`[]`/`()` block inside a value or prelude. A rule's own block is
/// not one of these; it is a list of [`BlockItem`]s.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleBlock<'a> {
    pub kind: BlockKind,
    pub values: Vec<Spanned<ComponentValue<'a>>>,
}

/// The contents of a rule's `{ … }`, in source order. The span covers the
/// braces; for a block that is never closed, it runs to the end of input.
pub type Block<'a> = Spanned<Vec<BlockItem<'a>>>;

/// A `selector { … }` rule, at any depth. Its prelude is handed on as
/// component values; [`crate::selector`] reads it as a selector list.
#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedRule<'a> {
    pub prelude: Vec<Spanned<ComponentValue<'a>>>,
    pub block: Block<'a>,
}

/// An `@name … { … }` or `@name …;` rule, at any depth.
#[derive(Debug, Clone, PartialEq)]
pub struct AtRule<'a> {
    pub name: Cow<'a, str>,
    /// Span of just the `@name` keyword token.
    pub name_span: Span,
    pub prelude: Vec<Spanned<ComponentValue<'a>>>,
    /// `None` for a statement at-rule (`@import …;`).
    pub block: Option<Block<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rule<'a> {
    Qualified(QualifiedRule<'a>),
    At(AtRule<'a>),
}

impl<'a> Rule<'a> {
    /// The rule's prelude: a qualified rule's selector, an at-rule's
    /// condition or name.
    pub fn prelude(&self) -> &[Spanned<ComponentValue<'a>>] {
        match self {
            Rule::Qualified(q) => &q.prelude,
            Rule::At(a) => &a.prelude,
        }
    }

    /// The rule's block, if it has one (every qualified rule does).
    pub fn block(&self) -> Option<&Block<'a>> {
        match self {
            Rule::Qualified(q) => Some(&q.block),
            Rule::At(a) => a.block.as_ref(),
        }
    }
}

/// A `property: value` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration<'a> {
    pub name: Cow<'a, str>,
    /// Span of just the property name, the thing a validator underlines.
    pub name_span: Span,
    /// The value, with leading and trailing whitespace and a trailing
    /// `!important` removed (§5.5.6). A custom property's value keeps its
    /// tokens as written; its original text is the source between the first
    /// and last value's spans.
    pub value: Vec<Spanned<ComponentValue<'a>>>,
    pub important: bool,
}

/// One item of a rule's block: a declaration or a nested rule (§5.5.5).
///
/// The spec groups consecutive declarations into lists; this keeps the flat
/// sequence instead, which loses nothing (the grouping is where a rule sits
/// between declarations) and is the order a validator reports in.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockItem<'a> {
    Declaration(Spanned<Declaration<'a>>),
    Rule(Spanned<Rule<'a>>),
}

impl BlockItem<'_> {
    pub fn span(&self) -> Span {
        match self {
            BlockItem::Declaration(d) => d.span,
            BlockItem::Rule(r) => r.span,
        }
    }
}

/// A parsed stylesheet: its top-level rules, each with a span.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stylesheet<'a> {
    pub rules: Vec<Spanned<Rule<'a>>>,
}

/// A syntax error the parser recovered from while building the tree.
///
/// Distinct from a [`Diagnostic`](crate::Diagnostic), which is a *semantic*
/// finding about a named construct (an unknown property, say) and carries the
/// offending name. A syntax error is purely *positional* — the CSS was
/// malformed at this span — so it has only a span and a reason, no name.
///
/// The parser is error-recovering per the CSS Syntax spec: it never fails,
/// it discards the malformed part and continues. These record *what* it
/// discarded and *where*, which a validating tool (e.g. an EPUB checker
/// mapping them to a "CSS parse error" message) otherwise cannot see.
///
/// Each defect is reported once. Errors come back sorted by `span.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    /// The source span of the malformed construct.
    pub span: Span,
    pub kind: SyntaxErrorKind,
}

/// What a [`SyntaxError`] reports, and where in a stylesheet it can occur.
///
/// | kind | top level | inside a block |
/// |---|---|---|
/// | [`BadString`](Self::BadString), [`BadUrl`](Self::BadUrl), [`InvalidUnicodeRange`](Self::InvalidUnicodeRange), [`UnterminatedBlock`](Self::UnterminatedBlock), [`NestingTooDeep`](Self::NestingTooDeep) | yes | yes |
/// | [`InvalidSelector`](Self::InvalidSelector) | yes | yes |
/// | [`UnterminatedRule`](Self::UnterminatedRule), [`DroppedCustomPropertyRule`](Self::DroppedCustomPropertyRule) | yes | never |
/// | [`MalformedDeclaration`](Self::MalformedDeclaration), [`UnexpectedToken`](Self::UnexpectedToken) | never | yes |
///
/// [`parse_block_contents`] reads its whole input as one block, so it reports
/// the "inside a block" kinds only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxErrorKind {
    /// A `<bad-string-token>`: a string with an unescaped newline.
    BadString,
    /// A `<bad-url-token>`: a malformed unquoted `url( … )`.
    BadUrl,
    /// An item in a block that is neither a declaration nor a rule, starting
    /// with an ident: `color red;`, `a b }`. Discarded up to the next `;` or
    /// the end of the block. The span is the leading ident.
    MalformedDeclaration,
    /// A top-level qualified rule whose prelude reached the end of input
    /// before its `{ … }` block (§5.5.3). Dropped. The span is the prelude's
    /// first token.
    UnterminatedRule,
    /// A `{`, `[`, or `(` that reached the end of input before its closing
    /// bracket. The span is the opening bracket; the node it opened (a
    /// rule's block, a simple block) still comes back, running to the end.
    UnterminatedBlock,
    /// Like [`MalformedDeclaration`](Self::MalformedDeclaration), for an
    /// item that starts with something other than an ident (`1px: red;`).
    /// Not reported when that first token is a bad string or bad url, which
    /// already has its own error.
    UnexpectedToken,
    /// A `U+…` unicode-range with more than six hex digits in a run — more
    /// than any code point needs, so malformed under any reading.
    InvalidUnicodeRange,
    /// A style rule whose prelude is not a valid selector list (Selectors
    /// Level 4 §3), at the top level, nested in a style rule, or inside a
    /// conditional group rule such as `@media`. A rule nested in a style rule
    /// is read as a *relative* selector list, so `> a` is fine there and not
    /// at the top level. The rule itself is kept in the tree. See [`crate::selector`] for what is and isn't reported — the
    /// check is syntactic and deliberately permissive.
    InvalidSelector,
    /// Block/function nesting past [`MAX_NESTING_DEPTH`]. The contents below
    /// that point are discarded rather than parsed, so this is reported
    /// once, at the outermost bracket that was refused.
    ///
    /// Unlike every other variant here this is not a defect in the CSS as
    /// such — it is the parser declining to recurse further, because the
    /// alternative is a stack overflow, which in Rust aborts the process
    /// rather than raising a catchable error. Real stylesheets nest 2 deep;
    /// anything reaching 256 is machine-generated or hostile.
    NestingTooDeep,
    /// A top-level rule whose prelude starts like a custom property,
    /// `--foo:hover { … }`. §5.5.3 drops it without naming a parse error;
    /// it is reported so the dropped content leaves a trace. The span covers
    /// the whole construct, prelude and block.
    DroppedCustomPropertyRule,
}

/// The deepest block/function nesting the parser will descend into.
///
/// Consuming a block and consuming a component value are mutually recursive,
/// so nesting costs stack in proportion to depth:
/// `a{color:((((…))))}`, `@media all{@media all{…}}`,
/// `rgb(rgb(rgb(…)))` and `:is(:is(:is(…)))` all reach it. Measured on
/// 0.6.1, all four abort the process between 10,000 and 20,000 deep on an
/// 8 MiB main thread - from stylesheets of about 1.2 KB - and proportionally
/// sooner on a 2 MiB worker thread. In Rust a stack overflow is `SIGABRT`,
/// not a catchable panic, so no caller can defend against it downstream;
/// the bound has to live here.
///
/// 256 comes from data: across a 65-book EPUB shelf the deepest stylesheet
/// nests **2** (median 2, p95 2), because CSS gets deep only through
/// `@media`-wrapped rules and nested functions. That leaves this limit ~128x
/// above real-world CSS and far below the crash.
///
/// The block of a top-level rule does not count towards it; every bracket,
/// function and nested rule's block inside one does. Past the limit the
/// parser skips to the matching close bracket, so the rest of the stylesheet
/// still parses, and reports [`SyntaxErrorKind::NestingTooDeep`].
pub const MAX_NESTING_DEPTH: usize = 256;

/// Parse a stylesheet (§5.4.3): the whole tree, and every [`SyntaxError`]
/// recovered from anywhere in it, sorted by position.
pub fn parse_stylesheet(css: &str) -> (Stylesheet<'_>, Vec<SyntaxError>) {
    let mut p = Parser::new(css);
    let rules = p.stylesheet_contents();
    (Stylesheet { rules }, p.finish())
}

/// The [`SyntaxError`]s in a stylesheet, in source order — a convenience over
/// [`parse_stylesheet`] for callers that only want the errors.
pub fn syntax_errors(css: &str) -> Vec<SyntaxError> {
    parse_stylesheet(css).1
}

/// Parse a block's contents (§5.4.5): the input read as the inside of a
/// rule's `{ … }`, without the braces. This is the reading for an HTML
/// `style="…"` attribute. Spans index into `css`.
///
/// A `}` the input never opened ends the block, as it does in the spec; the
/// rest of the input is not read, and an
/// [`UnexpectedToken`](SyntaxErrorKind::UnexpectedToken) marks where reading
/// stopped.
pub fn parse_block_contents(css: &str) -> (Vec<BlockItem<'_>>, Vec<SyntaxError>) {
    let mut p = Parser::new(css);
    // The element the attribute sits on plays the parent style rule.
    let items = p.block_contents(Context::Nested);
    if p.pos < p.toks.len() {
        p.error(p.toks[p.pos].span, SyntaxErrorKind::UnexpectedToken);
    }
    (items, p.finish())
}

/// How the preludes of qualified rules in a block are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// Children are style rules: their preludes are selector lists, checked
    /// by [`crate::selector`]. The stylesheet itself, and a conditional
    /// group rule there.
    Style,
    /// Children are style rules nested in a style rule, whose preludes are
    /// *relative* selector lists (CSS Nesting §2.1): `> a`, `+ b`. A style
    /// rule's block, and a conditional group rule inside one.
    Nested,
    /// Anything else. In `@keyframes` the children's preludes are keyframe
    /// selectors (`from`, `0%`), a different grammar under which `0%` is
    /// correct; in an at-rule this crate does not know, nothing is known
    /// about them. Either way they are handed back unexamined.
    Opaque,
}

/// At-rules whose block holds style rules: the conditional group rules of CSS
/// Conditional 3 §3, plus `@scope` and `@starting-style`. A vendor prefix is
/// stripped before the lookup, which is what carries `@-moz-document`.
///
/// This table decides only whether nested preludes are checked as selectors.
/// It does not decide how a block is parsed: every block is parsed the same
/// way. An at-rule missing from it errs toward silence.
const STYLE_RULE_BLOCKS: &[&str] = &[
    "media",
    "supports",
    "container",
    "layer",
    "scope",
    "document",
    "starting-style",
];

/// The context an at-rule's block is read in, given the context the at-rule
/// sits in: a grouping rule passes its own on (`@media` inside a style rule
/// still holds nested style rules), anything else is opaque.
fn context_for_at_rule(name: &str, outer: Context) -> Context {
    match (holds_style_rules(unprefixed(name)), outer) {
        (false, _) => Context::Opaque,
        (true, Context::Nested) => Context::Nested,
        (true, _) => Context::Style,
    }
}

/// Whether an at-rule (vendor prefix already stripped) holds style rules.
pub(crate) fn holds_style_rules(bare_name: &str) -> bool {
    STYLE_RULE_BLOCKS
        .iter()
        .any(|r| bare_name.eq_ignore_ascii_case(r))
}

/// Strip a leading vendor prefix (`-webkit-`, `-moz-`, `-ms-`, `-o-`, …):
/// a `-`, a run of letters, a `-`. A name that is not prefixed comes back
/// unchanged, including a custom `--name`, whose second character is `-`
/// rather than a letter.
pub(crate) fn unprefixed(name: &str) -> &str {
    let Some(rest) = name.strip_prefix('-') else {
        return name;
    };
    match rest.find('-') {
        Some(i) if i > 0 && rest[..i].chars().all(|c| c.is_ascii_alphabetic()) => &rest[i + 1..],
        _ => name,
    }
}

/// Where a scan along one level of the token stream stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// A `{` at this level, at the given index.
    Curly(usize),
    /// A `;` at this level (index), or the `}` that closes the enclosing
    /// block, or the end of input (index = `toks.len()`).
    End(usize),
}

struct Parser<'a> {
    input: &'a str,
    toks: Vec<Spanned<Token<'a>>>,
    /// For each opening token (`{`, `[`, `(`, a function), the index of the
    /// token that closes it, or `toks.len()` if nothing does. Unused for
    /// every other token.
    close: Vec<usize>,
    pos: usize,
    /// Current nesting depth; see [`MAX_NESTING_DEPTH`].
    depth: usize,
    errors: Vec<SyntaxError>,
    /// False while re-reading a unicode-range value, whose tokens were
    /// already reported on during the main pass.
    report: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        let mut p = Parser::from_tokens(input, Tokenizer::new(input), true);
        p.unicode_range_errors();
        p
    }

    /// Tokenize everything and pair the brackets, in one pass.
    ///
    /// Pairing follows the spec's own nesting rule: a block ends only at its
    /// mirror token, so in `( ] )` the `]` is an ordinary token inside the
    /// parentheses, and a closer that matches nothing open stays a token.
    fn from_tokens(input: &'a str, mut tokenizer: Tokenizer<'a>, report: bool) -> Self {
        let mut toks = Vec::with_capacity(input.len() / 3 + 1);
        let mut close = Vec::with_capacity(input.len() / 3 + 1);
        let mut open: Vec<(usize, u8)> = Vec::new();
        let mut errors = Vec::new();
        while let Some(t) = tokenizer.next_token_spanned() {
            let i = toks.len();
            close.push(usize::MAX);
            match &t.node {
                Token::LeftCurly => open.push((i, b'}')),
                Token::LeftSquare => open.push((i, b']')),
                Token::LeftParen | Token::Function(_) => open.push((i, b')')),
                Token::RightCurly | Token::RightSquare | Token::RightParen => {
                    let c = match t.node {
                        Token::RightCurly => b'}',
                        Token::RightSquare => b']',
                        _ => b')',
                    };
                    if let Some(&(o, want)) = open.last()
                        && want == c
                    {
                        close[o] = i;
                        open.pop();
                    }
                }
                Token::BadString => errors.push(SyntaxError {
                    span: t.span,
                    kind: SyntaxErrorKind::BadString,
                }),
                Token::BadUrl(_) => errors.push(SyntaxError {
                    span: t.span,
                    kind: SyntaxErrorKind::BadUrl,
                }),
                _ => {}
            }
            toks.push(t);
        }
        let len = toks.len();
        for (o, _) in open {
            close[o] = len;
        }
        if !report {
            errors.clear();
        }
        Parser {
            input,
            toks,
            close,
            pos: 0,
            depth: 0,
            errors,
            report,
        }
    }

    fn finish(mut self) -> Vec<SyntaxError> {
        self.errors.sort_by_key(|e| e.span.start);
        self.errors
    }

    fn error(&mut self, span: Span, kind: SyntaxErrorKind) {
        if self.report {
            self.errors.push(SyntaxError { span, kind });
        }
    }

    #[inline]
    fn tok(&self, i: usize) -> Option<&Token<'a>> {
        self.toks.get(i).map(|t| &t.node)
    }

    /// Move a token out of the stream. Each token is built into the tree at
    /// most once, so the placeholder left behind is never read as content.
    #[inline]
    fn take(&mut self, i: usize) -> Token<'a> {
        std::mem::replace(&mut self.toks[i].node, Token::Whitespace)
    }

    /// The span of the last token, for a node that runs to the end of input.
    fn last_span(&self) -> Span {
        self.toks.last().map_or(Span::new(0, 0), |t| t.span)
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while matches!(self.tok(i), Some(Token::Whitespace)) {
            i += 1;
        }
        i
    }

    /// The index just past the bracket or function opened at `i`.
    #[inline]
    fn after_close(&self, i: usize) -> usize {
        self.close[i].saturating_add(1).min(self.toks.len())
    }

    /// Walk one level of the stream from `i`, stepping over every bracketed
    /// group, to the first `{`, `;` or `}` at this level, or the end.
    ///
    /// `nested` is the spec's flag: inside a block a `}` ends the item; at the
    /// top level it is an ordinary token, and so is `;` for a qualified rule.
    fn scan(&self, mut i: usize, nested: bool) -> Stop {
        loop {
            match self.tok(i) {
                None => return Stop::End(i),
                Some(Token::LeftCurly) => return Stop::Curly(i),
                Some(Token::Semicolon | Token::RightCurly) if nested => return Stop::End(i),
                Some(Token::LeftSquare | Token::LeftParen | Token::Function(_)) => {
                    i = self.after_close(i)
                }
                Some(_) => i += 1,
            }
        }
    }

    /// §5.5.1 "Consume a stylesheet's contents".
    fn stylesheet_contents(&mut self) -> Vec<Spanned<Rule<'a>>> {
        let mut rules = Vec::new();
        while let Some(t) = self.tok(self.pos) {
            match t {
                Token::Whitespace | Token::Cdo | Token::Cdc => self.pos += 1,
                Token::AtKeyword(_) => {
                    let r = self.at_rule(false, Context::Style);
                    rules.push(r);
                }
                _ => {
                    if let Some(r) = self.top_level_qualified_rule() {
                        rules.push(r);
                    }
                }
            }
        }
        rules
    }

    /// §5.5.3 "Consume a qualified rule", with `nested` false.
    fn top_level_qualified_rule(&mut self) -> Option<Spanned<Rule<'a>>> {
        let start = self.pos;
        let brace = match self.scan(start, false) {
            Stop::Curly(b) => b,
            Stop::End(end) => {
                // The prelude ran to the end of input with no block.
                self.error(self.toks[start].span, SyntaxErrorKind::UnterminatedRule);
                self.pos = end;
                return None;
            }
        };
        if self.looks_like_custom_property(start, brace) {
            // `--foo:hover { … }`: §5.5.3 consumes the block and returns
            // nothing.
            let end = self.after_close(brace);
            let last = self.toks[end - 1].span;
            self.error(
                self.toks[start].span.to(last),
                SyntaxErrorKind::DroppedCustomPropertyRule,
            );
            self.pos = end;
            return None;
        }
        Some(self.qualified_rule(start, brace, Context::Style, false))
    }

    /// Whether a prelude's first two non-whitespace tokens are a `--name`
    /// ident and a colon (§5.5.3).
    fn looks_like_custom_property(&self, start: usize, end: usize) -> bool {
        let i = self.skip_ws(start);
        matches!(self.tok(i), Some(Token::Ident(n)) if n.starts_with("--"))
            && self.skip_ws(i + 1) < end
            && matches!(self.tok(self.skip_ws(i + 1)), Some(Token::Colon))
    }

    /// Build a qualified rule whose prelude is `start..brace` and whose block
    /// opens at `brace`. `ctx` is the context the rule sits in.
    fn qualified_rule(
        &mut self,
        start: usize,
        brace: usize,
        ctx: Context,
        nested: bool,
    ) -> Spanned<Rule<'a>> {
        let prelude = self.values_until(brace);
        if self.report {
            match ctx {
                Context::Style => self
                    .errors
                    .extend(crate::selector::validate_selector_list(&prelude)),
                Context::Nested => self
                    .errors
                    .extend(crate::selector::validate_relative_selector_list(&prelude)),
                Context::Opaque => {}
            }
        }
        // A style rule's children are nested style rules; a keyframe's, or
        // anything else's, are not known to be anything.
        let inner = match ctx {
            Context::Style | Context::Nested => Context::Nested,
            Context::Opaque => Context::Opaque,
        };
        let block = self.rule_block(brace, inner, nested);
        let span = self.toks[start].span.to(block.span);
        Spanned::new(Rule::Qualified(QualifiedRule { prelude, block }), span)
    }

    /// §5.5.2 "Consume an at-rule". The current token is the at-keyword.
    fn at_rule(&mut self, nested: bool, ctx: Context) -> Spanned<Rule<'a>> {
        let name_span = self.toks[self.pos].span;
        let Token::AtKeyword(name) = self.take(self.pos) else {
            unreachable!("at_rule requires an at-keyword as the current token")
        };
        self.pos += 1;
        let mut prelude = Vec::new();
        loop {
            match self.tok(self.pos) {
                None => break,
                Some(Token::Semicolon) => {
                    let semi = self.toks[self.pos].span;
                    self.pos += 1;
                    let node = AtRule {
                        name,
                        name_span,
                        prelude,
                        block: None,
                    };
                    return Spanned::new(Rule::At(node), name_span.to(semi));
                }
                Some(Token::RightCurly) if nested => break,
                Some(Token::LeftCurly) => {
                    let ctx = context_for_at_rule(&name, ctx);
                    let block = self.rule_block(self.pos, ctx, nested);
                    let span = name_span.to(block.span);
                    let node = AtRule {
                        name,
                        name_span,
                        prelude,
                        block: Some(block),
                    };
                    return Spanned::new(Rule::At(node), span);
                }
                Some(_) => {
                    let v = self.component_value();
                    prelude.push(v);
                }
            }
        }
        // Ended by the end of input, or by the `}` of the enclosing block.
        let end = prelude
            .iter()
            .rev()
            .find(|v| !matches!(v.node, ComponentValue::Token(Token::Whitespace)))
            .map_or(name_span, |v| v.span);
        let node = AtRule {
            name,
            name_span,
            prelude,
            block: None,
        };
        Spanned::new(Rule::At(node), name_span.to(end))
    }

    /// §5.5.4 "Consume a block" for the `{` at `open`, which belongs to a
    /// rule. `nested` is whether that rule sits inside another rule's block,
    /// which is what makes the block count towards [`MAX_NESTING_DEPTH`].
    fn rule_block(&mut self, open: usize, ctx: Context, nested: bool) -> Block<'a> {
        let open_span = self.toks[open].span;
        let close = self.close[open];
        if nested && self.depth >= MAX_NESTING_DEPTH {
            return self.refuse(open);
        }
        if nested {
            self.depth += 1;
        }
        self.pos = open + 1;
        let items = self.block_contents(ctx);
        debug_assert!(self.pos == close || (close == self.toks.len() && self.pos == close));
        let end = if close < self.toks.len() {
            self.pos = close + 1;
            self.toks[close].span
        } else {
            self.pos = self.toks.len();
            self.error(open_span, SyntaxErrorKind::UnterminatedBlock);
            self.last_span()
        };
        if nested {
            self.depth -= 1;
        }
        Spanned::new(items, open_span.to(end))
    }

    /// Report the refusal and skip the bracketed group at `open` whole.
    fn refuse<T: Default>(&mut self, open: usize) -> Spanned<T> {
        let open_span = self.toks[open].span;
        self.error(open_span, SyntaxErrorKind::NestingTooDeep);
        self.pos = self.after_close(open);
        let end = self.toks[self.pos - 1].span;
        Spanned::new(T::default(), open_span.to(end))
    }

    /// §5.5.5 "Consume a block's contents". Stops at the `}` that closes the
    /// block, or the end of input, without consuming it.
    fn block_contents(&mut self, ctx: Context) -> Vec<BlockItem<'a>> {
        let mut items = Vec::new();
        while let Some(t) = self.tok(self.pos) {
            match t {
                Token::Whitespace | Token::Semicolon => self.pos += 1,
                Token::RightCurly => break,
                Token::AtKeyword(_) => {
                    let r = self.at_rule(true, ctx);
                    items.push(BlockItem::Rule(r));
                }
                _ => {
                    if let Some(item) = self.block_item(ctx) {
                        items.push(item);
                    }
                }
            }
        }
        // As in `values_until`: most blocks are short, and a block item is
        // large enough for the spare capacity to dominate the tree.
        items.shrink_to_fit();
        items
    }

    /// One item of a block that is not an at-rule: "consume a declaration,
    /// and if nothing comes back, consume a qualified rule" — decided before
    /// building anything, by looking at this level of the stream only.
    fn block_item(&mut self, ctx: Context) -> Option<BlockItem<'a>> {
        let start = self.pos;
        if let Some(end) = self.declaration_end(start) {
            return Some(BlockItem::Declaration(self.declaration(start, end)));
        }
        // Not a declaration: re-read as a qualified rule, with `nested` set
        // and `;` as the stop token.
        match self.scan(start, true) {
            Stop::Curly(brace) => Some(BlockItem::Rule(
                self.qualified_rule(start, brace, ctx, true),
            )),
            Stop::End(end) => {
                // Neither reading works; the item is discarded up to the `;`
                // or the end of the block.
                let first = &self.toks[start];
                match &first.node {
                    Token::Ident(_) => {
                        self.error(first.span, SyntaxErrorKind::MalformedDeclaration)
                    }
                    // A bad string or url has its own error already.
                    Token::BadString | Token::BadUrl(_) => {}
                    _ => self.error(first.span, SyntaxErrorKind::UnexpectedToken),
                }
                self.pos = end;
                None
            }
        }
    }

    /// If the item at `start` is a declaration (§5.5.6), the index where its
    /// value ends: the `;`, the block's `}`, or the end of input.
    ///
    /// This is the CRD's list of early exits, which a property-blind parser
    /// can take in full: a declaration is an ident, a colon, and then a
    /// value that either holds no top-level `{ … }` or is nothing but one
    /// (plus `!important`). A custom property's value may hold anything.
    fn declaration_end(&self, start: usize) -> Option<usize> {
        let Some(Token::Ident(name)) = self.tok(start) else {
            return None;
        };
        let colon = self.skip_ws(start + 1);
        if !matches!(self.tok(colon), Some(Token::Colon)) {
            return None;
        }
        let first = self.skip_ws(colon + 1);
        if name.starts_with("--") {
            // Always a declaration: no rule can start like one.
            let mut i = first;
            loop {
                match self.tok(i) {
                    None | Some(Token::Semicolon | Token::RightCurly) => return Some(i),
                    Some(
                        Token::LeftCurly
                        | Token::LeftSquare
                        | Token::LeftParen
                        | Token::Function(_),
                    ) => i = self.after_close(i),
                    Some(_) => i += 1,
                }
            }
        }
        if matches!(self.tok(first), Some(Token::LeftCurly)) {
            // `foo: { … }` is a declaration only if nothing but `!important`
            // follows the block.
            let mut i = self.skip_ws(self.after_close(first));
            if matches!(self.tok(i), Some(Token::Delim('!'))) {
                let imp = self.skip_ws(i + 1);
                if matches!(self.tok(imp), Some(Token::Ident(s)) if s.eq_ignore_ascii_case("important"))
                {
                    i = self.skip_ws(imp + 1);
                }
            }
            return match self.tok(i) {
                None | Some(Token::Semicolon | Token::RightCurly) => Some(i),
                _ => None,
            };
        }
        // A `{` anywhere later in the value makes it a rule (`a:hover {`).
        match self.scan(first, true) {
            Stop::End(end) => Some(end),
            Stop::Curly(_) => None,
        }
    }

    /// Build the declaration at `start`, whose value ends at `end`.
    fn declaration(&mut self, start: usize, end: usize) -> Spanned<Declaration<'a>> {
        let name_span = self.toks[start].span;
        let Token::Ident(name) = self.take(start) else {
            unreachable!("declaration_end checked for an ident")
        };
        let colon = self.skip_ws(start + 1);
        let colon_span = self.toks[colon].span;
        self.pos = self.skip_ws(colon + 1);
        let mut value = self.values_until(end);
        while matches!(
            value.last().map(|v| &v.node),
            Some(ComponentValue::Token(Token::Whitespace))
        ) {
            value.pop();
        }
        // The declaration's span covers everything it consumed, including a
        // trailing `!important` (stripped from `value` but part of the text).
        let last = value.last().map_or(colon_span, |v| v.span);
        let important = strip_trailing_important(&mut value);
        if !name.starts_with("--")
            && name.eq_ignore_ascii_case("unicode-range")
            && let (Some(first), Some(last)) = (value.first(), value.last())
        {
            value = self.reread_unicode_range(first.span.start, last.span.end);
        }
        let node = Declaration {
            name,
            name_span,
            value,
            important,
        };
        Spanned::new(node, name_span.to(last))
    }

    /// §5.5.11: the value of a `unicode-range` descriptor is tokenized again
    /// from its source text, with unicode ranges allowed. Errors were already
    /// reported on the first reading and are not reported twice.
    fn reread_unicode_range(
        &mut self,
        start: usize,
        end: usize,
    ) -> Vec<Spanned<ComponentValue<'a>>> {
        let tokenizer = Tokenizer::unicode_range_value(self.input, start, end);
        let mut sub = Parser::from_tokens(self.input, tokenizer, false);
        sub.depth = self.depth;
        let n = sub.toks.len();
        sub.values_until(n)
    }

    /// Component values from the current position up to (not including)
    /// index `end`, which must be at the current level.
    ///
    /// The list is allocated at its exact length, counted first by stepping
    /// over this level: a tree is mostly short lists, and growing them leaves
    /// up to three quarters of each allocation empty.
    fn values_until(&mut self, end: usize) -> Vec<Spanned<ComponentValue<'a>>> {
        let mut n = 0;
        let mut i = self.pos;
        while i < end {
            n += 1;
            i = match self.tok(i) {
                Some(
                    Token::LeftCurly | Token::LeftSquare | Token::LeftParen | Token::Function(_),
                ) => self.after_close(i),
                _ => i + 1,
            };
        }
        let mut values = Vec::with_capacity(n);
        while self.pos < end {
            values.push(self.component_value());
        }
        values
    }

    /// §5.5.8 "Consume a component value".
    fn component_value(&mut self) -> Spanned<ComponentValue<'a>> {
        let i = self.pos;
        let kind = match &self.toks[i].node {
            Token::LeftCurly => Some(BlockKind::Curly),
            Token::LeftSquare => Some(BlockKind::Square),
            Token::LeftParen => Some(BlockKind::Paren),
            Token::Function(_) => None,
            _ => {
                let span = self.toks[i].span;
                let t = self.take(i);
                self.pos += 1;
                return Spanned::new(ComponentValue::Token(t), span);
            }
        };
        let open_span = self.toks[i].span;
        if self.depth >= MAX_NESTING_DEPTH {
            let skipped: Spanned<()> = self.refuse(i);
            let node = match kind {
                Some(kind) => ComponentValue::Block(SimpleBlock {
                    kind,
                    values: Vec::new(),
                }),
                None => {
                    let Token::Function(name) = self.take(i) else {
                        unreachable!()
                    };
                    ComponentValue::Function {
                        name,
                        args: Vec::new(),
                    }
                }
            };
            return Spanned::new(node, skipped.span);
        }
        let close = self.close[i];
        self.depth += 1;
        let name = match kind {
            None => match self.take(i) {
                Token::Function(name) => Some(name),
                _ => unreachable!(),
            },
            Some(_) => None,
        };
        self.pos = i + 1;
        let values = self.values_until(close.min(self.toks.len()));
        self.depth -= 1;
        let end = if close < self.toks.len() {
            self.pos = close + 1;
            self.toks[close].span
        } else {
            // Only an unclosed bracket is reported; an unclosed function is
            // not, as before 0.12 — the end of input closes it silently.
            if kind.is_some() {
                self.error(open_span, SyntaxErrorKind::UnterminatedBlock);
            }
            values.last().map_or(open_span, |v| v.span)
        };
        let node = match (kind, name) {
            (Some(kind), _) => ComponentValue::Block(SimpleBlock { kind, values }),
            (None, Some(name)) => ComponentValue::Function { name, args: values },
            (None, None) => unreachable!(),
        };
        Spanned::new(node, open_span.to(end))
    }

    /// Malformed `U+…` unicode-ranges, as epubcheck's CSS scanner reports them
    /// (`SCANNER_ILLEGAL_URANGE`).
    ///
    /// Its rule is narrower than it sounds: it walks the characters after `U+`
    /// that are hex digits, `?` or `-`, and errors when **seven** of them
    /// appear without an intervening `-`. Nothing else is checked — not the
    /// ordering of a range, not whether `?` only trails, not whether the
    /// endpoints make sense. Six hex digits is the most a real code point
    /// needs (`U+10FFFF`), so a run of seven is malformed under any reading,
    /// which is what makes this safe to report.
    ///
    /// Detection walks the **token stream**, not the raw text. `U+00000000`
    /// inside a string or a comment is one `String` token or skipped
    /// entirely, so it cannot be mistaken for a range — which scanning the
    /// source directly would do.
    fn unicode_range_errors(&mut self) {
        const MAX_RUN: usize = 6;
        for t in &self.toks {
            // Anchor on the `u` ident, then read the source after it. Reading
            // the *source* rather than the following tokens is deliberate:
            // `U+0-7F` tokenizes as Ident("U"), Number(+0), … because a
            // stylesheet is tokenized with unicode ranges off, and the
            // character run epubcheck counts does not survive that.
            let Token::Ident(name) = &t.node else {
                continue;
            };
            if !name.eq_ignore_ascii_case("u") {
                continue;
            }
            let after_ident = &self.input[t.span.end..];
            if !after_ident.starts_with('+') {
                continue;
            }
            let mut run = 0usize;
            for (i, c) in after_ident[1..].char_indices() {
                if c == '-' {
                    run = 0;
                    continue;
                }
                if !c.is_ascii_hexdigit() && c != '?' {
                    break;
                }
                run += 1;
                if run > MAX_RUN {
                    self.errors.push(SyntaxError {
                        span: Span::new(t.span.start, t.span.end + 1 + i + c.len_utf8()),
                        kind: SyntaxErrorKind::InvalidUnicodeRange,
                    });
                    break;
                }
            }
        }
    }
}

/// §5.5.6: if the last two non-whitespace values are `!` then an `important`
/// ident (case-insensitive), remove both and report it.
fn strip_trailing_important(value: &mut Vec<Spanned<ComponentValue<'_>>>) -> bool {
    let mut rev = value
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, v)| !matches!(v.node, ComponentValue::Token(Token::Whitespace)));
    let last = rev.next();
    let second_last = rev.next();
    if let (Some((li, lv)), Some((si, sv))) = (last, second_last) {
        let is_important = matches!(&lv.node, ComponentValue::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("important"));
        let is_bang = matches!(&sv.node, ComponentValue::Token(Token::Delim('!')));
        if is_important && is_bang {
            value.truncate(si);
            debug_assert!(li > si);
            while matches!(
                value.last().map(|v| &v.node),
                Some(ComponentValue::Token(Token::Whitespace))
            ) {
                value.pop();
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(css: &str) -> Vec<SyntaxErrorKind> {
        syntax_errors(css).into_iter().map(|e| e.kind).collect()
    }

    /// The items of the first top-level rule's block.
    fn items(css: &str) -> Vec<BlockItem<'_>> {
        let (mut sheet, _) = parse_stylesheet(css);
        let rule = sheet.rules.remove(0).node;
        match rule {
            Rule::Qualified(q) => q.block.node,
            Rule::At(a) => a.block.expect("a block").node,
        }
    }

    /// A compact picture of a block: `d:name` per declaration, `r:prelude`
    /// per qualified rule, `@name` per at-rule.
    fn shape(css: &str) -> Vec<String> {
        items(css)
            .iter()
            .map(|i| match i {
                BlockItem::Declaration(d) => format!("d:{}", d.node.name),
                BlockItem::Rule(r) => match &r.node {
                    Rule::Qualified(_) => {
                        format!("r:{}", r.span.slice(css).split('{').next().unwrap().trim())
                    }
                    Rule::At(a) => format!("@{}", a.name),
                },
            })
            .collect()
    }

    // --- the tree and its spans ---

    #[test]
    fn top_level_rule_span_covers_whole_rule() {
        let src = "body { color: red; }";
        let (sheet, errs) = parse_stylesheet(src);
        assert!(errs.is_empty());
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].span.slice(src), src);
        let block = sheet.rules[0].node.block().unwrap();
        assert_eq!(block.span.slice(src), "{ color: red; }");
    }

    #[test]
    fn declaration_is_located() {
        let src = "a {\n  color: red;\n}";
        let BlockItem::Declaration(d) = &items(src)[0] else {
            panic!("expected a declaration")
        };
        assert_eq!(d.node.name, "color");
        assert_eq!(d.node.name_span.slice(src), "color");
        assert_eq!(d.node.name_span.start_line_col(src), (2, 3));
        assert_eq!(d.span.slice(src), "color: red");
    }

    #[test]
    fn a_rule_nested_in_media_is_located() {
        let src = "@media screen {\n  div.box { padding: 0; }\n}";
        let (sheet, errs) = parse_stylesheet(src);
        assert!(errs.is_empty(), "{errs:?}");
        let Rule::At(at) = &sheet.rules[0].node else {
            panic!("expected an at-rule");
        };
        assert_eq!(at.name, "media");
        assert_eq!(at.name_span.slice(src), "@media");
        let BlockItem::Rule(inner) = &at.block.as_ref().unwrap().node[0] else {
            panic!("expected a nested rule")
        };
        assert_eq!(inner.span.slice(src), "div.box { padding: 0; }");
        let BlockItem::Declaration(d) = &inner.node.block().unwrap().node[0] else {
            panic!("expected a declaration")
        };
        assert_eq!(d.node.name_span.start_line_col(src), (2, 13));
    }

    #[test]
    fn value_whitespace_is_trimmed_and_important_stripped() {
        let src = "a { color :  red  ; padding: 0 ! IMPORTANT }";
        let it = items(src);
        let BlockItem::Declaration(color) = &it[0] else {
            panic!()
        };
        assert_eq!(color.node.value.len(), 1);
        assert_eq!(color.span.slice(src), "color :  red");
        assert!(!color.node.important);
        let BlockItem::Declaration(padding) = &it[1] else {
            panic!()
        };
        assert!(padding.node.important);
        assert_eq!(padding.node.value.len(), 1, "{:?}", padding.node.value);
        // The span reaches through the stripped `!important`.
        assert_eq!(padding.span.slice(src), "padding: 0 ! IMPORTANT");
    }

    /// The spec's rule is "the last two non-whitespace values", nothing
    /// looser: an `!important` that is not at the end is part of the value.
    #[test]
    fn important_must_be_last() {
        let BlockItem::Declaration(d) = &items("a { b: !important c }")[0] else {
            panic!()
        };
        assert!(!d.node.important);
        // `!`, `important`, a space, `c`.
        assert_eq!(d.node.value.len(), 4);
    }

    #[test]
    fn function_and_its_args_carry_spans() {
        let src = "a { color: rgb(1, 2, 3) }";
        let BlockItem::Declaration(d) = &items(src)[0] else {
            panic!()
        };
        let f = &d.node.value[0];
        assert!(
            matches!(&f.node, ComponentValue::Function { name, args } if name == "rgb" && args.len() == 7)
        );
        assert_eq!(f.span.slice(src), "rgb(1, 2, 3)");
    }

    #[test]
    fn a_statement_at_rule_ends_at_its_semicolon() {
        let src = "@import url(a.css) print; p { }";
        let (sheet, errs) = parse_stylesheet(src);
        assert!(errs.is_empty());
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].span.slice(src), "@import url(a.css) print;");
        assert!(sheet.rules[0].node.block().is_none());
    }

    #[test]
    fn html_comment_tokens_are_ignored_at_the_top_level() {
        let (sheet, errs) = parse_stylesheet("<!-- p { color: red } -->");
        assert!(errs.is_empty());
        assert_eq!(sheet.rules.len(), 1);
    }

    // --- nesting: §5.5.5 reads every block the same way ---

    #[test]
    fn a_style_rule_nested_in_a_style_rule() {
        let css = "p { color: red; a { color: blue } }";
        assert_eq!(shape(css), ["d:color", "r:a"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn declarations_and_rules_keep_their_order() {
        let css = "p { a { color: blue } color: red; & b { } margin: 0 }";
        assert_eq!(shape(css), ["r:a", "d:color", "r:& b", "d:margin"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn a_pseudo_class_selector_is_not_a_declaration() {
        // `a:hover` starts like `name: value`; the `{` after other values is
        // what makes it a rule (the CRD's fourth early exit).
        let css = "p { a:hover { color: blue } a:is(.x, .y) b { } }";
        assert_eq!(shape(css), ["r:a:hover", "r:a:is(.x, .y) b"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn a_nested_rule_with_a_bad_selector_is_kept_and_reported() {
        let css = "p { . a { color: blue } }";
        assert_eq!(shape(css), ["r:. a"]);
        assert_eq!(kinds(css), [SyntaxErrorKind::InvalidSelector]);
    }

    /// epubveri's example from the 2026-10-02 note: a block never closed,
    /// with two rules whose selectors are broken.
    #[test]
    fn an_unclosed_block_holds_real_nested_rules() {
        let css = "p { color: red; a. q { x: y }\n. r { x: y }";
        assert_eq!(shape(css), ["d:color", "r:a. q", "r:. r"]);
        let errs = syntax_errors(css);
        assert_eq!(
            errs.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [
                SyntaxErrorKind::UnterminatedBlock,
                SyntaxErrorKind::InvalidSelector,
                SyntaxErrorKind::InvalidSelector
            ]
        );
        // The block error sits at the `{`, and the rule runs to the end.
        assert_eq!(errs[0].span.slice(css), "{");
        let (sheet, _) = parse_stylesheet(css);
        assert_eq!(sheet.rules[0].span.end, css.len());
    }

    #[test]
    fn a_declaration_directly_in_media_is_a_declaration() {
        let css = "@media print { color: red }";
        assert_eq!(shape(css), ["d:color"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn a_rule_in_a_descriptor_at_rule_is_visible_and_not_a_selector() {
        let css = "@font-face { font-family: X; p { color: red } 0% { } }";
        assert_eq!(shape(css), ["d:font-family", "r:p", "r:0%"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn keyframe_selectors_are_not_selectors() {
        for css in [
            "@keyframes spin { 0% { opacity: 0 } 100% { opacity: 1 } }",
            "@keyframes spin { from { opacity: 0 } to { opacity: 1 } }",
            "@-webkit-keyframes spin { 50% { opacity: .5 } }",
            "@keyframes spin { }",
        ] {
            assert!(kinds(css).is_empty(), "{css}");
        }
    }

    #[test]
    fn style_rule_preludes_are_checked_in_every_grouping_rule() {
        for css in [
            "@media print { .. { color: red } }",
            "@supports (display: grid) { .. { color: red } }",
            "@starting-style { .. { opacity: 0 } }",
            "@-moz-document url-prefix() { .. { color: red } }",
            "@media print { @media (width > 0) { .. { color: red } } }",
        ] {
            assert_eq!(kinds(css), [SyntaxErrorKind::InvalidSelector], "{css}");
        }
        assert!(kinds("@media print { p.foo, #b > i { color: red } }").is_empty());
        // Unknown at-rules: preludes inside are not checked.
        assert!(kinds("@future (cond) { .. { color: red } }").is_empty());
    }

    /// A nested style rule's prelude is a relative selector list, so it may
    /// begin with a combinator. A top-level one may not.
    #[test]
    fn nested_selectors_may_be_relative() {
        for css in [
            "p { > a { } + b { } ~ c { } }",
            "p { > a, + b { } }",
            "p { @media print { > a { } } }",
            "p { a { > b { } } }",
        ] {
            assert!(kinds(css).is_empty(), "{css}: {:?}", kinds(css));
        }
        for css in [
            "> a { }",
            "@media print { > a { } }",
            "p { > { } }",
            "p { > > a { } }",
            "p { a > { } }",
        ] {
            assert_eq!(kinds(css), [SyntaxErrorKind::InvalidSelector], "{css}");
        }
        assert!(
            parse_block_contents("color: red; > a { color: blue }")
                .1
                .is_empty()
        );
    }

    #[test]
    fn a_non_custom_value_with_a_block_and_more_is_a_rule() {
        // §5.5.6: a top-level {}-block is only allowed as the whole value.
        let css = "p { color: red {} }";
        assert_eq!(shape(css), ["r:color: red"]);
        assert_eq!(kinds(css), [SyntaxErrorKind::InvalidSelector]);
    }

    #[test]
    fn a_block_as_the_whole_value_is_a_declaration() {
        let css = "p { foo: {a: b}; bar: { } !important; baz: {}}";
        assert_eq!(shape(css), ["d:foo", "d:bar", "d:baz"]);
        assert!(kinds(css).is_empty());
        let BlockItem::Declaration(bar) = &items(css)[1] else {
            panic!()
        };
        assert!(bar.node.important);
        // Something after the block makes it a rule, and what is left over
        // is a separate, malformed item.
        let css = "p { font: {} bar; color: red }";
        assert_eq!(shape(css), ["r:font:", "d:color"]);
        assert_eq!(
            kinds(css),
            [
                SyntaxErrorKind::InvalidSelector,
                SyntaxErrorKind::MalformedDeclaration
            ]
        );
    }

    #[test]
    fn a_custom_property_may_hold_anything() {
        let css = "p { --x: {a} b {c}; --y:hover {}; color: red }";
        assert_eq!(shape(css), ["d:--x", "d:--y", "d:color"]);
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn a_top_level_custom_property_lookalike_is_dropped_and_reported() {
        let css = "--foo:hover { color: blue } p { color: red }";
        let (sheet, errs) = parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].span.slice(css), "p { color: red }");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::DroppedCustomPropertyRule);
        assert_eq!(errs[0].span.slice(css), "--foo:hover { color: blue }");
    }

    #[test]
    fn a_nested_at_rule_keeps_its_name_span_and_ends_at_the_brace() {
        let css = "p { color: red; @media print { color: blue } @foo bar }";
        assert_eq!(shape(css), ["d:color", "@media", "@foo"]);
        let it = items(css);
        let BlockItem::Rule(r) = &it[2] else { panic!() };
        assert_eq!(r.span.slice(css), "@foo bar");
        let Rule::At(a) = &r.node else { panic!() };
        assert_eq!(a.name_span.slice(css), "@foo");
        assert!(kinds(css).is_empty());
    }

    #[test]
    fn statement_at_rules_in_a_block() {
        let css = "@media print { @import url(x.css); p { color: red } }";
        assert_eq!(shape(css), ["@import", "r:p"]);
        assert!(kinds(css).is_empty());
    }

    // --- errors ---

    #[test]
    fn clean_css_has_no_syntax_errors() {
        assert!(syntax_errors("a { color: red; background: url(x.png) }").is_empty());
    }

    #[test]
    fn bad_tokens_are_reported_once() {
        // Once each, however many readings the item they sit in goes through.
        let css = "a { content: \"oops\n; background: url(a b); x \"y\n }";
        assert_eq!(
            kinds(css),
            [
                SyntaxErrorKind::BadString,
                SyntaxErrorKind::BadUrl,
                SyntaxErrorKind::MalformedDeclaration,
                SyntaxErrorKind::BadString
            ]
        );
        // A bad token leading a malformed item is not also an unexpected
        // token.
        assert_eq!(kinds("a { url(a b) }"), [SyntaxErrorKind::BadUrl]);
    }

    #[test]
    fn unterminated_rule_is_reported() {
        assert_eq!(kinds("a b c"), [SyntaxErrorKind::UnterminatedRule]);
        assert_eq!(kinds("p { } a b"), [SyntaxErrorKind::UnterminatedRule]);
    }

    #[test]
    fn unterminated_block_is_reported_at_the_brace() {
        let css = "a { color: red";
        let errs = syntax_errors(css);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::UnterminatedBlock);
        assert_eq!(errs[0].span.slice(css), "{");
        let (sheet, _) = parse_stylesheet(css);
        assert_eq!(shape(css), ["d:color"]);
        assert_eq!(sheet.rules[0].span.slice(css), css);
    }

    #[test]
    fn an_item_that_is_neither_is_one_error() {
        for (body, kind) in [
            ("color red", SyntaxErrorKind::MalformedDeclaration),
            (
                "color red; width: 2px",
                SyntaxErrorKind::MalformedDeclaration,
            ),
            ("1px: red", SyntaxErrorKind::UnexpectedToken),
            ("1px: red; color: blue", SyntaxErrorKind::UnexpectedToken),
            ("# color: red", SyntaxErrorKind::UnexpectedToken),
            ("a b", SyntaxErrorKind::MalformedDeclaration),
        ] {
            let css = format!("p {{ {body} }}");
            assert_eq!(kinds(&css), [kind], "{body}");
        }
        assert_eq!(shape("p { color red; width: 2px }"), ["d:width"]);
        assert_eq!(shape("p { 1px: red; color: blue }"), ["d:color"]);
    }

    /// Before 0.12 `parse_rule_list` called this `UnterminatedRule`. A block
    /// is a block now, whoever holds it, and the item reads as in any other.
    #[test]
    fn a_trailing_fragment_in_a_block_is_a_malformed_item() {
        assert_eq!(
            kinds("@media print { p { color: red } i"),
            [
                SyntaxErrorKind::UnterminatedBlock,
                SyntaxErrorKind::MalformedDeclaration
            ]
        );
        assert!(kinds("@media print { p { color: red } ").len() == 1);
    }

    #[test]
    fn empty_items_are_not_errors() {
        assert!(kinds("p { ; ; color: red;; }").is_empty());
    }

    #[test]
    fn errors_are_sorted_by_position() {
        let css = ". a { x \"y\n } p { url(a b) } q { 1: 2 } , r { } s t";
        let errs = syntax_errors(css);
        assert!(errs.windows(2).all(|w| w[0].span.start <= w[1].span.start));
        assert_eq!(errs.len(), 7, "{errs:?}");
    }

    // --- parse_block_contents ---

    #[test]
    fn block_contents_of_a_style_attribute() {
        let src = "color: red; padding: 0 !important";
        let (items, errs) = parse_block_contents(src);
        assert!(errs.is_empty());
        assert_eq!(items.len(), 2);
        let BlockItem::Declaration(p) = &items[1] else {
            panic!()
        };
        assert_eq!(p.node.name_span.slice(src), "padding");
        assert!(p.node.important);
    }

    #[test]
    fn block_contents_report_malformed_items() {
        let (items, errs) = parse_block_contents("color red; width: 2px");
        assert_eq!(items.len(), 1);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::MalformedDeclaration);
    }

    #[test]
    fn a_stray_closing_brace_ends_block_contents() {
        let src = "color: red } width: 2px";
        let (items, errs) = parse_block_contents(src);
        assert_eq!(items.len(), 1);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::UnexpectedToken);
        assert_eq!(errs[0].span.slice(src), "}");
    }

    // --- unicode-range ---

    #[test]
    fn a_unicode_range_value_is_read_as_ranges() {
        let src = "@font-face { unicode-range: U+0-7F, u+4?? !important; }";
        let BlockItem::Declaration(d) = &items(src)[0] else {
            panic!()
        };
        let ranges: Vec<_> = d
            .node
            .value
            .iter()
            .filter_map(|v| match v.node {
                ComponentValue::Token(Token::UnicodeRange { start, end }) => {
                    Some((start, end, v.span.slice(src)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(ranges, [(0, 0x7F, "U+0-7F"), (0x400, 0x4FF, "u+4??")]);
        assert!(d.node.important);
        // Only there: elsewhere `u+a` is a selector.
        assert!(kinds("u+a { color: red }").is_empty());
        assert!(syntax_errors(src).is_empty());
    }

    #[test]
    fn real_unicode_ranges_are_accepted() {
        for css in [
            "@font-face { unicode-range: U+0-7F; }",
            "@font-face { unicode-range: U+0025-00FF; }",
            "@font-face { unicode-range: U+4??; }",
            "@font-face { unicode-range: U+0-10FFFF; }",
            "@font-face { unicode-range: U+10FFFF; }",
            "@font-face { unicode-range: U+0-7F, U+80-FF; }",
            "@font-face { unicode-range: u+26; }",
            "@font-face { unicode-range: U+??????; }",
        ] {
            assert!(kinds(css).is_empty(), "must be accepted: {css}");
        }
    }

    #[test]
    fn over_long_unicode_range_runs_are_reported() {
        let n = |css: &str| {
            kinds(css)
                .into_iter()
                .filter(|k| *k == SyntaxErrorKind::InvalidUnicodeRange)
                .count()
        };
        assert_eq!(n("@font-face { unicode-range: U+0000000; }"), 1);
        assert_eq!(n("@font-face { unicode-range: U+0-00000000; }"), 1);
        // A `U+` inside a string or a comment is not a range.
        assert_eq!(n("a { content: \"U+00000000\"; }"), 0);
        assert_eq!(n("/* U+00000000 */ a { color: red }"), 0);
    }

    #[test]
    fn a_vendor_prefix_is_stripped_and_a_custom_name_is_not() {
        assert_eq!(unprefixed("-webkit-keyframes"), "keyframes");
        assert_eq!(unprefixed("-moz-document"), "document");
        assert_eq!(unprefixed("-ms-viewport"), "viewport");
        assert_eq!(unprefixed("keyframes"), "keyframes");
        assert_eq!(unprefixed("--custom"), "--custom");
        assert_eq!(unprefixed("-"), "-");
        assert_eq!(unprefixed("-9-keyframes"), "-9-keyframes");
    }
}

#[cfg(test)]
mod nesting_guard_tests {
    use super::*;

    /// The shapes that reach the recursion. Each aborted the process between
    /// 10k and 20k deep before the guard; the assertion that matters is that
    /// these return at all.
    fn shapes(n: usize) -> Vec<(&'static str, String)> {
        vec![
            (
                "paren",
                format!("a{{color:{}red{}}}", "(".repeat(n), ")".repeat(n)),
            ),
            (
                "curly",
                format!("{}a{{color:red}}{}", "@media all{".repeat(n), "}".repeat(n)),
            ),
            (
                "function",
                format!("a{{color:{}1{}}}", "rgb(".repeat(n), ")".repeat(n)),
            ),
            (
                "selector",
                format!("{}a{}{{color:red}}", ":is(".repeat(n), ")".repeat(n)),
            ),
            (
                "nested-style",
                format!("{}color:red{}", "a{".repeat(n), "}".repeat(n)),
            ),
            (
                "value-block",
                format!("a{{{}x{}}}", "b:{".repeat(n), "}".repeat(n)),
            ),
            ("unclosed", format!("a{{{}", "b{c:(".repeat(n))),
        ]
    }

    /// A stack overflow is `SIGABRT`, not a catchable panic - a regression
    /// kills the test runner rather than failing an assert, so this test
    /// completing *is* the assertion.
    #[test]
    fn pathological_nesting_does_not_abort_the_process() {
        for (name, css) in shapes(100_000) {
            let (sheet, _) = parse_stylesheet(&css);
            assert!(!sheet.rules.is_empty(), "{name}: expected a rule back");
            drop(sheet);
            let _ = crate::validate_stylesheet(&css);
            let _ = crate::serialize_stylesheet(&parse_stylesheet(&css).0);
        }
    }

    /// The refusal is reported rather than silently truncating - a caller
    /// that cannot see the limit was hit would have no way to tell a refused
    /// stylesheet from a shallow one.
    #[test]
    fn the_refusal_is_reported() {
        for (name, css) in shapes(MAX_NESTING_DEPTH + 5) {
            let errs = syntax_errors(&css);
            assert_eq!(
                errs.iter()
                    .filter(|e| e.kind == SyntaxErrorKind::NestingTooDeep)
                    .count(),
                1,
                "{name}: expected one NestingTooDeep, got {errs:?}"
            );
        }
    }

    /// The false-positive direction: real stylesheets nest 2 deep (median
    /// and p95 across a 65-book shelf), so ordinary CSS must stay silent.
    #[test]
    fn real_world_css_is_untouched() {
        let css = "@media screen and (min-width: 40em) { \
                   body { color: rgb(1, 2, 3); font: bold 12px/1.4 serif; } \
                   a[href^=\"http\"]:not(.x) { margin: calc(1px + (2px * 3)); } }";
        let (sheet, errs) = parse_stylesheet(css);
        assert!(errs.is_empty(), "ordinary CSS must not report: {errs:?}");
        assert_eq!(sheet.rules.len(), 1);
    }

    /// Exactly at the limit still parses; one past it is refused. Without
    /// this the guard could drift to an off-by-one and silently start
    /// discarding one level of legitimate nesting.
    #[test]
    fn boundary_is_exact() {
        let deep = |n: usize| format!("a{{color:{}red{}}}", "(".repeat(n), ")".repeat(n));
        let refused = |css: &str| {
            syntax_errors(css)
                .iter()
                .any(|e| e.kind == SyntaxErrorKind::NestingTooDeep)
        };
        assert!(
            !refused(&deep(MAX_NESTING_DEPTH)),
            "exactly at the limit must still parse"
        );
        assert!(
            refused(&deep(MAX_NESTING_DEPTH + 1)),
            "one past the limit must be refused"
        );
        // Nested rules count the same way as brackets.
        let rules = |n: usize| format!("{}x:y{}", "a{".repeat(n + 1), "}".repeat(n + 1));
        assert!(!refused(&rules(MAX_NESTING_DEPTH)));
        assert!(refused(&rules(MAX_NESTING_DEPTH + 1)));
    }

    /// The skip must stay balanced, or everything after a refused block
    /// would be misparsed - turning a bounded refusal into a corrupted parse
    /// of the rest of the stylesheet.
    #[test]
    fn parsing_resumes_after_a_refused_block() {
        for css in [
            format!(
                "a{{color:{}red{}}} b{{color:blue}}",
                "(".repeat(MAX_NESTING_DEPTH + 5),
                ")".repeat(MAX_NESTING_DEPTH + 5)
            ),
            format!(
                "{}x:y{} b{{color:blue}}",
                "a{".repeat(MAX_NESTING_DEPTH + 5),
                "}".repeat(MAX_NESTING_DEPTH + 5)
            ),
        ] {
            let (sheet, _) = parse_stylesheet(&css);
            assert_eq!(
                sheet.rules.len(),
                2,
                "the rule after the refused one must still parse"
            );
        }
    }
}
