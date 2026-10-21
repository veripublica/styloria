//! CSS Values 4's value definition syntax (§2), read into a tree.
//!
//! Precedence, tightest first: multipliers, juxtaposition, `&&`, `||`, `|`.
//! A quoted `'['` … `']'` pair becomes a square-bracket block, which is how
//! such a value tokenizes (`grid-template-areas`' line names).

/// One node of a grammar.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    /// A keyword, compared ASCII case-insensitively.
    Keyword(String),
    /// A literal token: `,`, `/`, `'+'`, `':'` and the like.
    Literal(char),
    /// `<name>`, `<name()>` or `<name [min,max]>`.
    Type { name: String, range: Option<Range> },
    /// `<'property'>`: that property's grammar.
    Property(String),
    /// `name( … )`.
    Function(String, Box<Node>),
    /// `'[' … ']'`.
    SquareBlock(Box<Node>),
    /// `( … )` written bare: a parenthesis block.
    ParenBlock(Box<Node>),
    /// Juxtaposition: each in order.
    Seq(Vec<Node>),
    /// `a && b`: all, in any order.
    AllOf(Vec<Node>),
    /// `a || b`: one or more, in any order.
    AnyOf(Vec<Node>),
    /// `a | b`: exactly one.
    OneOf(Vec<Node>),
    /// `*`, `+`, `?`, `{A}`, `{A,B}`, `{A,}`, and with `comma` the `#`
    /// forms.
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        comma: bool,
    },
    /// `[ … ]!`: the group may not be empty.
    NonEmpty(Box<Node>),
}

/// A numeric range on a type, `[0,∞]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Range {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Open,
    Close,
    Bar,
    DoubleBar,
    DoubleAmp,
    Star,
    Plus,
    Question,
    Hash,
    Bang,
    Braces(u32, Option<u32>),
    Type(String, Option<Range>),
    Property(String),
    Quoted(String),
    FunctionStart(String),
    OpenParen,
    CloseParen,
    Word(String),
    Comma,
    Slash,
}

fn lex(src: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' => i += 1,
            b'[' => {
                out.push(Tok::Open);
                i += 1;
            }
            b']' => {
                out.push(Tok::Close);
                i += 1;
            }
            b'|' if b.get(i + 1) == Some(&b'|') => {
                out.push(Tok::DoubleBar);
                i += 2;
            }
            b'|' => {
                out.push(Tok::Bar);
                i += 1;
            }
            b'&' if b.get(i + 1) == Some(&b'&') => {
                out.push(Tok::DoubleAmp);
                i += 2;
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'?' => {
                out.push(Tok::Question);
                i += 1;
            }
            b'#' => {
                out.push(Tok::Hash);
                i += 1;
            }
            b'!' => {
                out.push(Tok::Bang);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'(' => {
                out.push(Tok::OpenParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::CloseParen);
                i += 1;
            }
            b'{' => {
                let end = i + src[i..].find('}')?;
                let inner = &src[i + 1..end];
                let (min, max) = match inner.split_once(',') {
                    None => {
                        let n = inner.trim().parse().ok()?;
                        (n, Some(n))
                    }
                    Some((a, b)) => {
                        let b = b.trim();
                        let max = if b.is_empty() {
                            None
                        } else {
                            Some(b.parse().ok()?)
                        };
                        (a.trim().parse().ok()?, max)
                    }
                };
                out.push(Tok::Braces(min, max));
                i = end + 1;
            }
            b'<' => {
                let end = i + src[i..].find('>')?;
                let inner = src[i + 1..end].trim();
                if let Some(p) = inner.strip_prefix('\'') {
                    out.push(Tok::Property(p.strip_suffix('\'')?.to_string()));
                } else {
                    let (name, range) = match inner.split_once('[') {
                        None => (inner, None),
                        Some((n, r)) => (n.trim(), Some(range(r.strip_suffix(']')?)?)),
                    };
                    out.push(Tok::Type(name.to_string(), range));
                }
                i = end + 1;
            }
            b'\'' => {
                let end = i + 1 + src[i + 1..].find('\'')?;
                out.push(Tok::Quoted(src[i + 1..end].to_string()));
                i = end + 1;
            }
            _ => {
                let start = i;
                while i < b.len() && !b" \t\n[]|&*+?#!,/(){}<>'".contains(&b[i]) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let word = src[start..i].to_string();
                if b.get(i) == Some(&b'(') {
                    out.push(Tok::FunctionStart(word));
                    i += 1;
                } else {
                    out.push(Tok::Word(word));
                }
            }
        }
    }
    Some(out)
}

