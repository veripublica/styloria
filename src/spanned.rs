//! Span-carrying parse tree — a mirror of [`crate::parser`]'s output where
//! every node (and every nested value) also carries the source [`Span`] it
//! was parsed from.
//!
//! This is **additive**: the existing position-less [`Parser`](crate::Parser)
//! and its types are untouched. Reach for this module when a consumer needs
//! to report the exact `line:column` of something it found in the CSS (its
//! reason for existing — see `SPAN_PROTOTYPE.md`); reach for the plain
//! parser when positions don't matter.
//!
//! The node types here deliberately share their names with the plain
//! parser's ([`Rule`], [`ComponentValue`], …) and are meant to be used
//! module-qualified (`styloria::spanned::Rule`). Each list of children is a
//! `Vec<Spanned<…>>`; a block's span covers its brackets, a rule's span
//! covers its whole text, and a single-token value's span is the token's.
//!
//! # Reading a whole stylesheet
//!
//! Parsing stops at each `{ … }`: CSS Syntax Level 3 §5.4.2 leaves a block's
//! contents uninterpreted, because what they mean depends on the construct
//! that holds them. So every entry point here reports about the level it was
//! asked to interpret and says nothing about the blocks below it, and a
//! caller that wants the whole stylesheet descends one level per call:
//!
//! | you hold | ask for |
//! |---|---|
//! | source text | [`parse_stylesheet_with_errors`] |
//! | a style rule's block | [`parse_declaration_list_from_values`] |
//! | an at-rule's block | [`parse_at_rule_block`] (it knows which at-rules hold what) |
//! | a `style="…"` value | [`parse_declaration_list_with_errors`] |
//!
//! **When** to descend stays the caller's decision; **what** a block holds is
//! answered here, because that is a fact about CSS rather than about the
//! caller (issue #4).
//!
//! ```
//! let sheet = styloria::spanned::parse_stylesheet("body {\n  color: red;\n}");
//! let rule = &sheet.rules[0];
//! assert_eq!(rule.span.start_line_col("body {\n  color: red;\n}"), (1, 1));
//! ```

use std::borrow::Cow;
use std::iter::Peekable;

use crate::parser::BlockKind;
use crate::span::{Span, Spanned};
use crate::token::Token;
use crate::tokenizer::{SpannedTokens, Tokenizer};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    /// The source span of the malformed construct.
    pub span: Span,
    pub kind: SyntaxErrorKind,
}

/// What a [`SyntaxError`] reports. All are conditions the parser recovers
/// from at a specific point in the CSS Syntax algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxErrorKind {
    /// A `<bad-string-token>`: a string with an unescaped newline.
    BadString,
    /// A `<bad-url-token>`: a malformed unquoted `url( … )`.
    BadUrl,
    /// A declaration whose name was not followed by `:` (§5.4.5) — discarded.
    ///
    /// Only ever comes from reading something *as a declaration list* — a
    /// `style="…"` attribute, or a block handed to
    /// [`parse_declaration_list_from_values`] or [`parse_at_rule_block`].
    /// [`parse_stylesheet_with_errors`] does not descend into blocks, so it
    /// never produces this.
    MalformedDeclaration,
    /// A qualified rule whose prelude reached EOF before its `{ … }` block.
    UnterminatedRule,
    /// A `{`, `[`, or `(` block that reached EOF before its closing bracket.
    UnterminatedBlock,
    /// A token where a declaration or at-rule was expected (§5.4.2) —
    /// discarded. Entry-point-dependent in the same way as
    /// [`MalformedDeclaration`](Self::MalformedDeclaration).
    UnexpectedToken,
    /// A `U+…` unicode-range with more than six hex digits in a run — more
    /// than any code point needs, so malformed under any reading.
    InvalidUnicodeRange,
    /// A qualified rule whose prelude is not a valid selector list
    /// (Selectors Level 4 §3). See [`crate::selector`] for what is and isn't
    /// reported — the check is syntactic and deliberately permissive.
    InvalidSelector,
    /// Block/function nesting past
    /// [`MAX_NESTING_DEPTH`](crate::parser::MAX_NESTING_DEPTH). The contents
    /// below that point are discarded rather than parsed, so this is
    /// reported once, at the outermost bracket that was refused.
    ///
    /// Unlike every other variant here this is not a defect in the CSS as
    /// such — it is the parser declining to recurse further, because the
    /// alternative is a stack overflow, which in Rust aborts the process
    /// rather than raising a catchable error. Real stylesheets nest 2 deep;
    /// anything reaching 256 is machine-generated or hostile.
    NestingTooDeep,
}

/// A component value, with spans on itself and every nested value. Mirrors
/// [`crate::ComponentValue`].
#[derive(Debug, Clone, PartialEq)]
pub enum ComponentValue<'a> {
    Token(Token<'a>),
    Function {
        name: Cow<'a, str>,
        args: Vec<Spanned<ComponentValue<'a>>>,
    },
    Block(SimpleBlock<'a>),
}

/// A `{}`/`[]`/`()` block whose contents each carry a span. Mirrors
/// [`crate::SimpleBlock`].
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleBlock<'a> {
    pub kind: BlockKind,
    pub values: Vec<Spanned<ComponentValue<'a>>>,
}

/// A `selector { … }` rule. Mirrors [`crate::QualifiedRule`].
#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedRule<'a> {
    pub prelude: Vec<Spanned<ComponentValue<'a>>>,
    pub block: Spanned<SimpleBlock<'a>>,
}

/// An `@name … { … }` (or `@name …;`) rule. Mirrors [`crate::AtRule`].
#[derive(Debug, Clone, PartialEq)]
pub struct AtRule<'a> {
    pub name: Cow<'a, str>,
    /// Span of just the `@name` keyword token.
    pub name_span: Span,
    pub prelude: Vec<Spanned<ComponentValue<'a>>>,
    pub block: Option<Spanned<SimpleBlock<'a>>>,
}

/// A top-level rule. Mirrors [`crate::Rule`].
#[derive(Debug, Clone, PartialEq)]
pub enum Rule<'a> {
    Qualified(QualifiedRule<'a>),
    At(AtRule<'a>),
}

/// A `property: value` declaration. Mirrors [`crate::Declaration`], plus a
/// `name_span` for just the property name (what a validator flagging a
/// property wants to point at).
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration<'a> {
    pub name: Cow<'a, str>,
    pub name_span: Span,
    pub value: Vec<Spanned<ComponentValue<'a>>>,
    pub important: bool,
}

/// One entry of a declaration list — a declaration, or a nested at-rule
/// (e.g. inside `@page`). Mirrors [`crate::DeclarationListItem`].
#[derive(Debug, Clone, PartialEq)]
pub enum DeclarationListItem<'a> {
    Declaration(Spanned<Declaration<'a>>),
    AtRule(Spanned<AtRule<'a>>),
}

/// A parsed stylesheet: its top-level rules, each with a span.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stylesheet<'a> {
    pub rules: Vec<Spanned<Rule<'a>>>,
}

/// Parse a stylesheet into the span-carrying tree (CSS Syntax Level 3
/// §5.3.3, same grammar as [`crate::Parser::parse_stylesheet`]).
pub fn parse_stylesheet(input: &str) -> Stylesheet<'_> {
    parse_stylesheet_with_errors(input).0
}

