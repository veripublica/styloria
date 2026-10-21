//! The cascade, as a query: which author declarations could set a property
//! on an element, in what order of precedence, and which one decides.
//!
//! Build a [`Cascade`] once from a document's stylesheets, in document
//! order. Then, per element, [`Cascade::style_for`] matches every rule once
//! and returns an [`ElementStyle`]: every declaration that could apply,
//! highest precedence first. Asking it about a property
//! ([`ElementStyle::property`]) is a lookup on that result, so asking about
//! many properties of one element costs one match.
//!
//! The answer is three-valued, like [`Match`]: a property is set (`Yes`), not
//! set by any author declaration (`No`), or possibly set (`Unknown`). Every
//! uncertain setter says why ([`UnknownReason`]), and every setter carries
//! its declaration and rule, whose spans point at the stylesheet's own text.
//!
//! **Conservative by design.** The question this answers is "could author CSS
//! set this?", so whatever cannot be settled from the stylesheets and the
//! tree alone counts as *possibly*, never as *no*:
//!
//! - a selector that matches Unknown ([`crate::matching`]);
//! - a rule inside `@media`, `@supports`, `@container`, `@scope`,
//!   `@document` or `@starting-style`, whose condition is not evaluated
//!   (`@media all` and an empty media list hold everywhere, so they are not
//!   conditions), every rule of a sheet the caller marks conditional (a
//!   `<link media>`), and every rule of an `@import` with a media list or a
//!   `supports()`;
//! - a style rule nested so deeply, or with so many `&`, that its resolved
//!   selector would pass [`MAX_RESOLVED_SIMPLES`](crate::selector::MAX_RESOLVED_SIMPLES);
//!   every other nested rule is resolved as CSS Nesting says
//!   ([`SelectorList::resolve_nesting`]), and declarations after a nested
//!   rule keep their place in the order;
//! - a logical property, which may stand for either physical one;
//! - a vendor-prefixed property, which only some engines honour;
//! - `revert` and `revert-layer`, which hand the property back to an earlier
//!   origin or layer.
//!
//! Shorthands set their longhands (`border` sets `border-top-width`), from a
//! table generated from MDN's data; `all` sets every property but
//! `direction`, `unicode-bidi` and custom properties.
//!
//! ```
//! use styloria::{Cascade, Element, Match, Sheet};
//!
//! // The caller's tree: here a lone `<img class="pic">`.
//! #[derive(Clone)]
//! struct Img;
//! impl Element for Img {
//!     fn parent(&self) -> Option<Self> { None }
//!     fn prev_sibling(&self) -> Option<Self> { None }
//!     fn next_sibling(&self) -> Option<Self> { None }
//!     fn has_children(&self) -> bool { false }
//!     fn local_name(&self) -> &str { "img" }
//!     fn namespace(&self) -> Option<&str> { Some(styloria::HTML_NAMESPACE) }
//!     fn attribute(&self, name: &str) -> Option<&str> { (name == "class").then_some("pic") }
//!     fn attribute_ns(&self, _: &str, _: &str) -> Option<&str> { None }
//!     fn is_html_document(&self) -> bool { false }
//! }
//!
//! let css = ".pic { border: 1px solid }\n@media print { img { width: 10em } }";
//! let sheet = styloria::parse_stylesheet(css).0;
//! let cascade = Cascade::new(&[Sheet { stylesheet: &sheet, conditional: false }]);
//!
//! // The `style` attribute may be parsed per element and dropped after.
//! let attr = String::from("height: 3em");
//! let inline = styloria::parse_block_contents(&attr).0;
//! let style = cascade.style_for(&Img, Some(&inline));
//!
//! assert_eq!(style.property("border-top-width").status, Match::Yes);
//! assert_eq!(style.property("height").status, Match::Yes);
//! assert_eq!(style.property("width").status, Match::Unknown); // inside @media
//! assert_eq!(style.property("padding").status, Match::No);
//!
//! let width = style.property("width");
//! let decl = &width.decided_by().unwrap().candidate.declaration;
//! assert_eq!(decl.node.name_span.start_line_col(css), (2, 22));
//! ```
//!
//! `@import` is followed when the caller hands over the imported sheets
//! ([`Cascade::with_imports`]): their rules take the `@import`'s place, in
//! its layer, under its conditions.
//!
//! A declaration whose value is certainly invalid
//! ([`check_declaration`](crate::check_declaration) says
//! [`Invalid`](crate::ValueCheck::Invalid)) is dropped, as a browser drops
//! it: `width: banana` sets nothing. One whose value is Unknown stays.
//!
//! **Out of scope:** user-agent and user styles, and inheritance, are not
//! modelled.

use crate::at_rule::{
    ImportLayer, ImportRule, LayerName, LayerRule, parse_import_rule, parse_layer_rule,
    parse_media_query_list,
};
use crate::matching::{Element, Match, Namespaces};
use crate::parser::{self, AtRule, BlockItem, ComponentValue, Declaration, Rule, Stylesheet};
use crate::properties::{logical_counterparts, longhands};
use crate::selector::{
    SelectorList, Specificity, parse_relative_selector_list, parse_selector_list,
};
use crate::span::Spanned;
use crate::token::Token;
use crate::values::{ValueCheck, check_declaration};

/// One stylesheet given to the cascade.
#[derive(Debug, Clone, Copy)]
pub struct Sheet<'s, 'a> {
    pub stylesheet: &'s Stylesheet<'a>,
    /// Whether the sheet applies only under a condition this crate does not
    /// evaluate: `<link media="print">`, `@import url(x) screen`. Every
    /// declaration in it is then only possibly applied.
    pub conditional: bool,
}

