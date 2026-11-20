//! Which declarations the parsed stylesheet keeps: the CSS Object Model a
//! browser builds from the tree, asked one declaration at a time.
//!
//! [`parse_stylesheet`](crate::parse_stylesheet) keeps every declaration it
//! reads, but a browser drops many of them. It drops a declaration whose value
//! fits no grammar. It drops every declaration in a rule it drops: a style rule
//! whose selector does not parse or uses an undeclared namespace prefix, an
//! unknown at-rule, an at-rule whose prelude does not fit its grammar, a rule
//! where its context allows none. [`cssom_declarations`] tells, for each
//! declaration in source order, whether it is still there.
//!
//! The question is not whether the declaration styles anything. A rule inside
//! `@media not all` or `@supports not (display: block)` stays in the sheet,
//! and so do its declarations. A value with `var()` stays too: it waits for
//! substitution, which happens later, element by element. To ask what styles
//! an element, use [`Cascade`](crate::Cascade).
//!
//! ```
//! use styloria::{Kept, cssom_declarations, parse_stylesheet};
//!
//! let css = "p { direction: rtl } p:foo { direction: rtl } \
//!            @media not all { p { direction: var(--d) } }";
//! let (sheet, _) = parse_stylesheet(css);
//! let kept: Vec<bool> = cssom_declarations(&sheet)
//!     .iter()
//!     .map(|d| d.kept == Kept::Yes)
//!     .collect();
//! assert_eq!(kept, [true, false, true]);
//! ```
//!
//! # Where the answer is Unknown
//!
//! [`Kept::Unknown`] means that the specifications, or this crate, leave the
//! question open:
//!
//! - a value [`check_declaration`] cannot judge, except a substitution;
//! - a selector with a vendor-prefixed pseudo-class or pseudo-element, which
//!   one engine keeps and another drops;
//! - a selector that parses under one reading of w3c/csswg-drafts#7129 and
//!   not the other;
//! - a vendor-prefixed at-rule (`@-webkit-keyframes`, `@-moz-document`);
//! - a known at-rule whose contents this crate does not read
//!   (`@font-feature-values`, `@function`);
//! - a property in `@page` that CSS 2.1 does not define, where CSS Paged
//!   Media 3 §6 leaves the behavior undefined;
//! - an `animation-*` property in a keyframe, which CSS Animations 1 says is
//!   "ignored" without saying whether the parser drops it;
//! - `!important` on a descriptor, which no specification rules on.
//!
//! # Where this follows the specifications rather than a browser
//!
//! - `@page { direction: rtl }` is kept: CSS Paged Media 3 Appendix A.1 lists
//!   `direction` as a page property. Firefox 160 drops it.
//! - In `@font-face { p { color: red } font-family: x }` the rule is dropped
//!   and `font-family` is kept, as CSS Syntax 3 reads that block. Firefox 160
//!   drops both.

use crate::at_rule::{LayerRule, parse_layer_rule, supports_condition_parses};
use crate::descriptors::descriptors_for;
use crate::matching::Namespaces;
use crate::parser::{
    self, AtRule, BlockItem, BlockKind, ComponentValue, Declaration, Rule, Stylesheet,
};
use crate::selector::{
    BeforeNarrowing, PseudoClass, SelectorList, SimpleSelector, parse_relative_selector_list,
    parse_selector_list, selector_list_before_7129,
};
use crate::span::{Span, Spanned};
use crate::token::Token;
use crate::values::{UnknownValue, ValueCheck, check_declaration, check_descriptor};

/// Whether the parsed stylesheet keeps a declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    Yes,
    /// It is dropped. `span` is what drops it: the declaration itself for its
    /// value or its place, the selector or the at-rule's name for a rule that
    /// takes it along. When more than one thing drops it, this is the
    /// outermost.
    No {
        reason: DropReason,
        span: Span,
    },
    /// The specifications leave it open, or this crate cannot tell. `span`
    /// is the construct that is in doubt, the outermost one. A drop inside
    /// it still makes the answer [`No`](Self::No).
    Unknown {
        reason: KeptUnknown,
        span: Span,
    },
}

