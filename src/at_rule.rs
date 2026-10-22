//! Structured preludes for the at-rules a cascade needs: `@namespace`,
//! `@import`, `@layer` and `@media`.
//!
//! The parser hands every at-rule's prelude on as component values, as CSS
//! Syntax says it should. These functions read the four whose preludes decide
//! which rules apply and in what order. Each returns `None` for a prelude its
//! grammar rejects; a browser ignores such a rule, and a caller can do the
//! same. Nothing here reports a [`SyntaxError`](crate::SyntaxError).
//!
//! The `@media` and `@supports` conditions themselves are kept as written,
//! not evaluated: whether `(min-width: 40em)` holds depends on a device this
//! crate knows nothing about.

use std::borrow::Cow;

use crate::matching::Namespaces;
use crate::parser::{AtRule, BlockKind, ComponentValue, Rule, Stylesheet};
use crate::span::{Span, Spanned};
use crate::token::Token;

type Values<'a> = [Spanned<ComponentValue<'a>>];

/// `@namespace prefix? url;`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRule<'a> {
    /// `None` declares the default namespace.
    pub prefix: Option<Cow<'a, str>>,
    pub uri: Cow<'a, str>,
}

/// A layer name: `base`, or the dotted `framework.theme`, one entry per part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerName<'a> {
    pub parts: Vec<Cow<'a, str>>,
}

/// What an `@layer` rule says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerRule<'a> {
    /// `@layer a, b.c;`: declares the layers' order, holds no rules.
    Statement(Vec<LayerName<'a>>),
    /// `@layer a { … }`: the block's rules go into this layer.
    Block(LayerName<'a>),
    /// `@layer { … }`: a layer of its own with no name.
    Anonymous,
}

/// The `layer` part of an `@import`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportLayer<'a> {
    /// No `layer`: the imported rules are not layered.
    None,
    /// `layer`: an anonymous layer.
    Anonymous,
    /// `layer(name)`.
    Named(LayerName<'a>),
}

/// `@import url layer? supports(…)? media-queries?;`
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRule<'a> {
    pub url: Cow<'a, str>,
    pub layer: ImportLayer<'a>,
    /// The inside of `supports( … )`, unread.
    pub supports: Option<Vec<Spanned<ComponentValue<'a>>>>,
    /// Empty when no media queries were written, which means "all".
    pub media: MediaQueryList<'a>,
}

/// A comma-separated media query list. Empty means no condition: it holds
/// everywhere.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaQueryList<'a> {
    pub queries: Vec<Spanned<MediaQuery<'a>>>,
}

/// One media query (Media Queries 4 §3).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaQuery<'a> {
    Query {
        /// `not` or `only` before a media type.
        qualifier: Option<MediaQualifier>,
        /// `screen`, `print`, `all`, … as written; `None` for a query that
        /// is a condition alone (`(min-width: 40em)`).
        media_type: Option<Cow<'a, str>>,
        /// The condition, as written: after `and` when there is a media type,
        /// otherwise the whole query (`not (color)` included).
        condition: Option<Vec<Spanned<ComponentValue<'a>>>>,
    },
    /// A query the grammar rejects. Media Queries treats it as `not all`:
    /// it never holds, and the rest of the list still counts.
    Invalid,
}

impl MediaQueryList<'_> {
    /// Whether the list holds on every device, without evaluating any
    /// condition: it is empty, or one of its queries is `all` or `only all`
    /// alone. `false` means "not known to always hold", not "never holds".
    pub fn always_holds(&self) -> bool {
        self.queries.is_empty()
            || self.queries.iter().any(|q| {
                matches!(&q.node, MediaQuery::Query {
                    qualifier: None | Some(MediaQualifier::Only),
                    media_type: Some(t),
                    condition: None,
                } if t.eq_ignore_ascii_case("all"))
            })
    }
}

/// The `not` or `only` before a media type (`not print`, `only screen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaQualifier {
    Not,
    Only,
}

/// The prelude of an `@namespace` rule.
pub fn parse_namespace_rule<'a>(prelude: &Values<'a>) -> Option<NamespaceRule<'a>> {
    let v = significant(prelude);
    match v.as_slice() {
        [uri] => Some(NamespaceRule {
            prefix: None,
            uri: url(uri)?,
        }),
        [ComponentValue::Token(Token::Ident(prefix)), uri] => Some(NamespaceRule {
            prefix: Some(prefix.clone()),
            uri: url(uri)?,
        }),
        _ => None,
    }
}