/// Why a declaration only *possibly* applies, or only possibly sets the
/// property asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnknownReason {
    /// The rule's selector matches the element Unknown (`:hover`, `:has()`,
    /// …).
    Selector,
    /// The rule sits inside `@media`, `@supports` or another conditional
    /// group rule.
    Conditional,
    /// The sheet was given as [`Sheet::conditional`].
    ConditionalSheet,
    /// The rule is nested in another style rule, and its selector, resolved
    /// through `&`, would pass
    /// [`MAX_RESOLVED_SIMPLES`](crate::selector::MAX_RESOLVED_SIMPLES), so it
    /// is left unresolved.
    Nested,
    /// The declaration is a logical property and the question a physical
    /// one, or the other way round.
    Logical,
    /// The declaration's property is vendor-prefixed.
    Prefixed,
    /// The value is `revert` or `revert-layer`.
    Revert,
}

/// A set of [`UnknownReason`]s; empty for a declaration that certainly
/// applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct UnknownReasons(u8);

const ALL_REASONS: [UnknownReason; 7] = [
    UnknownReason::Selector,
    UnknownReason::Conditional,
    UnknownReason::ConditionalSheet,
    UnknownReason::Nested,
    UnknownReason::Logical,
    UnknownReason::Prefixed,
    UnknownReason::Revert,
];

impl UnknownReasons {
    fn bit(r: UnknownReason) -> u8 {
        1 << (r as u8)
    }

    fn with(self, r: UnknownReason) -> UnknownReasons {
        UnknownReasons(self.0 | Self::bit(r))
    }

    fn with_if(self, r: UnknownReason, cond: bool) -> UnknownReasons {
        if cond { self.with(r) } else { self }
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, r: UnknownReason) -> bool {
        self.0 & Self::bit(r) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = UnknownReason> {
        ALL_REASONS.into_iter().filter(move |r| self.contains(*r))
    }

    /// `Yes` when empty, otherwise `Unknown`.
    pub fn as_match(self) -> Match {
        if self.is_empty() {
            Match::Yes
        } else {
            Match::Unknown
        }
    }
}

/// The stylesheets of one document, ready to be asked about its elements.
#[derive(Debug)]
pub struct Cascade<'s, 'a> {
    rules: Vec<CascadeRule<'s, 'a>>,
    namespaces: Vec<Namespaces<'a>>,
    /// For each layer, its place in the layer order; see `Layers`.
    layer_rank: Vec<u32>,
    unlayered: usize,
}

/// A style rule, ready to match: its parsed selectors and its declarations.
#[derive(Debug)]
struct CascadeRule<'s, 'a> {
    rule: &'s Spanned<Rule<'a>>,
    selectors: SelectorList<'a>,
    declarations: Vec<(&'s Spanned<Declaration<'a>>, u32)>,
    sheet: usize,
    layer: usize,
    reasons: UnknownReasons,
}

/// One declaration that could apply to an element.
///
/// The lifetime is the shorter of the stylesheets' and the `style`
/// attribute's, so an attribute parsed per element need not outlive the
/// [`Cascade`].
#[derive(Debug, Clone)]
pub struct Candidate<'e> {
    pub declaration: &'e Spanned<Declaration<'e>>,
    /// The style rule it is in; `None` for the element's own `style`
    /// attribute.
    pub rule: Option<&'e Spanned<Rule<'e>>>,
    /// The specificity of the most specific selector in the rule's list that
    /// matches; zero for the `style` attribute.
    pub specificity: Specificity,
    /// Why it only possibly applies; empty when it certainly does.
    pub reasons: UnknownReasons,
    key: (bool, bool, u32, Specificity, u32),
}

/// Every declaration that could apply to one element, highest precedence
/// first.
#[derive(Debug, Clone)]
pub struct ElementStyle<'e> {
    pub candidates: Vec<Candidate<'e>>,
}

/// A declaration that could set the property asked about.
#[derive(Debug, Clone, Copy)]
pub struct Setter<'c, 'e> {
    pub candidate: &'c Candidate<'e>,
    /// The candidate's reasons, plus those that come from the property
    /// question itself ([`Logical`](UnknownReason::Logical),
    /// [`Prefixed`](UnknownReason::Prefixed),
    /// [`Revert`](UnknownReason::Revert)).
    pub reasons: UnknownReasons,
}

/// The answer about one property of one element.
#[derive(Debug, Clone)]
pub struct PropertyAnswer<'c, 'e> {
    /// `Yes` if some declaration certainly sets it, `Unknown` if some
    /// possibly does, `No` if none could.
    pub status: Match,
    /// Every declaration that could set it, highest precedence first.
    pub setters: Vec<Setter<'c, 'e>>,
}

impl<'c, 'e> PropertyAnswer<'c, 'e> {
    /// The declaration that wins the cascade if it applies: the first setter.
    /// When it is itself uncertain, a lower one may win instead; the
    /// highest *certain* one is [`decided_by_certain`](Self::decided_by_certain).
    pub fn decided_by(&self) -> Option<&Setter<'c, 'e>> {
        self.setters.first()
    }

    /// The highest setter that certainly applies.
    pub fn decided_by_certain(&self) -> Option<&Setter<'c, 'e>> {
        self.setters.iter().find(|s| s.reasons.is_empty())
    }
}

impl<'s, 'a> Cascade<'s, 'a> {
    /// The cascade of these sheets, in document order (later wins at equal
    /// specificity). `@import` rules are not followed; see
    /// [`with_imports`](Self::with_imports).
    pub fn new(sheets: &[Sheet<'s, 'a>]) -> Cascade<'s, 'a> {
        Self::with_imports(sheets, |_, _| None)
    }