/// Why a declaration is not in the parsed stylesheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// The value fits no grammar of the property or descriptor. A value with
    /// a bad-string or bad-url token, or an empty one, is among these.
    InvalidValue,
    /// A style rule's selector does not parse, or a keyframe's or a page
    /// rule's selector does not fit its grammar.
    InvalidSelector,
    /// A style rule's selector uses a namespace prefix no `@namespace`
    /// declares.
    UndeclaredPrefix,
    /// An at-rule no specification defines.
    UnknownAtRule,
    /// A known at-rule whose prelude does not fit its grammar
    /// (`@keyframes none`, `@container !!!`, `@font-face foo`), or a
    /// statement at-rule written with a block (`@import url(x) { … }`).
    InvalidAtRule,
    /// An `@layer` whose prelude is not one layer name or none.
    InvalidLayer,
    /// An `@supports` whose prelude does not fit `<supports-condition>`. A
    /// condition that is only false drops nothing.
    InvalidSupports,
    /// The declaration or rule stands where its context allows none: a
    /// property in a descriptor block, a non-page property in `@page`, a
    /// declaration directly in `@keyframes` or in a top-level `@media`, an
    /// `@font-face` inside a style rule, a rule in a `style` attribute.
    NotAllowedHere,
    /// `!important` where it makes the declaration invalid: in a keyframe
    /// (CSS Animations 1), in `@position-try` (CSS Anchor Positioning 1).
    ImportantNotAllowed,
}

/// Why it cannot be told whether a declaration is in the parsed stylesheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeptUnknown {
    /// [`check_declaration`] or [`check_descriptor`] cannot judge the value.
    /// Never [`UnknownValue::Substitution`] for a property, which is kept, nor
    /// [`UnknownValue::BadToken`] or [`UnknownValue::Empty`], which are not.
    Value(UnknownValue),
    /// The selector holds a vendor-prefixed pseudo-class or pseudo-element.
    VendorSelector,
    /// The selector parses under one reading of w3c/csswg-drafts#7129 and
    /// not under the other.
    Selector7129,
    /// A vendor-prefixed at-rule.
    NonstandardAtRule,
    /// A known at-rule whose contents this crate does not read.
    UnreadAtRule,
    /// A page selector with a function (`:nth()`, CSS GCPM 3).
    PageSelector,
    /// A property in `@page` or a page-margin rule that CSS 2.1 does not
    /// define: CSS Paged Media 3 §6 leaves its behavior undefined.
    PageUndefined,
    /// An `animation-*` property other than `animation-timing-function` in a
    /// keyframe: CSS Animations 1 §3 says it is ignored.
    KeyframeAnimationProperty,
    /// `!important` on a descriptor.
    ImportantDescriptor,
}

/// Where a declaration stands, which decides what it may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationContext<'s> {
    /// A style rule, a nested group rule, `@scope`, a `style` attribute, or a
    /// place where no declaration may stand (it is then dropped).
    Style,
    /// A keyframe of `@keyframes`.
    Keyframe,
    /// `@page`.
    Page,
    /// A page-margin rule, named without its `@` (`"top-center"`).
    PageMargin(&'s str),
    /// A descriptor at-rule, named without its `@` (`"font-face"`).
    Descriptors(&'s str),
    /// `@position-try`.
    PositionTry,
    /// An at-rule whose contents this crate does not read, or one it drops
    /// whole.
    Unread,
}

/// One declaration and whether the parsed stylesheet keeps it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CssomDeclaration<'s, 'a> {
    pub declaration: &'s Spanned<Declaration<'a>>,
    pub context: DeclarationContext<'s>,
    pub kept: Kept,
}

/// Every declaration in `sheet`, in source order, and whether the parsed
/// stylesheet keeps it.
pub fn cssom_declarations<'s, 'a>(sheet: &'s Stylesheet<'a>) -> Vec<CssomDeclaration<'s, 'a>> {
    let namespaces = Namespaces::from_stylesheet(sheet);
    let mut walk = Walk {
        namespaces: &namespaces,
        out: Vec::new(),
    };
    for rule in &sheet.rules {
        walk.rule(rule, Holder::Top, Kept::Yes);
    }
    walk.out
}

/// [`cssom_declarations`] for a `style` attribute's contents, as
/// [`parse_block_contents`](crate::parse_block_contents) gives them. The
/// attribute holds declarations only, so a rule in it is dropped.
pub fn cssom_block_declarations<'s, 'a>(
    items: &'s [BlockItem<'a>],
) -> Vec<CssomDeclaration<'s, 'a>> {
    let namespaces = Namespaces::default();
    let mut walk = Walk {
        namespaces: &namespaces,
        out: Vec::new(),
    };
    walk.items(items, Holder::Attribute, Kept::Yes);
    walk.out
}

impl Kept {
    fn no(reason: DropReason, span: Span) -> Kept {
        Kept::No { reason, span }
    }

    fn unknown(reason: KeptUnknown, span: Span) -> Kept {
        Kept::Unknown { reason, span }
    }