/// Parse a stylesheet and also return the [`SyntaxError`]s the parser
/// recovered from *at the top level*, in source order. The tree is identical
/// to [`parse_stylesheet`]'s; this variant also hands back what was
/// discarded, for a tool that wants to report malformed CSS.
///
/// **It reports about rules, not about what is inside them.** A `{ … }`
/// block comes back as raw component values, because CSS Syntax Level 3
/// §5.4.2 leaves a block's meaning to whoever knows what kind of block it is
/// — so `a { color red }` is clean here, and the malformed declaration
/// inside it is found by reading the block:
/// [`parse_at_rule_block`] for an at-rule, or
/// [`parse_declaration_list_from_values`] for a style rule's. Each entry
/// point reports about the thing it was asked to interpret and stays silent
/// about the blocks below it; a caller wanting the whole stylesheet
/// descends, one level per call.
pub fn parse_stylesheet_with_errors(input: &str) -> (Stylesheet<'_>, Vec<SyntaxError>) {
    let mut p = SpannedParser {
        tokens: Tokenizer::new(input).spanned().peekable(),
        errors: Vec::new(),
        depth: 0,
    };
    let rules = p.consume_rules_list(true);
    let mut errors = p.errors;
    errors.extend(unicode_range_errors(input));
    errors.sort_by_key(|e| e.span.start);
    (Stylesheet { rules }, errors)
}

/// The [`SyntaxError`]s in a stylesheet, in source order — a convenience over
/// [`parse_stylesheet_with_errors`] for callers that only want the errors.
pub fn syntax_errors(input: &str) -> Vec<SyntaxError> {
    parse_stylesheet_with_errors(input).1
}

/// Parse a list of declarations into the span-carrying tree — the spanned
/// mirror of [`crate::Parser::parse_declaration_list`]. Use this for a
/// `style="…"` attribute's value, or an `@font-face` / `@page` body.
pub fn parse_declaration_list(input: &str) -> Vec<DeclarationListItem<'_>> {
    parse_declaration_list_with_errors(input).0
}

/// [`parse_declaration_list`] plus the [`SyntaxError`]s recovered from the
/// declaration list (a `style="…"` attribute, or an at-rule body).
pub fn parse_declaration_list_with_errors(
    input: &str,
) -> (Vec<DeclarationListItem<'_>>, Vec<SyntaxError>) {
    let mut p = SpannedParser {
        tokens: Tokenizer::new(input).spanned().peekable(),
        errors: Vec::new(),
        depth: 0,
    };
    let items = p.consume_declaration_list();
    let mut errors = p.errors;
    errors.extend(unicode_range_errors(input));
    errors.sort_by_key(|e| e.span.start);
    (items, errors)
}

/// Read already-parsed component values as a **declaration list**, returning
/// the declarations and the [`SyntaxError`]s found in them.
///
/// The twin of [`parse_rule_list`], and for the same reason: a `{ … }` block
/// holds either rules or declarations, CSS Syntax Level 3 does not say which,
/// and the caller is the one who knows. What the caller should not have to
/// reimplement is where each declaration ends — that is parsing, and doing it
/// downstream is how "is this a well-formed declaration" ended up living
/// outside this crate (issue #4).
///
/// The input is component values rather than source text for the same reason
/// as [`parse_rule_list`]: **spans stay absolute**, so a caller can report a
/// line and column in the original stylesheet.
///
/// One error per declaration, not per token: a chunk that cannot be a
/// declaration is reported once and discarded to the next `;`, matching
/// §5.4.2 and the token-based [`parse_declaration_list_with_errors`].
///
/// An empty chunk (`{;}`, `a;;b`) is **not** an error — §5.4.4 discards a
/// stray `<semicolon-token>`, so it is valid CSS and this stays silent about
/// it. epubcheck's older parser disagrees; that is a parity decision for a
/// consumer to document, not a spec question for this crate.
pub fn parse_declaration_list_from_values<'a>(
    values: &[Spanned<ComponentValue<'a>>],
) -> (Vec<DeclarationListItem<'a>>, Vec<SyntaxError>) {
    declaration_list_from_values(values, NestedRules::Rejected)
}

/// Whether a chunk shaped like a nested rule (`… { … }`) is a parse error.
///
/// It is, in the block of a *style rule*: that block holds declarations and
/// nothing else, which is what [`parse_declaration_list_from_values`]
/// exposes. It is not, in the block of an at-rule this crate has no table
/// entry for — there the contents are genuinely unknown, so a nested rule is
/// skipped rather than blamed. See [`parse_at_rule_block`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedRules {
    Rejected,
    Ignored,
}

fn declaration_list_from_values<'a>(
    values: &[Spanned<ComponentValue<'a>>],
    nested: NestedRules,
) -> (Vec<DeclarationListItem<'a>>, Vec<SyntaxError>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for chunk in values.split(|v| matches!(&v.node, ComponentValue::Token(Token::Semicolon))) {
        let mut it = chunk
            .iter()
            .filter(|v| !matches!(&v.node, ComponentValue::Token(Token::Whitespace)));
        let Some(first) = it.next() else {
            continue; // empty chunk: a stray `;`, discarded by §5.4.4
        };
        match &first.node {
            ComponentValue::Token(Token::Ident(name)) => {
                if matches!(
                    it.next().map(|v| &v.node),
                    Some(ComponentValue::Token(Token::Colon))
                ) {
                    let value: Vec<Spanned<ComponentValue<'a>>> = it.cloned().collect();
                    let important = value.iter().rev().any(|v| {
                        matches!(&v.node, ComponentValue::Token(Token::Ident(i))
                            if i.eq_ignore_ascii_case("important"))
                    }) && value
                        .iter()
                        .rev()
                        .any(|v| matches!(&v.node, ComponentValue::Token(Token::Delim('!'))));
                    let span = first.span.to(chunk.last().unwrap_or(first).span);
                    items.push(DeclarationListItem::Declaration(Spanned::new(
                        Declaration {
                            name: name.clone(),
                            name_span: first.span,
                            value,
                            important,
                        },
                        span,
                    )));
                } else if !(nested == NestedRules::Ignored && ends_in_curly_block(chunk)) {
                    errors.push(SyntaxError {
                        span: first.span,
                        kind: SyntaxErrorKind::MalformedDeclaration,
                    });
                }
            }
            // An at-rule inside a declaration list (`@page { @top-center {…} }`)
            // is handed back unexamined, exactly as `parse_rule_list` does with
            // a nested at-rule: whether its body is declarations or rules is
            // again the caller's question.
            ComponentValue::Token(Token::AtKeyword(_)) => {}
            _ if nested == NestedRules::Ignored && ends_in_curly_block(chunk) => {}
            _ => {
                errors.push(SyntaxError {
                    span: first.span,
                    kind: SyntaxErrorKind::UnexpectedToken,
                });
            }
        }
    }
    (items, errors)
}