    /// [`new`](Self::new), following `@import`. For each `@import` in its
    /// proper place (before every rule but `@charset` and `@layer`
    /// statements), `import` is given the importing sheet and the rule, and
    /// returns the imported sheet if the caller has it. Its rules then count
    /// where the `@import` stands: in `layer()`'s layer, conditional under a
    /// media list or `supports()`.
    ///
    /// An import the caller does not return adds nothing. A sheet that
    /// imports itself, directly or not, is followed once, and imports nest at
    /// most [`MAX_IMPORT_DEPTH`] deep.
    pub fn with_imports<F>(sheets: &[Sheet<'s, 'a>], mut import: F) -> Cascade<'s, 'a>
    where
        F: FnMut(&'s Stylesheet<'a>, &ImportRule<'a>) -> Option<&'s Stylesheet<'a>>,
    {
        let mut b = Builder {
            rules: Vec::new(),
            layers: Layers::new(),
            order: 0,
            namespaces: Vec::with_capacity(sheets.len()),
            import: &mut import,
            importing: Vec::new(),
        };
        for sheet in sheets {
            let reasons = UnknownReasons::default()
                .with_if(UnknownReason::ConditionalSheet, sheet.conditional);
            b.sheet(sheet.stylesheet, Layers::ROOT, reasons);
        }
        let layer_rank = b.layers.ranks();
        Cascade {
            rules: b.rules,
            namespaces: b.namespaces,
            unlayered: layer_rank.len() - 1,
            layer_rank,
        }
    }

    /// Every declaration that could apply to `element`, highest precedence
    /// first. `inline` is the element's own `style` attribute, read with
    /// [`parse_block_contents`](crate::parse_block_contents); its
    /// declarations beat every rule's, `!important` ones beat every
    /// `!important` rule's.
    pub fn style_for<'e, E: Element>(
        &'e self,
        element: &E,
        inline: Option<&'e [BlockItem<'e>]>,
    ) -> ElementStyle<'e>
    where
        's: 'e,
        'a: 'e,
    {
        let mut candidates = Vec::new();
        for r in &self.rules {
            let ns = &self.namespaces[r.sheet];
            let mut best = Match::No;
            let mut specificity = Specificity::default();
            for s in &r.selectors.selectors {
                let m = s.node.matches(element, ns);
                if m != Match::No {
                    best = best.or(m);
                    specificity = specificity.max(s.node.specificity());
                }
            }
            if best == Match::No {
                continue;
            }
            // A nested rule's selector is Unknown because its `&` is not
            // resolved; `Nested` already says so.
            let unsure = best == Match::Unknown && !r.reasons.contains(UnknownReason::Nested);
            let reasons = r.reasons.with_if(UnknownReason::Selector, unsure);
            for (d, order) in &r.declarations {
                candidates.push(Candidate {
                    declaration: d,
                    rule: Some(r.rule),
                    specificity,
                    reasons,
                    key: self.key(d.node.important, false, r.layer, specificity, *order),
                });
            }
        }
        for (i, item) in inline.unwrap_or_default().iter().enumerate() {
            if let BlockItem::Declaration(d) = item
                && check_declaration(&d.node) != ValueCheck::Invalid
            {
                let order = u32::try_from(i).unwrap_or(u32::MAX);
                candidates.push(Candidate {
                    declaration: d,
                    rule: None,
                    specificity: Specificity::default(),
                    reasons: UnknownReasons::default(),
                    key: self.key(
                        d.node.important,
                        true,
                        self.unlayered,
                        Specificity::default(),
                        order,
                    ),
                });
            }
        }
        candidates.sort_by_key(|c| std::cmp::Reverse(c.key));
        ElementStyle { candidates }
    }

    /// The precedence of a declaration, compared highest-wins: importance,
    /// then the `style` attribute over rules, then layer, then specificity,
    /// then order of appearance. For `!important` declarations the layer
    /// order is reversed (CSS Cascade 5).
    fn key(
        &self,
        important: bool,
        inline: bool,
        layer: usize,
        specificity: Specificity,
        order: u32,
    ) -> (bool, bool, u32, Specificity, u32) {
        let rank = self.layer_rank[layer];
        let max = self.layer_rank[self.unlayered];
        let layer = if important { max - rank } else { rank };
        (important, inline, layer, specificity, order)
    }
}

impl<'e> ElementStyle<'e> {
    /// What could set `property` on the element. The name is compared ASCII
    /// case-insensitively, except a custom property (`--x`), which is
    /// case-sensitive. A shorthand is answered through its longhands: asking
    /// about `border` finds `border-top-width: 0`.
    pub fn property<'c>(&'c self, property: &str) -> PropertyAnswer<'c, 'e> {
        let query = Query::new(property);
        let setters: Vec<_> = self
            .candidates
            .iter()
            .filter_map(|c| {
                query.sets(&c.declaration.node).map(|extra| Setter {
                    candidate: c,
                    reasons: UnknownReasons(c.reasons.0 | extra.0),
                })
            })
            .collect();
        let status = if setters.iter().any(|s| s.reasons.is_empty()) {
            Match::Yes
        } else if setters.is_empty() {
            Match::No
        } else {
            Match::Unknown
        };
        PropertyAnswer { status, setters }
    }
}

/// A property question: the name, and the longhands it stands for.
struct Query {
    custom: Option<String>,
    name: String,
    longhands: Vec<&'static str>,
    /// The longhands' logical/physical counterparts.
    counterparts: Vec<&'static str>,
}

impl Query {
    fn new(property: &str) -> Query {
        if property.starts_with("--") {
            return Query {
                custom: Some(property.to_string()),
                name: String::new(),
                longhands: Vec::new(),
                counterparts: Vec::new(),
            };
        }
        let name = property.to_ascii_lowercase();
        let longhands = longhands(&name).map_or_else(Vec::new, <[_]>::to_vec);
        let mut counterparts = Vec::new();
        for l in longhands
            .iter()
            .copied()
            .chain(std::iter::once(name.as_str()))
        {
            counterparts.extend(logical_counterparts(l));
        }
        Query {
            custom: None,
            name,
            longhands,
            counterparts,
        }
    }