    /// This verdict, from an outer construct, followed by `inner`'s: a drop
    /// anywhere wins, the outermost first; then a doubt, the outermost first.
    fn then(self, inner: Kept) -> Kept {
        match (self, inner) {
            (Kept::No { .. }, _) => self,
            (_, Kept::No { .. }) => inner,
            (Kept::Unknown { .. }, _) => self,
            (Kept::Yes, _) => inner,
        }
    }
}

/// The block a declaration or rule stands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder<'s> {
    /// The stylesheet itself.
    Top,
    /// A conditional group rule, `@layer`, `@scope` or `@starting-style`.
    /// `relative`: its style rules take relative selectors and it may hold
    /// declarations (it is in a style rule or in `@scope`). `in_style`: it is
    /// in a style rule, which allows only group rules inside.
    Group {
        relative: bool,
        in_style: bool,
    },
    Style,
    Attribute,
    Keyframes,
    Keyframe,
    Page,
    Margin(&'s str),
    Descriptors(&'static str),
    PositionTry,
    Unread,
}

impl<'s> Holder<'s> {
    fn context(self) -> DeclarationContext<'s> {
        match self {
            Holder::Keyframe => DeclarationContext::Keyframe,
            Holder::Page => DeclarationContext::Page,
            Holder::Margin(n) => DeclarationContext::PageMargin(n),
            Holder::Descriptors(n) => DeclarationContext::Descriptors(n),
            Holder::PositionTry => DeclarationContext::PositionTry,
            Holder::Unread => DeclarationContext::Unread,
            _ => DeclarationContext::Style,
        }
    }

    fn in_style(self) -> bool {
        matches!(self, Holder::Style | Holder::Group { in_style: true, .. })
    }
}

struct Walk<'n, 's, 'a> {
    namespaces: &'n Namespaces<'a>,
    out: Vec<CssomDeclaration<'s, 'a>>,
}

