//! Semantic validation on top of the property-agnostic parser.
//!
//! The parser ([`crate::spanned`]) is deliberately property-blind: it will
//! happily build a declaration named `font-eight`, because *syntactically*
//! it is a perfectly good declaration. This layer adds the vocabulary — it
//! knows which property names CSS actually defines — and reports the ones it
//! does not recognise, each pinned to the exact `name_span` a tool can
//! underline.
//!
//! # Scope
//!
//! Declarations in qualified rules (`selector { … }`) are checked, including
//! qualified rules nested inside **conditional group rules** — `@media`,
//! `@supports`, `@container`, `@layer { … }`, `@scope` — which hold a rule
//! list, not declarations. Other at-rule bodies are left alone on purpose:
//! `@font-face`/`@page` hold *descriptors* (`src`, `size`) from a different
//! vocabulary, and flagging those would be a false positive.
//!
//! The guiding rule is asymmetric on purpose: **failing to flag an unknown
//! property is safe; flagging a real one is not.** So every exemption below
//! errs toward silence.

use crate::known_properties::KNOWN_PROPERTIES;
use crate::span::{Span, Spanned};
use crate::spanned::{self, ComponentValue, Rule, SimpleBlock};
use crate::token::Token;

/// One validation finding, located by the source [`Span`] it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The span to underline — for an unknown property, the property name.
    pub span: Span,
    pub kind: DiagnosticKind,
    /// The offending text as the author wrote it (original case preserved).
    pub name: String,
}

/// What a [`Diagnostic`] reports. One variant today; more as the layer grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A declaration whose property name CSS does not define, and which is
    /// neither a custom property (`--*`) nor vendor-prefixed (`-webkit-…`).
    UnknownProperty,
}

/// Validate a stylesheet's declarations, returning every finding in source
/// order. Parses `css` with [`spanned::parse_stylesheet`] and checks each
/// declaration in a qualified rule, descending into conditional group
/// at-rules (see the module docs for scope).
pub fn validate_stylesheet(css: &str) -> Vec<Diagnostic> {
    let sheet = spanned::parse_stylesheet(css);
    let mut out = Vec::new();
    validate_rules(&sheet.rules, css, &mut out);
    out
}

/// Check every qualified rule in `rules`, descending into conditional group
/// at-rules. `css` is the source the rules' spans index into.
fn validate_rules(rules: &[Spanned<Rule<'_>>], css: &str, out: &mut Vec<Diagnostic>) {
    for rule in rules {
        match &rule.node {
            Rule::Qualified(q) => check_block(&q.block.node, out),
            Rule::At(at) if is_conditional_group(&at.name) => {
                if let Some(block) = &at.block {
                    validate_group_body(&block.node, css, out);
                }
            }
            // Other at-rules (@font-face, @page, …) hold descriptors, a
            // different vocabulary — see the module-level docs.
            Rule::At(_) => {}
        }
    }
}

/// True for at-rules whose body is a list of *rules* (which may contain
/// qualified rules with declarations), as opposed to descriptors. Names are
/// ASCII case-insensitive.
fn is_conditional_group(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "media" | "supports" | "container" | "layer" | "scope"
    )
}

/// A conditional group rule's body is itself a rule list. Re-parse that inner
/// text with the full parser — so nested `@media`, `@supports` conditions,
/// and a `@font-face` sitting inside are all handled exactly as at top level —
/// then remap the sub-parse's spans back onto the original source.
fn validate_group_body(block: &SimpleBlock<'_>, css: &str, out: &mut Vec<Diagnostic>) {
    // The inner text runs from the first contained value to the last; this
    // is exact and brace-independent (it works for an unterminated block).
    let (Some(first), Some(last)) = (block.values.first(), block.values.last()) else {
        return;
    };
    let base = first.span.start;
    let inner = &css[base..last.span.end];
    let sub = spanned::parse_stylesheet(inner);
    let mut sub_diags = Vec::new();
    validate_rules(&sub.rules, inner, &mut sub_diags);
    for mut d in sub_diags {
        d.span = Span::new(d.span.start + base, d.span.end + base);
        out.push(d);
    }
}

