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
//!
//! // Does a change reach this element? A property no browser knows does not.
//! let fixed = styloria::parse_stylesheet(".pic { border: 1px solid; colr: red }").0;
//! let fixed = Cascade::new(&[Sheet { stylesheet: &fixed, conditional: false }]);
//! let compare = cascade.compare_to(&fixed);
//! let result = compare.element(&Img, None, &Img, None);
//! assert_eq!(result.status, Match::Unknown); // the @media rule is gone
//! assert_eq!(result.differences[0].property, "width");
//! ```
//!
//! `@import` is followed when the caller hands over the imported sheets
//! ([`Cascade::with_imports`]): their rules take the `@import`'s place, in
//! its layer, under its conditions.
//!
//! A declaration whose value is certainly invalid
//! ([`check_declaration`] says
//! [`Invalid`](crate::ValueCheck::Invalid)) is dropped, as a browser drops
//! it: `width: banana` sets nothing. One whose value is Unknown stays.
//!
//! A pseudo-element's box has a cascade of its own:
//! [`Cascade::pseudo_elements_for`] names the ones rules could style on an
//! element, and [`Cascade::style_for_pseudo`] answers for one of them.
//!
//! **Comparing two states.** [`Cascade::compare_to`] asks whether a change to
//! a document's stylesheets, or to the document, changes what an element gets.
//! `Yes` for every element of a document means the same cascaded values, so the
//! same computed values, since what is inherited is the same too. That holds
//! as far as the comparison can see; it reads values as written and does not
//! normalize them:
//!
//! - `No` is a difference *as written*: `red` and `#f00` differ, and so does
//!   `url(missing.png)` and `none`, though both may draw the same thing.
//! - A `url()` is compared as text, so moving a sheet to another folder is not
//!   seen.
//! - The condition behind [`Sheet::conditional`] is the caller's, and is not
//!   compared: keep it the same in both states, or do not ask.
//! - Rules that style no element (`@font-face`, `@keyframes`, `@page`, …) are
//!   compared as text for the document as a whole
//!   ([`StyleComparer::differing_at_rules`]). A difference among them turns
//!   every `Yes` into `Unknown`.
//!
//! **Out of scope:** user-agent and user styles, and inheritance, are not
//! modelled.

use crate::at_rule::{
    ImportLayer, ImportRule, LayerName, LayerRule, Placement, evaluate_import_supports,
    evaluate_supports, parse_import_rule, parse_layer_rule, parse_media_query_list, placements,
};
use crate::matching::{Element, Match, Namespaces};
use crate::parser::{self, AtRule, BlockItem, ComponentValue, Declaration, Rule, Stylesheet};
use crate::properties::{logical_counterparts, longhands};
use crate::selector::{
    BeforeNarrowing, SelectorList, SimpleSelector, Specificity, parse_relative_selector_list,
    parse_selector_list, selector_list_before_7129,
};
use crate::serialize::{serialize_block_contents, serialize_rule, serialize_values};
use crate::span::Spanned;
use crate::token::Token;
use crate::values::{UnknownValue, ValueCheck, check_declaration};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

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
    /// The rule's selector reads otherwise before w3c/csswg-drafts#7129,
    /// which took some code points at or above U+0080 out of the ident code
    /// points (`.‣`, `:is(.a‣b)`), and the two readings answer this element
    /// differently: one matches and the other does not, or not with the same
    /// specificity. A rule only the old reading keeps
    /// ([`SyntaxErrorKind::NarrowedIdent`](crate::SyntaxErrorKind::NarrowedIdent))
    /// is one such.
    NarrowedIdent,
}

/// A set of [`UnknownReason`]s; empty for a declaration that certainly
/// applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct UnknownReasons(u8);

const ALL_REASONS: [UnknownReason; 8] = [
    UnknownReason::Selector,
    UnknownReason::Conditional,
    UnknownReason::ConditionalSheet,
    UnknownReason::Nested,
    UnknownReason::Logical,
    UnknownReason::Prefixed,
    UnknownReason::Revert,
    UnknownReason::NarrowedIdent,
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

    /// No reason: the answer is settled.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `r` is one of the reasons.
    pub fn contains(self, r: UnknownReason) -> bool {
        self.0 & Self::bit(r) != 0
    }

    /// The reasons, in [`UnknownReason`]'s declaration order.
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
    index: RuleIndex,
    namespaces: Vec<Namespaces<'a>>,
    /// For each layer, its place in the layer order; see `Layers`.
    layer_rank: Vec<u32>,
    unlayered: usize,
    /// Every sheet walked, imported ones included, in order.
    sheets: Vec<WalkedSheet<'s, 'a>>,
}

#[derive(Debug)]
struct WalkedSheet<'s, 'a> {
    stylesheet: &'s Stylesheet<'a>,
    reasons: UnknownReasons,
    conditions: Conditions<'s>,
}

/// What a rule sits inside: the group rules, `@import`s and parent style rules
/// around it, innermost first. Shared, since a deep nest of group rules holds
/// many rules and each must not copy the chain.
type Conditions<'s> = Option<Arc<Condition<'s>>>;

#[derive(Debug)]
struct Condition<'s> {
    rule: &'s Spanned<Rule<'s>>,
    outer: Conditions<'s>,
}

impl Drop for Condition<'_> {
    /// Iteratively: the chain is as deep as the rules allow, and a deep chain
    /// dropped by recursion would be a stack overflow.
    fn drop(&mut self) {
        let mut next = self.outer.take();
        while let Some(arc) = next {
            match Arc::try_unwrap(arc) {
                Ok(mut c) => next = c.outer.take(),
                Err(_) => break,
            }
        }
    }
}

/// A style rule, ready to match: its parsed selectors and its declarations.
#[derive(Debug)]
struct CascadeRule<'s, 'a> {
    rule: &'s Spanned<Rule<'a>>,
    /// Empty when a browser drops the rule.
    selectors: SelectorList<'a>,
    /// The selectors as they read before csswg-drafts#7129, where that
    /// reading may differ ([`UnknownReason::NarrowedIdent`]); empty when it
    /// drops the rule.
    before_7129: Option<SelectorList<'a>>,
    declarations: Vec<(&'s Spanned<Declaration<'a>>, u32)>,
    sheet: usize,
    layer: usize,
    reasons: UnknownReasons,
    conditions: Conditions<'s>,
}

