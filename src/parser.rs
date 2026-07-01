//! The CSS "core grammar" (CSS Syntax Level 3, §5:
//! <https://www.w3.org/TR/css-syntax-3/#parsing>) — turning a token stream
//! into a generic rule/declaration tree with **no** knowledge of what any
//! particular at-rule, selector, or property means. Selectors and
//! declaration values are left as unparsed `ComponentValue`s; that's a
//! later, separate layer (see `styloria`'s `CLAUDE.md`).
//!
//! Implements the two entry points needed first: "parse a stylesheet" and
//! "parse a list of declarations" (the latter for inline `style="..."`
//! attribute values, and for `@font-face`/`@page`-style block contents).

use std::borrow::Cow;
use std::iter::Peekable;

use crate::token::Token;
use crate::tokenizer::Tokenizer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Curly,
    Square,
    Paren,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComponentValue<'a> {
    Token(Token<'a>),
    Function {
        name: Cow<'a, str>,
        args: Vec<ComponentValue<'a>>,
    },
    Block(SimpleBlock<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimpleBlock<'a> {
    pub kind: BlockKind,
    pub values: Vec<ComponentValue<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedRule<'a> {
    pub prelude: Vec<ComponentValue<'a>>,
    pub block: SimpleBlock<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AtRule<'a> {
    pub name: Cow<'a, str>,
    pub prelude: Vec<ComponentValue<'a>>,
    pub block: Option<SimpleBlock<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rule<'a> {
    Qualified(QualifiedRule<'a>),
    At(AtRule<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration<'a> {
    pub name: Cow<'a, str>,
    pub value: Vec<ComponentValue<'a>>,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclarationListItem<'a> {
    Declaration(Declaration<'a>),
    AtRule(AtRule<'a>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stylesheet<'a> {
    pub rules: Vec<Rule<'a>>,
}

pub struct Parser<'a> {
    tokens: Peekable<Tokenizer<'a>>,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Self {
        Parser {
            tokens: Tokenizer::new(input).peekable(),
        }
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.tokens.next()
    }
    fn peek(&mut self) -> Option<&Token<'a>> {
        self.tokens.peek()
    }
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(Token::Whitespace)) {
            self.next();
        }
    }

    /// §5.3.3 "Parse a stylesheet". Top-level: CDO/CDC tokens are discarded
    /// rather than starting a qualified rule (they exist only for the
    /// legacy HTML-comment-hiding trick at a stylesheet's outermost level).
    pub fn parse_stylesheet(input: &'a str) -> Stylesheet<'a> {
        let mut p = Parser::new(input);
        Stylesheet {
            rules: p.consume_rules_list(true),
        }
    }

    /// §5.3.4-adjacent "Parse a list of declarations".
    pub fn parse_declaration_list(input: &'a str) -> Vec<DeclarationListItem<'a>> {
        let mut p = Parser::new(input);
        p.consume_declaration_list()
    }

    /// §5.4.1 "Consume a list of rules".
    fn consume_rules_list(&mut self, top_level: bool) -> Vec<Rule<'a>> {
        let mut rules = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(Token::Whitespace) => {
                    self.next();
                }
                Some(Token::Cdo | Token::Cdc) => {
                    if top_level {
                        self.next();
                    } else if let Some(r) = self.consume_qualified_rule() {
                        rules.push(Rule::Qualified(r));
                    }
                }
                Some(Token::AtKeyword(_)) => rules.push(Rule::At(self.consume_at_rule())),
                _ => {
                    if let Some(r) = self.consume_qualified_rule() {
                        rules.push(Rule::Qualified(r));
                    }
                }
            }
        }
        rules
    }

    /// §5.4.2 "Consume a list of declarations".
    fn consume_declaration_list(&mut self) -> Vec<DeclarationListItem<'a>> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(Token::Whitespace | Token::Semicolon) => {
                    self.next();
                }
                Some(Token::AtKeyword(_)) => {
                    items.push(DeclarationListItem::AtRule(self.consume_at_rule()))
                }
                Some(Token::Ident(_)) => {
                    if let Some(d) = self.consume_declaration() {
                        items.push(DeclarationListItem::Declaration(d));
                    }
                }
                _ => {
                    // Parse error: an unexpected token outside any
                    // recognized construct. Discard one component value and
                    // keep going — this guarantees forward progress without
                    // ever treating malformed CSS as a hard failure.
                    self.consume_component_value();
                }
            }
        }
        items
    }

    /// §5.4.4 "Consume a qualified rule". Returns `None` on the one error
    /// path the spec defines (EOF reached before a block) — the rule is
    /// simply dropped, per spec.
    fn consume_qualified_rule(&mut self) -> Option<QualifiedRule<'a>> {
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => return None,
                Some(Token::LeftCurly) => {
                    self.next();
                    let values = self.consume_block_contents(BlockKind::Curly);
                    return Some(QualifiedRule {
                        prelude,
                        block: SimpleBlock {
                            kind: BlockKind::Curly,
                            values,
                        },
                    });
                }
                _ => prelude.push(self.consume_component_value()),
            }
        }
    }

    /// §5.4.3 "Consume an at-rule". Assumes the current token is the
    /// at-keyword.
    fn consume_at_rule(&mut self) -> AtRule<'a> {
        let name = match self.next() {
            Some(Token::AtKeyword(n)) => n,
            _ => unreachable!("consume_at_rule requires an at-keyword as the current token"),
        };
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return AtRule {
                        name,
                        prelude,
                        block: None,
                    }
                }
                Some(Token::Semicolon) => {
                    self.next();
                    return AtRule {
                        name,
                        prelude,
                        block: None,
                    };
                }
                Some(Token::LeftCurly) => {
                    self.next();
                    let values = self.consume_block_contents(BlockKind::Curly);
                    return AtRule {
                        name,
                        prelude,
                        block: Some(SimpleBlock {
                            kind: BlockKind::Curly,
                            values,
                        }),
                    };
                }
                _ => prelude.push(self.consume_component_value()),
            }
        }
    }

    /// §5.4.5 "Consume a declaration". Assumes the current token is the
    /// declaration's name (an ident). Returns `None` if no `:` follows (a
    /// malformed declaration, per spec discarded rather than erroring).
    fn consume_declaration(&mut self) -> Option<Declaration<'a>> {
        let name = match self.next() {
            Some(Token::Ident(n)) => n,
            _ => unreachable!("consume_declaration requires an ident as the current token"),
        };
        self.skip_whitespace();
        if !matches!(self.peek(), Some(Token::Colon)) {
            return None;
        }
        self.next();
        self.skip_whitespace();
        let mut value = Vec::new();
        while !matches!(self.peek(), None | Some(Token::Semicolon)) {
            value.push(self.consume_component_value());
        }
        let important = strip_trailing_important(&mut value);
        Some(Declaration {
            name,
            value,
            important,
        })
    }

    /// §5.4.7 "Consume a simple block" (the contents only — the caller has
    /// already consumed the opening bracket token).
    fn consume_block_contents(&mut self, kind: BlockKind) -> Vec<ComponentValue<'a>> {
        let close = match kind {
            BlockKind::Curly => Token::RightCurly,
            BlockKind::Square => Token::RightSquare,
            BlockKind::Paren => Token::RightParen,
        };
        let mut values = Vec::new();
        loop {
            match self.peek() {
                None => return values,
                Some(t) if *t == close => {
                    self.next();
                    return values;
                }
                _ => values.push(self.consume_component_value()),
            }
        }
    }

    /// §5.4.8 "Consume a function" (the arguments only — the caller has
    /// already consumed the function-token itself).
    fn consume_function_args(&mut self) -> Vec<ComponentValue<'a>> {
        let mut args = Vec::new();
        loop {
            match self.peek() {
                None => return args,
                Some(Token::RightParen) => {
                    self.next();
                    return args;
                }
                _ => args.push(self.consume_component_value()),
            }
        }
    }

    /// §5.4.6 "Consume a component value".
    fn consume_component_value(&mut self) -> ComponentValue<'a> {
        match self.peek() {
            Some(Token::LeftCurly) => {
                self.next();
                ComponentValue::Block(SimpleBlock {
                    kind: BlockKind::Curly,
                    values: self.consume_block_contents(BlockKind::Curly),
                })
            }
            Some(Token::LeftSquare) => {
                self.next();
                ComponentValue::Block(SimpleBlock {
                    kind: BlockKind::Square,
                    values: self.consume_block_contents(BlockKind::Square),
                })
            }
            Some(Token::LeftParen) => {
                self.next();
                ComponentValue::Block(SimpleBlock {
                    kind: BlockKind::Paren,
                    values: self.consume_block_contents(BlockKind::Paren),
                })
            }
            Some(Token::Function(_)) => {
                let name = match self.next() {
                    Some(Token::Function(n)) => n,
                    _ => unreachable!(),
                };
                ComponentValue::Function {
                    name,
                    args: self.consume_function_args(),
                }
            }
            Some(_) => ComponentValue::Token(self.next().unwrap()),
            None => unreachable!("consume_component_value called at EOF"),
        }
    }
}