impl<'s, 'a> Walk<'_, 's, 'a> {
    fn items(&mut self, items: &'s [BlockItem<'a>], holder: Holder<'s>, outer: Kept) {
        for item in items {
            match item {
                BlockItem::Declaration(d) => self.out.push(CssomDeclaration {
                    declaration: d,
                    context: holder.context(),
                    kept: outer.then(declaration(d, holder)),
                }),
                BlockItem::Rule(r) => self.rule(r, holder, outer),
            }
        }
    }

    fn rule(&mut self, rule: &'s Spanned<Rule<'a>>, holder: Holder<'s>, outer: Kept) {
        match &rule.node {
            Rule::Qualified(q) => {
                let span = prelude_span(&q.prelude).unwrap_or(rule.span);
                let (verdict, inner) = match holder {
                    Holder::Top
                    | Holder::Group {
                        relative: false, ..
                    } => (self.selector(&q.prelude, false, span), Holder::Style),
                    Holder::Style | Holder::Group { relative: true, .. } => {
                        (self.selector(&q.prelude, true, span), Holder::Style)
                    }
                    Holder::Keyframes => (keyframe_selector(&q.prelude, span), Holder::Keyframe),
                    Holder::Unread => (Kept::Yes, Holder::Unread),
                    _ => (Kept::no(DropReason::NotAllowedHere, span), Holder::Style),
                };
                self.items(&q.block.node, inner, outer.then(verdict));
            }
            Rule::At(at) => {
                let Some(block) = &at.block else { return };
                let (verdict, inner) = self.at_rule(at, holder);
                self.items(&block.node, inner, outer.then(verdict));
            }
        }
    }

    /// An at-rule with a block: whether it stays, and what its block holds.
    fn at_rule(&self, at: &'s AtRule<'a>, holder: Holder<'s>) -> (Kept, Holder<'s>) {
        let span = at.name_span;
        if holder == Holder::Unread {
            return (Kept::Yes, Holder::Unread);
        }
        let bare = parser::unprefixed(&at.name).to_ascii_lowercase();
        let prefixed = bare.len() != at.name.len();
        let no = |reason| (Kept::no(reason, span), Holder::Unread);
        if !crate::at_rule::is_known_at_rule(&at.name) {
            return if prefixed {
                (
                    Kept::unknown(KeptUnknown::NonstandardAtRule, span),
                    Holder::Unread,
                )
            } else {
                no(DropReason::UnknownAtRule)
            };
        }
        // A prefixed name some engine may keep: in doubt, but read as the
        // unprefixed rule, so that a drop inside it still counts.
        let doubt = if prefixed {
            Kept::unknown(KeptUnknown::NonstandardAtRule, span)
        } else {
            Kept::Yes
        };
        let group = GROUP_RULES.contains(&bare.as_str());
        let margin = MARGIN_RULES.contains(&bare.as_str());
        let allowed = match holder {
            Holder::Top
            | Holder::Group {
                in_style: false, ..
            } => !margin && !in_feature_values(&bare),
            Holder::Style | Holder::Group { in_style: true, .. } => group,
            Holder::Page => margin,
            _ => false,
        };
        if !allowed {
            return no(DropReason::NotAllowedHere);
        }
        let p = &at.prelude;
        let (verdict, inner) = match bare.as_str() {
            // Nothing in it is CSS this crate knows: no engine has it unprefixed.
            "document" if !prefixed => return no(DropReason::UnknownAtRule),
            "import" | "namespace" | "charset" | "custom-media" => {
                return no(DropReason::InvalidAtRule);
            }
            "media" | "document" => (Kept::Yes, self.group(holder, false)),
            "supports" if !supports_condition_parses(p) => return no(DropReason::InvalidSupports),
            "supports" => (Kept::Yes, self.group(holder, false)),
            "layer" => match parse_layer_rule(at) {
                Some(LayerRule::Block(_) | LayerRule::Anonymous) => {
                    (Kept::Yes, self.group(holder, false))
                }
                _ => return no(DropReason::InvalidLayer),
            },
            "container" => (
                valid_or(container_prelude(p), span),
                self.group(holder, false),
            ),
            "scope" => (
                valid_or(self.scope_prelude(p), span),
                self.group(holder, true),
            ),
            "starting-style" => (valid_or(empty(p), span), self.group(holder, false)),
            "keyframes" => (valid_or(keyframes_name(p), span), Holder::Keyframes),
            "page" => (page_selectors(p, span), Holder::Page),
            "position-try" => (valid_or(dashed_ident(p), span), Holder::PositionTry),
            m if margin => (valid_or(empty(p), span), Holder::Margin(m_static(m))),
            d => match descriptors_for(d) {
                Some((name, _)) => (
                    valid_or(descriptor_prelude(name, p), span),
                    Holder::Descriptors(name),
                ),
                None => (
                    Kept::unknown(KeptUnknown::UnreadAtRule, span),
                    Holder::Unread,
                ),
            },
        };
        (doubt.then(verdict), inner)
    }

    fn group(&self, holder: Holder<'s>, scope: bool) -> Holder<'s> {
        let in_style = holder.in_style();
        let relative = scope || in_style || matches!(holder, Holder::Group { relative: true, .. });
        Holder::Group { relative, in_style }
    }

    /// A style rule's selector list: dropped when it does not parse or uses an
    /// undeclared prefix, in doubt when #7129's two readings disagree or a
    /// vendor-prefixed pseudo is in it.
    fn selector(
        &self,
        prelude: &[Spanned<ComponentValue<'a>>],
        relative: bool,
        span: Span,
    ) -> Kept {
        let parsed = if relative {
            parse_relative_selector_list(prelude)
        } else {
            parse_selector_list(prelude)
        };
        let now = match parsed {
            Ok(list) => match list.undeclared_prefix(self.namespaces) {
                Some(at) => Err(Kept::no(DropReason::UndeclaredPrefix, at)),
                None => Ok(list),
            },
            Err(_) => Err(Kept::no(DropReason::InvalidSelector, span)),
        };
        let before_kept = match selector_list_before_7129(prelude, relative) {
            BeforeNarrowing::Same => now.is_ok(),
            BeforeNarrowing::Parsed(list) => list.undeclared_prefix(self.namespaces).is_none(),
            BeforeNarrowing::Dropped => false,
        };
        match now {
            Ok(_) if !before_kept => Kept::unknown(KeptUnknown::Selector7129, span),
            Err(_) if before_kept => Kept::unknown(KeptUnknown::Selector7129, span),
            Err(no) => no,
            Ok(list) if has_vendor_pseudo(&list) => {
                Kept::unknown(KeptUnknown::VendorSelector, span)
            }
            Ok(_) => Kept::Yes,
        }
    }

    /// `[ ( <scope-start> ) ]? [ to ( <scope-end> ) ]?` (CSS Cascade 6).
    fn scope_prelude(&self, prelude: &[Spanned<ComponentValue<'a>>]) -> bool {
        let v = significant(prelude);
        let selectors = |cv: &Spanned<ComponentValue<'a>>| match &cv.node {
            ComponentValue::Block(b) if b.kind == BlockKind::Paren => {
                let ok = |list: SelectorList<'a>| list.undeclared_prefix(self.namespaces).is_none();
                parse_selector_list(&b.values).is_ok_and(ok)
                    || parse_relative_selector_list(&b.values).is_ok_and(ok)
            }
            _ => false,
        };
        let rest = match v.as_slice() {
            [first, rest @ ..] if matches!(first.node, ComponentValue::Block(_)) => {
                if !selectors(first) {
                    return false;
                }
                rest
            }
            all => all,
        };
        match rest {
            [] => true,
            [to, end] => is_ident(to, "to") && selectors(end),
            _ => false,
        }
    }
}

/// One declaration, on its own terms in `holder`.
fn declaration(d: &Spanned<Declaration<'_>>, holder: Holder<'_>) -> Kept {
    let name = d.node.name.to_ascii_lowercase();
    let not_here = Kept::no(DropReason::NotAllowedHere, d.span);
    match holder {
        Holder::Style | Holder::Attribute | Holder::Group { relative: true, .. } => property(d),
        Holder::Top
        | Holder::Group {
            relative: false, ..
        }
        | Holder::Keyframes => not_here,
        Holder::Keyframe if d.node.important => Kept::no(DropReason::ImportantNotAllowed, d.span),
        Holder::Keyframe
            if name.starts_with("animation") && name != "animation-timing-function" =>
        {
            Kept::unknown(KeptUnknown::KeyframeAnimationProperty, d.span).then(property(d))
        }
        Holder::Keyframe => property(d),
        Holder::Page => page_declaration(d, &name, PAGE_PROPERTIES),
        Holder::Margin(_) => page_declaration(d, &name, MARGIN_PROPERTIES),
        Holder::Descriptors(at_rule) => descriptor(d, &name, at_rule),
        Holder::PositionTry if d.node.important => {
            Kept::no(DropReason::ImportantNotAllowed, d.span)
        }
        Holder::PositionTry if position_try_accepts(&name) => property(d),
        Holder::PositionTry if is_prefixed(&name) => {
            Kept::unknown(KeptUnknown::Value(UnknownValue::PrefixedProperty), d.span)
        }
        Holder::PositionTry => not_here,
        Holder::Unread => Kept::Yes,
    }
}

/// A property's declaration, judged by its value.
fn property(d: &Spanned<Declaration<'_>>) -> Kept {
    from_check(check_declaration(&d.node), d.span, true)
}

fn from_check(check: ValueCheck, span: Span, property: bool) -> Kept {
    match check {
        ValueCheck::Valid => Kept::Yes,
        ValueCheck::Unknown(UnknownValue::Substitution) if property => Kept::Yes,
        ValueCheck::Invalid | ValueCheck::Unknown(UnknownValue::BadToken | UnknownValue::Empty) => {
            Kept::no(DropReason::InvalidValue, span)
        }
        ValueCheck::Unknown(u) => Kept::unknown(KeptUnknown::Value(u), span),
    }
}

/// A descriptor at-rule's declaration: one of its descriptors, judged by its
/// value, or dropped.
fn descriptor(d: &Spanned<Declaration<'_>>, name: &str, at_rule: &str) -> Kept {
    let names = descriptors_for(at_rule).map_or(&[][..], |(_, n)| n);
    if names.binary_search(&name).is_err() {
        return if is_prefixed(name) {
            Kept::unknown(KeptUnknown::Value(UnknownValue::PrefixedProperty), d.span)
        } else {
            Kept::no(DropReason::NotAllowedHere, d.span)
        };
    }
    let value = from_check(
        check_descriptor(at_rule, &d.node.name, &d.node.value),
        d.span,
        false,
    );
    if d.node.important {
        Kept::unknown(KeptUnknown::ImportantDescriptor, d.span).then(value)
    } else {
        value
    }
}

/// `@page` and its margin rules (CSS Paged Media 3 §4.3, §6, Appendix A):
/// `@page`'s descriptors, and the properties the appendix lists for the
/// context. Another CSS 2.1 property does not apply there, and the rule "can
/// only contain page properties". One CSS 2.1 does not define is left
/// undefined.
fn page_declaration(d: &Spanned<Declaration<'_>>, name: &str, list: &[&str]) -> Kept {
    let page = descriptors_for("page").map_or(&[][..], |(_, n)| n);
    if list == PAGE_PROPERTIES && page.binary_search(&name).is_ok() {
        return from_check(
            check_descriptor("page", &d.node.name, &d.node.value),
            d.span,
            false,
        );
    }
    if list.binary_search(&name).is_ok() {
        return property(d);
    }
    if CSS21_PROPERTIES.binary_search(&name).is_ok() {
        return Kept::no(DropReason::NotAllowedHere, d.span);
    }
    Kept::unknown(KeptUnknown::PageUndefined, d.span).then(property(d))
}

fn valid_or(ok: bool, span: Span) -> Kept {
    if ok {
        Kept::Yes
    } else {
        Kept::no(DropReason::InvalidAtRule, span)
    }
}

fn significant<'v, 'a>(
    values: &'v [Spanned<ComponentValue<'a>>],
) -> Vec<&'v Spanned<ComponentValue<'a>>> {
    values
        .iter()
        .filter(|cv| !matches!(cv.node, ComponentValue::Token(Token::Whitespace)))
        .collect()
}