/// Whether a declaration-list chunk is shaped like a rule rather than a
/// declaration: its last meaningful component value is a `{ … }` block.
///
/// Asked only *after* reading it as a declaration has failed, so a value that
/// legitimately ends in a block cannot be mistaken for a rule.
fn ends_in_curly_block(chunk: &[Spanned<ComponentValue<'_>>]) -> bool {
    chunk
        .iter()
        .rev()
        .find(|v| !matches!(&v.node, ComponentValue::Token(Token::Whitespace)))
        .is_some_and(|v| matches!(&v.node, ComponentValue::Block(b) if b.kind == BlockKind::Curly))
}

/// Read already-parsed component values as a **rule list**, returning the
/// rules and the [`SyntaxError`]s found in them.
///
/// This is for the contents of a conditional-group at-rule — the body of an
/// `@media`, `@supports`, `@container`, `@layer` — which holds *rules* where
/// an `@font-face` or `@page` body holds *declarations*. CSS Syntax Level 3
/// does not say which is which: §5.4.2 hands an at-rule's block on as a
/// simple block and leaves its interpretation to that at-rule's own
/// specification. This crate deliberately carries no such per-at-rule
/// knowledge, so the caller decides *when* to call this; what the caller
/// should not have to reimplement is where each nested rule's prelude ends,
/// which is parsing.
///
/// The input is component values rather than source text so that **spans
/// stay absolute** — errors point into the original stylesheet, not into a
/// re-tokenized fragment.
///
/// Preludes go through [`crate::validate_selector_list`], exactly as they do
/// for a top-level rule. Without this, a malformed selector was reported at
/// the top level and silently accepted one `@media` deep, because nothing
/// inside a simple block was ever re-entered as a rule (issue #2).
///
/// Nested at-rules are returned with their block unexamined, the same as at
/// the top level: whether *their* body is a rule list is again the caller's
/// question, so a caller walking `@media` inside `@media` recurses itself.
pub fn parse_rule_list<'a>(
    values: &[Spanned<ComponentValue<'a>>],
) -> (Vec<Spanned<Rule<'a>>>, Vec<SyntaxError>) {
    rule_list(values, Preludes::Selectors)
}

/// What a nested rule's prelude is, and so whether it is checked as a
/// selector list.
///
/// In a conditional-group at-rule the children are style rules, so their
/// preludes are selectors. In `@keyframes` they are *keyframe selectors* —
/// `from`, `to`, `0%` — a different grammar entirely (CSS Animations 1 §3),
/// under which `0%` is correct and would be a malformed selector. Reading
/// one as the other invents an error on valid CSS, which is the whole reason
/// this distinction is in the crate rather than in a caller's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preludes {
    Selectors,
    /// Handed back unexamined. This crate carries no keyframe-selector
    /// grammar; nothing needs it yet, and inventing one would be a
    /// restrictive check nobody asked for.
    Opaque,
}