/// The prelude of an `@layer` rule, which reads differently with and without
/// a block.
pub fn parse_layer_rule<'a>(rule: &AtRule<'a>) -> Option<LayerRule<'a>> {
    let names = layer_names(&rule.prelude)?;
    match (rule.block.is_some(), names.len()) {
        (true, 0) => Some(LayerRule::Anonymous),
        (true, 1) => names.into_iter().next().map(LayerRule::Block),
        (false, n) if n > 0 => Some(LayerRule::Statement(names)),
        _ => None,
    }
}

/// The prelude of an `@import` rule.
pub fn parse_import_rule<'a>(prelude: &Values<'a>) -> Option<ImportRule<'a>> {
    let mut i = skip_ws(prelude, 0);
    let url = url(&prelude.get(i)?.node)?;
    i = skip_ws(prelude, i + 1);

    let mut layer = ImportLayer::None;
    match prelude.get(i).map(|cv| &cv.node) {
        Some(ComponentValue::Token(Token::Ident(w))) if w.eq_ignore_ascii_case("layer") => {
            layer = ImportLayer::Anonymous;
            i = skip_ws(prelude, i + 1);
        }
        Some(ComponentValue::Function { name, args }) if name.eq_ignore_ascii_case("layer") => {
            let mut names = layer_names(args)?;
            if names.len() != 1 {
                return None;
            }
            layer = ImportLayer::Named(names.remove(0));
            i = skip_ws(prelude, i + 1);
        }
        _ => {}
    }

    let mut supports = None;
    if let Some(ComponentValue::Function { name, args }) = prelude.get(i).map(|cv| &cv.node)
        && name.eq_ignore_ascii_case("supports")
    {
        supports = Some(args.clone());
        i = skip_ws(prelude, i + 1);
    }

    Some(ImportRule {
        url,
        layer,
        supports,
        media: parse_media_query_list(&prelude[i..]),
    })
}

/// A media query list: the prelude of `@media`, or the tail of `@import`.
///
/// Never fails as a whole: a query the grammar rejects becomes
/// [`MediaQuery::Invalid`] and the others stand, as Media Queries says.
pub fn parse_media_query_list<'a>(values: &Values<'a>) -> MediaQueryList<'a> {
    if values
        .iter()
        .all(|cv| matches!(cv.node, ComponentValue::Token(Token::Whitespace)))
    {
        return MediaQueryList::default();
    }
    let queries = values
        .split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma)))
        .map(|part| {
            let span = trimmed_span(part);
            Spanned::new(media_query(part).unwrap_or(MediaQuery::Invalid), span)
        })
        .collect();
    MediaQueryList { queries }
}