fn range(r: &str) -> Option<Range> {
    let (a, b) = r.split_once(',')?;
    let bound = |s: &str| -> Option<f64> {
        let s = s.trim();
        match s {
            "∞" | "+∞" => Some(f64::INFINITY),
            "-∞" | "−∞" => Some(f64::NEG_INFINITY),
            // A range written with a unit (`[0px,∞]`) bounds the number.
            _ => {
                let end = s
                    .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
                    .unwrap_or(s.len());
                s[..end].parse().ok()
            }
        }
    };
    Some(Range {
        min: bound(a)?,
        max: bound(b)?,
    })
}

/// Parse a grammar; `None` when it is not one this reader understands.
pub(crate) fn parse(src: &str) -> Option<Node> {
    let toks = lex(src)?;
    let mut p = Parser { toks, at: 0 };
    let node = p.one_of()?;
    (p.at == p.toks.len()).then_some(node)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn one_of(&mut self) -> Option<Node> {
        self.list(Tok::Bar, Node::OneOf, Self::any_of)
    }

    fn any_of(&mut self) -> Option<Node> {
        self.list(Tok::DoubleBar, Node::AnyOf, Self::all_of)
    }

    fn all_of(&mut self) -> Option<Node> {
        self.list(Tok::DoubleAmp, Node::AllOf, Self::seq)
    }

    fn list(
        &mut self,
        sep: Tok,
        make: fn(Vec<Node>) -> Node,
        item: fn(&mut Self) -> Option<Node>,
    ) -> Option<Node> {
        let mut items = vec![item(self)?];
        while self.eat(&sep) {
            items.push(item(self)?);
        }
        Some(if items.len() == 1 {
            items.remove(0)
        } else {
            make(items)
        })
    }

    fn seq(&mut self) -> Option<Node> {
        let mut items = Vec::new();
        while let Some(t) = self.peek() {
            if matches!(
                t,
                Tok::Bar | Tok::DoubleBar | Tok::DoubleAmp | Tok::Close | Tok::CloseParen
            ) {
                break;
            }
            items.push(self.postfix()?);
        }
        Some(if items.len() == 1 {
            items.remove(0)
        } else {
            collapse_square(items)?
        })
    }

    fn postfix(&mut self) -> Option<Node> {
        let mut node = self.primary()?;
        loop {
            let (min, max, comma) = match self.peek() {
                Some(Tok::Star) => (0, None, false),
                Some(Tok::Plus) => (1, None, false),
                Some(Tok::Question) => (0, Some(1), false),
                Some(Tok::Braces(a, b)) => (*a, *b, false),
                Some(Tok::Hash) => {
                    self.at += 1;
                    if let Some(Tok::Braces(a, b)) = self.peek() {
                        let (a, b) = (*a, *b);
                        self.at += 1;
                        node = repeat(node, a, b, true);
                    } else {
                        node = repeat(node, 1, None, true);
                    }
                    continue;
                }
                Some(Tok::Bang) => {
                    self.at += 1;
                    node = Node::NonEmpty(Box::new(node));
                    continue;
                }
                _ => break,
            };
            self.at += 1;
            node = repeat(node, min, max, comma);
        }
        Some(node)
    }

    fn primary(&mut self) -> Option<Node> {
        let t = self.peek()?.clone();
        self.at += 1;
        Some(match t {
            Tok::Open => {
                let inner = self.one_of()?;
                if !self.eat(&Tok::Close) {
                    return None;
                }
                // A group stays a unit under its multiplier.
                inner
            }
            Tok::Type(name, range) => Node::Type { name, range },
            Tok::Property(p) => Node::Property(p),
            Tok::Quoted(q) => {
                let mut chars = q.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Node::Literal(c),
                    _ => Node::Keyword(q),
                }
            }
            Tok::FunctionStart(name) => {
                let args = if self.peek() == Some(&Tok::CloseParen) {
                    Node::Seq(Vec::new())
                } else {
                    self.one_of()?
                };
                if !self.eat(&Tok::CloseParen) {
                    return None;
                }
                Node::Function(name, Box::new(args))
            }
            Tok::OpenParen => {
                let inner = self.one_of()?;
                if !self.eat(&Tok::CloseParen) {
                    return None;
                }
                Node::ParenBlock(Box::new(inner))
            }
            Tok::Word(w) => Node::Keyword(w),
            Tok::Comma => Node::Literal(','),
            Tok::Slash => Node::Literal('/'),
            _ => return None,
        })
    }
}