fn rule_list<'a>(
    values: &[Spanned<ComponentValue<'a>>],
    preludes: Preludes,
) -> (Vec<Spanned<Rule<'a>>>, Vec<SyntaxError>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    let mut prelude: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
    let mut at: Option<(Cow<'a, str>, Span)> = None;

    for v in values {
        match &v.node {
            // Whitespace between rules is not part of the next prelude, and
            // leading whitespace would otherwise become the reported span.
            ComponentValue::Token(Token::Whitespace) if prelude.is_empty() && at.is_none() => {}
            // `@name …;` — an at-rule with no block ends at the semicolon.
            ComponentValue::Token(Token::Semicolon) if at.is_some() => {
                let (name, name_span) = at.take().expect("semicolon arm requires an at-rule");
                let span = name_span.to(v.span);
                rules.push(Spanned::new(
                    Rule::At(AtRule {
                        name,
                        name_span,
                        prelude: std::mem::take(&mut prelude),
                        block: None,
                    }),
                    span,
                ));
            }
            ComponentValue::Token(Token::AtKeyword(n)) if at.is_none() && prelude.is_empty() => {
                at = Some((n.clone(), v.span));
            }
            ComponentValue::Block(b) if b.kind == BlockKind::Curly => {
                let block = Spanned::new(b.clone(), v.span);
                match at.take() {
                    Some((name, name_span)) => {
                        let span = name_span.to(v.span);
                        rules.push(Spanned::new(
                            Rule::At(AtRule {
                                name,
                                name_span,
                                prelude: std::mem::take(&mut prelude),
                                block: Some(block),
                            }),
                            span,
                        ));
                    }
                    None => {
                        let prelude = std::mem::take(&mut prelude);
                        if preludes == Preludes::Selectors {
                            errors.extend(crate::selector::validate_selector_list(&prelude));
                        }
                        let span = prelude.first().map_or(v.span, |p| p.span).to(v.span);
                        rules.push(Spanned::new(
                            Rule::Qualified(QualifiedRule { prelude, block }),
                            span,
                        ));
                    }
                }
            }
            _ => prelude.push(v.clone()),
        }
    }

    // A prelude that never met its block: the same §5.4.4 case
    // `consume_qualified_rule` reports at the top level. Trailing whitespace
    // alone is not a rule.
    let trailing_content = prelude
        .iter()
        .any(|p| !matches!(&p.node, ComponentValue::Token(Token::Whitespace)));
    if trailing_content || at.is_some() {
        let span = at
            .map(|(_, s)| s)
            .or_else(|| prelude.first().map(|p| p.span));
        if let Some(span) = span {
            errors.push(SyntaxError {
                span,
                kind: SyntaxErrorKind::UnterminatedRule,
            });
        }
    }

    errors.sort_by_key(|e| e.span.start);
    (rules, errors)
}

/// What an at-rule's `{ … }` block was read as, by
/// [`parse_at_rule_block`].
#[derive(Debug, Clone, PartialEq)]
pub enum BlockContents<'a> {
    /// Declarations, and any nested at-rule, unexamined.
    Declarations(Vec<DeclarationListItem<'a>>),
    /// Nested rules — the block of a conditional-group at-rule, or of
    /// `@keyframes`.
    Rules(Vec<Spanned<Rule<'a>>>),
}

/// At-rules whose block holds *rules whose preludes are selectors*: the
/// conditional-group rules of CSS Conditional 3 §3, plus `@scope` and
/// `@starting-style`, whose children are likewise style rules.
///
/// A vendor prefix is stripped before the lookup, which is what carries
/// `@-moz-document`.
const RULE_BLOCKS: &[&str] = &[
    "media",
    "supports",
    "container",
    "layer",
    "scope",
    "document",
    "starting-style",
];

/// At-rules whose block holds rules whose preludes are *not* selectors.
/// `@-webkit-keyframes` and `@-moz-keyframes` arrive here prefix-stripped.
const KEYFRAME_BLOCKS: &[&str] = &["keyframes"];

/// Read an at-rule's `{ … }` block as whatever that at-rule holds.
///
/// A block holds either rules or declarations; CSS Syntax Level 3 §5.4.2
/// deliberately does not say which, and defers to each at-rule's own
/// specification. That deferral has to end somewhere, and this is the right
/// place for it to end: *which* at-rule holds what is a fact about CSS, so a
/// consumer that keeps its own table has taken on a copy of this crate's
/// subject matter — and will keep a stale one, because CSS keeps growing.
///
/// This does not take the *other* decision back. When to descend into a
/// block is still entirely the caller's: nothing here is reached from
/// [`parse_stylesheet_with_errors`], which reports about rules and stays
/// silent about what is inside them. The caller decides **when**; this
/// answers **what**.
///
/// An at-rule with no table entry — unregistered, experimental, or simply
/// newer than this crate — is read as declarations, and a chunk shaped like
/// a nested rule inside it is skipped in silence rather than reported. That
/// is the safe direction for the one case that is certain to recur: CSS
/// gains an at-rule, this table has not heard of it, and a validator built
/// on it must not start inventing errors on valid stylesheets. A malformed
/// *declaration* in such a block is still reported, since that is malformed
/// under any reading of the block.
///
/// `name` is the at-rule's name without the `@`, matched case-insensitively
/// and with a leading `-vendor-` prefix removed.
pub fn parse_at_rule_block<'a>(
    name: &str,
    values: &[Spanned<ComponentValue<'a>>],
) -> (BlockContents<'a>, Vec<SyntaxError>) {
    let bare = unprefixed(name);
    if RULE_BLOCKS.iter().any(|r| bare.eq_ignore_ascii_case(r)) {
        let (rules, errors) = rule_list(values, Preludes::Selectors);
        (BlockContents::Rules(rules), errors)
    } else if KEYFRAME_BLOCKS.iter().any(|r| bare.eq_ignore_ascii_case(r)) {
        let (rules, errors) = rule_list(values, Preludes::Opaque);
        (BlockContents::Rules(rules), errors)
    } else {
        let (items, errors) = declaration_list_from_values(values, NestedRules::Ignored);
        (BlockContents::Declarations(items), errors)
    }
}

/// Strip a leading vendor prefix (`-webkit-`, `-moz-`, `-ms-`, `-o-`, …):
/// a `-`, a run of letters, a `-`. A name that is not prefixed comes back
/// unchanged, including a custom `--name`, whose second character is `-`
/// rather than a letter.
fn unprefixed(name: &str) -> &str {
    let Some(rest) = name.strip_prefix('-') else {
        return name;
    };
    match rest.find('-') {
        Some(i) if i > 0 && rest[..i].chars().all(|c| c.is_ascii_alphabetic()) => &rest[i + 1..],
        _ => name,
    }
}

struct SpannedParser<'a> {
    tokens: Peekable<SpannedTokens<'a>>,
    errors: Vec<SyntaxError>,
    /// Current block/function nesting depth; see
    /// [`MAX_NESTING_DEPTH`](crate::parser::MAX_NESTING_DEPTH).
    depth: usize,
}

impl<'a> SpannedParser<'a> {
    fn next(&mut self) -> Option<Spanned<Token<'a>>> {
        self.tokens.next()
    }

    /// Report the refusal and consume tokens until the already-opened block
    /// is balanced again, without recursing. Returns the span of the token
    /// that closed it (or the last one seen at EOF) so the caller can still
    /// give the value a sensible extent.
    ///
    /// The error is pushed here rather than at each call site so the two
    /// entry points (a block, a function) cannot disagree about whether the
    /// refusal is reported — the silent-skip failure is the one a caller
    /// cannot notice.
    fn refuse_nesting(&mut self, open: Span) -> Span {
        self.errors.push(SyntaxError {
            span: open,
            kind: SyntaxErrorKind::NestingTooDeep,
        });
        let mut depth = 1usize;
        let mut end = open;
        while let Some(t) = self.next() {
            end = t.span;
            match t.node {
                Token::LeftCurly | Token::LeftSquare | Token::LeftParen | Token::Function(_) => {
                    depth += 1
                }
                Token::RightCurly | Token::RightSquare | Token::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        return end;
                    }
                }
                _ => {}
            }
        }
        end
    }
    fn peek_node(&mut self) -> Option<&Token<'a>> {
        self.tokens.peek().map(|s| &s.node)
    }
    fn skip_whitespace(&mut self) {
        while matches!(self.peek_node(), Some(Token::Whitespace)) {
            self.next();
        }
    }

    /// §5.4.2 "Consume a list of declarations", spanned.
    fn consume_declaration_list(&mut self) -> Vec<DeclarationListItem<'a>> {
        let mut items = Vec::new();
        loop {
            match self.peek_node() {
                None => break,
                Some(Token::Whitespace | Token::Semicolon) => {
                    self.next();
                }
                Some(Token::AtKeyword(_)) => {
                    items.push(DeclarationListItem::AtRule(self.consume_at_rule()));
                }
                Some(Token::Ident(_)) => {
                    if let Some(d) = self.consume_declaration() {
                        items.push(DeclarationListItem::Declaration(d));
                    } else {
                        // A malformed declaration (no `:`): §5.4.2 discards
                        // everything up to the next `;`/EOF, so its leftover
                        // tokens aren't re-parsed as another spurious
                        // declaration (which would also double-report the
                        // error). The item list is unchanged either way - a
                        // failed declaration yields no item.
                        while !matches!(self.peek_node(), None | Some(Token::Semicolon)) {
                            self.consume_component_value();
                        }
                    }
                }
                _ => {
                    // Parse error. §5.4.2: reconsume, then "as long as the
                    // next input token is anything other than a
                    // <semicolon-token> or <EOF-token>, consume a component
                    // value and throw away the returned value" - i.e. discard
                    // the whole malformed declaration, exactly as the ident
                    // branch above already does.
                    //
                    // This used to discard a single component value and loop,
                    // which reported one error per token rather than per
                    // declaration: `1px: red` gave UnexpectedToken twice and
                    // then MalformedDeclaration, three errors for one broken
                    // declaration. A consumer mapping every SyntaxErrorKind to
                    // one message id sees that as three findings (#4).
                    //
                    // If the first value was itself a bad token it already
                    // recorded its own, more specific error - don't
                    // double-report.
                    let before = self.errors.len();
                    let v = self.consume_component_value();
                    if self.errors.len() == before {
                        self.errors.push(SyntaxError {
                            span: v.span,
                            kind: SyntaxErrorKind::UnexpectedToken,
                        });
                    }
                    while !matches!(self.peek_node(), None | Some(Token::Semicolon)) {
                        self.consume_component_value();
                    }
                }
            }
        }
        items
    }

    /// §5.4.5 "Consume a declaration", spanned. Assumes the current token is
    /// the name ident; returns `None` if no `:` follows.
    fn consume_declaration(&mut self) -> Option<Spanned<Declaration<'a>>> {
        let name_tok = self.next().expect("consume_declaration requires an ident");
        let name = match name_tok.node {
            Token::Ident(n) => n,
            _ => unreachable!("consume_declaration requires an ident as the current token"),
        };
        let name_span = name_tok.span;
        self.skip_whitespace();
        if !matches!(self.peek_node(), Some(Token::Colon)) {
            self.errors.push(SyntaxError {
                span: name_span,
                kind: SyntaxErrorKind::MalformedDeclaration,
            });
            return None;
        }
        self.next();
        self.skip_whitespace();
        let mut value: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
        while !matches!(self.peek_node(), None | Some(Token::Semicolon)) {
            value.push(self.consume_component_value());
        }
        // The declaration's span covers everything it consumed, including a
        // trailing `!important` (stripped from `value` but part of the text).
        let end = value.last().map(|v| v.span).unwrap_or(name_span);
        let important = strip_trailing_important(&mut value);
        let node = Declaration {
            name,
            name_span,
            value,
            important,
        };
        Some(Spanned::new(node, name_span.to(end)))
    }

    fn consume_rules_list(&mut self, top_level: bool) -> Vec<Spanned<Rule<'a>>> {
        let mut rules = Vec::new();
        loop {
            match self.peek_node() {
                None => break,
                Some(Token::Whitespace) => {
                    self.next();
                }
                Some(Token::Cdo | Token::Cdc) => {
                    if top_level {
                        self.next();
                    } else if let Some(r) = self.consume_qualified_rule() {
                        rules.push(r.map(Rule::Qualified));
                    }
                }
                Some(Token::AtKeyword(_)) => {
                    let r = self.consume_at_rule();
                    rules.push(r.map(Rule::At));
                }
                _ => {
                    if let Some(r) = self.consume_qualified_rule() {
                        rules.push(r.map(Rule::Qualified));
                    }
                }
            }
        }
        rules
    }

    fn consume_qualified_rule(&mut self) -> Option<Spanned<QualifiedRule<'a>>> {
        let mut prelude: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
        let mut start: Option<Span> = None;
        loop {
            match self.peek_node() {
                None => {
                    // Prelude ran to EOF with no `{ … }` block: a qualified rule
                    // that never opened its block (§5.4.4 returns nothing).
                    if let Some(s) = start {
                        self.errors.push(SyntaxError {
                            span: s,
                            kind: SyntaxErrorKind::UnterminatedRule,
                        });
                    }
                    return None;
                }
                Some(Token::LeftCurly) => {
                    let open = self.next().unwrap().span;
                    let block = self.consume_simple_block(BlockKind::Curly, open);
                    let span = start.unwrap_or(block.span).to(block.span);
                    // The Syntax spec hands the prelude on unexamined; read
                    // it as a selector list so a malformed one is reported
                    // rather than silently accepted (`crate::selector`).
                    self.errors
                        .extend(crate::selector::validate_selector_list(&prelude));
                    return Some(Spanned::new(QualifiedRule { prelude, block }, span));
                }
                _ => {
                    let v = self.consume_component_value();
                    start.get_or_insert(v.span);
                    prelude.push(v);
                }
            }
        }
    }

    fn consume_at_rule(&mut self) -> Spanned<AtRule<'a>> {
        let at = self.next().expect("consume_at_rule requires an at-keyword");
        let name = match at.node {
            Token::AtKeyword(n) => n,
            _ => unreachable!("consume_at_rule requires an at-keyword as the current token"),
        };
        let name_span = at.span;
        let mut prelude: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
        let mut end = name_span;
        loop {
            match self.peek_node() {
                None => {
                    let node = AtRule {
                        name,
                        name_span,
                        prelude,
                        block: None,
                    };
                    return Spanned::new(node, name_span.to(end));
                }
                Some(Token::Semicolon) => {
                    let semi = self.next().unwrap().span;
                    let node = AtRule {
                        name,
                        name_span,
                        prelude,
                        block: None,
                    };
                    return Spanned::new(node, name_span.to(semi));
                }
                Some(Token::LeftCurly) => {
                    let open = self.next().unwrap().span;
                    let block = self.consume_simple_block(BlockKind::Curly, open);
                    let span = name_span.to(block.span);
                    let node = AtRule {
                        name,
                        name_span,
                        prelude,
                        block: Some(block),
                    };
                    return Spanned::new(node, span);
                }
                _ => {
                    let v = self.consume_component_value();
                    end = v.span;
                    prelude.push(v);
                }
            }
        }
    }

    /// The current token has already been confirmed not to be a block/list
    /// terminator or EOF by the caller (mirrors the plain parser's
    /// invariant for `consume_component_value`).
    fn consume_component_value(&mut self) -> Spanned<ComponentValue<'a>> {
        let st = self.next().expect("consume_component_value called at EOF");
        match st.node {
            Token::LeftCurly => self.finish_block_value(BlockKind::Curly, st.span),
            Token::LeftSquare => self.finish_block_value(BlockKind::Square, st.span),
            Token::LeftParen => self.finish_block_value(BlockKind::Paren, st.span),
            Token::Function(name) => {
                if self.depth >= crate::parser::MAX_NESTING_DEPTH {
                    let end = self.refuse_nesting(st.span);
                    return Spanned::new(
                        ComponentValue::Function {
                            name,
                            args: Vec::new(),
                        },
                        st.span.to(end),
                    );
                }
                self.depth += 1;
                let (args, end) = self.consume_function_args(st.span);
                self.depth -= 1;
                let span = st.span.to(end);
                Spanned::new(ComponentValue::Function { name, args }, span)
            }
            other => {
                // A bad-string / bad-url token is the tokenizer's signal that
                // it recovered from malformed input; record it where it sits.
                match other {
                    Token::BadString => self.errors.push(SyntaxError {
                        span: st.span,
                        kind: SyntaxErrorKind::BadString,
                    }),
                    Token::BadUrl => self.errors.push(SyntaxError {
                        span: st.span,
                        kind: SyntaxErrorKind::BadUrl,
                    }),
                    _ => {}
                }
                Spanned::new(ComponentValue::Token(other), st.span)
            }
        }
    }

    fn finish_block_value(&mut self, kind: BlockKind, open: Span) -> Spanned<ComponentValue<'a>> {
        if self.depth >= crate::parser::MAX_NESTING_DEPTH {
            let end = self.refuse_nesting(open);
            return Spanned::new(
                ComponentValue::Block(SimpleBlock {
                    kind,
                    values: Vec::new(),
                }),
                open.to(end),
            );
        }
        self.depth += 1;
        let block = self.consume_simple_block(kind, open);
        self.depth -= 1;
        Spanned::new(ComponentValue::Block(block.node), block.span)
    }

    fn consume_simple_block(&mut self, kind: BlockKind, open: Span) -> Spanned<SimpleBlock<'a>> {
        let close = match kind {
            BlockKind::Curly => Token::RightCurly,
            BlockKind::Square => Token::RightSquare,
            BlockKind::Paren => Token::RightParen,
        };
        let mut values: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
        let mut end = open;
        loop {
            match self.peek_node() {
                None => {
                    // Reached EOF before the closing bracket (§ "consume a
                    // simple block" stops at EOF, leaving the block unclosed).
                    self.errors.push(SyntaxError {
                        span: open,
                        kind: SyntaxErrorKind::UnterminatedBlock,
                    });
                    break;
                }
                Some(t) if *t == close => {
                    end = self.next().unwrap().span;
                    break;
                }
                _ => {
                    let v = self.consume_component_value();
                    end = v.span;
                    values.push(v);
                }
            }
        }
        Spanned::new(SimpleBlock { kind, values }, open.to(end))
    }

    fn consume_function_args(
        &mut self,
        fn_token: Span,
    ) -> (Vec<Spanned<ComponentValue<'a>>>, Span) {
        let mut args: Vec<Spanned<ComponentValue<'a>>> = Vec::new();
        let mut end = fn_token;
        loop {
            match self.peek_node() {
                None => return (args, end),
                Some(Token::RightParen) => {
                    end = self.next().unwrap().span;
                    return (args, end);
                }
                _ => {
                    let v = self.consume_component_value();
                    end = v.span;
                    args.push(v);
                }
            }
        }
    }
}