    /// Whether `d` could set the property: `None` if not, else the reasons
    /// it adds (empty for a certain set).
    fn sets(&self, d: &Declaration<'_>) -> Option<UnknownReasons> {
        if let Some(custom) = &self.custom {
            return (d.name == custom.as_str()).then(UnknownReasons::default);
        }
        if d.name.starts_with("--") {
            return None;
        }
        let lower = d.name.to_ascii_lowercase();
        // `-webkit-border-before` is in the shorthand table under its own
        // name; other prefixed names are read as their unprefixed property.
        let prefixed = lower.starts_with('-');
        let name = if prefixed && longhands(&lower).is_none() {
            parser::unprefixed(&lower)
        } else {
            lower.as_str()
        };
        let reasons = UnknownReasons::default()
            .with_if(UnknownReason::Prefixed, prefixed)
            .with_if(UnknownReason::Revert, is_revert(d));
        if name == "all" {
            let exempt = ["direction", "unicode-bidi"].contains(&self.name.as_str());
            return (!exempt).then_some(reasons);
        }
        let declared = longhands(name);
        let declared: &[&str] = declared.unwrap_or(std::slice::from_ref(&name));
        let asked = |l: &str| l == self.name || self.longhands.contains(&l);
        if declared.iter().any(|l| asked(l)) {
            return Some(reasons);
        }
        if declared.iter().any(|l| self.counterparts.contains(l)) {
            return Some(reasons.with(UnknownReason::Logical));
        }
        None
    }
}

/// Whether a declaration's value is `revert` or `revert-layer` alone.
fn is_revert(d: &Declaration<'_>) -> bool {
    match d.value.as_slice() {
        [v] => matches!(&v.node, ComponentValue::Token(Token::Ident(w))
            if w.eq_ignore_ascii_case("revert") || w.eq_ignore_ascii_case("revert-layer")),
        _ => false,
    }
}

/// How deep `@import`s are followed: a sheet imported by a sheet imported
/// by … one of the sheets given.
pub const MAX_IMPORT_DEPTH: usize = 32;

type ImportFn<'f, 's, 'a> =
    dyn FnMut(&'s Stylesheet<'a>, &ImportRule<'a>) -> Option<&'s Stylesheet<'a>> + 'f;

struct Builder<'f, 's, 'a> {
    rules: Vec<CascadeRule<'s, 'a>>,
    layers: Layers,
    order: u32,
    /// One entry per sheet walked, imported ones included.
    namespaces: Vec<Namespaces<'a>>,
    import: &'f mut ImportFn<'f, 's, 'a>,
    /// The sheets being walked, outermost first: the import chain.
    importing: Vec<&'s Stylesheet<'a>>,
}

/// Where in the sheets a rule sits.
struct Ctx<'p, 's, 'a> {
    sheet: usize,
    layer: usize,
    reasons: UnknownReasons,
    /// The style rule this one is nested in, and its resolved selectors.
    parent: Option<(&'s Spanned<Rule<'a>>, &'p SelectorList<'a>)>,
    /// Inside `@scope`, where `&` at the top stands for the scoping root.
    scoped: bool,
}