/// The rules each element is worth trying, as a browser finds them: by the
/// id, a class or the name the subject compound of one of their selectors
/// requires. A rule with a selector that requires none of them (`*`,
/// `:is(…)`, `::before`) is always tried. Rules are listed by index, so the
/// ones an element gets are tried in the order they were written.
#[derive(Debug, Default)]
struct RuleIndex {
    ids: HashMap<String, Vec<u32>>,
    classes: HashMap<String, Vec<u32>>,
    /// Lower-cased: in an HTML document names compare case-insensitively,
    /// and matching sorts out the rest.
    names: HashMap<String, Vec<u32>>,
    always: Vec<u32>,
    /// The rules with a selector for a pseudo-element, in order.
    pseudo: Vec<u32>,
}

/// What a subject compound requires of the element.
enum Key<'k> {
    Id(&'k str),
    Class(&'k str),
    Name(&'k str),
}

impl RuleIndex {
    fn new(rules: &[CascadeRule<'_, '_>]) -> RuleIndex {
        let mut index = RuleIndex::default();
        let add = |map: &mut HashMap<String, Vec<u32>>, k: String, r: u32| {
            let list = map.entry(k).or_default();
            if list.last() != Some(&r) {
                list.push(r);
            }
        };
        for (r, rule) in rules.iter().enumerate() {
            let r = u32::try_from(r).unwrap_or(u32::MAX);
            let mut always = false;
            let mut pseudo = false;
            let old = rule.before_7129.iter().flat_map(|l| &l.selectors);
            for s in rule.selectors.selectors.iter().chain(old) {
                pseudo |= s.node.compounds.last().is_some_and(|c| {
                    c.node
                        .simples
                        .iter()
                        .any(|s| matches!(s.node, SimpleSelector::PseudoElement(_)))
                });
                let subject = s.node.compounds.last().map(|c| &c.node.simples);
                match subject.and_then(|simples| key(simples)) {
                    Some(Key::Id(v)) => add(&mut index.ids, v.to_string(), r),
                    Some(Key::Class(v)) => add(&mut index.classes, v.to_string(), r),
                    Some(Key::Name(v)) => add(&mut index.names, v.to_ascii_lowercase(), r),
                    None => always = true,
                }
            }
            if always {
                index.always.push(r);
            }
            if pseudo {
                index.pseudo.push(r);
            }
        }
        index
    }

    /// The rules worth trying on `element`, by index, in order.
    fn rules_for<E: Element>(&self, element: &E) -> Vec<u32> {
        let mut out = self.always.clone();
        if let Some(list) = element.id().and_then(|id| self.ids.get(id)) {
            out.extend_from_slice(list);
        }
        if !self.classes.is_empty() {
            element.each_class(&mut |c| {
                if let Some(list) = self.classes.get(c) {
                    out.extend_from_slice(list);
                }
            });
        }
        let name = element.local_name();
        let list = if name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.names.get(&name.to_ascii_lowercase())
        } else {
            self.names.get(name)
        };
        if let Some(list) = list {
            out.extend_from_slice(list);
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The most telling thing a compound requires: an id, else a class, else a
/// name. `None` if it requires none of them.
fn key<'k>(simples: &'k [Spanned<SimpleSelector<'_>>]) -> Option<Key<'k>> {
    let mut found = None;
    for s in simples {
        match &s.node {
            SimpleSelector::Id(v) => return Some(Key::Id(v)),
            SimpleSelector::Class(v) if !matches!(found, Some(Key::Class(_))) => {
                found = Some(Key::Class(v));
            }
            SimpleSelector::Type(n) if found.is_none() => found = Some(Key::Name(&n.local.node)),
            _ => {}
        }
    }
    found
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
    conditions: Option<&'e Condition<'e>>,
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
            sheets: Vec::with_capacity(sheets.len()),
        };
        for sheet in sheets {
            let reasons = UnknownReasons::default()
                .with_if(UnknownReason::ConditionalSheet, sheet.conditional);
            b.sheet(sheet.stylesheet, Layers::ROOT, reasons, None);
        }
        let layer_rank = b.layers.ranks();
        Cascade {
            index: RuleIndex::new(&b.rules),
            rules: b.rules,
            namespaces: b.namespaces,
            unlayered: layer_rank.len() - 1,
            layer_rank,
            sheets: b.sheets,
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
        let mut candidates = self.rule_candidates(&self.index.rules_for(element), element, None);
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
                    conditions: None,
                });
            }
        }
        candidates.sort_by_key(|c| std::cmp::Reverse(c.key));
        ElementStyle { candidates }
    }

    /// Every declaration that could apply to the pseudo-element `pseudo` of
    /// `element`, highest precedence first. The name is compared as
    /// [`ComplexSelector::pseudo_element`](crate::ComplexSelector::pseudo_element)
    /// gives it, with or without its colons. A `style` attribute styles the
    /// element, not its pseudo-elements, so there is none to give.
    pub fn style_for_pseudo<'e, E: Element>(&'e self, element: &E, pseudo: &str) -> ElementStyle<'e>
    where
        's: 'e,
        'a: 'e,
    {
        let pseudo = crate::matching::pseudo_name(pseudo);
        let mut candidates = self.rule_candidates(&self.index.pseudo, element, Some(&pseudo));
        candidates.sort_by_key(|c| std::cmp::Reverse(c.key));
        ElementStyle { candidates }
    }

    /// The pseudo-elements of `element` some rule could style, sorted, with
    /// names as [`style_for_pseudo`](Self::style_for_pseudo) takes them.
    pub fn pseudo_elements_for<E: Element>(&self, element: &E) -> Vec<String> {
        let mut out = BTreeSet::new();
        for &r in &self.index.pseudo {
            let r = &self.rules[r as usize];
            let ns = &self.namespaces[r.sheet];
            let old = r.before_7129.iter().flat_map(|l| &l.selectors);
            for s in r.selectors.selectors.iter().chain(old) {
                if let Some(name) = s.node.pseudo_element()
                    && !out.contains(&name)
                    && s.node.matches_pseudo(element, ns, &name) != Match::No
                {
                    out.insert(name);
                }
            }
        }
        out.into_iter().collect()
    }

    /// Compare this state of a document with another:
    /// [`StyleComparer::element`] answers per element. See the
    /// [module documentation](self) for what the answer covers.
    pub fn compare_to<'c, 't, 'b>(
        &'c self,
        after: &'c Cascade<'t, 'b>,
    ) -> StyleComparer<'c, 's, 'a, 't, 'b> {
        let (x, y) = (self.other_rules(), after.other_rules());
        let (only_x, only_y) = (differing(&x, &y), differing(&y, &x));
        let mut differing_at_rules: Vec<String> = only_x.into_iter().chain(only_y).collect();
        differing_at_rules.sort();
        differing_at_rules.dedup();
        StyleComparer {
            before: self,
            after,
            differing_at_rules,
        }
    }

    /// The rules that style no element, in order, as text with what they sit
    /// inside.
    fn other_rules(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for s in &self.sheets {
            let mut around = condition_text(s.conditions.as_deref());
            around.push_str(&format!("{:?} ", s.reasons));
            let rules: Vec<_> = s.stylesheet.rules.iter().collect();
            other_rules(&rules, &around, &mut out);
        }
        out
    }

    /// The declarations of the rules listed, as they apply to `element` or,
    /// given `pseudo`, to that pseudo-element of it. Unsorted.
    fn rule_candidates<'e, E: Element>(
        &'e self,
        rules: &[u32],
        element: &E,
        pseudo: Option<&str>,
    ) -> Vec<Candidate<'e>>
    where
        's: 'e,
        'a: 'e,
    {
        let mut candidates = Vec::new();
        for &r in rules {
            let r = &self.rules[r as usize];
            let ns = &self.namespaces[r.sheet];
            let best_of = |list: &SelectorList<'a>| {
                let mut best = Match::No;
                let mut specificity = Specificity::default();
                for s in &list.selectors {
                    let m = match pseudo {
                        Some(p) => s.node.matches_pseudo(element, ns, p),
                        None => s.node.matches(element, ns),
                    };
                    if m != Match::No {
                        best = best.or(m);
                        specificity = specificity.max(s.node.specificity());
                    }
                }
                (best, specificity)
            };
            let (mut best, mut specificity) = best_of(&r.selectors);
            // Either reading may be the one a browser follows: where they
            // part, the rule only possibly applies.
            let mut unsure = best == Match::Unknown;
            let mut narrowed = false;
            if let Some(old) = &r.before_7129 {
                let (old_best, old_specificity) = best_of(old);
                unsure |= old_best == Match::Unknown;
                if (old_best, old_specificity) != (best, specificity) {
                    narrowed = true;
                    if best != Match::No || old_best != Match::No {
                        best = Match::Unknown;
                    }
                    specificity = specificity.max(old_specificity);
                }
            }
            if best == Match::No {
                continue;
            }
            // A nested rule's selector is Unknown because its `&` is not
            // resolved; `Nested` already says so.
            let unsure = unsure && !r.reasons.contains(UnknownReason::Nested);
            let reasons = r
                .reasons
                .with_if(UnknownReason::Selector, unsure)
                .with_if(UnknownReason::NarrowedIdent, narrowed);
            for (d, order) in &r.declarations {
                candidates.push(Candidate {
                    declaration: d,
                    rule: Some(r.rule),
                    specificity,
                    reasons,
                    key: self.key(d.node.important, false, r.layer, specificity, *order),
                    conditions: r.conditions.as_deref(),
                });
            }
        }
        candidates
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