/// Mirror of the plain parser's trailing-`!important` strip, over spanned
/// component values: if the last two non-whitespace values are `!` then an
/// `important` ident (case-insensitive), remove both and report it.
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
            value.remove(li);
            value.remove(si);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident_of<'a>(v: &'a Spanned<ComponentValue<'a>>) -> Option<&'a str> {
        match &v.node {
            ComponentValue::Token(Token::Ident(n)) => Some(n.as_ref()),
            _ => None,
        }
    }

    #[test]
    fn top_level_rule_span_covers_whole_rule() {
        let src = "body { color: red; }";
        let sheet = parse_stylesheet(src);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].span.slice(src), src);
    }

    #[test]
    fn declaration_property_inside_block_is_located() {
        // The end goal: a property token inside a rule block carries a span,
        // so a validator flagging e.g. `color` can report its line:column.
        let src = "a {\n  color: red;\n}";
        let sheet = parse_stylesheet(src);
        let Rule::Qualified(q) = &sheet.rules[0].node else {
            panic!("expected a qualified rule");
        };
        let color = q
            .block
            .node
            .values
            .iter()
            .find(|v| ident_of(v) == Some("color"))
            .expect("expected the `color` ident value");
        assert_eq!(color.span.slice(src), "color");
        assert_eq!(color.span.start_line_col(src), (2, 3));
    }

    #[test]
    fn nested_media_rule_selectors_are_located() {
        // A qualified rule nested inside `@media` gets its own span, and so
        // do the declarations inside it — exactly what epubveri needs to
        // give CSS findings a position (issue #5's stylesheets).
        let src = "@media screen {\n  div.box { padding: 0; }\n}";
        let sheet = parse_stylesheet(src);
        let Rule::At(at) = &sheet.rules[0].node else {
            panic!("expected an at-rule");
        };
        assert_eq!(at.name, "media");
        assert_eq!(at.name_span.slice(src), "@media");
        // The @media block's single value is the nested `div.box { … }`
        // block; dig into it and locate the `padding` property.
        let block = at.block.as_ref().expect("expected a block");
        let nested = block
            .node
            .values
            .iter()
            .find_map(|v| match &v.node {
                ComponentValue::Block(b) => Some(b),
                _ => None,
            })
            .expect("expected a nested rule block");
        let padding = nested
            .values
            .iter()
            .find(|v| ident_of(v) == Some("padding"))
            .expect("expected the `padding` ident");
        assert_eq!(padding.span.start_line_col(src), (2, 13));
    }

    #[test]
    fn declaration_list_locates_property_and_important() {
        // The `style="…"` / at-rule-body path: each declaration carries a
        // `name_span` pointing at just the property.
        let src = "color: red; padding: 0 !important";
        let items = parse_declaration_list(src);
        assert_eq!(items.len(), 2);
        let DeclarationListItem::Declaration(color) = &items[0] else {
            panic!("expected a declaration");
        };
        assert_eq!(color.node.name, "color");
        assert_eq!(color.node.name_span.slice(src), "color");
        assert!(!color.node.important);
        let DeclarationListItem::Declaration(padding) = &items[1] else {
            panic!("expected a declaration");
        };
        assert_eq!(padding.node.name, "padding");
        assert!(padding.node.important);
        // The declaration span reaches through `!important`.
        assert_eq!(padding.span.slice(src), "padding: 0 !important");
    }

    #[test]
    fn function_and_its_args_carry_spans() {
        // A real function token (`rgb(…)`); note `url(x.png)` in its bare
        // form is a single `Url` token, not a function, per the tokenizer.
        let src = "a { color: rgb(1, 2, 3) }";
        let sheet = parse_stylesheet(src);
        let Rule::Qualified(q) = &sheet.rules[0].node else {
            panic!("expected a qualified rule");
        };
        let func = q
            .block
            .node
            .values
            .iter()
            .find(|v| matches!(&v.node, ComponentValue::Function { name, .. } if name == "rgb"))
            .expect("expected the rgb() function");
        assert_eq!(func.span.slice(src), "rgb(1, 2, 3)");
    }

    // --- syntax-error recovery ---

    fn kinds(css: &str) -> Vec<SyntaxErrorKind> {
        syntax_errors(css).into_iter().map(|e| e.kind).collect()
    }

    #[test]
    fn clean_css_has_no_syntax_errors() {
        assert!(syntax_errors("a { color: red; background: url(x.png) }").is_empty());
    }

    #[test]
    fn bad_string_is_reported() {
        // An unterminated string (newline before the closing quote) is a
        // <bad-string-token>.
        let css = "a { content: \"oops\n }";
        assert!(kinds(css).contains(&SyntaxErrorKind::BadString));
    }

    #[test]
    fn bad_url_is_reported() {
        // An unquoted url() with an illegal character is a <bad-url-token>.
        let css = "a { background: url(foo'bar) }";
        assert!(kinds(css).contains(&SyntaxErrorKind::BadUrl));
    }

    #[test]
    fn unterminated_rule_is_reported() {
        // A prelude that reaches EOF with no `{ … }` block.
        let errs = syntax_errors("a b c");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::UnterminatedRule);
    }

    #[test]
    fn unterminated_block_is_reported() {
        // A `{` that reaches EOF before its `}`. The error points at the `{`.
        let css = "a { color: red";
        let errs = syntax_errors(css);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::UnterminatedBlock);
        assert_eq!(errs[0].span.slice(css), "{");
    }

    /// The two declaration-list entry points must not drift. One takes source
    /// text, the other already-parsed component values (so spans stay
    /// absolute); they share the same rules and are asserted to agree here,
    /// because the reason the values-based one exists is that a consumer was
    /// hand-rolling it and getting a different answer (#4).
    #[test]
    fn both_declaration_list_entry_points_agree() {
        for body in [
            "color: red",
            ";color: red",
            "color: red;;",
            "color red",
            "1px: red",
            "color: red; width: 2px",
            "color red; width: 2px",
            "1px: red; color: blue",
            "color: red !important",
            "",
        ] {
            let (a_items, a_errs) = parse_declaration_list_with_errors(body);
            let css = format!("a {{{body}}}");
            let sheet = parse_stylesheet(&css);
            let values = match &sheet.rules.first().expect("one rule").node {
                Rule::Qualified(q) => q.block.node.values.clone(),
                _ => panic!("expected a qualified rule"),
            };
            let (b_items, b_errs) = parse_declaration_list_from_values(&values);
            assert_eq!(
                a_items.len(),
                b_items.len(),
                "declaration count differs for {body:?}"
            );
            let a: Vec<_> = a_errs.iter().map(|e| e.kind).collect();
            let b: Vec<_> = b_errs.iter().map(|e| e.kind).collect();
            assert_eq!(a, b, "errors differ for {body:?}");
        }
    }

    /// §5.4.2 discards a malformed declaration up to the next `;`, so one
    /// broken declaration is one error - not one per token. `1px: red` used to
    /// give UnexpectedToken twice and then MalformedDeclaration, which a
    /// consumer mapping every kind to a single message id reports three times.
    #[test]
    fn a_malformed_declaration_is_one_error_not_one_per_token() {
        let (items, errs) = parse_declaration_list_with_errors("1px: red");
        assert_eq!(errs.len(), 1, "got {errs:?}");
        assert_eq!(items.len(), 0);
        // Recovery still resumes at the next declaration.
        let (items, errs) = parse_declaration_list_with_errors("1px: red; color: blue");
        assert_eq!(errs.len(), 1, "got {errs:?}");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn malformed_declaration_in_a_declaration_list() {
        // A name with no `:` in a declaration list (a style="…" attribute).
        let (_, errs) = parse_declaration_list_with_errors("color red; width: 2px");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, SyntaxErrorKind::MalformedDeclaration);
    }

    #[test]
    fn unexpected_token_in_a_declaration_list() {
        // A token where a declaration was expected, discarded (§5.4.2). Not
        // double-counted with any bad-token error.
        let (_, errs) = parse_declaration_list_with_errors("# color: red");
        assert_eq!(
            errs,
            vec![SyntaxError {
                span: errs[0].span,
                kind: SyntaxErrorKind::UnexpectedToken,
            }]
        );
    }
}