impl<'a> Namespaces<'a> {
    /// The namespaces a stylesheet declares.
    ///
    /// Only `@namespace` rules in their proper place count: after `@charset`,
    /// `@import` and `@layer` statements, before any other rule (CSS
    /// Namespaces §2). A browser ignores one written later, and so does this.
    pub fn from_stylesheet(sheet: &Stylesheet<'a>) -> Namespaces<'a> {
        let mut ns = Namespaces::default();
        for (rule, place) in sheet.rules.iter().zip(placements(sheet)) {
            if place != Placement::InOrder {
                continue;
            }
            let Rule::At(at) = &rule.node else { continue };
            if at.name.eq_ignore_ascii_case("namespace")
                && let Some(decl) = parse_namespace_rule(&at.prelude)
            {
                match decl.prefix {
                    Some(p) => ns.prefixes.push((p, decl.uri)),
                    None => ns.default = Some(decl.uri),
                }
            }
        }
        ns
    }
}

/// At-rule names the CSS specifications define, without the `@`: MDN's
/// `at-rules.json` (mdn-data 2.37.2) and the at-rules the specs' drafts add,
/// including the ones only valid inside another (`@page`'s margin rules,
/// `@font-feature-values`' feature blocks). Lowercase, sorted. Over-inclusion
/// only misses a report; omission makes one up.
const KNOWN_AT_RULES: &[&str] = &[
    "annotation",    // CSS Fonts 4, in @font-feature-values
    "apply",         // CSS Mixins ED
    "bottom-center", // CSS Paged Media 3 margin rules, in @page
    "bottom-left",
    "bottom-left-corner",
    "bottom-right",
    "bottom-right-corner",
    "character-variant", // CSS Fonts 4, in @font-feature-values
    "charset",
    "color-profile", // CSS Color 5
    "container",
    "contents", // CSS Mixins ED
    "counter-style",
    "custom-media", // Media Queries 5
    "document",     // CSS Conditional 3 (dropped draft), in MDN
    "else",         // CSS Conditional 5
    "font-face",
    "font-feature-values",
    "font-palette-values",
    "function",         // CSS Functions and Mixins ED
    "historical-forms", // CSS Fonts 4, in @font-feature-values
    "import",
    "keyframes",
    "layer",
    "left-bottom", // CSS Paged Media 3 margin rules, in @page
    "left-middle",
    "left-top",
    "media",
    "mixin", // CSS Mixins ED
    "namespace",
    "nest",      // CSS Nesting (earlier drafts)
    "ornaments", // CSS Fonts 4, in @font-feature-values
    "page",
    "position-try",
    "property",
    "result",       // CSS Mixins ED
    "right-bottom", // CSS Paged Media 3 margin rules, in @page
    "right-middle",
    "right-top",
    "route", // proposal (route matching)
    "scope",
    "starting-style",
    "styleset", // CSS Fonts 4, in @font-feature-values
    "stylistic",
    "supports",
    "supports-condition", // proposal (named conditions)
    "swash",
    "top-center", // CSS Paged Media 3 margin rules, in @page
    "top-left",
    "top-left-corner",
    "top-right",
    "top-right-corner",
    "view-transition",
    "viewport", // CSS Device Adaptation (retired), in old books
    "when",     // CSS Conditional 5
];

/// Whether some CSS specification defines an at-rule of this name. A
/// vendor-prefixed name counts when its unprefixed name does
/// (`@-webkit-keyframes`); one whose unprefixed name is unknown does not.
pub(crate) fn is_known_at_rule(name: &str) -> bool {
    let bare = crate::parser::unprefixed(name).to_ascii_lowercase();
    KNOWN_AT_RULES.binary_search(&bare.as_str()).is_ok()
}

/// Where a top-level rule stands in the order a stylesheet must keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Where it may be: any rule but a misplaced `@import` or `@namespace`.
    InOrder,
    /// An `@import` after another valid rule, or an `@namespace` after one
    /// other than `@import`: a browser ignores it.
    Misplaced,
}

/// Each top-level rule's [`Placement`]. `@import` must precede every other
/// valid rule (CSS Cascade 5, "Importing Style Sheets"), and `@namespace`
/// every one but `@import` (CSS Namespaces, "Declaring namespaces").
/// `@charset` and `@layer` statements may stand anywhere among them. A rule a browser drops
/// does not count: an unknown at-rule, a style rule whose selector is
/// invalid, an `@import` or `@namespace` whose prelude is.
pub(crate) fn placements(sheet: &Stylesheet<'_>) -> Vec<Placement> {
    #[derive(PartialEq, PartialOrd)]
    enum Phase {
        Imports,
        Namespaces,
        Body,
    }
    let mut phase = Phase::Imports;
    let mut out = Vec::with_capacity(sheet.rules.len());
    for rule in &sheet.rules {
        let mut place = Placement::InOrder;
        match &rule.node {
            Rule::At(at) => {
                let is = |n: &str| at.name.eq_ignore_ascii_case(n);
                if is("charset") || (is("layer") && at.block.is_none()) {
                } else if is("import") {
                    if phase > Phase::Imports {
                        place = Placement::Misplaced;
                    }
                } else if is("namespace") {
                    if phase > Phase::Namespaces {
                        place = Placement::Misplaced;
                    } else if parse_namespace_rule(&at.prelude).is_some() {
                        phase = Phase::Namespaces;
                    }
                } else if is_known_at_rule(&at.name) {
                    phase = Phase::Body;
                }
            }
            Rule::Qualified(q) => {
                if phase < Phase::Body && crate::selector::parse_selector_list(&q.prelude).is_ok() {
                    phase = Phase::Body;
                }
            }
        }
        out.push(place);
    }
    out
}

