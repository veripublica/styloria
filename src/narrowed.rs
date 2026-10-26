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
            // The longer name may now start an ident: `#-‣`.
            Head::Hash(value, is_id) => Token::Hash {
                is_id: is_id || starts_ident(&value),
                value: value.into(),
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

/// Whether a name starts an ident, every code point at or above U+0080 a
/// name code point; escapes are already resolved.
fn starts_ident(name: &str) -> bool {
    let start = |c: char| c.is_ascii_alphabetic() || c == '_' || c >= '\u{80}';
    let mut chars = name.chars();
    match chars.next() {
        Some('-') => chars.next().is_some_and(|c| start(c) || c == '-'),
        Some(c) => start(c),
        None => false,
    }
}

/// Whether `values` hold a code point [`is_narrowed_out`], at any depth.
fn holds_one(values: &[Spanned<ComponentValue<'_>>]) -> bool {
    values.iter().any(|v| match &v.node {
        ComponentValue::Token(Token::Delim(c)) => is_narrowed_out(*c),
        ComponentValue::Function { args, .. } => holds_one(args),
        ComponentValue::Block(b) => holds_one(&b.values),
        ComponentValue::Token(_) => false,
    })
}

fn narrowed_out(v: &ComponentValue<'_>) -> Option<char> {
    match v {
        ComponentValue::Token(Token::Delim(c)) if is_narrowed_out(*c) => Some(*c),
        _ => None,
    }
}

/// Where the old tokenizer would have stopped reading a name in `repr`, a
/// number's text: at its first `.`, or at an exponent's `+`.
fn name_prefix(repr: &str) -> usize {
    repr.find(['.', '+']).unwrap_or(repr.len())
}

/// The number token the old tokenizer read after a name ended inside a
/// number's text: `.5` of `‣1.5`. `tail` is the token that text came from,
/// for its unit or `%`.
fn rest_number<'a>(rest: &'a str, tail: &Token<'a>) -> Token<'a> {
    let value = rest.parse::<f64>().unwrap_or(0.0);
    let num_type = if rest.contains(['.', 'e', 'E']) {
        NumericType::Number
    } else {
        NumericType::Integer
    };
    match tail {
        Token::Dimension { unit, .. } => Token::Dimension {
            value,
            num_type,
            unit: unit.clone(),
            repr: rest,
        },
        Token::Percentage { .. } => Token::Percentage { value, repr: rest },
        _ => Token::Number {
            value,
            num_type,
            repr: rest,
        },
    }
}

fn relaxed_or_same<'a>(
    values: &[Spanned<ComponentValue<'a>>],
    depth: usize,
) -> Option<Vec<Spanned<ComponentValue<'a>>>> {
    match relax(values, depth) {
        Relaxed::Same => Some(values.to_vec()),
        Relaxed::Unsure => None,
        Relaxed::Read(v) => Some(v),
    }
}

/// `values` read with every code point at or above U+0080 a name code
/// point, functions and blocks included, to `depth`.
///
/// Where the old tokenizer would have read `url(` on as part of a longer
/// name (`‣url(x)`), the result is a function by that name with no
/// arguments: no grammar and no pseudo-class names it, so what it holds
/// decides nothing.
pub(crate) fn relax<'a>(values: &[Spanned<ComponentValue<'a>>], depth: usize) -> Relaxed<'a> {
    if !holds_one(values) {
        return Relaxed::Same;
    }
    if depth == 0 {
        return Relaxed::Unsure;
    }
    // A name the old tokenizer ended inside a number leaves the rest of
    // that number to be read on, so the list is worked on in place.
    let mut values = values.to_vec();
    let is_out = |values: &[Spanned<ComponentValue<'a>>], k: usize| {
        values
            .get(k)
            .is_some_and(|n| narrowed_out(&n.node).is_some())
    };
    let mut out = Vec::with_capacity(values.len());
    let mut changed = false;
    let mut i = 0;
    while i < values.len() {
        let v = values[i].clone();
        let starts = is_out(&values, i) || is_out(&values, i + 1);
        let head = match &v.node {
            ComponentValue::Token(t) if starts => match t {
                Token::Delim(c) if is_narrowed_out(*c) => Some(Head::Ident(c.to_string())),
                Token::Ident(s) => Some(Head::Ident(s.to_string())),
                Token::Hash { value, is_id } => Some(Head::Hash(value.to_string(), *is_id)),
                Token::AtKeyword(s) => Some(Head::At(s.to_string())),
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
                Token::Delim('-') => Some(Head::Ident("-".into())),
                Token::Delim('#') => Some(Head::Hash(String::new(), true)),
                Token::Delim('@') => Some(Head::At(String::new())),
                _ => None,
            },
            _ => None,
        };
        let dash_then_out = matches!(
            values.get(i + 1).map(|n| &n.node),
            Some(ComponentValue::Token(Token::Delim('-')))
        ) && is_out(&values, i + 2);
        // A number takes a unit (`1‣`), and `@` an at-keyword's name, also
        // through a `-` that was a delim: `1-‣`, `@-‣`.
        let head = head.or_else(|| match &v.node {
            ComponentValue::Token(Token::Number {
                value,
                num_type,
                repr,
            }) if is_out(&values, i + 1) || dash_then_out => Some(Head::Dimension {
                value: *value,
                num_type: *num_type,
                repr,
                unit: String::new(),
            }),
            ComponentValue::Token(Token::Delim('@')) if dash_then_out => {
                Some(Head::At(String::new()))
            }
            _ => None,
        });
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
        // A function's arguments, when the name goes on to be one.
        let mut function = None;
        let mut j = i + 1;
        while let Some(n) = values.get(j).cloned() {
            // What is left of `n` once the name has ended inside it; it is
            // read on from where it stands.
            let mut left = None;
            match &n.node {
                ComponentValue::Token(Token::Delim(c)) if is_narrowed_out(*c) => {
                    head.push(c.encode_utf8(&mut [0; 4]));
                }
                ComponentValue::Token(Token::Delim('-')) => head.push("-"),
                ComponentValue::Token(Token::Ident(s)) => head.push(s),
                ComponentValue::Token(
                    t @ (Token::Number { repr, .. }
                    | Token::Dimension { repr, .. }
                    | Token::Percentage { repr, .. }),
                ) => {
                    let repr: &'a str = repr;
                    let cut = name_prefix(repr);
                    if cut == 0 {
                        break;
                    }
                    head.push(&repr[..cut]);
                    // `‣1e+3`: what follows the `+` may run on into the
                    // next token (`‣1e+3.5`), past what is rebuilt here.
                    if repr[cut..].starts_with('+') {
                        return Relaxed::Unsure;
                    }
                    if cut < repr.len() {
                        left = Some(rest_number(&repr[cut..], t));
                    } else {
                        match t {
                            Token::Dimension { unit, .. } => head.push(unit),
                            Token::Percentage { .. } => left = Some(Token::Delim('%')),
                            _ => {}
                        }
                    }
                }
                // `‣-->`: the name takes `--`, and `>` is left.
                ComponentValue::Token(Token::Cdc) => {
                    head.push("--");
                    left = Some(Token::Delim('>'));
                }
                ComponentValue::Token(Token::Url(_) | Token::BadUrl(_)) => {
                    head.push("url");
                    function = Some(Vec::new());
                    end = n.span.end;
                    j += 1;
                    break;
                }
                ComponentValue::Function { name, args } => {
                    head.push(name);
                    let Some(args) = relaxed_or_same(args, depth - 1) else {
                        return Relaxed::Unsure;
                    };
                    function = Some(args);
                    end = n.span.end;
                    j += 1;
                    break;
                }
                ComponentValue::Block(b) if b.kind == BlockKind::Paren => {
                    if let Head::Ident(_) = head {
                        let Some(args) = relaxed_or_same(&b.values, depth - 1) else {
                            return Relaxed::Unsure;
                        };
                        function = Some(args);
                        end = n.span.end;
                        j += 1;
                    }
                    break;
                }
                _ => break,
            }
            if let Some(t) = left {
                values[j].node = ComponentValue::Token(t);
                break;
            }
            end = n.span.end;
            j += 1;
        }
        let span = Span { start, end };
        match (function, head) {
            (Some(args), Head::Ident(name)) => out.push(Spanned {
                node: ComponentValue::Function {
                    name: Cow::Owned(name),
                    args,
                },
                span,
            }),
            // Only an ident goes on to be a function: after any other name,
            // `(` opens a block.
            (Some(values), head) => {
                out.push(Spanned {
                    node: head.token(),
                    span,
                });
                out.push(Spanned {
                    node: ComponentValue::Block(SimpleBlock {
                        kind: BlockKind::Paren,
                        values,
                    }),
                    span,
                });
            }
            (None, head) => out.push(Spanned {
                node: head.token(),
                span,
            }),
        }
        i = j;
    }
    if changed {
        Relaxed::Read(out)
    } else {
        Relaxed::Same
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value's component values, spans left out.
    fn shape<'a>(values: &[Spanned<ComponentValue<'a>>]) -> Vec<ComponentValue<'a>> {
        values
            .iter()
            .map(|v| match &v.node {
                ComponentValue::Function { name, args } => ComponentValue::Function {
                    name: name.clone(),
                    args: zeroed(args),
                },
                ComponentValue::Block(b) => ComponentValue::Block(SimpleBlock {
                    kind: b.kind,
                    values: zeroed(&b.values),
                }),
                t => t.clone(),
            })
            .collect()
    }

    fn zeroed<'a>(values: &[Spanned<ComponentValue<'a>>]) -> Vec<Spanned<ComponentValue<'a>>> {
        shape(values)
            .into_iter()
            .map(|node| Spanned {
                node,
                span: Span { start: 0, end: 0 },
            })
            .collect()
    }

    fn value(text: &str) -> Vec<Spanned<ComponentValue<'static>>> {
        let css: &'static str = Box::leak(format!("x: {text}").into_boxed_str());
        match crate::parse_block_contents(css).0.into_iter().next() {
            Some(crate::BlockItem::Declaration(d)) => d.node.value,
            other => panic!("{text:?}: {other:?}"),
        }
    }

    /// An escaped code point is a name code point under either reading, so
    /// `\‣1.5` read today is what `‣1.5` was read as before #7129.
    /// Whether `relax` reads `text` as the escaped text reads; `false` when
    /// it is unsure.
    #[track_caller]
    fn same_as_escaped(text: &str) -> bool {
        let escaped: String = text
            .chars()
            .flat_map(|c| {
                let esc = is_narrowed_out(c).then_some('\\');
                esc.into_iter().chain(std::iter::once(c))
            })
            .collect();
        let want = shape(&value(&escaped));
        let got = match relax(&value(text), 64) {
            Relaxed::Same => shape(&value(text)),
            Relaxed::Read(v) => shape(&v),
            Relaxed::Unsure => return false,
        };
        assert_eq!(got, want, "{text:?} against {escaped:?}");
        true
    }

    #[test]
    fn the_old_reading_is_the_escaped_one() {
        for text in [
            "‣",
            "a‣b",
            "‣1.5",
            "‣1.5px",
            "‣5%",
            "‣1.5%",
            "1‣",
            "1-‣",
            "1.5-‣x",
            "#a‣b",
            "#‣",
            "@‣",
            "-‣",
            "--‣",
            "@-‣",
            "‣-->",
            "‣b(1)",
            "‣(1)",
            "#a‣b(1)",
            "1px‣(2)",
            "a\u{A0}b",
            "f(‣ x, [‣])",
            "‣ ‣",
            "‣1.5◰",
            "1.5‣.5px",
        ] {
            assert!(same_as_escaped(text), "{text:?}: unsure");
        }
        // An exponent's sign is the one place left unsure.
        assert!(!same_as_escaped("‣1e+3"));
        // `‣url(x)` was a function `‣url`; its arguments are not rebuilt.
        for text in ["‣url(x)", "a‣url(x y)"] {
            let Relaxed::Read(v) = relax(&value(text), 64) else {
                panic!("{text:?}");
            };
            assert!(
                matches!(&v[..], [Spanned { node: ComponentValue::Function { name, args }, .. }]
                    if name.ends_with("‣url") && args.is_empty()),
                "{text:?}: {v:?}"
            );
        }
    }

    /// The same on 200,000 values made of pieces that meet in every order.
    #[test]
    fn the_old_reading_is_the_escaped_one_everywhere() {
        const PIECES: &[&str] = &[
            "‣", "◰", "\u{A0}", "a", "_", "-", "--", "+", ".", "1", "1.5", "5%", "1px", "1.5px",
            "1e+3", "1e3", "1-", "#", "@", " ", ",", "(", ")", "[", "]", "b(", "-->", "x", "é",
            "\\", "e", "%", "\"s\"", "!",
        ];
        let mut seed = 0x5EED_u64;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % n as u64) as usize
        };
        let (mut tried, mut unsure) = (0, 0);
        for _ in 0..200_000 {
            let len = 1 + next(7);
            let text: String = (0..len).map(|_| PIECES[next(PIECES.len())]).collect();
            // A trailing backslash escapes nothing; one before a narrowed
            // code point would already escape it.
            let escapes_one = text
                .split('\\')
                .skip(1)
                .any(|after| after.chars().next().is_none_or(is_narrowed_out));
            if escapes_one {
                continue;
            }
            tried += 1;
            if !same_as_escaped(&text) {
                assert!(text.contains("e+"), "{text:?}: unsure");
                unsure += 1;
            }
        }
        assert!(unsure * 50 < tried, "{unsure} of {tried} unsure");
    }
}