/// Malformed `U+…` unicode-ranges, as epubcheck's CSS scanner reports them
/// (`SCANNER_ILLEGAL_URANGE`).
///
/// Its rule is narrower than it sounds: it walks the characters after `U+`
/// that are hex digits, `?` or `-`, and errors when **seven** of them appear
/// without an intervening `-`. Nothing else is checked — not the ordering of
/// a range, not whether `?` only trails, not whether the endpoints make
/// sense. Six hex digits is the most a real code point needs (`U+10FFFF`),
/// so a run of seven is malformed under any reading, which is what makes this
/// safe to report.
///
/// Detection walks the **token stream**, not the raw text. `U+00000000`
/// inside a string or a comment is one `String` token or skipped entirely,
/// so it cannot be mistaken for a range — which scanning the source directly
/// would do.
fn unicode_range_errors(input: &str) -> Vec<SyntaxError> {
    const MAX_RUN: usize = 6;
    let mut out = Vec::new();
    for t in Tokenizer::new(input).spanned() {
        // Anchor on the `u` ident, then read the source after it. Anchoring
        // is what keeps a `U+…` inside a string or comment out of scope - it
        // is one String token, or skipped, and never an Ident here.
        //
        // Reading the *source* rather than the following tokens is
        // deliberate: `U+0-7F` tokenizes as Ident("U"), Number(+0), Delim,
        // Dimension… because CSS Syntax Level 3 dropped the unicode-range
        // token and `+0` is simply a number. The character run epubcheck
        // counts does not survive that, so count it where it still exists.
        let Token::Ident(name) = &t.node else {
            continue;
        };
        if !name.eq_ignore_ascii_case("u") {
            continue;
        }
        let after_ident = &input[t.span.end..];
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
                out.push(SyntaxError {
                    span: Span::new(t.span.start, t.span.end + 1 + i + c.len_utf8()),
                    kind: SyntaxErrorKind::InvalidUnicodeRange,
                });
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod unicode_range_tests {
    use super::{SyntaxErrorKind, syntax_errors};

    fn ranges(css: &str) -> usize {
        syntax_errors(css)
            .into_iter()
            .filter(|e| e.kind == SyntaxErrorKind::InvalidUnicodeRange)
            .count()
    }

    /// Six hex digits is the most any code point needs (`U+10FFFF`), so
    /// everything real must pass. The list is long on purpose: this check
    /// makes us *stricter*, and the cost of a wrong entry is a rejected
    /// stylesheet.
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
            assert_eq!(ranges(css), 0, "must be accepted: {css}");
        }
    }

    #[test]
    fn over_long_runs_are_reported() {
        assert_eq!(ranges("@font-face { unicode-range: U+0000000; }"), 1);
        assert_eq!(ranges("@font-face { unicode-range: U+0-00000000; }"), 1);
    }

    /// The detection walks tokens, not raw text, so a `U+` that is really
    /// part of a string or a comment cannot be mistaken for a range. Scanning
    /// the source directly would report both of these.
    #[test]
    fn a_u_plus_inside_a_string_or_comment_is_not_a_range() {
        assert_eq!(ranges("a { content: \"U+00000000\"; }"), 0);
        assert_eq!(ranges("/* U+00000000 */ a { color: red }"), 0);
    }
}

