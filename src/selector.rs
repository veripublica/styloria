//! Selector-list syntax validation for a qualified rule's prelude.
//!
//! CSS Syntax Level 3 — which [`crate::parser`] implements — deliberately
//! does *not* look at what a qualified rule's prelude says. It groups the
//! prelude into component values and hands them on, so `a > > b { }` and
//! `[= x] { }` parse as happily as `a > b { }`. That is correct per the
//! Syntax spec, and useless to a tool that wants to report malformed CSS.
//!
//! This module reads that prelude as a **selector list** (Selectors Level 4
//! §3) and reports the places it cannot be one. It is a *syntactic* check
//! only — the shape of the selector, never its meaning:
//!
//! - it does not care whether `dvi` is a real element name (a nonexistent
//!   type selector is a semantic question, and an advisory one);
//! - it does not check pseudo-class or attribute *names* against any list,
//!   so `:hovr` and `[hrefff]` pass;
//! - it does not compute specificity or matching.
//!
//! **Deliberately permissive, in two named places.** Reporting a valid
//! selector as broken is far worse than missing a broken one — a false
//! positive lands on somebody's real book — so:
//!
//! 1. **Functional pseudo arguments are not inspected.** `:not()`, `:is()`,
//!    `:has()`, `:nth-child()` and friends take grammars of their own
//!    (`An+B of S`, relative selectors, forgiving selector lists) that keep
//!    growing; anything in the parentheses is accepted. The parentheses
//!    themselves still have to balance, which the tokenizer already ensures.
//! 2. **Unknown constructs that are merely newer than this code are
//!    accepted** — `&` nesting, `::part()`, a pseudo name nobody has heard
//!    of. Only shapes that no version of Selectors can produce are reported.

use crate::span::{Span, Spanned};
use crate::spanned::{ComponentValue, SyntaxError, SyntaxErrorKind};
use crate::token::Token;

/// Report every place a qualified rule's prelude fails to be a selector
/// list. An empty result means "this is a selector list as far as syntax
/// goes" — not that the selector matches anything.
pub fn validate_selector_list(prelude: &[Spanned<ComponentValue<'_>>]) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    // A prelude splits on top-level commas into complex selectors. An empty
    // side (`, p`, `a,`, `a,,b`) is the one comma error worth reporting.
    for part in split_on_commas(prelude, &mut errors) {
        validate_complex(part, &mut errors);
    }
    errors
}

/// The comma-separated slices of a prelude, reporting an empty one as it
/// goes. The error span is the comma itself: it is the token the author has
/// to look at.
fn split_on_commas<'p, 'a>(
    prelude: &'p [Spanned<ComponentValue<'a>>],
    errors: &mut Vec<SyntaxError>,
) -> Vec<&'p [Spanned<ComponentValue<'a>>]> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, cv) in prelude.iter().enumerate() {
        if matches!(&cv.node, ComponentValue::Token(Token::Comma)) {
            let part = &prelude[start..i];
            if is_blank(part) {
                errors.push(SyntaxError {
                    span: cv.span,
                    kind: SyntaxErrorKind::InvalidSelector,
                });
            } else {
                parts.push(part);
            }
            start = i + 1;
        }
    }
    let tail = &prelude[start..];
    // A prelude that is entirely whitespace is not a selector list either,
    // but the parser has no rule to attach that to and an empty prelude is
    // already an UnterminatedRule/UnexpectedToken case upstream. Only a
    // *trailing* comma (parts already non-empty) is reported here.
    if is_blank(tail) {
        if !parts.is_empty()
            && let Some(last) = prelude.last()
        {
            errors.push(SyntaxError {
                span: last.span,
                kind: SyntaxErrorKind::InvalidSelector,
            });
        }
    } else {
        parts.push(tail);
    }
    parts
}