impl<'s, 'a> Builder<'_, 's, 'a> {
    /// A whole sheet, its imports in their place.
    fn sheet(&mut self, sheet: &'s Stylesheet<'a>, layer: usize, reasons: UnknownReasons) {
        if self.importing.len() > MAX_IMPORT_DEPTH
            || self.importing.iter().any(|s| std::ptr::eq(*s, sheet))
        {
            return;
        }
        let ctx = Ctx {
            sheet: self.namespaces.len(),
            layer,
            reasons,
            parent: None,
            scoped: false,
        };
        self.namespaces.push(Namespaces::from_stylesheet(sheet));
        self.importing.push(sheet);
        // `@import` counts only before every other rule but `@charset` and
        // `@layer` statements; a browser ignores a later one.
        let mut imports_allowed = true;
        for rule in &sheet.rules {
            if let Rule::At(at) = &rule.node {
                let is = |n: &str| at.name.eq_ignore_ascii_case(n);
                if is("import") {
                    if imports_allowed {
                        self.import(sheet, at, &ctx);
                    }
                    continue;
                }
                if !(is("charset") || (is("layer") && at.block.is_none())) {
                    imports_allowed = false;
                }
            } else {
                imports_allowed = false;
            }
            self.rule(rule, &ctx);
        }
        self.importing.pop();
    }

    fn import(&mut self, from: &'s Stylesheet<'a>, at: &AtRule<'a>, ctx: &Ctx<'_, 's, 'a>) {
        let Some(rule) = parse_import_rule(&at.prelude) else {
            return;
        };
        let Some(sheet) = (self.import)(from, &rule) else {
            return;
        };
        let layer = match &rule.layer {
            ImportLayer::None => ctx.layer,
            ImportLayer::Anonymous => self.layers.anonymous(ctx.layer),
            ImportLayer::Named(n) => self.layers.declare(ctx.layer, n),
        };
        let conditional = rule.supports.is_some() || !rule.media.always_holds();
        let reasons = ctx.reasons.with_if(UnknownReason::Conditional, conditional);
        self.sheet(sheet, layer, reasons);
    }

    fn rule(&mut self, rule: &'s Spanned<Rule<'a>>, ctx: &Ctx<'_, 's, 'a>) {
        match &rule.node {
            Rule::Qualified(q) => {
                let parsed = if ctx.parent.is_some() {
                    parse_relative_selector_list(&q.prelude)
                } else {
                    parse_selector_list(&q.prelude)
                };
                // A rule whose selector does not parse is dropped, nested
                // rules and all, as a browser does.
                let Ok(selectors) = parsed else { return };
                let resolved = match ctx.parent {
                    Some((_, parent)) => selectors.resolve_nesting(Some(parent)),
                    // At the top of `@scope`, `&` is the scoping root, which
                    // this does not know: it stays `&` and matches Unknown.
                    None if ctx.scoped => Some(selectors.clone()),
                    None => selectors.resolve_nesting(None),
                };
                let unresolved = resolved.is_none();
                let selectors = resolved.unwrap_or(selectors);
                let reasons = ctx.reasons.with_if(UnknownReason::Nested, unresolved);
                self.style_rule(rule, selectors, &q.block.node, ctx, reasons);
            }
            Rule::At(at) => {
                let Some(block) = &at.block else {
                    // `@layer a, b;` fixes the order of layers to come.
                    if at.name.eq_ignore_ascii_case("layer")
                        && let Some(LayerRule::Statement(names)) = parse_layer_rule(at)
                    {
                        for n in &names {
                            self.layers.declare(ctx.layer, n);
                        }
                    }
                    return;
                };
                let bare = parser::unprefixed(&at.name);
                let layer = if bare.eq_ignore_ascii_case("layer") {
                    match parse_layer_rule(at) {
                        Some(LayerRule::Block(n)) => self.layers.declare(ctx.layer, &n),
                        Some(LayerRule::Anonymous) => self.layers.anonymous(ctx.layer),
                        // An invalid `@layer` is dropped with its contents.
                        _ => return,
                    }
                } else if parser::holds_style_rules(bare) {
                    ctx.layer
                } else {
                    // `@font-face`, `@page`, `@keyframes`, an unknown
                    // at-rule: nothing in it styles an element.
                    return;
                };
                let conditional = if bare.eq_ignore_ascii_case("layer") {
                    false
                } else if bare.eq_ignore_ascii_case("media") {
                    !parse_media_query_list(&at.prelude).always_holds()
                } else {
                    true
                };
                let inner = Ctx {
                    sheet: ctx.sheet,
                    layer,
                    reasons: ctx.reasons.with_if(UnknownReason::Conditional, conditional),
                    parent: ctx.parent,
                    scoped: ctx.scoped || bare.eq_ignore_ascii_case("scope"),
                };
                match inner.parent {
                    // Inside a style rule, a group rule's own declarations
                    // belong to that rule's selector.
                    Some((parent, selectors)) => {
                        let selectors = selectors.clone();
                        let reasons = inner.reasons;
                        self.style_rule(parent, selectors, &block.node, &inner, reasons);
                    }
                    None => {
                        for item in &block.node {
                            if let BlockItem::Rule(r) = item {
                                self.rule(r, &inner);
                            }
                        }
                    }
                }
            }
        }
    }

    /// A style rule's declarations, and the rules nested in its block, in
    /// source order. Declarations after a nested rule come after that rule's
    /// in the order of appearance (CSS Nesting's nested declarations rule),
    /// with this rule's selectors.
    fn style_rule(
        &mut self,
        rule: &'s Spanned<Rule<'a>>,
        selectors: SelectorList<'a>,
        items: &'s [BlockItem<'a>],
        ctx: &Ctx<'_, 's, 'a>,
        reasons: UnknownReasons,
    ) {
        let inner = Ctx {
            sheet: ctx.sheet,
            layer: ctx.layer,
            reasons,
            parent: Some((rule, &selectors)),
            scoped: ctx.scoped,
        };
        // Each run of declarations becomes a rule of its own; their
        // selectors are filled in at the end, so children can borrow them
        // meanwhile.
        let mut runs = Vec::new();
        let mut current = None;
        for item in items {
            match item {
                // A browser drops a declaration it cannot read.
                BlockItem::Declaration(d) if check_declaration(&d.node) == ValueCheck::Invalid => {}
                BlockItem::Declaration(d) => {
                    let index = *current.get_or_insert_with(|| {
                        self.rules.push(CascadeRule {
                            rule,
                            selectors: SelectorList::default(),
                            declarations: Vec::new(),
                            sheet: ctx.sheet,
                            layer: ctx.layer,
                            reasons,
                        });
                        runs.push(self.rules.len() - 1);
                        self.rules.len() - 1
                    });
                    self.rules[index].declarations.push((d, self.order));
                    self.order += 1;
                }
                BlockItem::Rule(r) => {
                    current = None;
                    self.rule(r, &inner);
                }
            }
        }
        if let Some((&last, rest)) = runs.split_last() {
            for &i in rest {
                self.rules[i].selectors = selectors.clone();
            }
            self.rules[last].selectors = selectors;
        }
    }
}

/// The layer tree (CSS Cascade 5): layers in order of first declaration,
/// each with its sublayers. Node 0 is the root, which holds the unlayered
/// rules.
struct Layers {
    nodes: Vec<LayerNode>,
}

struct LayerNode {
    name: Option<String>,
    children: Vec<usize>,
}

impl Layers {
    const ROOT: usize = 0;

    fn new() -> Layers {
        Layers {
            nodes: vec![LayerNode {
                name: None,
                children: Vec::new(),
            }],
        }
    }

    /// The layer `name` (dotted) inside `parent`, declaring it if new.
    fn declare(&mut self, parent: usize, name: &LayerName<'_>) -> usize {
        let mut at = parent;
        for part in &name.parts {
            let found = self.nodes[at]
                .children
                .iter()
                .copied()
                .find(|&c| self.nodes[c].name.as_deref() == Some(part.as_ref()));
            at = match found {
                Some(c) => c,
                None => self.push(at, Some(part.to_string())),
            };
        }
        at
    }

    fn anonymous(&mut self, parent: usize) -> usize {
        self.push(parent, None)
    }

    fn push(&mut self, parent: usize, name: Option<String>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(LayerNode {
            name,
            children: Vec::new(),
        });
        self.nodes[parent].children.push(id);
        id
    }