#[cfg(test)]
mod rule_list_tests {
    use super::*;

    /// The values of the first top-level at-rule's block — what a caller
    /// that knows `@media` holds rules would hand to `parse_rule_list`.
    fn body(css: &str) -> (Stylesheet<'_>, Vec<Spanned<ComponentValue<'_>>>) {
        let sheet = parse_stylesheet(css);
        let values = match &sheet.rules[0].node {
            Rule::At(a) => a
                .block
                .as_ref()
                .expect("at-rule has a block")
                .node
                .values
                .clone(),
            Rule::Qualified(_) => panic!("expected an at-rule"),
        };
        (sheet, values)
    }

    fn errors(css: &str) -> Vec<SyntaxErrorKind> {
        let (_sheet, values) = body(css);
        parse_rule_list(&values)
            .1
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    fn rules(css: &str) -> usize {
        let (_sheet, values) = body(css);
        parse_rule_list(&values).0.len()
    }

    /// The gap issue #2 was opened for: a malformed selector is reported at
    /// the top level and was silently accepted one `@media` deep.
    #[test]
    fn a_nested_prelude_is_validated_as_a_selector_list() {
        assert_eq!(
            errors("@media print { . foo { color: red } }"),
            vec![SyntaxErrorKind::InvalidSelector]
        );
        assert_eq!(
            errors("@media print { img . foo { color: red } }"),
            vec![SyntaxErrorKind::InvalidSelector]
        );
        // Both halves of a comma-separated list are checked, not just the
        // first - the shape a real book turned out to carry.
        assert_eq!(
            errors("@media print { . a, . b { color: red } }"),
            vec![
                SyntaxErrorKind::InvalidSelector,
                SyntaxErrorKind::InvalidSelector
            ]
        );
        assert!(errors("@media print { p.foo, #b > i { color: red } }").is_empty());
    }

    /// Spans point into the original stylesheet. This is the whole reason
    /// the entry point takes component values rather than a source slice.
    #[test]
    fn spans_stay_absolute() {
        let css = "@media print { . foo { color: red } }";
        let (_sheet, values) = body(css);
        let errs = parse_rule_list(&values).1;
        assert_eq!(errs.len(), 1);
        assert_eq!(&css[errs[0].span.start..errs[0].span.start + 1], ".");
        assert_eq!(errs[0].span.start, css.find(". foo").unwrap());
    }

    /// A nested at-rule is a rule, and its prelude is a condition — running
    /// it through the selector check would invent errors on every one.
    #[test]
    fn a_nested_at_rule_is_not_read_as_a_selector() {
        assert!(errors("@media print { @media (width > 0) { p { color: red } } }").is_empty());
        assert_eq!(
            rules("@media print { @media (width > 0) { p { color: red } } }"),
            1
        );
        // The block-less form ends at its semicolon rather than swallowing
        // the rule that follows.
        assert!(errors("@media print { @import url(x.css); p { color: red } }").is_empty());
        assert_eq!(
            rules("@media print { @import url(x.css); p { color: red } }"),
            2
        );
    }

    /// Counting rules, so a boundary error cannot hide behind an empty
    /// error list.
    #[test]
    fn rule_boundaries() {
        assert_eq!(rules("@media print { }"), 0);
        assert_eq!(rules("@media print { p { color: red } }"), 1);
        assert_eq!(
            rules("@media print { p { color: red } i { color: blue } }"),
            2
        );
        // A `[]` block is part of a prelude, not a rule body (an attribute
        // selector) - reading it as one is how the sibling bug in the
        // consumer went, so it is pinned here too.
        assert_eq!(rules("@media print { img[alt] { color: red } }"), 1);
        assert!(errors("@media print { img[alt] { color: red } }").is_empty());
    }

    /// A prelude that never meets its block, the §5.4.4 case the top-level
    /// parser already reports. Trailing whitespace alone is not a rule.
    #[test]
    fn an_unterminated_nested_rule_is_reported() {
        let (_sheet, values) = body("@media print { p { color: red } i");
        assert_eq!(
            parse_rule_list(&values)
                .1
                .into_iter()
                .map(|e| e.kind)
                .collect::<Vec<_>>(),
            vec![SyntaxErrorKind::UnterminatedRule]
        );
        assert!(errors("@media print { p { color: red } ").is_empty());
        assert!(errors("@media print {  }").is_empty());
    }
}

#[cfg(test)]
mod at_rule_block_tests {
    use super::*;