fn prelude_span(prelude: &[Spanned<ComponentValue<'_>>]) -> Option<Span> {
    let v = significant(prelude);
    Some(Span::new(v.first()?.span.start, v.last()?.span.end))
}

fn empty(prelude: &[Spanned<ComponentValue<'_>>]) -> bool {
    significant(prelude).is_empty()
}

fn is_ident(cv: &Spanned<ComponentValue<'_>>, word: &str) -> bool {
    matches!(&cv.node, ComponentValue::Token(Token::Ident(w)) if w.eq_ignore_ascii_case(word))
}

fn is_prefixed(name: &str) -> bool {
    !name.starts_with("--") && parser::unprefixed(name).len() != name.len()
}

/// `<custom-ident>`: an identifier that is none of the CSS-wide keywords,
/// `default`, or `also`.
fn custom_ident<'v>(cv: &'v Spanned<ComponentValue<'_>>, also: &[&str]) -> Option<&'v str> {
    match &cv.node {
        ComponentValue::Token(Token::Ident(w))
            if !CSS_WIDE_AND_DEFAULT
                .iter()
                .chain(also)
                .any(|k| w.eq_ignore_ascii_case(k)) =>
        {
            Some(w)
        }
        _ => None,
    }
}

const CSS_WIDE_AND_DEFAULT: &[&str] = &[
    "inherit",
    "initial",
    "unset",
    "revert",
    "revert-layer",
    "revert-rule",
    "default",
];

/// `<keyframes-name>`: a `<custom-ident>` other than `none`, or a string.
fn keyframes_name(prelude: &[Spanned<ComponentValue<'_>>]) -> bool {
    match significant(prelude).as_slice() {
        [one] => {
            matches!(one.node, ComponentValue::Token(Token::String(_)))
                || custom_ident(one, &["none"]).is_some()
        }
        _ => false,
    }
}

fn dashed_ident(prelude: &[Spanned<ComponentValue<'_>>]) -> bool {
    matches!(significant(prelude).as_slice(),
        [one] if matches!(&one.node, ComponentValue::Token(Token::Ident(w)) if w.starts_with("--")))
}

/// The preludes of the descriptor at-rules: `@counter-style
/// <counter-style-name>`, `@property <custom-property-name>`,
/// `@font-palette-values <dashed-ident>`, and nothing for the others.
fn descriptor_prelude(at_rule: &str, prelude: &[Spanned<ComponentValue<'_>>]) -> bool {
    match at_rule {
        // CSS Counter Styles 3 §3: these names cannot be redefined.
        "counter-style" => matches!(significant(prelude).as_slice(), [one] if custom_ident(one, &[
            "none", "decimal", "disc", "square", "circle", "disclosure-open", "disclosure-closed",
        ]).is_some()),
        "property" | "font-palette-values" => dashed_ident(prelude),
        _ => empty(prelude),
    }
}

/// `<container-condition>#` (CSS Containment 3 §5.1): each item an optional
/// container name and an optional query, at least one of them. A query is
/// `not X`, or `X` joined by one of `and` and `or`, where `X` is a
/// parenthesized block or a function (`<general-enclosed>` takes any).
fn container_prelude(prelude: &[Spanned<ComponentValue<'_>>]) -> bool {
    let v = significant(prelude);
    v.split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma)))
        .all(|item| {
            let query = match item {
                [name, rest @ ..]
                    if custom_ident(name, &["none", "and", "not", "or"]).is_some() =>
                {
                    rest
                }
                [] => return false,
                all => all,
            };
            let operand = |cv: &&Spanned<ComponentValue<'_>>| {
                matches!(&cv.node, ComponentValue::Function { .. })
                    || matches!(&cv.node, ComponentValue::Block(b) if b.kind == BlockKind::Paren)
            };
            match query {
                [] => item.len() == 1,
                [not, x] if is_ident(not, "not") => operand(x),
                [first, rest @ ..] => {
                    let joiner = rest.first().map(|w| is_ident(w, "and"));
                    operand(first)
                        && rest.len() % 2 == 0
                        && rest.chunks(2).all(|pair| {
                            operand(&pair[1])
                                && if joiner == Some(true) {
                                    is_ident(pair[0], "and")
                                } else {
                                    is_ident(pair[0], "or")
                                }
                        })
                }
            }
        })
}

/// `<keyframe-selector>#`: `from`, `to`, a percentage from 0% to 100%, or a
/// timeline range name and a percentage (Scroll-driven Animations 1 §4.3).
fn keyframe_selector(prelude: &[Spanned<ComponentValue<'_>>], span: Span) -> Kept {
    let percent = |cv: &Spanned<ComponentValue<'_>>, bounded: bool| {
        matches!(cv.node, ComponentValue::Token(Token::Percentage { value, .. })
            if !bounded || (0.0..=100.0).contains(&value))
    };
    let v = significant(prelude);
    let ok = !v.is_empty()
        && v.split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma)))
            .all(|item| match item {
                [one] => is_ident(one, "from") || is_ident(one, "to") || percent(one, true),
                [range, p] => {
                    TIMELINE_RANGES.iter().any(|r| is_ident(range, r)) && percent(p, false)
                }
                _ => false,
            });
    if ok {
        Kept::Yes
    } else {
        Kept::no(DropReason::InvalidSelector, span)
    }
}