/// §5.4.5's trailing bit: if the last two non-whitespace component values
/// of a declaration's value are `!` followed by an `important` ident
/// (case-insensitively), strip exactly those two tokens and report it.
fn strip_trailing_important(value: &mut Vec<ComponentValue<'_>>) -> bool {
    let mut rev = value
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, v)| !matches!(v, ComponentValue::Token(Token::Whitespace)));
    let last = rev.next();
    let second_last = rev.next();
    if let (Some((li, lv)), Some((si, sv))) = (last, second_last) {
        let is_important = matches!(lv, ComponentValue::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("important"));
        let is_bang = matches!(sv, ComponentValue::Token(Token::Delim('!')));
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

    #[test]
    fn simple_ruleset() {
        let sheet = Parser::parse_stylesheet("p { color: red; }");
        assert_eq!(sheet.rules.len(), 1);
        let Rule::Qualified(r) = &sheet.rules[0] else {
            panic!("expected qualified rule")
        };
        assert!(matches!(r.prelude[0], ComponentValue::Token(Token::Ident(ref s)) if s == "p"));
        assert!(!r.block.values.is_empty());
    }

    #[test]
    fn at_rule_with_block() {
        let sheet = Parser::parse_stylesheet("@media screen { a { color: blue; } }");
        assert_eq!(sheet.rules.len(), 1);
        let Rule::At(r) = &sheet.rules[0] else {
            panic!("expected at-rule")
        };
        assert_eq!(r.name, "media");
        assert!(r.block.is_some());
    }

    #[test]
    fn at_rule_without_block() {
        let sheet = Parser::parse_stylesheet("@import url(foo.css);");
        let Rule::At(r) = &sheet.rules[0] else {
            panic!("expected at-rule")
        };
        assert_eq!(r.name, "import");
        assert!(r.block.is_none());
    }

    #[test]
    fn nested_blocks_and_functions() {
        let sheet = Parser::parse_stylesheet("a { background: rgb(1, 2, 3) url(x.png); }");
        let Rule::Qualified(r) = &sheet.rules[0] else {
            panic!()
        };
        let has_function = r
            .block
            .values
            .iter()
            .any(|v| matches!(v, ComponentValue::Function { name, .. } if name == "rgb"));
        assert!(has_function);
    }

    #[test]
    fn declaration_list_basic() {
        let items = Parser::parse_declaration_list("color: red; margin: 0 auto;");
        assert_eq!(items.len(), 2);
        let DeclarationListItem::Declaration(d0) = &items[0] else {
            panic!()
        };
        assert_eq!(d0.name, "color");
        assert!(!d0.important);
    }

    #[test]
    fn important_flag_stripped() {
        let items = Parser::parse_declaration_list("color: red !important;");
        let DeclarationListItem::Declaration(d) = &items[0] else {
            panic!()
        };
        assert!(d.important);
        // the "!important" tokens themselves must not remain in the value
        assert!(!d.value.iter().any(|v| matches!(v, ComponentValue::Token(Token::Ident(s)) if s.eq_ignore_ascii_case("important"))));
    }

    #[test]
    fn malformed_declaration_without_colon_is_skipped_not_fatal() {
        // "color red" (no colon) is discarded; the next valid declaration
        // must still be parsed — nothing here should ever panic/error.
        let items = Parser::parse_declaration_list("color red; margin: 0;");
        assert_eq!(items.len(), 1);
        let DeclarationListItem::Declaration(d) = &items[0] else {
            panic!()
        };
        assert_eq!(d.name, "margin");
    }

    #[test]
    fn custom_property_value_preserved_verbatim() {
        // custom properties' values are supposed to stay as raw component
        // values (no attempt to make sense of arbitrary token soup)
        let items = Parser::parse_declaration_list("--x: {a b c};");
        let DeclarationListItem::Declaration(d) = &items[0] else {
            panic!()
        };
        assert_eq!(d.name, "--x");
        assert!(matches!(d.value[0], ComponentValue::Block(_)));
    }

    #[test]
    fn unterminated_block_recovers_at_eof() {
        // a stylesheet cut off mid-rule must not panic; the rule is simply
        // dropped per spec (EOF before the qualified rule's block starts —
        // here there IS a block, so it parses with whatever content is
        // present up to EOF).
        let sheet = Parser::parse_stylesheet("a { color: red");
        assert_eq!(sheet.rules.len(), 1);
    }
}