/// One state of a document compared with another; made by
/// [`Cascade::compare_to`].
#[derive(Debug)]
pub struct StyleComparer<'c, 's, 'a, 't, 'b> {
    before: &'c Cascade<'s, 'a>,
    after: &'c Cascade<'t, 'b>,
    /// The rules that style no element (`@font-face`, `@keyframes`, `@page`,
    /// an unknown at-rule) and are in only one of the states, as text, sorted.
    /// Not empty: every [`Match::Yes`] becomes [`Match::Unknown`], since the
    /// comparison of elements cannot see what such a rule does.
    pub differing_at_rules: Vec<String>,
}

/// What a change does to one element and its pseudo-elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleComparison {
    /// `Yes`: the same as far as the comparison sees. `No`: some property
    /// certainly differs as written. `Unknown`: neither can be said.
    pub status: Match,
    /// The properties that are not the same, sorted, longhands where the
    /// declaration is a shorthand.
    pub differences: Vec<StyleDifference>,
}

/// One property that is not the same in both states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleDifference {
    /// The pseudo-element, as [`Cascade::style_for_pseudo`] takes it; `None`
    /// for the element itself.
    pub pseudo: Option<String>,
    pub property: String,
    /// `No` or `Unknown`, as [`StyleComparison::status`].
    pub status: Match,
    /// Why it is `Unknown`, when a declaration only possibly applies. Empty
    /// for `No`, and for two certain winners that cannot be told apart by
    /// reading them (`margin: 0` and `margin-left: 0`).
    pub reasons: UnknownReasons,
}

