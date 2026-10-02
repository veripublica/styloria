//! Semantic validation on top of the property-agnostic parser.
//!
//! The parser ([`crate::parser`]) is deliberately property-blind: it will
//! happily build a declaration named `font-eight`, because *syntactically*
//! it is a perfectly good declaration. This layer adds the vocabulary — it
//! knows which property names CSS actually defines — and reports the ones it
//! does not recognise, each pinned to the exact `name_span` a tool can
//! underline.
//!
//! # Scope
//!
//! Declarations in style rules (`selector { … }`) are checked against the set
//! of CSS properties, at any depth: inside conditional group rules (`@media`,
//! `@supports`, `@container`, `@layer`, `@scope`, …), nested in other style
//! rules, and in `@keyframes` blocks. A declaration directly inside a
//! conditional group rule is a property too (CSS Nesting's nested
//! declarations), and is checked as one.
//!
//! **Descriptor at-rules** are checked too, each against its own vocabulary:
//! `@font-face`, `@counter-style`, `@property`, `@font-palette-values`,
//! `@view-transition`. `@page` is special — it mixes its page descriptors with
//! ordinary properties, so it is checked against the union of both. An
//! at-rule this crate has no vocabulary for (`@font-feature-values`, an
//! unknown or newer one) is left alone, and so is everything inside it.
//!
//! For the contents of an inline `style="…"` attribute use
//! [`validate_declaration_list`], which checks against the property
//! vocabulary.
//!
//! The guiding rule is asymmetric on purpose: **failing to flag an unknown
//! name is safe; flagging a real one is not.** So every exemption below errs
//! toward silence.

use crate::descriptors::descriptors_for;
use crate::known_properties::KNOWN_PROPERTIES;
use crate::parser::{self, BlockItem, Rule, Stylesheet};
use crate::span::{Span, Spanned};

/// One validation finding, located by the source [`Span`] it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The span to underline — for an unknown property, the property name.
    pub span: Span,
    pub kind: DiagnosticKind,
    /// The offending text as the author wrote it (original case preserved).
    pub name: String,
}

/// What a [`Diagnostic`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A declaration in a style rule whose property name CSS does not define,
    /// and which is neither custom (`--*`) nor vendor-prefixed (`-webkit-…`).
    UnknownProperty,
    /// A declaration in an at-rule whose name that at-rule does not define as
    /// a descriptor. `at_rule` is the canonical at-rule name without the `@`
    /// (e.g. `"font-face"`).
    UnknownDescriptor { at_rule: &'static str },
}

/// The vocabulary a declaration's name is checked against — which set of
/// names is valid, and which [`DiagnosticKind`] an unknown one produces.
enum Vocab {
    /// A style rule: the name must be a known CSS property.
    Property,
    /// An at-rule: the name must be one of `names`. `at_rule` is the canonical
    /// name for the diagnostic. `allow_properties` is set only for `@page`,
    /// which also accepts ordinary properties alongside its descriptors.
    Descriptor {
        at_rule: &'static str,
        names: &'static [&'static str],
        allow_properties: bool,
    },
}

/// Validate a stylesheet's declarations, returning every finding in source
/// order. Parses `css` with [`parse_stylesheet`](crate::parse_stylesheet);
/// a caller that already holds the tree uses [`validate_parsed_stylesheet`]
/// instead and saves the second parse.
pub fn validate_stylesheet(css: &str) -> Vec<Diagnostic> {
    validate_parsed_stylesheet(&parser::parse_stylesheet(css).0)
}

/// [`validate_stylesheet`] over a tree that is already parsed.
pub fn validate_parsed_stylesheet(sheet: &Stylesheet<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for rule in &sheet.rules {
        check_rule(rule, &mut out);
    }
    out
}

/// Validate the contents of an inline `style="…"` attribute — read with
/// [`parse_block_contents`](crate::parse_block_contents) — against the CSS
/// property vocabulary. Each finding's span indexes directly into `css`.
pub fn validate_declaration_list(css: &str) -> Vec<Diagnostic> {
    validate_parsed_block(&parser::parse_block_contents(css).0)
}

/// [`validate_declaration_list`] over block items that are already parsed.
pub fn validate_parsed_block(items: &[BlockItem<'_>]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_items(items, &Vocab::Property, &mut out);
    out
}

fn check_rule(rule: &Spanned<Rule<'_>>, out: &mut Vec<Diagnostic>) {
    match &rule.node {
        Rule::Qualified(q) => check_items(&q.block.node, &Vocab::Property, out),
        Rule::At(at) => {
            let Some(block) = &at.block else { return };
            let bare = parser::unprefixed(&at.name).to_ascii_lowercase();
            if let Some((at_rule, names)) = descriptors_for(&bare) {
                // A descriptor at-rule (@font-face, @counter-style, …): its
                // declarations against that at-rule's own vocabulary.
                let vocab = Vocab::Descriptor {
                    at_rule,
                    names,
                    allow_properties: at_rule == "page",
                };
                check_items(&block.node, &vocab, out);
            } else if parser::holds_style_rules(&bare) || bare == "keyframes" {
                check_items(&block.node, &Vocab::Property, out);
            }
            // Any other at-rule's body means something this crate does not
            // know — left alone.
        }
    }
}

fn check_items(items: &[BlockItem<'_>], vocab: &Vocab, out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            BlockItem::Declaration(d) => check_name(&d.node.name, d.node.name_span, vocab, out),
            BlockItem::Rule(r) => check_rule(r, out),
        }
    }
}