    /// The name and block of the first top-level at-rule.
    fn at(css: &str) -> (String, Vec<Spanned<ComponentValue<'_>>>) {
        match &parse_stylesheet(css).rules[0].node {
            Rule::At(a) => (
                a.name.to_string(),
                a.block
                    .as_ref()
                    .expect("at-rule has a block")
                    .node
                    .values
                    .clone(),
            ),
            Rule::Qualified(_) => panic!("expected an at-rule"),
        }
    }

    fn read(css: &str) -> (BlockContents<'_>, Vec<SyntaxErrorKind>) {
        let (name, values) = at(css);
        let (contents, errors) = parse_at_rule_block(&name, &values);
        (contents, errors.into_iter().map(|e| e.kind).collect())
    }

    fn is_rules(c: &BlockContents<'_>) -> bool {
        matches!(c, BlockContents::Rules(_))
    }

    /// The case that sent this table into the crate. `@keyframes` holds
    /// rules, and its preludes are keyframe selectors: `0%` is correct CSS
    /// under CSS Animations 1 §3 and a malformed *selector* under Selectors
    /// 4. A consumer whose table had only the conditional-group rules read
    /// the block as declarations and reported the whole keyframe as one
    /// malformed declaration — an error on valid CSS that epubcheck, the
    /// reference implementation it was matching, does not report.
    #[test]
    fn a_keyframes_block_is_rules_with_unvalidated_preludes() {
        for css in [
            "@keyframes spin { 0% { opacity: 0 } 100% { opacity: 1 } }",
            "@keyframes spin { from { opacity: 0 } to { opacity: 1 } }",
            "@-webkit-keyframes spin { 50% { opacity: .5 } }",
            "@-moz-keyframes spin { 50% { opacity: .5 } }",
            "@keyframes spin { }",
        ] {
            let (contents, errors) = read(css);
            assert!(is_rules(&contents), "{css} should read as rules");
            assert!(errors.is_empty(), "{css} produced {errors:?}");
        }
    }

    /// Reading the keyframe selector as a selector list is the failure the
    /// `Preludes::Opaque` branch exists to prevent, so assert it directly:
    /// the same values through `parse_rule_list` do produce the error.
    #[test]
    fn the_same_keyframes_block_is_two_bad_selectors_to_parse_rule_list() {
        let (_, values) = at("@keyframes spin { 0% { opacity: 0 } 100% { opacity: 1 } }");
        assert_eq!(
            parse_rule_list(&values)
                .1
                .into_iter()
                .map(|e| e.kind)
                .collect::<Vec<_>>(),
            vec![
                SyntaxErrorKind::InvalidSelector,
                SyntaxErrorKind::InvalidSelector
            ]
        );
    }

    /// A conditional-group rule keeps the selector check it gained in #2 —
    /// the point of the table is that the two blocks differ.
    #[test]
    fn a_grouping_block_is_rules_with_selectors_validated() {
        for css in [
            "@media print { .. { color: red } }",
            "@starting-style { .. { opacity: 0 } }",
        ] {
            let (contents, errors) = read(css);
            assert!(is_rules(&contents), "{css} should read as rules");
            assert_eq!(errors, vec![SyntaxErrorKind::InvalidSelector], "{css}");
        }
        assert!(read("@media print { p { color: red } }").1.is_empty());
        assert!(read("@starting-style { .a { opacity: 0 } }").1.is_empty());
    }

    #[test]
    fn a_declaration_at_rule_is_declarations() {
        for css in [
            "@font-face { font-family: X; src: url(x.ttf) }",
            "@page { margin: 1em }",
            "@counter-style thumbs { system: cyclic; symbols: \"x\" }",
            "@property --x { syntax: \"<length>\"; inherits: false }",
        ] {
            let (contents, errors) = read(css);
            assert!(matches!(contents, BlockContents::Declarations(_)), "{css}");
            assert!(errors.is_empty(), "{css} produced {errors:?}");
        }
        assert_eq!(
            read("@font-face { font-family: X; src url(x.ttf) }").1,
            vec![SyntaxErrorKind::MalformedDeclaration]
        );
    }

    /// The direction an unknown at-rule has to fail in. CSS keeps gaining
    /// at-rules, so this table is permanently one release behind the
    /// language; what it must not do is turn its own ignorance into an
    /// error on a valid stylesheet. A malformed declaration inside is still
    /// reported — that is malformed whatever the block turns out to hold.
    #[test]
    fn an_unknown_at_rule_reports_a_bad_declaration_and_not_a_nested_rule() {
        assert!(read("@future { p { color: red } }").1.is_empty());
        assert!(
            read("@future (cond) { .a b, .c { color: red } }")
                .1
                .is_empty()
        );
        assert_eq!(
            read("@future { color red }").1,
            vec![SyntaxErrorKind::MalformedDeclaration]
        );
        assert_eq!(
            read("@future { p { color: red } color red }").1,
            vec![SyntaxErrorKind::MalformedDeclaration]
        );
    }

    /// The tolerance above is scoped to at-rule blocks. A *style rule's*
    /// block holds declarations and nothing else, and the entry point for
    /// it still says so.
    #[test]
    fn a_style_rules_block_still_rejects_a_nested_rule() {
        let sheet = parse_stylesheet("a { color: red; & b { color: blue } }");
        let Rule::Qualified(q) = &sheet.rules[0].node else {
            panic!("expected a qualified rule")
        };
        assert_eq!(
            parse_declaration_list_from_values(&q.block.node.values)
                .1
                .into_iter()
                .map(|e| e.kind)
                .collect::<Vec<_>>(),
            vec![SyntaxErrorKind::UnexpectedToken]
        );
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