const TIMELINE_RANGES: &[&str] = &[
    "cover",
    "contain",
    "entry",
    "exit",
    "entry-crossing",
    "exit-crossing",
    "scroll",
];

/// `<page-selector-list>?` (CSS Paged Media 3 §4.3): each selector a page
/// type name, pseudo-pages, or both, written with no whitespace inside.
fn page_selectors(prelude: &[Spanned<ComponentValue<'_>>], span: Span) -> Kept {
    let mut verdict = Kept::Yes;
    let items = prelude.split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma)));
    let trimmed: Vec<_> = items
        .map(|item| {
            let ws = |cv: &Spanned<ComponentValue<'_>>| {
                matches!(cv.node, ComponentValue::Token(Token::Whitespace))
            };
            let start = item.iter().position(|cv| !ws(cv)).unwrap_or(item.len());
            let end = item.iter().rposition(|cv| !ws(cv)).map_or(start, |i| i + 1);
            &item[start..end]
        })
        .collect();
    if trimmed.len() == 1 && trimmed[0].is_empty() {
        return Kept::Yes;
    }
    for item in trimmed {
        let rest = match item {
            [name, rest @ ..] if matches!(name.node, ComponentValue::Token(Token::Ident(_))) => {
                rest
            }
            [] => return Kept::no(DropReason::InvalidSelector, span),
            all => all,
        };
        if rest.len() % 2 != 0 {
            return Kept::no(DropReason::InvalidSelector, span);
        }
        for pair in rest.chunks(2) {
            if !matches!(pair[0].node, ComponentValue::Token(Token::Colon)) {
                return Kept::no(DropReason::InvalidSelector, span);
            }
            match &pair[1].node {
                ComponentValue::Token(Token::Ident(_))
                    if ["left", "right", "first", "blank"]
                        .iter()
                        .any(|p| is_ident(&pair[1], p)) => {}
                ComponentValue::Function { .. } => {
                    verdict = verdict.then(Kept::unknown(KeptUnknown::PageSelector, span));
                }
                _ => return Kept::no(DropReason::InvalidSelector, span),
            }
        }
    }
    verdict
}