fn repeat(node: Node, min: u32, max: Option<u32>, comma: bool) -> Node {
    Node::Repeat {
        node: Box::new(node),
        min,
        max,
        comma,
    }
}

/// Turn `'[' … ']'` inside a sequence into a square block.
fn collapse_square(items: Vec<Node>) -> Option<Node> {
    if !items.contains(&Node::Literal('[')) {
        return Some(Node::Seq(items));
    }
    let mut out = Vec::new();
    let mut stack: Vec<Vec<Node>> = Vec::new();
    for item in items {
        match item {
            Node::Literal('[') => stack.push(std::mem::take(&mut out)),
            Node::Literal(']') => {
                let inner = std::mem::replace(&mut out, stack.pop()?);
                out.push(Node::SquareBlock(Box::new(Node::Seq(inner))));
            }
            other => out.push(other),
        }
    }
    if !stack.is_empty() {
        return None;
    }
    Some(if out.len() == 1 {
        out.remove(0)
    } else {
        Node::Seq(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_syntaxes::{PROPERTY_SYNTAXES, TYPE_SYNTAXES};

    #[test]
    fn precedence_and_multipliers() {
        let n = parse("a b | c && d || e").unwrap();
        let kw = |s: &str| Node::Keyword(s.into());
        assert_eq!(
            n,
            Node::OneOf(vec![
                Node::Seq(vec![kw("a"), kw("b")]),
                Node::AnyOf(vec![Node::AllOf(vec![kw("c"), kw("d")]), kw("e")]),
            ])
        );
        assert_eq!(
            parse("<length [0,∞]>#{1,4}").unwrap(),
            Node::Repeat {
                node: Box::new(Node::Type {
                    name: "length".into(),
                    range: Some(Range {
                        min: 0.0,
                        max: f64::INFINITY
                    })
                }),
                min: 1,
                max: Some(4),
                comma: true,
            }
        );
        assert!(matches!(
            parse("fit-content( <length-percentage> )").unwrap(),
            Node::Function(ref f, _) if f == "fit-content"
        ));
        assert!(matches!(
            parse("'[' <custom-ident>* ']'").unwrap(),
            Node::SquareBlock(_)
        ));
        assert!(matches!(parse("[ a | b ]!").unwrap(), Node::NonEmpty(_)));
    }

    #[test]
    fn every_mdn_grammar_parses() {
        // Two at-rule grammars mdn writes loosely (an unbalanced `)`, a
        // `{` block); no property uses them, and an unread type is Unknown.
        let skipped = ["general-enclosed", "keyframe-block"];
        let bad: Vec<_> = PROPERTY_SYNTAXES
            .iter()
            .chain(TYPE_SYNTAXES)
            .filter(|(n, s)| parse(s).is_none() && !skipped.contains(n))
            .map(|(n, s)| format!("{n}: {s}"))
            .collect();
        assert!(
            bad.is_empty(),
            "{} do not parse:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}
