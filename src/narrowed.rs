//! A value read as it tokenized before w3c/csswg-drafts#7129.
//!
//! The 2026 CRD narrowed the non-ASCII ident code points to the ones HTML
//! allows in custom element names; before, everything at or above U+0080
//! was one. The issue is open again (Agenda+, 2026-10-12), so a value that
//! fails only because such a code point now stands alone as a delim is not
//! certainly invalid. [`relax`] rebuilds the value the old way, so it can be
//! matched once more; a selector likewise.

use std::borrow::Cow;

use crate::parser::{BlockKind, ComponentValue, SimpleBlock};
use crate::span::{Span, Spanned};
use crate::token::{NumericType, Token};
use crate::tokenizer::is_narrowed_out;

/// The spans of the code points in `values`, at any depth, that
/// [`is_narrowed_out`], in source order.
pub(crate) fn spots(values: &[Spanned<ComponentValue<'_>>], out: &mut Vec<Span>) {
    for v in values {
        match &v.node {
            ComponentValue::Token(Token::Delim(c)) if is_narrowed_out(*c) => out.push(v.span),
            ComponentValue::Function { args, .. } => spots(args, out),
            ComponentValue::Block(b) => spots(&b.values, out),
            ComponentValue::Token(_) => {}
        }
    }
}

/// What reading a value with the old ident code points gives.
pub(crate) enum Relaxed<'a> {
    /// It reads the same.
    Same,
    /// It reads differently, in a way not rebuilt here.
    Unsure,
    Read(Vec<Spanned<ComponentValue<'a>>>),
}

/// The token a run of name characters becomes; only an ident can go on to
/// be a function.
enum Head<'a> {
    Ident(String),
    Hash(String, bool),
    At(String),
    Dimension {
        value: f64,
        num_type: NumericType,
        repr: &'a str,
        unit: String,
    },
}

impl<'a> Head<'a> {
    fn push(&mut self, s: &str) {
        match self {
            Head::Ident(t) | Head::Hash(t, _) | Head::At(t) => t.push_str(s),
            Head::Dimension { unit, .. } => unit.push_str(s),
        }
    }

    fn token(self) -> ComponentValue<'a> {
        ComponentValue::Token(match self {
            Head::Ident(t) => Token::Ident(t.into()),
            Head::Hash(value, is_id) => Token::Hash {
                value: value.into(),
                is_id,
            },
            Head::At(t) => Token::AtKeyword(t.into()),
            Head::Dimension {
                value,
                num_type,
                repr,
                unit,
            } => Token::Dimension {
                value,
                num_type,
                unit: unit.into(),
                repr,
            },
        })
    }
}

fn narrowed_out(v: &ComponentValue<'_>) -> Option<char> {
    match v {
        ComponentValue::Token(Token::Delim(c)) if is_narrowed_out(*c) => Some(*c),
        _ => None,
    }
}

/// Whether `s` is all name code points, so that the old tokenizer would
/// have read it on as part of a name.
fn all_name(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c >= '\u{80}')
}