/// Whether a selector list holds a vendor-prefixed pseudo-class or
/// pseudo-element outside `:is()` and `:where()`, which drop an argument they
/// cannot read on their own.
fn has_vendor_pseudo(list: &SelectorList<'_>) -> bool {
    list.selectors
        .iter()
        .flat_map(|c| &c.node.compounds)
        .flat_map(|c| &c.node.simples)
        .any(|s| match &s.node {
            SimpleSelector::PseudoElement(p) => p.name.starts_with('-'),
            SimpleSelector::PseudoClass(PseudoClass::Other { name, .. }) => name.starts_with('-'),
            SimpleSelector::PseudoClass(
                PseudoClass::Not(l)
                | PseudoClass::Has(l)
                | PseudoClass::NthChild { of: Some(l), .. }
                | PseudoClass::NthLastChild { of: Some(l), .. },
            ) => has_vendor_pseudo(l),
            _ => false,
        })
}

/// Conditional group rules and the others whose block holds style rules,
/// which a style rule may also hold (CSS Nesting §3).
const GROUP_RULES: &[&str] = &[
    "container",
    "document",
    "layer",
    "media",
    "scope",
    "starting-style",
    "supports",
];

/// `@page`'s margin rules (CSS Paged Media 3 §4.3).
const MARGIN_RULES: &[&str] = &[
    "bottom-center",
    "bottom-left",
    "bottom-left-corner",
    "bottom-right",
    "bottom-right-corner",
    "left-bottom",
    "left-middle",
    "left-top",
    "right-bottom",
    "right-middle",
    "right-top",
    "top-center",
    "top-left",
    "top-left-corner",
    "top-right",
    "top-right-corner",
];