/// Report `name` as unknown unless it is valid in `vocab` or exempt. Names are
/// ASCII case-insensitive, so the lookup is done in lower case.
fn check_name(name: &str, span: Span, vocab: &Vocab, out: &mut Vec<Diagnostic>) {
    // Any leading-dash name is exempt: `--*` is an author-defined custom
    // property, and `-webkit-`/`-moz-`/etc. are vendor extensions outside the
    // standard registry. No standard property or descriptor starts with a
    // dash, so this exemption can never hide a typo of a real name.
    if name.starts_with('-') {
        return;
    }
    let lower = name.to_ascii_lowercase();
    let is_property = || KNOWN_PROPERTIES.binary_search(&lower.as_str()).is_ok();
    let (valid, kind) = match vocab {
        Vocab::Property => (is_property(), DiagnosticKind::UnknownProperty),
        Vocab::Descriptor {
            at_rule,
            names,
            allow_properties,
        } => {
            let ok = names.binary_search(&lower.as_str()).is_ok()
                || (*allow_properties && is_property());
            (ok, DiagnosticKind::UnknownDescriptor { at_rule })
        }
    };
    if !valid {
        out.push(Diagnostic {
            span,
            kind,
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
    fn valid_font_face_descriptors_are_clean() {
        let css = "@font-face { font-family: Foo; src: url(f.woff2); font-weight: 700 }";
        assert!(validate_stylesheet(css).is_empty());
    }

    #[test]
    fn unknown_font_face_descriptor_is_flagged() {
        // A property that is NOT a @font-face descriptor: `color` is a real
        // property but meaningless in @font-face.
        let css = "@font-face { font-family: Foo; color: red }";
        let d = validate_stylesheet(css);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "color");
        assert_eq!(
            d[0].kind,
            DiagnosticKind::UnknownDescriptor {
                at_rule: "font-face"
            }
        );
        assert_eq!(d[0].span.slice(css), "color");
    }

    #[test]
    fn misspelled_font_face_descriptor_is_flagged() {
        let d = validate_stylesheet("@font-face { font-familly: Foo }");
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["font-familly"]
        );
    }

    #[test]
    fn page_accepts_both_descriptors_and_properties() {
        // `size`/`marks` are @page descriptors; `margin`/`color` are ordinary
        // properties @page also accepts — none should be flagged.
        let css = "@page { size: A4; marks: crop; margin: 1cm; color: black }";
        assert!(validate_stylesheet(css).is_empty());
    }

    #[test]
    fn page_flags_a_genuine_unknown() {
        let d = validate_stylesheet("@page { size: A4; bogus-thing: 1 }");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "bogus-thing");
        assert_eq!(
            d[0].kind,
            DiagnosticKind::UnknownDescriptor { at_rule: "page" }
        );
    }

    #[test]
    fn property_at_rule_descriptors_are_checked() {
        assert!(
            validate_stylesheet("@property --x { syntax: \"<color>\"; inherits: false }")
                .is_empty()
        );
        let d = validate_stylesheet("@property --x { syntax: \"<color>\"; nonsense: 1 }");
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["nonsense"]
        );
    }

    #[test]
    fn declaration_list_clean_when_all_known() {
        // The shape of an inline style="…" attribute: a bare declaration list.
        assert!(validate_declaration_list("color: red; font-weight: bold").is_empty());
    }

    #[test]
    fn declaration_list_flags_unknown_property() {
        let css = "color: red; font-eight: bold";
        let d = validate_declaration_list(css);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "font-eight");
        assert_eq!(d[0].kind, DiagnosticKind::UnknownProperty);
        assert_eq!(d[0].span.slice(css), "font-eight");
    }

    #[test]
    fn declaration_list_exempts_custom_and_vendor() {
        assert!(validate_declaration_list("--x: 1; -webkit-hyphens: auto").is_empty());
    }

    #[test]
    fn declaration_list_ignores_important_and_empty() {
        assert!(validate_declaration_list("color: red !important").is_empty());
        assert!(validate_declaration_list("").is_empty());
        assert!(validate_declaration_list("   ;  ; ").is_empty());
    }

    #[test]
    fn keyframes_blocks_hold_properties() {
        // @keyframes holds keyframe blocks (from/to/percent), not descriptors,
        // and those blocks' declarations ARE properties.
        assert!(validate_stylesheet("@keyframes spin { from { color: red } }").is_empty());
        let d = validate_stylesheet("@-webkit-keyframes spin { 50% { colr: red } }");
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["colr"]
        );
    }

    #[test]
    fn nested_style_rules_are_checked() {
        let css = "p { color: red; a:hover { colr: blue } & b { font-eight: 1 } }";
        let d = validate_stylesheet(css);
        assert_eq!(
            d.iter().map(|d| d.span.slice(css)).collect::<Vec<_>>(),
            ["colr", "font-eight"]
        );
    }

    #[test]
    fn an_unknown_at_rule_is_left_alone() {
        assert!(validate_stylesheet("@future { colr: red; p { bogus: 1 } }").is_empty());
        assert!(validate_stylesheet("@font-feature-values X { @swash { fancy: 1 } }").is_empty());
    }

    #[test]
    fn a_style_attribute_with_a_nested_rule_is_checked() {
        let css = "color: red; a { colr: blue }";
        let d = validate_declaration_list(css);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].span.slice(css), "colr");
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
    fn font_face_nested_in_media_is_descriptor_checked() {
        // Valid descriptors nested in a group rule stay clean...
        let ok = "@media print { @font-face { src: url(f.woff2) } p { color: red } }";
        assert!(validate_stylesheet(ok).is_empty());
        // ...and an invalid one is caught, with the right kind.
        let bad = "@media print { @font-face { srcc: url(f.woff2) } }";
        let d = validate_stylesheet(bad);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "srcc");
        assert_eq!(
            d[0].kind,
            DiagnosticKind::UnknownDescriptor {
                at_rule: "font-face"
            }
        );
        assert_eq!(d[0].span.slice(bad), "srcc");
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