/// `values` read with every code point at or above U+0080 a name code
/// point, functions and blocks included, to `depth`.
pub(crate) fn relax<'a>(values: &[Spanned<ComponentValue<'a>>], depth: usize) -> Relaxed<'a> {
    if depth == 0 {
        return Relaxed::Unsure;
    }
    let mut out = Vec::with_capacity(values.len());
    let mut changed = false;
    let mut i = 0;
    while i < values.len() {
        let v = &values[i];
        let before_one = values
            .get(i + 1)
            .is_some_and(|n| narrowed_out(&n.node).is_some());
        let head = match &v.node {
            ComponentValue::Token(t) if before_one || narrowed_out(&v.node).is_some() => match t {
                Token::Delim(c) if is_narrowed_out(*c) => Some(Head::Ident(c.to_string())),
                Token::Ident(s) => Some(Head::Ident(s.to_string())),
                Token::Hash { value, is_id } => Some(Head::Hash(value.to_string(), *is_id)),
                Token::AtKeyword(s) => Some(Head::At(s.to_string())),
                Token::Number {
                    value,
                    num_type,
                    repr,
                } => Some(Head::Dimension {
                    value: *value,
                    num_type: *num_type,
                    repr,
                    unit: String::new(),
                }),
                Token::Dimension {
                    value,
                    num_type,
                    unit,
                    repr,
                } => Some(Head::Dimension {
                    value: *value,
                    num_type: *num_type,
                    repr,
                    unit: unit.to_string(),
                }),
                // `1-‣` was one dimension, not a number and an ident.
                Token::Delim('-' | '#' | '@')
                    if i > 0
                        && matches!(
                            values[i - 1].node,
                            ComponentValue::Token(Token::Number { .. })
                        ) =>
                {
                    return Relaxed::Unsure;
                }
                Token::Delim('-') => Some(Head::Ident("-".into())),
                Token::Delim('#') => Some(Head::Hash(String::new(), true)),
                Token::Delim('@') => Some(Head::At(String::new())),
                _ => None,
            },
            _ => None,
        };
        let Some(mut head) = head else {
            let node = match &v.node {
                ComponentValue::Function { name, args } => match relax(args, depth - 1) {
                    Relaxed::Same => v.node.clone(),
                    Relaxed::Unsure => return Relaxed::Unsure,
                    Relaxed::Read(args) => {
                        changed = true;
                        ComponentValue::Function {
                            name: name.clone(),
                            args,
                        }
                    }
                },
                ComponentValue::Block(b) => match relax(&b.values, depth - 1) {
                    Relaxed::Same => v.node.clone(),
                    Relaxed::Unsure => return Relaxed::Unsure,
                    Relaxed::Read(values) => {
                        changed = true;
                        ComponentValue::Block(SimpleBlock {
                            kind: b.kind,
                            values,
                        })
                    }
                },
                ComponentValue::Token(_) => v.node.clone(),
            };
            out.push(Spanned { node, span: v.span });
            i += 1;
            continue;
        };
        changed = true;
        let start = v.span.start;
        let mut end = v.span.end;
        let mut function = None;
        let mut j = i + 1;
        while let Some(n) = values.get(j) {
            match &n.node {
                ComponentValue::Token(Token::Delim(c)) if is_narrowed_out(*c) => {
                    head.push(c.encode_utf8(&mut [0; 4]));
                }
                ComponentValue::Token(Token::Delim('-')) => head.push("-"),
                ComponentValue::Token(Token::Ident(s)) => head.push(s),
                ComponentValue::Token(
                    Token::Number { repr, .. }
                    | Token::Dimension { repr, .. }
                    | Token::Percentage { repr, .. },
                ) if !repr.starts_with(['+', '.']) => {
                    let unit = match &n.node {
                        ComponentValue::Token(Token::Dimension { unit, .. }) => unit.as_ref(),
                        _ => "",
                    };
                    let is_percentage =
                        matches!(n.node, ComponentValue::Token(Token::Percentage { .. }));
                    if is_percentage || !all_name(repr) {
                        return Relaxed::Unsure;
                    }
                    head.push(repr);
                    head.push(unit);
                }
                ComponentValue::Function { name, args } => {
                    let Head::Ident(_) = head else {
                        return Relaxed::Unsure;
                    };
                    function = Some((name.as_ref(), args));
                    end = n.span.end;
                    j += 1;
                    break;
                }
                ComponentValue::Block(b) if b.kind == BlockKind::Paren => {
                    if let Head::Ident(_) = head {
                        function = Some(("", &b.values));
                        end = n.span.end;
                        j += 1;
                    }
                    break;
                }
                ComponentValue::Token(Token::Url(_) | Token::BadUrl(_) | Token::Cdc) => {
                    return Relaxed::Unsure;
                }
                _ => break,
            }
            end = n.span.end;
            j += 1;
        }
        let node = match (function, head) {
            (Some((name, args)), Head::Ident(mut acc)) => {
                acc.push_str(name);
                let args = match relax(args, depth - 1) {
                    Relaxed::Same => args.clone(),
                    Relaxed::Unsure => return Relaxed::Unsure,
                    Relaxed::Read(args) => args,
                };
                ComponentValue::Function {
                    name: Cow::Owned(acc),
                    args,
                }
            }
            (_, head) => head.token(),
        };
        out.push(Spanned {
            node,
            span: Span { start, end },
        });
        i = j;
    }
    if changed {
        Relaxed::Read(out)
    } else {
        Relaxed::Same
    }
}