fn m_static(name: &str) -> &'static str {
    MARGIN_RULES
        .iter()
        .find(|m| **m == name)
        .copied()
        .unwrap_or("")
}

/// The feature-value blocks of `@font-feature-values` (CSS Fonts 4 §6.9).
fn in_feature_values(bare: &str) -> bool {
    [
        "annotation",
        "character-variant",
        "historical-forms",
        "ornaments",
        "styleset",
        "stylistic",
        "swash",
    ]
    .contains(&bare)
}

/// CSS Anchor Positioning 1 §5.1: `@position-try` accepts the inset,
/// margin, sizing and self-alignment properties, `position-anchor` and
/// `position-area`.
fn position_try_accepts(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "align-self",
        "block-size",
        "bottom",
        "height",
        "inline-size",
        "inset",
        "inset-block",
        "inset-block-end",
        "inset-block-start",
        "inset-inline",
        "inset-inline-end",
        "inset-inline-start",
        "justify-self",
        "left",
        "margin",
        "margin-block",
        "margin-block-end",
        "margin-block-start",
        "margin-bottom",
        "margin-inline",
        "margin-inline-end",
        "margin-inline-start",
        "margin-left",
        "margin-right",
        "margin-top",
        "max-block-size",
        "max-height",
        "max-inline-size",
        "max-width",
        "min-block-size",
        "min-height",
        "min-inline-size",
        "min-width",
        "place-self",
        "position-anchor",
        "position-area",
        "right",
        "top",
        "width",
    ];
    NAMES.binary_search(&name).is_ok()
}

/// CSS Paged Media 3 Appendix A.1, the page properties. Sorted.
const PAGE_PROPERTIES: &[&str] = &[
    "background",
    "background-attachment",
    "background-color",
    "background-image",
    "background-position",
    "background-repeat",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-style",
    "border-bottom-width",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-style",
    "border-top-width",
    "border-width",
    "color",
    "counter-increment",
    "counter-reset",
    "direction",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "height",
    "letter-spacing",
    "line-height",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "outline",
    "outline-color",
    "outline-style",
    "outline-width",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "quotes",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "visibility",
    "white-space",
    "width",
    "word-spacing",
];

/// CSS Paged Media 3 Appendix A.2, the page-margin properties: the page
/// properties and five more. Sorted.
const MARGIN_PROPERTIES: &[&str] = &[
    "background",
    "background-attachment",
    "background-color",
    "background-image",
    "background-position",
    "background-repeat",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-style",
    "border-bottom-width",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-style",
    "border-top-width",
    "border-width",
    "color",
    "content",
    "counter-increment",
    "counter-reset",
    "direction",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "height",
    "letter-spacing",
    "line-height",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "outline",
    "outline-color",
    "outline-style",
    "outline-width",
    "overflow",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "quotes",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "unicode-bidi",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "word-spacing",
    "z-index",
];

/// The properties of CSS 2.1 (REC-CSS2-20110607, Appendix F). Sorted.
const CSS21_PROPERTIES: &[&str] = &[
    "azimuth",
    "background",
    "background-attachment",
    "background-color",
    "background-image",
    "background-position",
    "background-repeat",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-style",
    "border-top-width",
    "border-width",
    "bottom",
    "caption-side",
    "clear",
    "clip",
    "color",
    "content",
    "counter-increment",
    "counter-reset",
    "cue",
    "cue-after",
    "cue-before",
    "cursor",
    "direction",
    "display",
    "elevation",
    "empty-cells",
    "float",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "height",
    "left",
    "letter-spacing",
    "line-height",
    "list-style",
    "list-style-image",
    "list-style-position",
    "list-style-type",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "orphans",
    "outline",
    "outline-color",
    "outline-style",
    "outline-width",
    "overflow",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "page-break-after",
    "page-break-before",
    "page-break-inside",
    "pause",
    "pause-after",
    "pause-before",
    "pitch",
    "pitch-range",
    "play-during",
    "position",
    "quotes",
    "richness",
    "right",
    "speak",
    "speak-header",
    "speak-numeral",
    "speak-punctuation",
    "speech-rate",
    "stress",
    "table-layout",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "top",
    "unicode-bidi",
    "vertical-align",
    "visibility",
    "voice-family",
    "volume",
    "white-space",
    "widows",
    "width",
    "word-spacing",
    "z-index",
];

#[cfg(test)]
mod tests;