    /// Each layer's rank, higher winning for normal declarations: sublayers
    /// in order, before the layer's own rules, so the root — the unlayered
    /// rules — ranks highest.
    fn ranks(&self) -> Vec<u32> {
        let mut rank = vec![0; self.nodes.len()];
        let mut next = 0;
        // Post-order without recursion: a layer tree comes from author CSS
        // and can be deep.
        let mut stack = vec![(Self::ROOT, 0usize)];
        while let Some((node, child)) = stack.pop() {
            if let Some(&c) = self.nodes[node].children.get(child) {
                stack.push((node, child + 1));
                stack.push((c, 0));
            } else {
                rank[node] = next;
                next += 1;
            }
        }
        rank
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::HTML_NAMESPACE;

    /// A one-path DOM: html > body > (p#a.x, img.pic, p.y).
    #[derive(Clone, Copy)]
    struct El(usize);

    /// `(name, parent, attributes)`.
    type Node = (
        &'static str,
        Option<usize>,
        &'static [(&'static str, &'static str)],
    );

    const NODES: &[Node] = &[
        ("html", None, &[]),
        ("body", Some(0), &[]),
        ("p", Some(1), &[("id", "a"), ("class", "x")]),
        ("img", Some(1), &[("class", "pic"), ("width", "46")]),
        ("p", Some(1), &[("class", "y")]),
    ];

    impl Element for El {
        fn parent(&self) -> Option<Self> {
            NODES[self.0].1.map(El)
        }
        fn prev_sibling(&self) -> Option<Self> {
            let p = NODES[self.0].1?;
            (0..self.0).rev().find(|&i| NODES[i].1 == Some(p)).map(El)
        }
        fn next_sibling(&self) -> Option<Self> {
            let p = NODES[self.0].1?;
            (self.0 + 1..NODES.len())
                .find(|&i| NODES[i].1 == Some(p))
                .map(El)
        }
        fn has_children(&self) -> bool {
            NODES.iter().any(|n| n.1 == Some(self.0))
        }
        fn local_name(&self) -> &str {
            NODES[self.0].0
        }
        fn namespace(&self) -> Option<&str> {
            Some(HTML_NAMESPACE)
        }
        fn attribute(&self, local_name: &str) -> Option<&str> {
            NODES[self.0]
                .2
                .iter()
                .find(|(k, _)| *k == local_name)
                .map(|(_, v)| *v)
        }
        fn attribute_ns(&self, _: &str, _: &str) -> Option<&str> {
            None
        }
        fn is_html_document(&self) -> bool {
            false
        }
    }

    const IMG: El = El(3);
    const P_A: El = El(2);

    fn leak(css: &str) -> &'static Stylesheet<'static> {
        let css: &'static str = Box::leak(css.to_string().into_boxed_str());
        Box::leak(Box::new(crate::parse_stylesheet(css).0))
    }

    fn cascade(sheets: &[&str]) -> Cascade<'static, 'static> {
        let sheets: Vec<_> = sheets
            .iter()
            .map(|css| Sheet {
                stylesheet: leak(css),
                conditional: false,
            })
            .collect();
        Cascade::new(&sheets)
    }

    fn inline(css: &str) -> &'static [BlockItem<'static>] {
        let css: &'static str = Box::leak(css.to_string().into_boxed_str());
        Box::leak(crate::parse_block_contents(css).0.into_boxed_slice())
    }

    /// The status, and the winning declaration as `name: value`.
    fn ask(
        c: &Cascade<'static, 'static>,
        el: El,
        inl: Option<&str>,
        prop: &str,
    ) -> (Match, String) {
        let style = c.style_for(&el, inl.map(inline));
        let a = style.property(prop);
        let winner = a.decided_by().map_or(String::new(), |s| {
            let d = &s.candidate.declaration.node;
            let v: Vec<String> = d
                .value
                .iter()
                .map(|v| match &v.node {
                    ComponentValue::Token(Token::Ident(i)) => i.to_string(),
                    ComponentValue::Token(Token::Dimension { repr, unit, .. }) => {
                        format!("{repr}{unit}")
                    }
                    ComponentValue::Token(Token::Number { repr, .. }) => repr.to_string(),
                    _ => "?".into(),
                })
                .filter(|s| s != "?")
                .collect();
            format!("{}: {}", d.name, v.join(" "))
        });
        (a.status, winner)
    }

    fn reasons(c: &Cascade<'static, 'static>, el: El, prop: &str) -> Vec<UnknownReason> {
        let style = c.style_for(&el, None);
        let a = style.property(prop);
        a.decided_by()
            .map_or(vec![], |s| s.reasons.iter().collect())
    }

    #[test]
    fn nothing_sets_what_no_rule_names() {
        let c = cascade(&["img { height: 10px } p { width: 1px }"]);
        assert_eq!(ask(&c, IMG, None, "width"), (Match::No, String::new()));
        assert_eq!(ask(&c, IMG, None, "height").0, Match::Yes);
    }