impl StyleComparer<'_, '_, '_, '_, '_> {
    /// Compare `before_element` in the first state with `after_element` in
    /// the second, which the caller knows to be the same element. Their `style`
    /// attributes are read as [`Cascade::style_for`] reads them.
    pub fn element<A: Element, B: Element>(
        &self,
        before_element: &A,
        before_inline: Option<&[BlockItem<'_>]>,
        after_element: &B,
        after_inline: Option<&[BlockItem<'_>]>,
    ) -> StyleComparison {
        let mut differences = Vec::new();
        let x = self.before.style_for(before_element, before_inline);
        let y = self.after.style_for(after_element, after_inline);
        compare_styles(&x, &y, None, &mut differences);
        let mut pseudos = self.before.pseudo_elements_for(before_element);
        pseudos.extend(self.after.pseudo_elements_for(after_element));
        pseudos.sort();
        pseudos.dedup();
        for p in pseudos {
            let x = self.before.style_for_pseudo(before_element, &p);
            let y = self.after.style_for_pseudo(after_element, &p);
            compare_styles(&x, &y, Some(&p), &mut differences);
        }
        let status = if differences.iter().any(|d| d.status == Match::No) {
            Match::No
        } else if !differences.is_empty() || !self.differing_at_rules.is_empty() {
            Match::Unknown
        } else {
            Match::Yes
        };
        StyleComparison {
            status,
            differences,
        }
    }
}

/// The properties two sets of candidates could set, each by its own name, a
/// shorthand by its longhands, a prefixed name by the name it stands for. A
/// property no specification or browser data knows sets nothing, and is left
/// out.
fn asked_properties(style: &ElementStyle<'_>, names: &mut BTreeSet<String>) {
    for c in &style.candidates {
        let d = &c.declaration.node;
        if d.name.starts_with("--") {
            names.insert(d.name.to_string());
            continue;
        }
        if check_declaration(d) == ValueCheck::Unknown(UnknownValue::UnknownProperty) {
            continue;
        }
        let lower = d.name.to_ascii_lowercase();
        let name = if lower.starts_with('-') && longhands(&lower).is_none() {
            parser::unprefixed(&lower)
        } else {
            lower.as_str()
        };
        match longhands(name) {
            Some(l) => names.extend(l.iter().map(|n| n.to_string())),
            None => {
                names.insert(name.to_string());
            }
        }
    }
}

/// A setter as the comparison sees it. An uncertain one is the same only under
/// the same conditions, so what it sits inside counts too.
fn setter_key(s: &Setter<'_, '_>) -> String {
    let d = &s.candidate.declaration.node;
    let mut key = d.name.to_ascii_lowercase();
    key.push(':');
    serialize_values(&d.value, &mut key);
    if !s.reasons.is_empty() {
        key.push_str(&format!(" {:?} ", s.reasons));
        if let Some(r) = s.candidate.rule {
            rule_head(r, &mut key);
        }
        key.push_str(&condition_text(s.candidate.conditions));
    }
    key
}

fn rule_head(rule: &Spanned<Rule<'_>>, out: &mut String) {
    if let Rule::At(a) = &rule.node {
        out.push('@');
        out.push_str(&a.name.to_ascii_lowercase());
        out.push(' ');
    }
    // The whitespace at its ends is the block's and the name's, not the
    // prelude's: `a {` and `a{` are one rule.
    let mut prelude = rule.node.prelude();
    let is_space = |v: &Spanned<ComponentValue<'_>>| {
        matches!(v.node, ComponentValue::Token(Token::Whitespace))
    };
    while let [first, rest @ ..] = prelude
        && is_space(first)
    {
        prelude = rest;
    }
    while let [rest @ .., last] = prelude
        && is_space(last)
    {
        prelude = rest;
    }
    serialize_values(prelude, out);
    out.push_str(" / ");
}

fn condition_text(mut c: Option<&Condition<'_>>) -> String {
    let mut out = String::new();
    while let Some(here) = c {
        rule_head(here.rule, &mut out);
        c = here.outer.as_deref();
    }
    out
}

/// The setters that could win: down to the first certain one.
fn could_win<'p, 'c, 'e>(answer: &'p PropertyAnswer<'c, 'e>) -> &'p [Setter<'c, 'e>] {
    let end = answer
        .setters
        .iter()
        .position(|s| s.reasons.is_empty())
        .map_or(answer.setters.len(), |i| i + 1);
    &answer.setters[..end]
}

/// `inherit`, `initial` and the like, which may give what no declaration gives.
fn is_css_wide(d: &Declaration<'_>) -> bool {
    let mut text = String::new();
    serialize_values(&d.value, &mut text);
    ["inherit", "initial", "unset", "revert", "revert-layer"]
        .iter()
        .any(|k| text.eq_ignore_ascii_case(k))
}

fn compare_styles(
    x: &ElementStyle<'_>,
    y: &ElementStyle<'_>,
    pseudo: Option<&str>,
    out: &mut Vec<StyleDifference>,
) {
    let mut names = BTreeSet::new();
    asked_properties(x, &mut names);
    asked_properties(y, &mut names);
    for property in names {
        let (a, b) = (x.property(&property), y.property(&property));
        let (a, b) = (could_win(&a), could_win(&b));
        if a.len() == b.len() && a.iter().zip(b).all(|(p, q)| setter_key(p) == setter_key(q)) {
            continue;
        }
        let reasons = a.iter().chain(b).fold(UnknownReasons::default(), |r, s| {
            UnknownReasons(r.0 | s.reasons.0)
        });
        let name = |s: &Setter<'_, '_>| s.candidate.declaration.node.name.to_ascii_lowercase();
        let certain = match (a.first(), b.first()) {
            (Some(p), Some(q)) if p.reasons.is_empty() && q.reasons.is_empty() => {
                name(p) == name(q)
            }
            (Some(s), None) | (None, Some(s)) if s.reasons.is_empty() => {
                !is_css_wide(&s.candidate.declaration.node)
            }
            _ => false,
        };
        out.push(StyleDifference {
            pseudo: pseudo.map(str::to_string),
            property,
            status: if certain { Match::No } else { Match::Unknown },
            reasons: if certain {
                UnknownReasons::default()
            } else {
                reasons
            },
        });
    }
}