/// Media features of the "range" type, which also take `min-` and `max-`
/// (Media Queries 4 and 5; `video-*` from earlier Media Queries 5 drafts).
const RANGE_MEDIA_FEATURES: &[&str] = &[
    "aspect-ratio",
    "color",
    "color-index",
    "device-aspect-ratio",
    "device-height",
    "device-width",
    "height",
    "horizontal-viewport-segments",
    "monochrome",
    "resolution",
    "vertical-viewport-segments",
    "video-height",
    "video-resolution",
    "video-width",
    "width",
];

/// Discrete media features (Media Queries 4 and 5, Web App Manifest's
/// `display-mode`, Device Posture's `device-posture`, Round Display's
/// `shape`).
const DISCRETE_MEDIA_FEATURES: &[&str] = &[
    "any-hover",
    "any-pointer",
    "color-gamut",
    "device-posture",
    "display-mode",
    "dynamic-range",
    "environment-blending",
    "forced-colors",
    "grid",
    "hover",
    "inverted-colors",
    "light-level",
    "nav-controls",
    "orientation",
    "overflow-block",
    "overflow-inline",
    "pointer",
    "prefers-color-scheme",
    "prefers-contrast",
    "prefers-reduced-data",
    "prefers-reduced-motion",
    "prefers-reduced-transparency",
    "scan",
    "scripting",
    "shape",
    "update",
    "video-color-gamut",
    "video-dynamic-range",
];

/// Whether some specification defines a media feature of this name. A
/// vendor-prefixed name (`-webkit-min-device-pixel-ratio`) always counts.
pub(crate) fn is_known_media_feature(name: &str) -> bool {
    if name.starts_with('-') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    let range = |n: &str| RANGE_MEDIA_FEATURES.binary_search(&n).is_ok();
    match lower.strip_prefix("min-").or(lower.strip_prefix("max-")) {
        Some(base) => range(base),
        None => {
            range(&lower)
                || DISCRETE_MEDIA_FEATURES
                    .binary_search(&lower.as_str())
                    .is_ok()
        }
    }
}

/// The names of the media features a condition tests, with their spans:
/// `(name)`, `(name: value)` and the range forms, at any depth of `not`,
/// `and`, `or` and parentheses. A parenthesis that is none of these (the
/// spec's `<general-enclosed>`) yields nothing, and so does a range whose
/// name is not clear.
pub(crate) fn media_feature_names<'v, 'a>(
    condition: &'v Values<'a>,
) -> Vec<&'v Spanned<ComponentValue<'a>>> {
    let mut out = Vec::new();
    feature_names(condition, &mut out, 0);
    out
}

fn feature_names<'v, 'a>(
    values: &'v Values<'a>,
    out: &mut Vec<&'v Spanned<ComponentValue<'a>>>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    for v in values {
        let ComponentValue::Block(b) = &v.node else {
            continue;
        };
        if b.kind != BlockKind::Paren {
            continue;
        }
        let inner: Vec<&Spanned<ComponentValue<'a>>> = b
            .values
            .iter()
            .filter(|cv| !matches!(cv.node, ComponentValue::Token(Token::Whitespace)))
            .collect();
        let ident = |cv: &Spanned<ComponentValue<'a>>| {
            matches!(cv.node, ComponentValue::Token(Token::Ident(_)))
        };
        let nested = match inner.first().map(|cv| &cv.node) {
            Some(ComponentValue::Block(inner)) => inner.kind == BlockKind::Paren,
            Some(ComponentValue::Token(Token::Ident(w))) => w.eq_ignore_ascii_case("not"),
            _ => false,
        };
        if nested {
            feature_names(&b.values, out, depth + 1);
            continue;
        }
        match inner.as_slice() {
            // `(name)`
            [name] if ident(name) => out.push(name),
            // `(name: value…)`
            [name, colon, ..]
                if ident(name) && matches!(colon.node, ComponentValue::Token(Token::Colon)) =>
            {
                out.push(name)
            }
            // A range: the one identifier beside a comparison.
            _ => {
                let compares = inner.iter().any(|cv| {
                    matches!(
                        cv.node,
                        ComponentValue::Token(Token::Delim('<' | '>' | '='))
                    )
                });
                let mut idents = inner.iter().filter(|cv| ident(cv));
                if compares && let (Some(name), None) = (idents.next(), idents.next()) {
                    out.push(name);
                }
            }
        }
    }
}

