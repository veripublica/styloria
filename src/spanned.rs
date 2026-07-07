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
    let mut p = SpannedParser {
        tokens: Tokenizer::new(input).spanned().peekable(),
    };
    Stylesheet {
        rules: p.consume_rules_list(true),
    }
}

/// Parse a list of declarations into the span-carrying tree — the spanned
/// mirror of [`crate::Parser::parse_declaration_list`]. Use this for a
/// `style="…"` attribute's value, or an `@font-face` / `@page` body.
pub fn parse_declaration_list(input: &str) -> Vec<DeclarationListItem<'_>> {
    let mut p = SpannedParser {
        tokens: Tokenizer::new(input).spanned().peekable(),
    };
    p.consume_declaration_list()
}

struct SpannedParser<'a> {
    tokens: Peekable<SpannedTokens<'a>>,
}

impl<'a> SpannedParser<'a> {
    fn next(&mut self) -> Option<Spanned<Token<'a>>> {
        self.tokens.next()
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
                    }
                }
                _ => {
                    // Parse error: discard one component value, keep going.
                    self.consume_component_value();
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
                None => return None,
                Some(Token::LeftCurly) => {
                    let open = self.next().unwrap().span;
                    let block = self.consume_simple_block(BlockKind::Curly, open);
                    let span = start.unwrap_or(block.span).to(block.span);
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
                let (args, end) = self.consume_function_args(st.span);
                let span = st.span.to(end);
                Spanned::new(ComponentValue::Function { name, args }, span)
            }
            other => Spanned::new(ComponentValue::Token(other), st.span),
        }
    }

    fn finish_block_value(&mut self, kind: BlockKind, open: Span) -> Spanned<ComponentValue<'a>> {
        let block = self.consume_simple_block(kind, open);
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
                None => break,
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
}