/// Walk a style block's raw component values, splitting them into
/// declarations the way CSS Syntax §5.4.2/§5.4.5 does, and check each
/// declaration's property name. Every value already carries its absolute
/// span, so findings need no offset remapping.
fn check_block(block: &SimpleBlock<'_>, out: &mut Vec<Diagnostic>) {
    let vals = &block.values;
    let mut i = 0;
    while i < vals.len() {
        match &vals[i].node {
            ComponentValue::Token(Token::Whitespace | Token::Semicolon) => {
                i += 1;
            }
            // A nested at-rule inside a style block (e.g. a margin at-rule):
            // skip to its terminating `;` or its block, whichever comes first.
            ComponentValue::Token(Token::AtKeyword(_)) => {
                i += 1;
                while i < vals.len() {
                    match &vals[i].node {
                        ComponentValue::Token(Token::Semicolon) | ComponentValue::Block(_) => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            // A declaration begins with an ident. It is a real declaration
            // only if a `:` follows (after optional whitespace); otherwise the
            // ident is stray and we skip the run to the next `;` so that value
            // tokens are never mistaken for property names.
            ComponentValue::Token(Token::Ident(name)) => {
                let name_span = vals[i].span;
                let mut j = i + 1;
                while matches!(
                    vals.get(j).map(|v| &v.node),
                    Some(ComponentValue::Token(Token::Whitespace))
                ) {
                    j += 1;
                }
                let is_declaration = matches!(
                    vals.get(j).map(|v| &v.node),
                    Some(ComponentValue::Token(Token::Colon))
                );
                if is_declaration {
                    check_property(name, name_span, out);
                }
                // Advance past the whole declaration (or stray run): everything
                // up to the next `;` is its value, per §5.4.5.
                i = j;
                while i < vals.len()
                    && !matches!(&vals[i].node, ComponentValue::Token(Token::Semicolon))
                {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// Report `name` as unknown unless it is exempt. Property names are ASCII
/// case-insensitive, so the lookup is done in lower case.
fn check_property(name: &str, span: Span, out: &mut Vec<Diagnostic>) {
    // Any leading-dash name is exempt: `--*` is an author-defined custom
    // property, and `-webkit-`/`-moz-`/etc. are vendor extensions outside the
    // standard registry. No standard property starts with a dash, so this
    // exemption can never hide a typo of a real property.
    if name.starts_with('-') {
        return;
    }
    let lower = name.to_ascii_lowercase();
    if KNOWN_PROPERTIES.binary_search(&lower.as_str()).is_err() {
        out.push(Diagnostic {
            span,
            kind: DiagnosticKind::UnknownProperty,
            name: name.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown_names(css: &str) -> Vec<String> {
        validate_stylesheet(css)
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[test]
    fn known_property_is_clean() {
        assert!(validate_stylesheet("p { color: red }").is_empty());
    }

    #[test]
    fn misspelled_property_is_flagged() {
        // JSWolf's real case: `font-eight` for `font-weight`.
        let d = validate_stylesheet("p { font-eight: bold }");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiagnosticKind::UnknownProperty);
        assert_eq!(d[0].name, "font-eight");
    }

    #[test]
    fn flagged_span_points_at_the_property_name() {
        let css = "p { font-eight: bold }";
        let d = validate_stylesheet(css);
        assert_eq!(d[0].span.slice(css), "font-eight");
    }

    #[test]
    fn custom_property_is_exempt() {
        assert!(validate_stylesheet("p { --my-color: red }").is_empty());
    }

    #[test]
    fn vendor_prefixed_is_exempt() {
        assert!(validate_stylesheet("p { -webkit-hyphens: auto; -moz-nonsense: 1 }").is_empty());
    }

    #[test]
    fn property_name_is_case_insensitive() {
        assert!(validate_stylesheet("p { COLOR: red; Background: blue }").is_empty());
    }

    #[test]
    fn value_idents_are_not_mistaken_for_properties() {
        // `bold` and `red` are values, not property names.
        assert!(validate_stylesheet("p { font-weight: bold; color: red }").is_empty());
    }

    #[test]
    fn multiple_declarations_each_checked() {
        let names = unknown_names("p { colr: red; font-weight: bold; bckground: blue }");
        assert_eq!(names, vec!["colr", "bckground"]);
    }

    #[test]
    fn missing_semicolon_does_not_invent_a_property() {
        // With no `;`, `red font-eight: bold` is all one value per §5.4.5;
        // `font-eight` is a value token here, not a property.
        assert!(validate_stylesheet("p { color: red font-eight: bold }").is_empty());
    }

    #[test]
    fn at_rule_descriptors_are_not_flagged() {
        // `src` is a valid @font-face descriptor, not a property — and this
        // slice skips at-rule bodies entirely, so it must stay silent.
        let css = "@font-face { font-family: Foo; src: url(f.woff2) }";
        assert!(validate_stylesheet(css).is_empty());
    }

    #[test]
    fn declaration_inside_media_is_checked() {
        let css = "@media print { p { font-eight: bold } }";
        let d = validate_stylesheet(css);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "font-eight");
        // the remapped span still points at the property in the original text
        assert_eq!(d[0].span.slice(css), "font-eight");
    }

    #[test]
    fn declaration_inside_supports_is_checked() {
        let css = "@supports (display: grid) { .g { colr: red } }";
        let d = validate_stylesheet(css);
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["colr"]
        );
    }

    #[test]
    fn nested_conditional_groups_recurse() {
        let css = "@media screen { @supports (gap: 1px) { a { bckground: red } } }";
        let d = validate_stylesheet(css);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "bckground");
        assert_eq!(d[0].span.slice(css), "bckground");
    }

    #[test]
    fn font_face_inside_media_is_still_skipped() {
        // Descriptors stay exempt even nested in a group rule.
        let css = "@media print { @font-face { src: url(f.woff2) } p { color: red } }";
        assert!(validate_stylesheet(css).is_empty());
    }

    #[test]
    fn multiple_rules_in_a_group_are_all_checked() {
        let css = "@media all { a { colr: red } b { font-weight: bold } c { bg: blue } }";
        let names: Vec<_> = validate_stylesheet(css)
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, vec!["colr", "bg"]);
    }
}