/// The rules among `rules` that style no element, with what surrounds them.
/// The first string is what tells it apart, the second what to show.
fn other_rules(rules: &[&Spanned<Rule<'_>>], around: &str, out: &mut Vec<(String, String)>) {
    for r in rules {
        let Rule::At(at) = &r.node else { continue };
        let bare = parser::unprefixed(&at.name);
        let plain = ["charset", "import", "namespace", "layer"]
            .iter()
            .any(|n| bare.eq_ignore_ascii_case(n));
        if at.block.is_none() && plain {
            continue;
        }
        if let Some(block) = &at.block
            && (bare.eq_ignore_ascii_case("layer") || parser::holds_style_rules(bare))
        {
            let mut inner = around.to_string();
            rule_head(r, &mut inner);
            let nested: Vec<_> = block
                .node
                .iter()
                .filter_map(|i| match i {
                    BlockItem::Rule(r) => Some(r),
                    BlockItem::Declaration(_) => None,
                })
                .collect();
            other_rules(&nested, &inner, out);
            continue;
        }
        let mut key = around.to_string();
        rule_head(r, &mut key);
        if let Some(block) = &at.block {
            key.push_str(&serialize_block_contents(&block.node));
        }
        let mut text = String::new();
        serialize_rule(&r.node, &mut text);
        out.push((key, text));
    }
}

/// What `x` holds that `y` does not, or holds in another order: rules that
/// style no element can depend on their order (the later `@font-face` wins).
fn differing(x: &[(String, String)], y: &[(String, String)]) -> Vec<String> {
    if x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.0 == q.0) {
        return Vec::new();
    }
    let keys: BTreeSet<&str> = y.iter().map(|q| q.0.as_str()).collect();
    let only: Vec<String> = x
        .iter()
        .filter(|p| !keys.contains(p.0.as_str()))
        .map(|p| p.1.clone())
        .collect();
    if only.is_empty() {
        // The same rules, in another order.
        x.iter()
            .zip(y)
            .filter(|(p, q)| p.0 != q.0)
            .map(|(p, _)| p.1.clone())
            .collect()
    } else {
        only
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
    sheets: Vec<WalkedSheet<'s, 'a>>,
}

/// Where in the sheets a rule sits.
struct Ctx<'p, 's, 'a> {
    sheet: usize,
    layer: usize,
    reasons: UnknownReasons,
    /// The style rule this one is nested in, and its resolved selectors:
    /// as they read, and before csswg-drafts#7129 where that may differ.
    /// An empty list is a rule a browser drops.
    parent: Option<(
        &'s Spanned<Rule<'a>>,
        &'p SelectorList<'a>,
        Option<&'p SelectorList<'a>>,
    )>,
    /// Inside `@scope`, where `&` at the top stands for the scoping root.
    scoped: bool,
    conditions: Conditions<'s>,
}

fn around<'s>(rule: &'s Spanned<Rule<'s>>, outer: &Conditions<'s>) -> Conditions<'s> {
    Some(Arc::new(Condition {
        rule,
        outer: outer.clone(),
    }))
}