fn media_query<'a>(part: &Values<'a>) -> Option<MediaQuery<'a>> {
    let start = skip_ws(part, 0);
    let ident = |i: usize| match part.get(i).map(|cv| &cv.node) {
        Some(ComponentValue::Token(Token::Ident(s))) => Some(s),
        _ => None,
    };
    let first = ident(start);
    // `(…)` or `not (…)`: a condition alone.
    let condition_start = match first {
        Some(w) if w.eq_ignore_ascii_case("not") => skip_ws(part, start + 1),
        _ => start,
    };
    if is_paren(part.get(condition_start)) {
        return Some(MediaQuery::Query {
            qualifier: None,
            media_type: None,
            condition: Some(trim(&part[start..]).to_vec()),
        });
    }

    // `[not | only]? <media-type> [and <condition>]?`
    let mut i = start;
    let mut qualifier = None;
    if let Some(w) = first {
        if w.eq_ignore_ascii_case("not") {
            qualifier = Some(MediaQualifier::Not);
        } else if w.eq_ignore_ascii_case("only") {
            qualifier = Some(MediaQualifier::Only);
        }
    }
    if qualifier.is_some() {
        i = skip_ws(part, i + 1);
    }
    let media_type = ident(i)?;
    if ["only", "not", "and", "or", "layer"]
        .iter()
        .any(|r| media_type.eq_ignore_ascii_case(r))
    {
        return None;
    }
    i = skip_ws(part, i + 1);
    let condition = match ident(i) {
        None if i >= part.len() => None,
        Some(w) if w.eq_ignore_ascii_case("and") => {
            let rest = trim(&part[i + 1..]);
            let c = skip_ws(rest, 0);
            let c = match rest.get(c).map(|cv| &cv.node) {
                Some(ComponentValue::Token(Token::Ident(n))) if n.eq_ignore_ascii_case("not") => {
                    skip_ws(rest, c + 1)
                }
                _ => c,
            };
            if !is_paren(rest.get(c)) {
                return None;
            }
            Some(rest.to_vec())
        }
        _ => return None,
    };
    Some(MediaQuery::Query {
        qualifier,
        media_type: Some(media_type.clone()),
        condition,
    })
}

/// Dotted layer names separated by commas: `a, b.c`. Empty for no names; `None` when
/// the prelude is not a list of names.
fn layer_names<'a>(values: &Values<'a>) -> Option<Vec<LayerName<'a>>> {
    let mut names = Vec::new();
    if values
        .iter()
        .all(|cv| matches!(cv.node, ComponentValue::Token(Token::Whitespace)))
    {
        return Some(names);
    }
    for part in values.split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma))) {
        let part = trim(part);
        let mut parts = Vec::new();
        let mut expect_name = true;
        for cv in part {
            match (&cv.node, expect_name) {
                (ComponentValue::Token(Token::Ident(n)), true) => parts.push(n.clone()),
                (ComponentValue::Token(Token::Delim('.')), false) => {}
                _ => return None,
            }
            expect_name = !expect_name;
        }
        // Empty, or ending in a dot.
        if parts.is_empty() || expect_name {
            return None;
        }
        names.push(LayerName { parts });
    }
    Some(names)
}