/// A complex selector: compounds joined by combinators. Reports a combinator
/// with nothing on one side (`> p`, `a >`, `a > > b`).
fn validate_complex(part: &[Spanned<ComponentValue<'_>>], errors: &mut Vec<SyntaxError>) {
    let mut expect_compound = true;
    let mut i = 0;
    let mut saw_any = false;
    while i < part.len() {
        let cv = &part[i];
        if is_whitespace(&cv.node) {
            i += 1;
            continue;
        }
        if let Some(_c) = as_combinator(&cv.node) {
            if expect_compound {
                // Nothing to combine with on the left: a leading combinator,
                // or two in a row.
                errors.push(SyntaxError {
                    span: cv.span,
                    kind: SyntaxErrorKind::InvalidSelector,
                });
            }
            expect_compound = true;
            i += 1;
            continue;
        }
        // A compound selector: consume every simple selector that follows
        // without intervening whitespace.
        let consumed = validate_compound(part, i, errors);
        debug_assert!(consumed > 0, "validate_compound must always advance");
        i += consumed;
        expect_compound = false;
        saw_any = true;
    }
    // A trailing combinator has nothing on its right.
    if expect_compound
        && saw_any
        && let Some(last) = part.iter().rev().find(|cv| !is_whitespace(&cv.node))
    {
        errors.push(SyntaxError {
            span: last.span,
            kind: SyntaxErrorKind::InvalidSelector,
        });
    }
}

/// Validate one compound selector starting at `start`, returning how many
/// component values it consumed (always at least one, so the caller can't
/// loop forever). A compound runs until whitespace or a combinator.
fn validate_compound(
    part: &[Spanned<ComponentValue<'_>>],
    start: usize,
    errors: &mut Vec<SyntaxError>,
) -> usize {
    let mut i = start;
    while i < part.len() {
        let cv = &part[i];
        if is_whitespace(&cv.node) || as_combinator(&cv.node).is_some() {
            break;
        }
        i += validate_simple(part, i, errors);
    }
    (i - start).max(1)
}

/// Validate one simple selector, returning how many component values it took.
fn validate_simple(
    part: &[Spanned<ComponentValue<'_>>],
    i: usize,
    errors: &mut Vec<SyntaxError>,
) -> usize {
    let cv = &part[i];
    match &cv.node {
        // `E`, and the `ns|E` / `ns|*` qualified forms.
        ComponentValue::Token(Token::Ident(_)) => 1 + namespace_tail(part, i + 1, errors),
        // `#id`.
        ComponentValue::Token(Token::Hash { .. }) => 1,
        // `[attr…]` — the tokenizer already balanced the brackets.
        ComponentValue::Block(b) if b.kind == crate::parser::BlockKind::Square => {
            validate_attribute(&b.values, cv.span, errors);
            1
        }
        // `:pseudo`, `::pseudo`, `:func(…)`. The name has to be there; what
        // it is, and what is inside a function, is not this check's business.
        ComponentValue::Token(Token::Colon) => {
            let mut n = 1;
            if matches!(
                part.get(i + 1).map(|c| &c.node),
                Some(ComponentValue::Token(Token::Colon))
            ) {
                n += 1; // `::`
            }
            match part.get(i + n).map(|c| &c.node) {
                Some(ComponentValue::Token(Token::Ident(_)))
                | Some(ComponentValue::Function { .. }) => n + 1,
                _ => {
                    errors.push(SyntaxError {
                        span: cv.span,
                        kind: SyntaxErrorKind::InvalidSelector,
                    });
                    n
                }
            }
        }
        ComponentValue::Token(Token::Delim(c)) => match c {
            // `.class` — the class name must follow immediately.
            '.' => match part.get(i + 1).map(|c| &c.node) {
                Some(ComponentValue::Token(Token::Ident(_))) => 2,
                _ => {
                    errors.push(SyntaxError {
                        span: cv.span,
                        kind: SyntaxErrorKind::InvalidSelector,
                    });
                    1
                }
            },
            // `*`, and `*|E`.
            '*' => 1 + namespace_tail(part, i + 1, errors),
            // `|E` (no namespace) — the bare form of the qualified name.
            '|' => match part.get(i + 1).map(|c| &c.node) {
                Some(ComponentValue::Token(Token::Ident(_) | Token::Delim('*'))) => 2,
                _ => {
                    errors.push(SyntaxError {
                        span: cv.span,
                        kind: SyntaxErrorKind::InvalidSelector,
                    });
                    1
                }
            },
            // `&` is CSS Nesting's parent reference — newer than this code
            // has any business objecting to.
            '&' => 1,
            _ => {
                errors.push(SyntaxError {
                    span: cv.span,
                    kind: SyntaxErrorKind::InvalidSelector,
                });
                1
            }
        },
        // Anything else — a number, a string, a `( … )` — cannot start a
        // simple selector.
        _ => {
            errors.push(SyntaxError {
                span: cv.span,
                kind: SyntaxErrorKind::InvalidSelector,
            });
            1
        }
    }
}

/// After a type/universal selector, an immediately following `|` makes what
/// came before a namespace prefix, and an ident or `*` must follow it.
/// Returns how many extra component values were consumed.
fn namespace_tail(
    part: &[Spanned<ComponentValue<'_>>],
    i: usize,
    errors: &mut Vec<SyntaxError>,
) -> usize {
    let Some(cv) = part.get(i) else { return 0 };
    if !matches!(&cv.node, ComponentValue::Token(Token::Delim('|'))) {
        return 0;
    }
    // `a |= b` can't appear here (that lives inside `[ … ]`), so a `|` at
    // this point is always a namespace separator.
    match part.get(i + 1).map(|c| &c.node) {
        Some(ComponentValue::Token(Token::Ident(_) | Token::Delim('*'))) => 2,
        _ => {
            errors.push(SyntaxError {
                span: cv.span,
                kind: SyntaxErrorKind::InvalidSelector,
            });
            1
        }
    }
}

/// The inside of an `[ … ]` attribute selector: a (possibly
/// namespace-qualified) name, optionally followed by a matcher and a value,
/// optionally followed by a case-sensitivity flag.
fn validate_attribute(
    values: &[Spanned<ComponentValue<'_>>],
    bracket_span: Span,
    errors: &mut Vec<SyntaxError>,
) {
    // Index-based over the non-whitespace values: `[ns|href]` needs one
    // token of lookahead to tell a namespace separator from the `|=` matcher,
    // and that reads badly through an iterator.
    let v: Vec<&Spanned<ComponentValue<'_>>> = values
        .iter()
        .filter(|cv| !is_whitespace(&cv.node))
        .collect();
    let bad = |errors: &mut Vec<SyntaxError>, span: Span| {
        errors.push(SyntaxError {
            span,
            kind: SyntaxErrorKind::InvalidSelector,
        });
    };
    if v.is_empty() {
        // `[]`
        bad(errors, bracket_span);
        return;
    }

    // The attribute name: `href`, `ns|href`, `*|href`, `|href`.
    let mut i = 0;
    let name_ok = match &v[i].node {
        ComponentValue::Token(Token::Ident(_)) => {
            i += 1;
            // A `|` here is a namespace separator only when a name follows
            // it; `[a|="x"]` is the dash-match matcher, not a namespace.
            if is_delim(v.get(i), '|')
                && matches!(
                    v.get(i + 1).map(|c| &c.node),
                    Some(ComponentValue::Token(Token::Ident(_)))
                )
            {
                i += 2;
            }
            true
        }
        // `*|href` and `|href` - the prefix carries no name of its own.
        ComponentValue::Token(Token::Delim('*')) if is_delim(v.get(i + 1), '|') => {
            i += 2;
            let ok = matches!(
                v.get(i).map(|c| &c.node),
                Some(ComponentValue::Token(Token::Ident(_)))
            );
            i += usize::from(ok);
            ok
        }
        ComponentValue::Token(Token::Delim('|')) => {
            i += 1;
            let ok = matches!(
                v.get(i).map(|c| &c.node),
                Some(ComponentValue::Token(Token::Ident(_)))
            );
            i += usize::from(ok);
            ok
        }
        _ => false,
    };
    if !name_ok {
        bad(errors, v[0].span);
        return;
    }

    // Bare `[href]` is complete.
    let Some(matcher) = v.get(i) else { return };

    // A matcher is `=` alone, or one of ~ | ^ $ * immediately followed by `=`.
    let matcher_len = match &matcher.node {
        ComponentValue::Token(Token::Delim('=')) => 1,
        ComponentValue::Token(Token::Delim('~' | '|' | '^' | '$' | '*'))
            if is_delim(v.get(i + 1), '=') =>
        {
            2
        }
        _ => {
            bad(errors, matcher.span);
            return;
        }
    };
    i += matcher_len;

    // A value must follow: `[href=]` is the classic mistake.
    match v.get(i).map(|c| (&c.node, c.span)) {
        Some((ComponentValue::Token(Token::Ident(_) | Token::String(_)), _)) => {}
        Some((_, span)) => bad(errors, span),
        None => bad(errors, matcher.span),
    }
    // Anything after the value is a flag (`i`, `s`) - accepted without
    // checking which letter, since that list has grown before.
}

fn is_delim(cv: Option<&&Spanned<ComponentValue<'_>>>, c: char) -> bool {
    matches!(cv.map(|v| &v.node), Some(ComponentValue::Token(Token::Delim(d))) if *d == c)
}

fn is_whitespace(cv: &ComponentValue<'_>) -> bool {
    matches!(cv, ComponentValue::Token(Token::Whitespace))
}

fn is_blank(part: &[Spanned<ComponentValue<'_>>]) -> bool {
    part.iter().all(|cv| is_whitespace(&cv.node))
}

/// `>`, `+`, `~` and `||`. Descendant combination is plain whitespace and is
/// handled by the compound loop, not here.
fn as_combinator(cv: &ComponentValue<'_>) -> Option<char> {
    match cv {
        ComponentValue::Token(Token::Delim(c @ ('>' | '+' | '~'))) => Some(*c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::spanned::{SyntaxErrorKind, syntax_errors};

    /// Every selector error the module reports, for one stylesheet.
    fn sel_errors(css: &str) -> usize {
        syntax_errors(css)
            .into_iter()
            .filter(|e| e.kind == SyntaxErrorKind::InvalidSelector)
            .count()
    }

    /// The half that matters. A false positive here lands on somebody's real
    /// book, so this list is deliberately long and deliberately includes
    /// selectors newer than this parser: the rule is that anything we don't
    /// understand is *accepted*, not reported.
    #[test]
    fn valid_selectors_are_silent() {
        for css in [
            // The everyday shapes.
            "p { color: red }",
            "* { margin: 0 }",
            ".c { color: red }",
            "#id { color: red }",
            "a.c#id { color: red }",
            "div p { color: red }",
            "div > p { color: red }",
            "div + p { color: red }",
            "div ~ p { color: red }",
            "h1, h2, h3 { color: red }",
            "div > p ~ span + a { color: red }",
            // Attribute selectors, every matcher, with and without flags.
            "[hidden] { color: red }",
            "[href=x] { color: red }",
            "[href=\"x\"] { color: red }",
            "a[href^=\"http\"] { color: red }",
            "a[href$=\".pdf\"] { color: red }",
            "a[href*=\"x\"] { color: red }",
            "a[lang|=\"en\"] { color: red }",
            "a[class~=\"c\"] { color: red }",
            "a[href=\"x\" i] { color: red }",
            // Namespaces.
            "ns|E { color: red }",
            "*|E { color: red }",
            "|E { color: red }",
            "ns|* { color: red }",
            "[ns|href] { color: red }",
            "[*|href] { color: red }",
            // Pseudo-classes and -elements, including names we don't know.
            "a:hover { color: red }",
            "p::first-line { color: red }",
            "li:nth-child(2n+1) { color: red }",
            "p:not(.c) { color: red }",
            "p:is(h1, h2) { color: red }",
            "p:where(.a, .b) { color: red }",
            "a:has(> img) { color: red }",
            "p:lang(ja) { color: red }",
            ":root { color: red }",
            // Deliberately newer / unknown than this code: must be accepted.
            "::part(label) { color: red }",
            "li:nth-child(2n+1 of .c) { color: red }",
            "input::-webkit-search-cancel-button { color: red }",
            "p:future-pseudo-nobody-invented-yet { color: red }",
            "& .c { color: red }",
            "a:not(:has(> .x)) { color: red }",
        ] {
            assert_eq!(sel_errors(css), 0, "must be accepted: {css}");
        }
    }

    /// Shapes no version of Selectors can produce.
    #[test]
    fn malformed_selectors_are_reported() {
        for css in [
            // Combinator with nothing on one side.
            "> p { color: red }",
            "div > { color: red }",
            "div > > p { color: red }",
            // Empty side of a comma.
            ", p { color: red }",
            "h1, { color: red }",
            "h1,, h2 { color: red }",
            // A class or namespace separator with no name after it.
            ". { color: red }",
            "ns| { color: red }",
            "| { color: red }",
            // A colon with no pseudo name.
            "a: { color: red }",
            // Attribute selectors.
            "[] { color: red }",
            "[=x] { color: red }",
            "[href=] { color: red }",
            "[href~] { color: red }",
            // A token that cannot start a simple selector.
            "\"str\" { color: red }",
            "42 { color: red }",
        ] {
            assert!(sel_errors(css) > 0, "must be reported: {css}");
        }
    }

    /// The error has to point somewhere useful, not at the whole sheet.
    #[test]
    fn error_span_points_at_the_offending_token() {
        let css = "h1 > > h2 { color: red }";
        let errs: Vec<_> = syntax_errors(css)
            .into_iter()
            .filter(|e| e.kind == SyntaxErrorKind::InvalidSelector)
            .collect();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].span.slice(css), ">");
        assert_eq!(errs[0].span.start_line_col(css), (1, 6));
    }

    /// A UTF-8 BOM ahead of the stylesheet is a byte-order mark, not
    /// content. Left in the stream it tokenizes as a delim, which makes the
    /// `@charset` after it look like the start of a qualified rule's prelude
    /// and cascades into spurious selector errors — found by scanning
    /// epubcheck's own `bom-charset15.css` fixture, which is valid CSS.
    #[test]
    fn a_leading_bom_is_not_part_of_the_first_selector() {
        let css = "\u{FEFF}@charset \"iso-8859-15\";\n.hello { color: red }";
        assert_eq!(sel_errors(css), 0, "a BOM must not produce selector errors");
        let sheet = crate::spanned::parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 2, "@charset and .hello are two rules");
    }

    /// A selector error must not stop the rest of the sheet being parsed -
    /// the parser stays error-recovering, as it is everywhere else.
    #[test]
    fn a_bad_selector_does_not_swallow_the_stylesheet() {
        let sheet = crate::spanned::parse_stylesheet("> bad { color: red } p { color: blue }");
        assert_eq!(sheet.rules.len(), 2, "both rules still parse");
    }
}