impl<'s, 'a> Builder<'_, 's, 'a> {
    /// A whole sheet, its imports in their place.
    fn sheet(
        &mut self,
        sheet: &'s Stylesheet<'a>,
        layer: usize,
        reasons: UnknownReasons,
        conditions: Conditions<'s>,
    ) {
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
            conditions: conditions.clone(),
        };
        self.sheets.push(WalkedSheet {
            stylesheet: sheet,
            reasons,
            conditions,
        });
        self.namespaces.push(Namespaces::from_stylesheet(sheet));
        self.importing.push(sheet);
        // A misplaced `@import` or `@namespace` is ignored, as a browser does.
        for (rule, place) in sheet.rules.iter().zip(placements(sheet)) {
            if let Rule::At(at) = &rule.node
                && at.name.eq_ignore_ascii_case("import")
            {
                if place == Placement::InOrder {
                    self.import(sheet, rule, at, &ctx);
                }
                continue;
            }
            self.rule(rule, &ctx);
        }
        self.importing.pop();
    }

    fn import(
        &mut self,
        from: &'s Stylesheet<'a>,
        written: &'s Spanned<Rule<'a>>,
        at: &AtRule<'a>,
        ctx: &Ctx<'_, 's, 'a>,
    ) {
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
        // A `supports()` no browser can meet keeps the sheet out.
        if rule
            .supports
            .as_ref()
            .is_some_and(|s| evaluate_import_supports(s) == Match::No)
        {
            return;
        }
        let conditional = rule.supports.is_some() || !rule.media.always_holds();
        let reasons = ctx.reasons.with_if(UnknownReason::Conditional, conditional);
        self.sheet(sheet, layer, reasons, around(written, &ctx.conditions));
    }

    fn rule(&mut self, rule: &'s Spanned<Rule<'a>>, ctx: &Ctx<'_, 's, 'a>) {
        match &rule.node {
            Rule::Qualified(q) => {
                let relative = ctx.parent.is_some();
                let parsed = if relative {
                    parse_relative_selector_list(&q.prelude)
                } else {
                    parse_selector_list(&q.prelude)
                };
                // A rule whose selector does not parse is dropped, nested
                // rules and all, as a browser does: its list stays empty.
                let now = parsed.unwrap_or_default();
                // Before csswg-drafts#7129 it may read otherwise, through its
                // own selector or its parent's.
                let parent_now = ctx.parent.map(|(_, now, _)| now);
                let parent_before = ctx.parent.and_then(|(_, _, before)| before);
                let before = match selector_list_before_7129(&q.prelude, relative) {
                    BeforeNarrowing::Same => parent_before.map(|_| now.clone()),
                    BeforeNarrowing::Parsed(list) => Some(list),
                    BeforeNarrowing::Dropped => Some(SelectorList::default()),
                };
                let resolve = |list: SelectorList<'a>, parent: Option<&SelectorList<'a>>| {
                    let resolved = match parent {
                        _ if list.selectors.is_empty() => return (list, false),
                        // Inside a rule a browser drops: dropped too.
                        Some(p) if p.selectors.is_empty() => {
                            return (SelectorList::default(), false);
                        }
                        Some(p) => list.resolve_nesting(Some(p)),
                        // At the top of `@scope`, `&` is the scoping root,
                        // which this does not know: it stays `&` and
                        // matches Unknown.
                        None if ctx.scoped => Some(list.clone()),
                        None => list.resolve_nesting(None),
                    };
                    match resolved {
                        Some(r) => (r, false),
                        None => (list, true),
                    }
                };
                let (now, unresolved) = resolve(now, parent_now);
                let (before, unresolved_before) = match before {
                    Some(b) => {
                        let (b, unresolved) = resolve(b, parent_before.or(parent_now));
                        (Some(b), unresolved)
                    }
                    None => (None, false),
                };
                if now.selectors.is_empty()
                    && before.as_ref().is_none_or(|b| b.selectors.is_empty())
                {
                    return;
                }
                let reasons = ctx
                    .reasons
                    .with_if(UnknownReason::Nested, unresolved || unresolved_before);
                self.style_rule(rule, now, before, &q.block.node, ctx, reasons);
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
                } else if bare.eq_ignore_ascii_case("supports")
                    && evaluate_supports(&at.prelude) == Match::No
                {
                    // No browser meets the condition: nothing in it applies.
                    return;
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
                    conditions: around(rule, &ctx.conditions),
                };
                match inner.parent {
                    // Inside a style rule, a group rule's own declarations
                    // belong to that rule's selector.
                    Some((parent, selectors, before)) => {
                        let selectors = selectors.clone();
                        let before = before.cloned();
                        let reasons = inner.reasons;
                        self.style_rule(parent, selectors, before, &block.node, &inner, reasons);
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
        before_7129: Option<SelectorList<'a>>,
        items: &'s [BlockItem<'a>],
        ctx: &Ctx<'_, 's, 'a>,
        reasons: UnknownReasons,
    ) {
        let inner = Ctx {
            sheet: ctx.sheet,
            layer: ctx.layer,
            reasons,
            parent: Some((rule, &selectors, before_7129.as_ref())),
            scoped: ctx.scoped,
            conditions: around(rule, &ctx.conditions),
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
                            before_7129: None,
                            declarations: Vec::new(),
                            sheet: ctx.sheet,
                            layer: ctx.layer,
                            reasons,
                            conditions: ctx.conditions.clone(),
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
                self.rules[i].before_7129 = before_7129.clone();
            }
            self.rules[last].selectors = selectors;
            self.rules[last].before_7129 = before_7129;
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
    fn a_pseudo_element_has_its_own_candidates() {
        let c = cascade(&[
            "img[width]::before { content: 'x' } img:before { color: red } \
             p::before { color: blue } img { width: 1px }",
        ]);
        assert_eq!(c.pseudo_elements_for(&IMG), ["before"]);
        let names = |style: ElementStyle<'_>| -> Vec<String> {
            let c = style.candidates.iter();
            c.map(|c| c.declaration.node.name.to_string()).collect()
        };
        // The attribute selector counts for more, so it comes first.
        assert_eq!(
            names(c.style_for_pseudo(&IMG, "::BEFORE")),
            ["content", "color"]
        );
        assert_eq!(
            names(c.style_for_pseudo(&IMG, "before")),
            ["content", "color"]
        );
        assert!(names(c.style_for_pseudo(&IMG, "after")).is_empty());
        assert_eq!(
            names(c.style_for(&IMG, None)),
            ["width"],
            "the element's own"
        );
        assert_eq!(names(c.style_for_pseudo(&P_A, "before")), ["color"]);
    }

    #[test]
    fn a_pseudo_element_only_a_tree_cannot_settle_is_unknown() {
        for (css, name) in [
            ("img::before:hover { color: red }", "before"),
            ("img::part(x) { color: red }", "part(x)"),
        ] {
            let c = cascade(&[css]);
            assert_eq!(c.pseudo_elements_for(&IMG), [name]);
            let style = c.style_for_pseudo(&IMG, name);
            assert_eq!(style.property("color").status, Match::Unknown, "{css}");
        }
        // A chain is its own target, not the first of its links.
        let c = cascade(&["img::before::marker { color: red }"]);
        assert_eq!(c.pseudo_elements_for(&IMG), ["before::marker"]);
        assert_eq!(c.style_for_pseudo(&IMG, "before").candidates.len(), 0);
        // What stands left of it is about the element, as ever.
        let c = cascade(&["p > img::after { color: red } body > img::after { top: 0 }"]);
        assert_eq!(c.style_for_pseudo(&IMG, "after").candidates.len(), 1);
    }

    /// One element, both states, no attribute.
    fn compare(before: &str, after: &str) -> StyleComparison {
        let (x, y) = (cascade(&[before]), cascade(&[after]));
        x.compare_to(&y).element(&IMG, None, &IMG, None)
    }

    #[test]
    fn what_cannot_change_a_style_compares_the_same() {
        let base = "img { width: 1px; margin: 0 } p { color: red }";
        for after in [
            base,
            "/* note */ img{ width:1px ;margin:0 }  p{color:red}",
            "img { width: 1px; margin: 0; width: banana } p { color: red }",
            "img { width: 1px; margin: 0; widht: 2px } p { color: red }",
            "img { width: 1px; margin: 0 } p { color: red",
            "img { width: 1px; margin: 0 }",
            "img { width: 1px !important; margin: 0 }",
            "img { width: 9px; width: 1px; margin: 0 }",
        ] {
            let c = compare(base, after);
            assert_eq!(c.status, Match::Yes, "{after}: {:?}", c.differences);
        }
    }

    #[test]
    fn a_certain_difference_is_no_and_names_the_longhand() {
        let c = compare("img { width: 1px }", "img { width: 2px }");
        assert_eq!(c.status, Match::No);
        assert_eq!(c.differences[0].property, "width");
        assert_eq!(c.differences[0].pseudo, None);
        assert_eq!(compare("img {}", "img { width: 2px }").status, Match::No);
        assert_eq!(compare("img { color: red }", "img {}").status, Match::No);
        let c = compare("img { margin: 0 }", "img { margin: 1px }");
        assert_eq!(c.differences.len(), 4);
        assert_eq!(c.differences[0].property, "margin-bottom");
        // Custom properties reach the values that use them.
        assert_eq!(
            compare("img { --x: 1 }", "img { --x: 2 }").status,
            Match::No
        );
        // As written: no normalizing.
        assert_eq!(
            compare("img { color: red }", "img { color: #f00 }").status,
            Match::No
        );
    }

    #[test]
    fn what_reading_cannot_tell_apart_is_unknown() {
        for (before, after) in [
            // The same value through another name.
            (
                "img { margin-top: 0; margin-right: 0; margin-bottom: 0; margin-left: 0 }",
                "img { margin: 0 }",
            ),
            // The value a browser would compute anyway.
            ("img {}", "img { color: inherit }"),
            ("img {}", "img { color: initial }"),
            // Added, removed or changed under what a static tree cannot say.
            ("img {}", "img:hover { width: 1px }"),
            ("img {}", "@media print { img { width: 1px } }"),
            (
                "img { width: 1px }",
                "img { width: 1px } img:hover { width: 2px }",
            ),
            // Standard and prefixed, which only some reading systems honour.
            ("img { -webkit-hyphens: auto }", "img {}"),
            ("img { -epub-hyphens: auto }", "img { -epub-hyphens: none }"),
            // A logical property may be either physical one.
            ("img { margin-left: 0 }", "img { margin-inline-start: 0 }"),
        ] {
            let c = compare(before, after);
            assert_eq!(c.status, Match::Unknown, "{before} -> {after}: {c:?}");
        }
    }

    /// The same uncertain value is not the same under another condition.
    #[test]
    fn an_uncertain_setter_counts_with_its_condition() {
        let same = |css: &str| assert_eq!(compare(css, css).status, Match::Yes, "{css}");
        same("@media print { img { width: 1px } }");
        same("img:hover { width: 1px } img { width: 2px }");
        for (before, after) in [
            (
                "@media print { img { width: 1px } }",
                "@media screen { img { width: 1px } }",
            ),
            ("img:hover { width: 1px }", "img:focus { width: 1px }"),
            (
                "img { &:hover { width: 1px } }",
                "img { &:focus { width: 1px } }",
            ),
            (
                "body img:hover { width: 1px }",
                "html img:hover { width: 1px }",
            ),
            (
                "@media print { @supports (color: red) { img { width: 1px } } }",
                "@media screen { @supports (color: red) { img { width: 1px } } }",
            ),
        ] {
            let c = compare(before, after);
            assert_eq!(c.status, Match::Unknown, "{before} -> {after}: {c:?}");
            assert!(!c.differences[0].reasons.is_empty());
        }
        // Below a certain winner nothing can win, so what changes there does
        // not count; a certain winner's own changes do.
        let top = "img { width: 1px !important }";
        let (x, y) = (
            format!("{top} img:hover {{ width: 2px }}"),
            format!("{top} img:focus {{ width: 3px }}"),
        );
        assert_eq!(compare(&x, &y).status, Match::Yes);
    }

    /// What `style_for` alone cannot see.
    #[test]
    fn pseudo_elements_are_compared() {
        let c = compare("img::before { content: 'x' }", "img { }");
        assert_eq!(c.status, Match::No);
        assert_eq!(c.differences[0].pseudo.as_deref(), Some("before"));
        assert_eq!(c.differences[0].property, "content");
        assert_eq!(
            compare(
                "img::before { content: 'x' }",
                "img::before { content: 'x' }"
            )
            .status,
            Match::Yes
        );
        // An attribute only one state's element has.
        let c = cascade(&["[width]::before { content: 'x' }"]);
        let result = c.compare_to(&c).element(&IMG, None, &P_A, None);
        assert_eq!(result.status, Match::No);
        let c = compare("img::before:hover { color: red }", "img { }");
        assert_eq!(c.status, Match::Unknown);
    }

    #[test]
    fn a_style_attribute_is_compared() {
        let c = cascade(&["img { width: 1px }"]);
        let attr = |s: &'static str| crate::parse_block_contents(s).0;
        let compare = |x: &[BlockItem<'_>], y: &[BlockItem<'_>]| {
            c.compare_to(&c)
                .element(&IMG, Some(x), &IMG, Some(y))
                .status
        };
        assert_eq!(
            compare(&attr("color: red"), &attr("color:red; colr: blue")),
            Match::Yes
        );
        assert_eq!(
            compare(&attr("color: red"), &attr("color: blue")),
            Match::No
        );
    }

    #[test]
    fn rules_that_style_no_element_are_compared_for_the_document() {
        let face = "@font-face { font-family: a; src: url(a.woff) }";
        let other = "@font-face { font-family: b; src: url(b.woff) }";
        let (x, y) = (
            cascade(&[face]),
            cascade(&["/* */ @font-face{font-family:a;src:url(a.woff)}"]),
        );
        let same = x.compare_to(&y);
        assert!(same.differing_at_rules.is_empty());
        assert_eq!(same.element(&IMG, None, &IMG, None).status, Match::Yes);
        for (before, after) in [
            (face.to_string(), other.to_string()),
            (face.to_string(), String::new()),
            (format!("{face} {other}"), format!("{other} {face}")),
            (
                format!("@media print {{ {face} }}"),
                format!("@media screen {{ {face} }}"),
            ),
            (
                "@keyframes a { to { top: 0 } }".to_string(),
                "@keyframes a { to { top: 1px } }".to_string(),
            ),
            ("@page { margin: 0 }".to_string(), String::new()),
            ("@unheard x;".to_string(), String::new()),
        ] {
            let (x, y) = (cascade(&[&before]), cascade(&[&after]));
            let comparer = x.compare_to(&y);
            assert!(
                !comparer.differing_at_rules.is_empty(),
                "{before} -> {after}"
            );
            let c = comparer.element(&IMG, None, &IMG, None);
            assert_eq!(c.status, Match::Unknown, "{before} -> {after}");
            assert!(c.differences.is_empty());
        }
        // A certain difference stays one.
        let (x, y) = (
            cascade(&[&format!("{face} img {{ width: 1px }}")]),
            cascade(&[&format!("{other} img {{ width: 2px }}")]),
        );
        assert_eq!(
            x.compare_to(&y).element(&IMG, None, &IMG, None).status,
            Match::No
        );
    }

    /// A chain of conditions as deep as the parser allows must not overflow
    /// the stack when it is dropped.
    #[test]
    fn deep_nesting_is_bounded_in_a_comparison() {
        let n = 100_000;
        let deep = format!(
            "{}img {{ width: 1px }}{}",
            "@media print {".repeat(n),
            "}".repeat(n)
        );
        let c = compare(&deep, &deep);
        assert_eq!(c.status, Match::Yes);
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
            "@charset \"utf-8\"; @layer base; @unknown-rule; . bad { }\
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
        // Rules a browser drops do not end the imports: `b.css` counts.
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
    fn a_supports_condition_no_browser_meets_applies_nowhere() {
        let c = cascade(&["@supports (display: gird) { img { width: 1px } }"]);
        assert_eq!(ask(&c, IMG, None, "width").0, Match::No);
        let c = cascade(&["@supports not (display: gird) { img { width: 1px } }"]);
        assert_eq!(reasons(&c, IMG, "width"), [UnknownReason::Conditional]);
        let main = leak(
            "@import \"a.css\" supports(display: gird); @import \"b.css\" supports(display: grid);",
        );
        let a = leak("img { width: 1px }");
        let b = leak("img { height: 1px }");
        let c = Cascade::with_imports(
            &[Sheet {
                stylesheet: main,
                conditional: false,
            }],
            |_, rule| match rule.url.as_ref() {
                "a.css" => Some(a),
                "b.css" => Some(b),
                _ => None,
            },
        );
        assert_eq!(ask(&c, IMG, None, "width").0, Match::No);
        assert_eq!(reasons(&c, IMG, "height"), [UnknownReason::Conditional]);
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

    #[test]
    fn the_rule_index_misses_nothing_and_keeps_the_order() {
        // `p#a.x`: one rule per way of being found, each later one equal or
        // more specific, so the last that matches wins.
        let css = "* { width: 1px } p { width: 2px } .x { width: 3px } #a { width: 4px }\
                   :is(.x) { width: 5px } P { height: 1px } .nope, p { color: red }\
                   #b, .x { margin: 0 } div.x { padding: 0 }";
        let c = cascade(&[css]);
        assert_eq!(ask(&c, P_A, None, "width").1, "width: 4px");
        let style = c.style_for(&P_A, None);
        let widths = style.property("width").setters.len();
        assert_eq!(widths, 5);
        // `P` is not `p` in XHTML: found, then rejected by matching.
        assert_eq!(ask(&c, P_A, None, "height").0, Match::No);
        assert_eq!(ask(&c, P_A, None, "color").0, Match::Yes);
        assert_eq!(ask(&c, P_A, None, "margin-top").0, Match::Yes);
        assert_eq!(ask(&c, P_A, None, "padding-top").0, Match::No);
        // Equal specificity: the later rule wins, whichever bucket found it.
        let c = cascade(&[".x { width: 1px } p { width: 2px } .x { width: 3px }"]);
        assert_eq!(ask(&c, P_A, None, "width").1, "width: 3px");
    }

    /// A rule whose selector fails only through csswg-drafts#7129 is read
    /// the old way and only possibly applies; one that fails in both
    /// readings is dropped.
    #[test]
    fn narrowed_ident_selectors_only_possibly_apply() {
        let c = cascade(&["p:not(.‣) { width: 1px }"]);
        assert_eq!(
            ask(&c, P_A, None, "width"),
            (Match::Unknown, "width: 1px".into())
        );
        assert_eq!(reasons(&c, P_A, "width"), [UnknownReason::NarrowedIdent]);
        // Nested rules come along, as possibly applied too.
        let c = cascade(&["p:not(.‣) { & { width: 1px } }"]);
        assert_eq!(reasons(&c, P_A, "width"), [UnknownReason::NarrowedIdent]);
        // Not matched in the old reading either.
        let c = cascade(&[".‣ { width: 1px }"]);
        assert_eq!(ask(&c, P_A, None, "width").0, Match::No);
        // Broken in both readings: dropped.
        let c = cascade(&["p:not(.‣) . b, p { width: 1px }"]);
        assert_eq!(ask(&c, P_A, None, "width").0, Match::No);
        // A nested rule that fails only through #7129, in a rule that parses.
        let c = cascade(&["p { &:not(.‣) { width: 1px } }"]);
        assert_eq!(reasons(&c, P_A, "width"), [UnknownReason::NarrowedIdent]);
    }

    /// A forgiving list drops the argument as written (and a dropped argument
    /// matches Unknown), and keeps it in the old reading: where the two
    /// readings answer alike, the answer stands.
    #[test]
    fn forgiving_lists_are_read_both_ways() {
        // Matched alike, with the same specificity: settled.
        let c = cascade(&["p:where(.‣, .x) { width: 1px }"]);
        assert_eq!(
            ask(&c, P_A, None, "width"),
            (Match::Yes, "width: 1px".into())
        );
        use UnknownReason::{NarrowedIdent, Selector};
        // Unknown as written, No in the old reading.
        let c = cascade(&["p:is(.‣) { width: 1px }"]);
        assert_eq!(reasons(&c, P_A, "width"), [Selector, NarrowedIdent]);
        let c = cascade(&["p:not(:is(.‣)) { width: 1px }"]);
        assert_eq!(reasons(&c, P_A, "width"), [Selector, NarrowedIdent]);
        // Nested rules follow their parent's two readings.
        // Matched in both, but `.‣` counts toward the old reading's
        // specificity, which can decide against another rule.
        let c = cascade(&[":is(p, .‣) { & .z, & { width: 1px } }"]);
        assert_eq!(reasons(&c, P_A, "width"), [NarrowedIdent]);
        let c = cascade(&["p:where(.‣, .x) { & { width: 1px } }"]);
        assert_eq!(
            ask(&c, P_A, None, "width"),
            (Match::Yes, "width: 1px".into())
        );
    }
}