    #[test]
    fn specificity_then_order_decide() {
        let c = cascade(&["img.pic { width: 1px } img { width: 2px } .pic { width: 3px }"]);
        assert_eq!(
            ask(&c, IMG, None, "width").1,
            "width: 1px",
            "(0,1,1) beats (0,1,0)"
        );
        let c = cascade(&[".pic { width: 1px }", ".pic { width: 2px }"]);
        assert_eq!(
            ask(&c, IMG, None, "width").1,
            "width: 2px",
            "later sheet wins"
        );
        // The list's matching selector decides the specificity, not its
        // most specific one.
        let c = cascade(&["#nope, img { width: 1px } .pic { width: 2px }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
    }

    #[test]
    fn important_and_the_style_attribute() {
        let c = cascade(&["#a { width: 1px } p { width: 2px !important }"]);
        assert_eq!(ask(&c, P_A, None, "width").1, "width: 2px");
        // The style attribute beats any normal rule, not an important one.
        assert_eq!(ask(&c, P_A, Some("width: 3px"), "width").1, "width: 2px");
        let c = cascade(&["#a { width: 1px }"]);
        assert_eq!(ask(&c, P_A, Some("width: 3px"), "width").1, "width: 3px");
        assert_eq!(
            ask(&c, P_A, Some("width: 3px !important"), "width").1,
            "width: 3px"
        );
        // Only the attribute sets it: Yes, from the attribute.
        let c = cascade(&[]);
        let style = c.style_for(&IMG, Some(inline("width: 50%")));
        let a = style.property("width");
        assert_eq!(a.status, Match::Yes);
        assert!(a.decided_by().unwrap().candidate.rule.is_none());
    }

    #[test]
    fn layers_order_normal_and_important_oppositely() {
        let c = cascade(&["@layer base, theme;\
             @layer theme { img { width: 1px; height: 1px !important } }\
             @layer base { img { width: 2px; height: 2px !important } }\
             img.pic { color: red }"]);
        // Normal: the later layer (theme) wins, whatever the order in source.
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 1px");
        // Important: the earlier layer (base) wins.
        assert_eq!(ask(&c, IMG, None, "height").1, "height: 2px");
        // Unlayered beats layered for normal declarations, even when less
        // specific.
        let c = cascade(&["@layer x { img.pic { width: 1px } } img { width: 2px }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
        // A layer's own rules beat its sublayers'.
        let c = cascade(&["@layer a { @layer b { img { width: 1px } } img { width: 2px } }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
        let c = cascade(&["@layer a.b { img { width: 1px } } @layer a { img { width: 2px } }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
    }

    #[test]
    fn shorthands_set_their_longhands() {
        let c =
            cascade(&["img { border: 1px solid } .pic { padding: 0 } img { font: 12px serif }"]);
        assert_eq!(ask(&c, IMG, None, "border-top-width").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "border-left-style").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "border-image-source").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "padding-left").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "font-size").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "margin-left").0, Match::No);
        // Asking about a shorthand finds its longhands.
        let c = cascade(&["img { border-top-width: 0 }"]);
        assert_eq!(ask(&c, IMG, None, "border").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "border-width").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "border-style").0, Match::No);
        // A shorthand later in the cascade beats an earlier longhand.
        let c = cascade(&["img { border-top-width: 5px } img { border: 1px solid }"]);
        assert_eq!(
            ask(&c, IMG, None, "border-top-width").1,
            "border: 1px solid"
        );
    }

    #[test]
    fn all_sets_everything_but_three() {
        let c = cascade(&["* { all: unset }"]);
        assert_eq!(ask(&c, IMG, None, "width").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "border").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "direction").0, Match::No);
        assert_eq!(ask(&c, IMG, None, "unicode-bidi").0, Match::No);
        assert_eq!(ask(&c, IMG, None, "--x").0, Match::No);
    }

    #[test]
    fn what_cannot_be_settled_is_unknown_with_its_reason() {
        use UnknownReason::*;
        for (css, prop, why) in [
            (
                "@media print { img { width: 1px } }",
                "width",
                vec![Conditional],
            ),
            (
                "@supports (display: grid) { img { width: 1px } }",
                "width",
                vec![Conditional],
            ),
            ("img:hover { width: 1px }", "width", vec![Selector]),
            ("img { inline-size: 1px }", "width", vec![Logical]),
            ("img { width: 1px }", "inline-size", vec![Logical]),
            (
                "img { margin-inline-start: 0 }",
                "margin-left",
                vec![Logical],
            ),
            ("img { -webkit-width: 1px }", "width", vec![Prefixed]),
            (
                "img { -webkit-border-before: 1px solid }",
                "border-top-width",
                vec![Logical, Prefixed],
            ),
            ("img { width: revert }", "width", vec![Revert]),
            (
                "@scope (body) { & img { width: 1px } }",
                "width",
                vec![Selector, Conditional],
            ),
            (
                "body { @media print { width: 1px } }",
                "width",
                vec![Conditional],
            ),
        ] {
            let c = cascade(&[css]);
            // The one rule that styles `body` itself; the rest style `img`.
            let el = if css.contains("@media print { width") {
                El(1)
            } else {
                IMG
            };
            let style = c.style_for(&el, None);
            assert_eq!(style.property(prop).status, Match::Unknown, "{css}");
            assert_eq!(reasons(&c, el, prop), why, "{css}");
        }
        // A certain setter anywhere makes it Yes, even below an uncertain one.
        let c = cascade(&["img { width: 1px } @media print { img { width: 2px !important } }"]);
        let style = c.style_for(&IMG, None);
        let a = style.property("width");
        assert_eq!(a.status, Match::Yes);
        assert_eq!(
            a.decided_by().unwrap().reasons.iter().collect::<Vec<_>>(),
            [Conditional]
        );
        assert!(a.decided_by_certain().unwrap().reasons.is_empty());
    }

    #[test]
    fn conditional_sheets_and_dropped_rules() {
        let sheet = leak("img { width: 1px }");
        let c = Cascade::new(&[Sheet {
            stylesheet: sheet,
            conditional: true,
        }]);
        assert_eq!(reasons(&c, IMG, "width"), [UnknownReason::ConditionalSheet]);
        // A rule whose selector does not parse is dropped, as in a browser.
        let c = cascade(&["img, . x { width: 1px } img::before { height: 1px }"]);
        assert_eq!(ask(&c, IMG, None, "width").0, Match::No);
        assert_eq!(
            ask(&c, IMG, None, "height").0,
            Match::No,
            "a pseudo-element is not the element"
        );
        // Rules in at-rules that style nothing are not candidates.
        let c = cascade(&["@font-face { width: 1px } @page { width: 1px }"]);
        assert_eq!(ask(&c, IMG, None, "width").0, Match::No);
    }

    #[test]
    fn custom_properties_are_case_sensitive() {
        let c = cascade(&["img { --Gap: 1px }"]);
        assert_eq!(ask(&c, IMG, None, "--Gap").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "--gap").0, Match::No);
        // Property names are not.
        let c = cascade(&["img { WIDTH: 1px }"]);
        assert_eq!(ask(&c, IMG, None, "Width").0, Match::Yes);
    }

    #[test]
    fn spans_point_back_at_the_stylesheet() {
        let css = "p { color: red }\nimg.pic { width: 1px }";
        let c = cascade(&[css]);
        let style = c.style_for(&IMG, None);
        let a = style.property("width");
        let s = a.decided_by().unwrap();
        assert_eq!(
            s.candidate.declaration.node.name_span.start_line_col(css),
            (2, 11)
        );
        assert_eq!(s.candidate.rule.unwrap().span.start_line_col(css), (2, 1));
        assert_eq!(
            s.candidate.specificity,
            Specificity {
                ids: 0,
                classes: 1,
                types: 1
            }
        );
    }

    #[test]
    fn nested_rules_resolve_through_their_parent() {
        // Implicit `& `, an explicit `&`, a leading combinator.
        for (css, want) in [
            ("body { img { width: 1px } }", Match::Yes),
            ("body { & img { width: 1px } }", Match::Yes),
            ("body { > img { width: 1px } }", Match::Yes),
            ("html { > img { width: 1px } }", Match::No),
            ("p { & img { width: 1px } }", Match::No),
            (".pic { &.pic { width: 1px } }", Match::Yes),
            (".pic { &:not(.pic) { width: 1px } }", Match::No),
            ("html { body { & > .pic { width: 1px } } }", Match::Yes),
            ("body { :not(&) > img { width: 1px } }", Match::No),
            ("body { @media all { img { width: 1px } } }", Match::Yes),
        ] {
            assert_eq!(ask(&cascade(&[css]), IMG, None, "width").0, want, "{css}");
        }
        // `&` counts the parent list's highest specificity, as `:is()`.
        let c = cascade(&[".pic { width: 1px } body, #z { & img { width: 2px } }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
        let style = c.style_for(&IMG, None);
        assert_eq!(
            style
                .property("width")
                .decided_by()
                .unwrap()
                .candidate
                .specificity,
            Specificity {
                ids: 1,
                classes: 0,
                types: 1
            }
        );
        // `&` at the top is the root.
        let c = cascade(&["& { width: 1px }"]);
        assert_eq!(ask(&c, El(0), None, "width").0, Match::Yes);
        assert_eq!(ask(&c, IMG, None, "width").0, Match::No);
    }

    #[test]
    fn declarations_after_a_nested_rule_come_after_it() {
        let c = cascade(&["img { width: 1px; & { width: 2px } width: 3px }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 3px");
        let c = cascade(&["img { width: 1px; & { width: 2px } }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 2px");
        // The later run keeps the rule's own selectors and span.
        let css = "img { & { width: 2px } width: 3px }";
        let c = cascade(&[css]);
        let style = c.style_for(&IMG, None);
        let a = style.property("width");
        assert_eq!(a.status, Match::Yes);
        assert_eq!(
            a.decided_by().unwrap().candidate.rule.unwrap().span.start,
            0
        );
    }

    #[test]
    fn nesting_too_large_to_resolve_stays_unknown() {
        let depth = 14;
        let mut css = String::from("*");
        for _ in 0..depth {
            css.push_str(" { :is(&, &)");
        }
        css.push_str(" { width: 1px }");
        css.push_str(&" }".repeat(depth));
        let c = cascade(&[&css]);
        let style = c.style_for(&IMG, None);
        let a = style.property("width");
        assert_eq!(a.status, Match::Unknown);
        assert!(
            a.decided_by()
                .unwrap()
                .reasons
                .contains(UnknownReason::Nested)
        );
    }

    #[test]
    fn media_all_is_no_condition() {
        for (css, want) in [
            ("@media all { img { width: 1px } }", Match::Yes),
            ("@media only all { img { width: 1px } }", Match::Yes),
            ("@media print, ALL { img { width: 1px } }", Match::Yes),
            ("@media { img { width: 1px } }", Match::Yes),
            ("@media not all { img { width: 1px } }", Match::Unknown),
            (
                "@media all and (color) { img { width: 1px } }",
                Match::Unknown,
            ),
        ] {
            assert_eq!(ask(&cascade(&[css]), IMG, None, "width").0, want, "{css}");
        }
    }

    #[test]
    fn imports_take_the_place_of_the_import_rule() {
        let main = leak(
            "@charset \"utf-8\"; @layer base;\
             @import \"b.css\" layer(x);\
             @import url(print.css) print;\
             @import \"self.css\";\
             img { width: 1px }\
             @import \"late.css\";",
        );
        let b = leak("img { width: 2px; height: 2px }");
        let print = leak("img { color: red }");
        let late = leak("img { margin: 0 }");
        let mut asked = Vec::new();
        let c = Cascade::with_imports(
            &[Sheet {
                stylesheet: main,
                conditional: false,
            }],
            |from, rule| {
                asked.push(rule.url.to_string());
                match rule.url.as_ref() {
                    "b.css" => Some(b),
                    "print.css" => Some(print),
                    "self.css" => Some(from),
                    "late.css" => Some(late),
                    _ => None,
                }
            },
        );
        // Unlayered beats the imported sheet's layer `x`.
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 1px");
        assert_eq!(ask(&c, IMG, None, "height").0, Match::Yes);
        assert_eq!(reasons(&c, IMG, "color"), [UnknownReason::Conditional]);
        // An `@import` after a style rule does not count.
        assert_eq!(ask(&c, IMG, None, "margin-left").0, Match::No);
        assert!(!asked.contains(&"late.css".to_string()));
        // A sheet importing itself is followed once.
        assert_eq!(asked.iter().filter(|u| *u == "self.css").count(), 1);
    }

    #[test]
    fn an_invalid_declaration_sets_nothing() {
        let c = cascade(&["img { width: 1px } img { width: banana }"]);
        assert_eq!(ask(&c, IMG, None, "width").1, "width: 1px");
        let c = cascade(&["img { height: 102 }"]);
        assert_eq!(ask(&c, IMG, None, "height").0, Match::No);
        // Unknown is not invalid: it stays.
        let c = cascade(&["img { width: 1px } img { width: var(--w) }"]);
        let style = c.style_for(&IMG, None);
        assert_eq!(style.property("width").setters.len(), 2);
        // The `style` attribute too.
        let c = cascade(&["img { width: 1px }"]);
        assert_eq!(ask(&c, IMG, Some("width: banana"), "width").1, "width: 1px");
    }
}