/// A url, as `url(x)`, `url("x")`, `src("x")` or a bare string.
fn url<'a>(cv: &ComponentValue<'a>) -> Option<Cow<'a, str>> {
    match cv {
        ComponentValue::Token(Token::Url(u) | Token::String(u)) => Some(u.clone()),
        ComponentValue::Function { name, args }
            if name.eq_ignore_ascii_case("url") || name.eq_ignore_ascii_case("src") =>
        {
            match significant(args).as_slice() {
                [ComponentValue::Token(Token::String(u))] => Some(u.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn is_paren(cv: Option<&Spanned<ComponentValue<'_>>>) -> bool {
    matches!(cv.map(|c| &c.node), Some(ComponentValue::Block(b)) if b.kind == BlockKind::Paren)
}

fn significant<'v, 'a>(values: &'v Values<'a>) -> Vec<&'v ComponentValue<'a>> {
    values
        .iter()
        .map(|cv| &cv.node)
        .filter(|cv| !matches!(cv, ComponentValue::Token(Token::Whitespace)))
        .collect()
}

fn skip_ws(values: &Values<'_>, mut i: usize) -> usize {
    while matches!(
        values.get(i).map(|cv| &cv.node),
        Some(ComponentValue::Token(Token::Whitespace))
    ) {
        i += 1;
    }
    i
}

fn trim<'v, 'a>(values: &'v Values<'a>) -> &'v Values<'a> {
    let ws = |cv: &Spanned<ComponentValue<'_>>| {
        matches!(cv.node, ComponentValue::Token(Token::Whitespace))
    };
    let start = values.iter().position(|cv| !ws(cv)).unwrap_or(values.len());
    let end = values
        .iter()
        .rposition(|cv| !ws(cv))
        .map_or(start, |e| e + 1);
    &values[start..end]
}

/// The span of a part without its surrounding whitespace; empty at the
/// part's start when it is blank.
fn trimmed_span(part: &Values<'_>) -> Span {
    let t = trim(part);
    match (t.first(), t.last()) {
        (Some(a), Some(b)) => a.span.to(b.span),
        _ => part.first().map_or(Span::new(0, 0), |cv| {
            Span::new(cv.span.start, cv.span.start)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(css: &'static str) -> AtRule<'static> {
        let (sheet, _) = crate::parse_stylesheet(css);
        match sheet.rules.into_iter().next().map(|r| r.node) {
            Some(Rule::At(a)) => a,
            other => panic!("{css}: {other:?}"),
        }
    }

    #[test]
    fn namespace_rules() {
        let r =
            parse_namespace_rule(&at("@namespace svg url(http://www.w3.org/2000/svg);").prelude)
                .unwrap();
        assert_eq!(r.prefix.as_deref(), Some("svg"));
        assert_eq!(r.uri, "http://www.w3.org/2000/svg");
        let r = parse_namespace_rule(&at("@namespace \"http://www.w3.org/1999/xhtml\";").prelude)
            .unwrap();
        assert_eq!(r.prefix, None);
        let r = parse_namespace_rule(&at("@namespace x url(\"u\");").prelude).unwrap();
        assert_eq!(r.uri, "u");
        for bad in [
            "@namespace;",
            "@namespace svg;",
            "@namespace a b \"u\";",
            "@namespace 1 \"u\";",
        ] {
            assert_eq!(parse_namespace_rule(&at(bad).prelude), None, "{bad}");
        }
    }

    #[test]
    fn namespaces_come_from_the_head_of_the_sheet_only() {
        let css = "@charset \"utf-8\"; @import \"a.css\"; @layer x;\n\
                   @namespace svg url(S); @namespace url(H); @namespace svg url(S2);\n\
                   p { } @namespace late url(L);";
        let (sheet, _) = crate::parse_stylesheet(css);
        let ns = Namespaces::from_stylesheet(&sheet);
        assert_eq!(ns.default.as_deref(), Some("H"));
        assert_eq!(ns.resolve("svg"), Some("S2"), "the later declaration wins");
        assert_eq!(ns.resolve("late"), None, "after a style rule: ignored");
        assert_eq!(ns.resolve("SVG"), None, "prefixes are case-sensitive");
    }

    #[test]
    fn layer_rules() {
        let names = |r: LayerRule<'_>| match r {
            LayerRule::Statement(n) => n.into_iter().map(|n| n.parts.join(".")).collect(),
            LayerRule::Block(n) => vec![n.parts.join(".")],
            LayerRule::Anonymous => vec![],
        };
        assert_eq!(
            names(parse_layer_rule(&at("@layer reset, base.theme ;")).unwrap()),
            ["reset", "base.theme"]
        );
        assert_eq!(
            names(parse_layer_rule(&at("@layer base { }")).unwrap()),
            ["base"]
        );
        assert_eq!(
            parse_layer_rule(&at("@layer { }")),
            Some(LayerRule::Anonymous)
        );
        for bad in [
            "@layer;",
            "@layer a, b { }",
            "@layer a. b;",
            "@layer a.;",
            "@layer .a;",
            "@layer a,;",
            "@layer 1;",
        ] {
            assert_eq!(parse_layer_rule(&at(bad)), None, "{bad}");
        }
    }

    #[test]
    fn import_rules() {
        let r = parse_import_rule(&at("@import url(a.css);").prelude).unwrap();
        assert_eq!(r.url, "a.css");
        assert_eq!(r.layer, ImportLayer::None);
        assert!(r.supports.is_none() && r.media.queries.is_empty());

        let r = parse_import_rule(
            &at("@import \"b.css\" layer(x.y) supports(display: grid) screen, print;").prelude,
        )
        .unwrap();
        assert_eq!(r.url, "b.css");
        assert_eq!(
            r.layer,
            ImportLayer::Named(LayerName {
                parts: vec!["x".into(), "y".into()]
            })
        );
        assert!(r.supports.is_some());
        assert_eq!(r.media.queries.len(), 2);

        let r = parse_import_rule(&at("@import 'c.css' layer;").prelude).unwrap();
        assert_eq!(r.layer, ImportLayer::Anonymous);

        assert!(parse_import_rule(&at("@import;").prelude).is_none());
        assert!(parse_import_rule(&at("@import foo;").prelude).is_none());
        assert!(parse_import_rule(&at("@import 'a' layer(a, b);").prelude).is_none());
    }

    fn mq(prelude: &'static str) -> Vec<MediaQuery<'static>> {
        let css: &'static str = Box::leak(format!("@media {prelude} {{}}").into_boxed_str());
        parse_media_query_list(&at(css).prelude)
            .queries
            .into_iter()
            .map(|q| q.node)
            .collect()
    }

    fn shape(q: &MediaQuery<'_>) -> String {
        match q {
            MediaQuery::Invalid => "invalid".into(),
            MediaQuery::Query {
                qualifier,
                media_type,
                condition,
            } => format!(
                "{qualifier:?}/{}/{}",
                media_type.as_deref().unwrap_or("-"),
                condition.as_ref().map_or(0, |c| c.len())
            ),
        }
    }

    #[test]
    fn media_queries() {
        let shapes = |p| mq(p).iter().map(shape).collect::<Vec<_>>();
        assert_eq!(shapes("screen"), ["None/screen/0"]);
        assert_eq!(
            shapes("only screen and (min-width: 40em)"),
            ["Some(Only)/screen/1"]
        );
        assert_eq!(shapes("not print"), ["Some(Not)/print/0"]);
        assert_eq!(shapes("(color)"), ["None/-/1"]);
        assert_eq!(shapes("not (color)"), ["None/-/3"]);
        assert_eq!(shapes("(a) and (b)"), ["None/-/5"]);
        assert_eq!(shapes("screen and not (color)"), ["None/screen/3"]);
        assert_eq!(
            shapes("screen, print and (orientation: portrait)"),
            ["None/screen/0", "None/print/1"]
        );
        assert!(mq("").is_empty());
        // Invalid queries stand alone; the rest of the list counts.
        assert_eq!(shapes("screen and, print"), ["invalid", "None/print/0"]);
        assert_eq!(shapes("and"), ["invalid"]);
        assert_eq!(shapes("only"), ["invalid"]);
        assert_eq!(shapes("screen print"), ["invalid"]);
        assert_eq!(shapes("screen and color"), ["invalid"]);
        assert_eq!(shapes("1px"), ["invalid"]);
        assert_eq!(shapes("screen,"), ["None/screen/0", "invalid"]);
    }

    #[test]
    fn media_query_spans_skip_whitespace() {
        let css = "@media  screen ,  print and (color)  {}";
        let (sheet, _) = crate::parse_stylesheet(css);
        let list = parse_media_query_list(sheet.rules[0].node.prelude());
        let spans: Vec<_> = list.queries.iter().map(|q| q.span.slice(css)).collect();
        assert_eq!(spans, ["screen", "print and (color)"]);
    }

    #[test]
    fn name_tables_are_sorted_and_lowercase() {
        for table in [
            KNOWN_AT_RULES,
            RANGE_MEDIA_FEATURES,
            DISCRETE_MEDIA_FEATURES,
        ] {
            assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
            assert!(table.iter().all(|n| *n == n.to_ascii_lowercase()));
        }
        for table in [
            crate::pseudos::PSEUDO_CLASSES,
            crate::pseudos::PSEUDO_CLASS_FUNCTIONS,
            crate::pseudos::PSEUDO_ELEMENTS,
            crate::pseudos::PSEUDO_ELEMENT_FUNCTIONS,
        ] {
            assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
        }
    }
}
