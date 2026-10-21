//! Selectors (Selectors Level 4) read from a qualified rule's prelude.
//!
//! CSS Syntax Level 3 — which [`crate::parser`] implements — deliberately
//! does *not* look at what a qualified rule's prelude says. It groups the
//! prelude into component values and hands them on, so `a > > b { }` and
//! `[= x] { }` parse as happily as `a > b { }`. That is correct per the
//! Syntax spec, and useless to a tool that wants to report malformed CSS or
//! to know what a rule applies to.
//!
//! This module reads that prelude as a **selector list** and builds its tree:
//! [`SelectorList`] → [`ComplexSelector`] (compounds joined by combinators) →
//! [`CompoundSelector`] → [`SimpleSelector`]. Every node carries its span. A
//! prelude that cannot be a selector list comes back as the
//! [`SyntaxError`]s saying where. [`Specificity`] is computed from the tree.
//!
//! **Two kinds of error**, because tools that check CSS draw the line there:
//!
//! - [`SyntaxErrorKind::InvalidSelector`]: a token that has no place at that
//!   point in any selector — a stray `.`, a combinator with nothing on one
//!   side, an attribute selector with a missing value or a second flag.
//! - [`SyntaxErrorKind::InvalidSelectorStructure`]: every token is a valid
//!   selector piece, but they are put together in a way Selectors does not
//!   allow — an id that is not an identifier (`#1a`), a type selector after
//!   another simple selector (`[x]p`), anything but a pseudo-class or
//!   pseudo-element after a pseudo-element (`p::before.x`), a pseudo-element
//!   before the last compound (`p::before span`), an `An+B` that does not
//!   parse (`:nth-child(foo)`), an empty `:has()`. Browsers drop such a
//!   rule just the same; epubcheck accepts it.
//!
//! The line follows what epubcheck 5.4.0 does, measured shape by shape, so
//! one case sits where the definitions alone would not put it: an empty
//! `:not()` is an `InvalidSelector`, because epubcheck reports it.
//!
//! When one selector has both, the token error is the one reported.
//!
//! The reading is *syntactic*. It does not care whether `dvi` is a real
//! element name, or whether `[hrefff]` is a real attribute.
//!
//! **Deliberately permissive, in two named places.** Reporting a valid
//! selector as broken is far worse than missing a broken one — a false
//! positive lands on somebody's real book — so:
//!
//! 1. **Pseudo-classes and pseudo-elements this code does not know are
//!    accepted**, with or without arguments: `::part()`, `:hover`, a vendor
//!    name, a name nobody has invented yet. They come back as
//!    [`PseudoClass::Other`] and [`PseudoElement`] with their arguments
//!    unread. Only the ones whose grammar is fixed — `:not()`, `:is()`,
//!    `:where()`, `:has()` and the `:nth-*()` family — have their arguments
//!    read and checked.
//! 2. **`:is()` and `:where()` are forgiving**, as the spec says: an argument
//!    that is not a selector is dropped, not reported, and the count of
//!    dropped arguments is kept so a matcher can answer "unknown" rather than
//!    "no".

use std::borrow::Cow;

use crate::parser::{BlockKind, ComponentValue, SyntaxError, SyntaxErrorKind};
use crate::span::{Span, Spanned};
use crate::token::{NumericType, Token};

type Values<'a> = [Spanned<ComponentValue<'a>>];

/// How deep selector arguments are read: `:not(:is(…))` is two. Deeper
/// pseudo-classes come back as [`PseudoClass::Other`], unread and not
/// reported.
///
/// Reading a selector inside a pseudo-class's parentheses recurses through
/// several functions per level, and the parser's own bound
/// ([`MAX_NESTING_DEPTH`](crate::MAX_NESTING_DEPTH), 256) is far more than
/// that recursion can take on a 2 MiB thread. Real stylesheets nest selector
/// arguments two or three deep, so 32 is generous.
pub const MAX_SELECTOR_DEPTH: usize = 32;

/// A comma-separated list of complex selectors: a style rule's prelude, or
/// the argument of `:not()`, `:is()`, `:where()`, `:has()` and `:nth-child(… of …)`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SelectorList<'a> {
    pub selectors: Vec<Spanned<ComplexSelector<'a>>>,
}

/// Compound selectors joined by combinators: `div > p.note`.
///
/// `combinators[i]` sits between `compounds[i]` and `compounds[i + 1]`, so
/// there is always one fewer combinator than compound. The parser never
/// builds an empty `compounds`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexSelector<'a> {
    /// The combinator a **relative** selector starts with (`> a` in a nested
    /// rule or in `:has()`); `None` when it starts with a compound. Never set
    /// in a selector parsed as non-relative.
    pub leading: Option<Combinator>,
    pub compounds: Vec<Spanned<CompoundSelector<'a>>>,
    pub combinators: Vec<Combinator>,
}

/// How two compound selectors relate. Whitespace is [`Descendant`](Self::Descendant).
///
/// The column combinator `||` is not read: no browser implements it, and a
/// prelude using it is reported as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b`
    Descendant,
    /// `a > b`
    Child,
    /// `a + b`
    NextSibling,
    /// `a ~ b`
    SubsequentSibling,
}

/// Simple selectors with nothing between them: `p.note#first:hover`.
///
/// In source order. The parser enforces the order Selectors requires: a type
/// or universal selector only at the start (or after `&`, which CSS Nesting
/// lets go first), and nothing but pseudo-classes and pseudo-elements after
/// a pseudo-element.
#[derive(Debug, Clone, PartialEq)]
pub struct CompoundSelector<'a> {
    pub simples: Vec<Spanned<SimpleSelector<'a>>>,
}

/// One simple selector. The span covers the whole of it (`ns|E`, `.note`,
/// `[href^="x" i]`, `:not(.a)`).
#[derive(Debug, Clone, PartialEq)]
pub enum SimpleSelector<'a> {
    /// `E`, `ns|E`, `*|E`, `|E`.
    Type(QualifiedName<'a>),
    /// `*`, `ns|*`, `*|*`, `|*`. The namespace is `None` when no prefix was
    /// written.
    Universal(Option<Namespace<'a>>),
    /// `#id`.
    Id(Cow<'a, str>),
    /// `.class`.
    Class(Cow<'a, str>),
    /// `[attr]`, `[attr="value" i]`.
    /// Boxed: it is the largest variant by far and the rarest of the common
    /// ones, and an unboxed one made every simple selector twice the size.
    Attribute(Box<AttributeSelector<'a>>),
    PseudoClass(PseudoClass<'a>),
    PseudoElement(PseudoElement<'a>),
    /// `&`, CSS Nesting's reference to the parent rule's selector.
    Nesting,
}

/// A name with an optional namespace prefix, as written.
#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedName<'a> {
    /// `None` when no prefix was written. For a type selector the default
    /// namespace then applies; for an attribute, no namespace.
    pub namespace: Option<Namespace<'a>>,
    /// The local name and its span: the `circle` of `svg|circle`.
    pub local: Spanned<Cow<'a, str>>,
}

/// The namespace part of a qualified name.
#[derive(Debug, Clone, PartialEq)]
pub enum Namespace<'a> {
    /// `|E`: in no namespace.
    NoNamespace,
    /// `*|E`: in any namespace, or none.
    Any,
    /// `ns|E`: the namespace that `@namespace ns …` declares. Resolving the
    /// prefix is the caller's business.
    Prefix(Cow<'a, str>),
}

/// `[name]`, or `[name op value flag]`.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeSelector<'a> {
    pub name: QualifiedName<'a>,
    /// `None` for `[name]`, which tests presence only.
    pub value: Option<AttributeValue<'a>>,
}

/// The `op value flag` part of an attribute selector.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeValue<'a> {
    pub operator: AttributeOperator,
    /// The value, unquoted.
    pub value: Cow<'a, str>,
    pub case: AttributeCase<'a>,
}

/// The matcher in `[name op value]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeOperator {
    /// `=`
    Equal,
    /// `~=`: one of the whitespace-separated words.
    Includes,
    /// `|=`: exactly, or followed by `-`.
    DashMatch,
    /// `^=`
    Prefix,
    /// `$=`
    Suffix,
    /// `*=`
    Substring,
}

/// The flag after an attribute selector's value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeCase<'a> {
    /// No flag: the document language decides.
    Default,
    /// `i`: ASCII case-insensitive.
    Insensitive,
    /// `s`: case-sensitive.
    Sensitive,
    /// A flag this code does not know. Accepted, since that list has grown
    /// before.
    Other(Cow<'a, str>),
}

/// A pseudo-class. The ones a matcher can answer from the document tree
/// alone have variants of their own; everything else is [`Other`](Self::Other).
#[derive(Debug, Clone, PartialEq)]
pub enum PseudoClass<'a> {
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    /// `:nth-child(An+B)`, `:nth-child(An+B of S)`.
    NthChild {
        nth: AnB,
        of: Option<SelectorList<'a>>,
    },
    NthLastChild {
        nth: AnB,
        of: Option<SelectorList<'a>>,
    },
    NthOfType(AnB),
    NthLastOfType(AnB),
    /// `:not(S)`.
    Not(SelectorList<'a>),
    /// `:is(S)`. `dropped` counts the arguments the forgiving parse left out.
    Is {
        selectors: SelectorList<'a>,
        dropped: usize,
    },
    /// `:where(S)`, the zero-specificity `:is()`.
    Where {
        selectors: SelectorList<'a>,
        dropped: usize,
    },
    /// `:has(S)`, with a relative selector list.
    Has(SelectorList<'a>),
    /// Any other pseudo-class: user-action and link states (`:hover`,
    /// `:visited`), `:lang()`, vendor and future names. `args` is `None` for
    /// `:name`, the unread arguments for `:name(…)`.
    Other {
        name: Cow<'a, str>,
        args: Option<Vec<Spanned<ComponentValue<'a>>>>,
    },
}

/// A pseudo-element: `::before`, `::part(label)`, and the four that CSS 2
/// wrote with one colon (`:before`, `:after`, `:first-line`,
/// `:first-letter`).
#[derive(Debug, Clone, PartialEq)]
pub struct PseudoElement<'a> {
    pub name: Cow<'a, str>,
    /// `None` for `::name`, the unread arguments for `::name(…)`.
    pub args: Option<Vec<Spanned<ComponentValue<'a>>>>,
}

/// The `An+B` of `:nth-child()` and its family (CSS Syntax §7): the
/// positions `A×n + B` for n = 0, 1, 2, …, counting from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnB {
    pub a: i32,
    pub b: i32,
}

impl AnB {
    /// Whether the 1-based `index` is one of the positions.
    pub fn matches(self, index: i32) -> bool {
        let (a, b) = (i64::from(self.a), i64::from(self.b));
        let d = i64::from(index) - b;
        if a == 0 {
            d == 0
        } else {
            d % a == 0 && d / a >= 0
        }
    }
}

/// A selector's specificity (Selectors Level 4 §17): ids, then classes
/// (with attributes and pseudo-classes), then types (with pseudo-elements).
/// The derived order compares them in that order, which is the cascade's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Specificity {
    pub ids: u32,
    pub classes: u32,
    pub types: u32,
}

impl Specificity {
    fn add(self, other: Specificity) -> Specificity {
        Specificity {
            ids: self.ids.saturating_add(other.ids),
            classes: self.classes.saturating_add(other.classes),
            types: self.types.saturating_add(other.types),
        }
    }

    const ID: Specificity = Specificity {
        ids: 1,
        classes: 0,
        types: 0,
    };
    const CLASS: Specificity = Specificity {
        ids: 0,
        classes: 1,
        types: 0,
    };
    const TYPE: Specificity = Specificity {
        ids: 0,
        classes: 0,
        types: 1,
    };
}

impl SelectorList<'_> {
    /// The largest specificity in the list: what `:is()`, `:not()` and
    /// `:has()` contribute. Zero for an empty list.
    pub fn specificity(&self) -> Specificity {
        self.selectors
            .iter()
            .map(|s| s.node.specificity())
            .max()
            .unwrap_or_default()
    }
}

impl ComplexSelector<'_> {
    /// This selector's specificity.
    ///
    /// `&` counts as zero here. Its specificity is that of the parent rule's
    /// selector list, which this selector alone does not know.
    pub fn specificity(&self) -> Specificity {
        self.compounds
            .iter()
            .flat_map(|c| &c.node.simples)
            .map(|s| s.node.specificity())
            .fold(Specificity::default(), Specificity::add)
    }
}

impl SimpleSelector<'_> {
    pub fn specificity(&self) -> Specificity {
        match self {
            SimpleSelector::Type(_) | SimpleSelector::PseudoElement(_) => Specificity::TYPE,
            SimpleSelector::Universal(_) | SimpleSelector::Nesting => Specificity::default(),
            SimpleSelector::Id(_) => Specificity::ID,
            SimpleSelector::Class(_) | SimpleSelector::Attribute(_) => Specificity::CLASS,
            SimpleSelector::PseudoClass(p) => match p {
                PseudoClass::Not(s)
                | PseudoClass::Has(s)
                | PseudoClass::Is { selectors: s, .. } => s.specificity(),
                PseudoClass::Where { .. } => Specificity::default(),
                PseudoClass::NthChild { of: Some(s), .. }
                | PseudoClass::NthLastChild { of: Some(s), .. } => {
                    Specificity::CLASS.add(s.specificity())
                }
                _ => Specificity::CLASS,
            },
        }
    }
}

/// The most simple selectors [`SelectorList::resolve_nesting`] builds. Every
/// `&` becomes a copy of the parent's list, so `& &` in each of a few dozen
/// nested rules would double the size per level; real nesting stays far
/// below this.
pub const MAX_RESOLVED_SIMPLES: usize = 4096;

impl<'a> SelectorList<'a> {
    /// This list with CSS Nesting resolved (CSS Nesting §2–3): the selectors
    /// a nested style rule stands for on its own.
    ///
    /// `parent` is the enclosing style rule's list, itself already resolved.
    /// Each `&` becomes `:is(parent)`, so it matches what the parent matches
    /// and counts the parent's highest specificity. A selector that starts
    /// with a combinator, or has no `&` anywhere, gets `&` put in front: `> a`
    /// is `& > a`, `a` is `& a`.
    ///
    /// With no parent, the rule is not nested, and `&` stands for `:scope`,
    /// which in a document's stylesheet is the root element: it becomes
    /// [`PseudoClass::Root`]. Inside `@scope` that is wrong, so a caller
    /// there leaves the list unresolved.
    ///
    /// `None` when the result would hold more than [`MAX_RESOLVED_SIMPLES`]
    /// simple selectors or nest arguments deeper than
    /// [`MAX_SELECTOR_DEPTH`]. Spans of the inserted pieces are those of the
    /// `&` they replace; an implicit `&` has an empty span where its selector
    /// starts.
    pub fn resolve_nesting(&self, parent: Option<&SelectorList<'a>>) -> Option<SelectorList<'a>> {
        let own = size(self);
        let (with, implicit) = match parent {
            Some(p) => {
                let p_size = size(p);
                let implicit = self
                    .selectors
                    .iter()
                    .filter(|s| needs_implicit_nesting(&s.node))
                    .count();
                let copies = own.nestings + implicit;
                let simples = copies
                    .checked_mul(p_size.simples.max(1))?
                    .checked_add(own.simples)?;
                let depth = if copies > 0 {
                    own.depth + 1 + p_size.depth
                } else {
                    own.depth
                };
                if simples > MAX_RESOLVED_SIMPLES || depth > MAX_SELECTOR_DEPTH {
                    return None;
                }
                let is = PseudoClass::Is {
                    selectors: p.clone(),
                    dropped: 0,
                };
                (SimpleSelector::PseudoClass(is), true)
            }
            None => (SimpleSelector::PseudoClass(PseudoClass::Root), false),
        };
        let selectors = self
            .selectors
            .iter()
            .map(|s| {
                let mut c = substitute_complex(&s.node, &with);
                if implicit && needs_implicit_nesting(&s.node) {
                    let at = Span::new(s.span.start, s.span.start);
                    let amp = CompoundSelector {
                        simples: vec![Spanned::new(with.clone(), at)],
                    };
                    c.compounds.insert(0, Spanned::new(amp, at));
                    c.combinators
                        .insert(0, c.leading.take().unwrap_or(Combinator::Descendant));
                }
                Spanned::new(c, s.span)
            })
            .collect();
        Some(SelectorList { selectors })
    }
}

/// Whether a nested rule's selector gets an `&` put in front: when it starts
/// with a combinator, or has none of its own.
fn needs_implicit_nesting(c: &ComplexSelector<'_>) -> bool {
    c.leading.is_some() || size_complex(c).nestings == 0
}

/// What [`SelectorList::resolve_nesting`] budgets: simple selectors, `&`s
/// among them, and how deep selector arguments nest.
#[derive(Default, Clone, Copy)]
struct Size {
    simples: usize,
    nestings: usize,
    depth: usize,
}

fn size(list: &SelectorList<'_>) -> Size {
    list.selectors
        .iter()
        .map(|s| size_complex(&s.node))
        .fold(Size::default(), |a, b| Size {
            simples: a.simples + b.simples,
            nestings: a.nestings + b.nestings,
            depth: a.depth.max(b.depth),
        })
}

fn size_complex(c: &ComplexSelector<'_>) -> Size {
    let mut total = Size::default();
    for s in c.compounds.iter().flat_map(|c| &c.node.simples) {
        total.simples += 1;
        match &s.node {
            SimpleSelector::Nesting => total.nestings += 1,
            SimpleSelector::PseudoClass(p) => {
                if let Some(list) = argument_selectors(p) {
                    let inner = size(list);
                    total.simples += inner.simples;
                    total.nestings += inner.nestings;
                    total.depth = total.depth.max(inner.depth + 1);
                }
            }
            _ => {}
        }
    }
    total
}

/// The selector list a pseudo-class takes as its argument, if any.
fn argument_selectors<'p, 'a>(p: &'p PseudoClass<'a>) -> Option<&'p SelectorList<'a>> {
    match p {
        PseudoClass::Not(s)
        | PseudoClass::Has(s)
        | PseudoClass::Is { selectors: s, .. }
        | PseudoClass::Where { selectors: s, .. }
        | PseudoClass::NthChild { of: Some(s), .. }
        | PseudoClass::NthLastChild { of: Some(s), .. } => Some(s),
        _ => None,
    }
}

/// `c` with every `&`, at any depth, replaced by `with`.
fn substitute_complex<'a>(
    c: &ComplexSelector<'a>,
    with: &SimpleSelector<'a>,
) -> ComplexSelector<'a> {
    let compounds = c
        .compounds
        .iter()
        .map(|compound| {
            let simples = compound
                .node
                .simples
                .iter()
                .map(|s| Spanned::new(substitute_simple(&s.node, with), s.span))
                .collect();
            Spanned::new(CompoundSelector { simples }, compound.span)
        })
        .collect();
    ComplexSelector {
        leading: c.leading,
        compounds,
        combinators: c.combinators.clone(),
    }
}

fn substitute_simple<'a>(s: &SimpleSelector<'a>, with: &SimpleSelector<'a>) -> SimpleSelector<'a> {
    let list = |l: &SelectorList<'a>| SelectorList {
        selectors: l
            .selectors
            .iter()
            .map(|c| Spanned::new(substitute_complex(&c.node, with), c.span))
            .collect(),
    };
    let p = match s {
        SimpleSelector::Nesting => return with.clone(),
        SimpleSelector::PseudoClass(p) if argument_selectors(p).is_some() => p,
        _ => return s.clone(),
    };
    SimpleSelector::PseudoClass(match p {
        PseudoClass::Not(l) => PseudoClass::Not(list(l)),
        PseudoClass::Has(l) => PseudoClass::Has(list(l)),
        PseudoClass::Is { selectors, dropped } => PseudoClass::Is {
            selectors: list(selectors),
            dropped: *dropped,
        },
        PseudoClass::Where { selectors, dropped } => PseudoClass::Where {
            selectors: list(selectors),
            dropped: *dropped,
        },
        PseudoClass::NthChild { nth, of } => PseudoClass::NthChild {
            nth: *nth,
            of: of.as_ref().map(list),
        },
        PseudoClass::NthLastChild { nth, of } => PseudoClass::NthLastChild {
            nth: *nth,
            of: of.as_ref().map(list),
        },
        other => other.clone(),
    })
}

/// Parse a style rule's prelude as a selector list. On failure, the errors
/// say where: **at most one per comma-separated selector**, plus one for each
/// empty side of a comma.
///
/// The granularity is deliberate and sits between two wrong answers.
/// Reporting per bad *token* turns one broken construct into a pile: a
/// stylesheet beginning with a stray declaration, `text-indent:1.5em;`, is
/// read as a qualified rule whose prelude is that text, and walking it token
/// by token blamed the `:`, the `1.5em`, the `;` and the `@` of the next
/// rule — four errors for one mistake, where a browser and epubcheck each
/// see one. Reporting per *prelude* would be too coarse the other way:
/// `. h-100, . y-100 { }` really is two independently broken selectors, and
/// naming both is the thing a caller can act on.
///
/// A list with one bad selector is a bad list: per Selectors, the whole style
/// rule is dropped, and so the result is `Err` even when other selectors in
/// it parsed.
pub fn parse_selector_list<'a>(
    prelude: &[Spanned<ComponentValue<'a>>],
) -> Result<SelectorList<'a>, Vec<SyntaxError>> {
    into_result(list(prelude, false, 0, false))
}

/// [`parse_selector_list`] for a **relative** selector list (Selectors Level
/// 4 §3.6): the prelude of a style rule nested in another style rule (CSS
/// Nesting §2.1), where each selector may begin with a combinator —
/// `p { > a { … } + b { … } }`.
pub fn parse_relative_selector_list<'a>(
    prelude: &[Spanned<ComponentValue<'a>>],
) -> Result<SelectorList<'a>, Vec<SyntaxError>> {
    into_result(list(prelude, true, 0, false))
}

/// Whether a prelude is certainly a valid selector list of the plainest
/// kind: compounds of an optional type or `*`, then classes and ids, joined
/// by whitespace, `>`, `+` and `~`, separated by commas. Builds nothing, so
/// the parser can skip [`parse_selector_list`] for most style rules. `false`
/// means only "not that plain": the full parser decides.
pub(crate) fn is_plain_selector_list(prelude: &Values<'_>, relative: bool) -> bool {
    let tok = |i: usize| match prelude.get(i).map(|v| &v.node) {
        Some(ComponentValue::Token(t)) => Some(t),
        _ => None,
    };
    let is_combinator = |i: usize| matches!(tok(i), Some(Token::Delim('>' | '+' | '~')));
    let mut i = 0;
    loop {
        // A complex selector; in a relative one, a combinator may lead.
        i = skip_ws(prelude, i);
        if relative && is_combinator(i) {
            i = skip_ws(prelude, i + 1);
        }
        loop {
            // A compound, which must not be empty.
            let start = i;
            if matches!(tok(i), Some(Token::Ident(_) | Token::Delim('*'))) {
                i += 1;
            }
            loop {
                match tok(i) {
                    Some(Token::Delim('.')) if matches!(tok(i + 1), Some(Token::Ident(_))) => {
                        i += 2
                    }
                    Some(Token::Hash { is_id: true, .. }) => i += 1,
                    _ => break,
                }
            }
            if i == start {
                return false;
            }
            // Then the end, a comma, a combinator or whitespace. Anything
            // else (`|`, `:`, `[`, a function) is not plain.
            let next = skip_ws(prelude, i);
            if next == prelude.len() {
                return true;
            }
            if matches!(tok(next), Some(Token::Comma)) {
                i = next + 1;
                break;
            }
            if is_combinator(next) {
                i = skip_ws(prelude, next + 1);
            } else if next > i {
                i = next;
            } else {
                return false;
            }
        }
    }
}

fn into_result<'a>(
    (list, errors): (SelectorList<'a>, Vec<SyntaxError>),
) -> Result<SelectorList<'a>, Vec<SyntaxError>> {
    if errors.is_empty() {
        Ok(list)
    } else {
        Err(errors)
    }
}

/// Every selector in a prelude that names something, for
/// [`selector_references`](crate::selector_references): read as a relative
/// list when `relative`, keeping a selector with a structure error (it still
/// names what it names) and skipping one with a token error. The flag says
/// whether the list had any error, which drops the rule.
pub(crate) fn parse_selector_list_lenient<'a>(
    prelude: &[Spanned<ComponentValue<'a>>],
    relative: bool,
) -> (SelectorList<'a>, bool) {
    let (list, errors) = list(prelude, relative, 0, true);
    (list, !errors.is_empty())
}

/// The selectors that parsed, and the errors for those that did not. With
/// `keep_flawed`, a selector with only a structure error is kept as well as
/// reported.
fn list<'a>(
    values: &Values<'a>,
    relative: bool,
    depth: usize,
    keep_flawed: bool,
) -> (SelectorList<'a>, Vec<SyntaxError>) {
    let mut selectors = Vec::new();
    let mut errors = Vec::new();
    split_on_commas(
        values,
        &mut errors,
        |part, errors| match complex_with_structure(part, relative, depth) {
            Ok((c, None)) => selectors.push(c),
            Ok((c, Some(span))) => {
                errors.push(structure_error(span));
                if keep_flawed {
                    selectors.push(c);
                }
            }
            Err(e) => errors.push(e),
        },
    );
    (SelectorList { selectors }, errors)
}

/// A forgiving selector list (`:is()`, `:where()`): what parses, and how many
/// arguments did not. Empty arguments are not counted; the spec drops them
/// silently too.
fn forgiving_list<'a>(values: &Values<'a>, depth: usize) -> (SelectorList<'a>, usize) {
    let mut selectors = Vec::new();
    let mut dropped = 0;
    for part in values.split(|cv| matches!(cv.node, ComponentValue::Token(Token::Comma))) {
        if is_blank(part) {
            continue;
        }
        match complex(part, false, depth) {
            Ok(c) => selectors.push(c),
            Err(_) => dropped += 1,
        }
    }
    (SelectorList { selectors }, dropped)
}

/// A non-forgiving list inside a pseudo-class's parentheses. A token error
/// inside is `Err`, and stands for the whole outer selector. A structure
/// error inside, or an empty list, goes into `structure` for the outer
/// selector, whose reading carries on so that a token error later in it
/// still wins.
fn argument_list<'a>(
    values: &Values<'a>,
    relative: bool,
    function: Span,
    depth: usize,
    structure: &mut Option<Span>,
) -> Result<SelectorList<'a>, SyntaxError> {
    let (list, errors) = list(values, relative, depth, false);
    let earliest = |kind| {
        errors
            .iter()
            .filter(|e| e.kind == kind)
            .min_by_key(|e| e.span.start)
            .copied()
    };
    if let Some(e) = earliest(SyntaxErrorKind::InvalidSelector) {
        return Err(e);
    }
    if let Some(e) = earliest(SyntaxErrorKind::InvalidSelectorStructure) {
        note(structure, e.span);
    } else if list.selectors.is_empty() {
        note(structure, function);
    }
    Ok(list)
}

/// The comma-separated slices of a prelude, reporting an empty one as it
/// goes. The error span is the comma itself: it is the token the author has
/// to look at.
fn split_on_commas<'p, 'a>(
    prelude: &'p Values<'a>,
    errors: &mut Vec<SyntaxError>,
    mut each: impl FnMut(&'p Values<'a>, &mut Vec<SyntaxError>),
) {
    let mut parts = 0;
    let mut start = 0;
    for (i, cv) in prelude.iter().enumerate() {
        if matches!(&cv.node, ComponentValue::Token(Token::Comma)) {
            let part = &prelude[start..i];
            if is_blank(part) {
                errors.push(bad(cv.span));
            } else {
                parts += 1;
                each(part, errors);
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
        if parts > 0
            && let Some(last) = prelude.last()
        {
            errors.push(bad(last.span));
        }
    } else {
        each(tail, errors);
    }
}

/// A complex selector: compounds joined by combinators. `part` is not blank.
///
/// In a relative selector the first thing may be a combinator, which then has
/// the nesting parent (or `:has()`'s subject) on its left; it still needs a
/// compound on its right.
fn complex<'a>(
    part: &Values<'a>,
    relative: bool,
    depth: usize,
) -> Result<Spanned<ComplexSelector<'a>>, SyntaxError> {
    match complex_with_structure(part, relative, depth)? {
        (c, None) => Ok(c),
        (_, Some(span)) => Err(structure_error(span)),
    }
}

/// [`complex`], with a structure error handed back beside the selector
/// rather than instead of it. A token error is still `Err`, and wins.
fn complex_with_structure<'a>(
    part: &Values<'a>,
    relative: bool,
    depth: usize,
) -> Result<(Spanned<ComplexSelector<'a>>, Option<Span>), SyntaxError> {
    // The first structure error, if any; reading goes on past it.
    let mut structure = None;
    let mut i = skip_ws(part, 0);
    let first = part[i].span;
    let mut leading = None;
    // A combinator still waiting for the compound on its right.
    let mut pending = None;
    if let Some(c) = combinator(&part[i].node) {
        if !relative {
            return Err(bad(part[i].span));
        }
        leading = Some(c);
        pending = Some(part[i].span);
        i += 1;
    }
    let mut compounds: Vec<Spanned<CompoundSelector<'a>>> = Vec::new();
    let mut combinators = Vec::new();
    loop {
        i = skip_ws(part, i);
        let Some(cv) = part.get(i) else { break };
        if let Some(c) = combinator(&cv.node) {
            // Nothing to combine with on the left: two in a row.
            if pending.is_some() {
                return Err(bad(cv.span));
            }
            combinators.push(c);
            pending = Some(cv.span);
            i += 1;
            continue;
        }
        // A compound after a compound: they were separated by whitespace,
        // since a compound runs until whitespace or a combinator.
        if !compounds.is_empty() && pending.is_none() {
            combinators.push(Combinator::Descendant);
        }
        pending = None;
        let (c, n) = compound(part, i, depth, &mut structure)?;
        compounds.push(c);
        i += n;
    }
    // A trailing combinator has nothing on its right.
    if let Some(span) = pending {
        return Err(bad(span));
    }
    // A pseudo-element ends the selector: it can only be in the last compound.
    if let Some((_, init)) = compounds.split_last()
        && let Some(pe) = init
            .iter()
            .flat_map(|c| &c.node.simples)
            .find(|s| matches!(s.node, SimpleSelector::PseudoElement(_)))
    {
        note(&mut structure, pe.span);
    }
    let last = compounds.last().map_or(first, |c| c.span);
    Ok((
        Spanned::new(
            ComplexSelector {
                leading,
                compounds,
                combinators,
            },
            first.to(last),
        ),
        structure,
    ))
}

/// One compound selector starting at `start`, and how many component values
/// it consumed (always at least one). A compound runs until whitespace or a
/// combinator.
fn compound<'a>(
    part: &Values<'a>,
    start: usize,
    depth: usize,
    structure: &mut Option<Span>,
) -> Result<(Spanned<CompoundSelector<'a>>, usize), SyntaxError> {
    let mut simples: Vec<Spanned<SimpleSelector<'a>>> = Vec::new();
    let mut after_pseudo_element = false;
    let mut i = start;
    while let Some(cv) = part.get(i) {
        if is_whitespace(&cv.node) || combinator(&cv.node).is_some() {
            break;
        }
        let (s, n) = simple(part, i, depth, structure)?;
        let in_order = match &s.node {
            // Type and universal come first; CSS Nesting lets `&` go before.
            SimpleSelector::Type(_) | SimpleSelector::Universal(_) => simples
                .iter()
                .all(|p| matches!(p.node, SimpleSelector::Nesting)),
            SimpleSelector::PseudoClass(_) => true,
            SimpleSelector::PseudoElement(_) => {
                after_pseudo_element = true;
                true
            }
            _ => !after_pseudo_element,
        };
        if !in_order {
            note(structure, s.span);
        }
        simples.push(s);
        i += n;
    }
    let span = part[start].span.to(part[i - 1].span);
    Ok((Spanned::new(CompoundSelector { simples }, span), i - start))
}

/// One simple selector at `i`, and how many component values it took.
fn simple<'a>(
    part: &Values<'a>,
    i: usize,
    depth: usize,
    structure: &mut Option<Span>,
) -> Result<(Spanned<SimpleSelector<'a>>, usize), SyntaxError> {
    let cv = &part[i];
    let one = |s| Ok((Spanned::new(s, cv.span), 1));
    match &cv.node {
        // `E`, `ns|E`, `ns|*`.
        ComponentValue::Token(Token::Ident(name)) => {
            if is_delim(part.get(i + 1), '|') {
                let ns = Namespace::Prefix(name.clone());
                namespaced(part, i, i + 1, ns)
            } else {
                one(SimpleSelector::Type(QualifiedName {
                    namespace: None,
                    local: Spanned::new(name.clone(), cv.span),
                }))
            }
        }
        // `*`, `*|E`, `*|*`.
        ComponentValue::Token(Token::Delim('*')) => {
            if is_delim(part.get(i + 1), '|') {
                namespaced(part, i, i + 1, Namespace::Any)
            } else {
                one(SimpleSelector::Universal(None))
            }
        }
        // `|E`, `|*`.
        ComponentValue::Token(Token::Delim('|')) => namespaced(part, i, i, Namespace::NoNamespace),
        // `#id`: the hash has to be a valid identifier, so `#1a` is not one.
        ComponentValue::Token(Token::Hash { value, is_id }) => {
            if !is_id {
                note(structure, cv.span);
            }
            one(SimpleSelector::Id(value.clone()))
        }
        // `.class`: the name must follow immediately.
        ComponentValue::Token(Token::Delim('.')) => match part.get(i + 1) {
            Some(Spanned {
                node: ComponentValue::Token(Token::Ident(name)),
                span,
            }) => Ok((
                Spanned::new(SimpleSelector::Class(name.clone()), cv.span.to(*span)),
                2,
            )),
            _ => Err(bad(cv.span)),
        },
        // `&`, CSS Nesting's parent reference.
        ComponentValue::Token(Token::Delim('&')) => one(SimpleSelector::Nesting),
        // `[attr…]`: the tokenizer already balanced the brackets.
        ComponentValue::Block(b) if b.kind == BlockKind::Square => one(SimpleSelector::Attribute(
            Box::new(attribute(&b.values, cv.span)?),
        )),
        ComponentValue::Token(Token::Colon) => pseudo(part, i, depth, structure),
        // Anything else — a number, a string, a `( … )` — cannot start a
        // simple selector.
        _ => Err(bad(cv.span)),
    }
}

/// The `|E` or `|*` tail of a qualified name whose `|` is at `bar`, the whole
/// starting at `start`.
fn namespaced<'a>(
    part: &Values<'a>,
    start: usize,
    bar: usize,
    namespace: Namespace<'a>,
) -> Result<(Spanned<SimpleSelector<'a>>, usize), SyntaxError> {
    let Some(name) = part.get(bar + 1) else {
        return Err(bad(part[bar].span));
    };
    let s = match &name.node {
        ComponentValue::Token(Token::Ident(local)) => SimpleSelector::Type(QualifiedName {
            namespace: Some(namespace),
            local: Spanned::new(local.clone(), name.span),
        }),
        ComponentValue::Token(Token::Delim('*')) => SimpleSelector::Universal(Some(namespace)),
        _ => return Err(bad(part[bar].span)),
    };
    Ok((
        Spanned::new(s, part[start].span.to(name.span)),
        bar + 2 - start,
    ))
}

/// `:name`, `::name`, `:name(…)`, `::name(…)` at `i`.
fn pseudo<'a>(
    part: &Values<'a>,
    i: usize,
    depth: usize,
    structure: &mut Option<Span>,
) -> Result<(Spanned<SimpleSelector<'a>>, usize), SyntaxError> {
    let colon = part[i].span;
    let double = matches!(
        part.get(i + 1).map(|c| &c.node),
        Some(ComponentValue::Token(Token::Colon))
    );
    let n = if double { 2 } else { 1 };
    let Some(target) = part.get(i + n) else {
        return Err(bad(colon));
    };
    let s = match (&target.node, double) {
        (ComponentValue::Token(Token::Ident(name)), true) => {
            SimpleSelector::PseudoElement(PseudoElement {
                name: name.clone(),
                args: None,
            })
        }
        (ComponentValue::Function { name, args }, true) => {
            SimpleSelector::PseudoElement(PseudoElement {
                name: name.clone(),
                args: Some(args.clone()),
            })
        }
        (ComponentValue::Token(Token::Ident(name)), false) => pseudo_class_ident(name),
        (ComponentValue::Function { name, args }, false) => SimpleSelector::PseudoClass(
            pseudo_class_function(name, args, target.span, depth, structure)?,
        ),
        _ => return Err(bad(colon)),
    };
    Ok((Spanned::new(s, colon.to(target.span)), n + 1))
}

/// `:name`. The four CSS 2 pseudo-elements are pseudo-elements still.
fn pseudo_class_ident<'a>(name: &Cow<'a, str>) -> SimpleSelector<'a> {
    let is = |s: &str| name.eq_ignore_ascii_case(s);
    if is("before") || is("after") || is("first-line") || is("first-letter") {
        return SimpleSelector::PseudoElement(PseudoElement {
            name: name.clone(),
            args: None,
        });
    }
    SimpleSelector::PseudoClass(if is("root") {
        PseudoClass::Root
    } else if is("empty") {
        PseudoClass::Empty
    } else if is("first-child") {
        PseudoClass::FirstChild
    } else if is("last-child") {
        PseudoClass::LastChild
    } else if is("only-child") {
        PseudoClass::OnlyChild
    } else if is("first-of-type") {
        PseudoClass::FirstOfType
    } else if is("last-of-type") {
        PseudoClass::LastOfType
    } else if is("only-of-type") {
        PseudoClass::OnlyOfType
    } else {
        PseudoClass::Other {
            name: name.clone(),
            args: None,
        }
    })
}

/// `:name(…)`. Only the pseudo-classes whose argument grammar is fixed are
/// read; any other comes back with its arguments as written.
fn pseudo_class_function<'a>(
    name: &Cow<'a, str>,
    args: &Values<'a>,
    span: Span,
    depth: usize,
    structure: &mut Option<Span>,
) -> Result<PseudoClass<'a>, SyntaxError> {
    let is = |s: &str| name.eq_ignore_ascii_case(s);
    let inner = depth + 1;
    let other = || PseudoClass::Other {
        name: name.clone(),
        args: Some(args.to_vec()),
    };
    // An `An+B` that does not parse is a structure error; the pseudo-class
    // is kept unread.
    let nth = |values: &Values<'a>, structure: &mut Option<Span>| {
        let n = an_plus_b(values);
        if n.is_none() {
            note(structure, span);
        }
        n
    };
    Ok(if inner > MAX_SELECTOR_DEPTH {
        other()
    } else if is("not") {
        // An empty `:not()` is a token error, not a structure one: epubcheck
        // 5.4.0 reports it, where it passes an empty `:has()` (measured by
        // epubveri, 2026-10-18).
        if is_blank(args) {
            return Err(bad(span));
        }
        PseudoClass::Not(argument_list(args, false, span, inner, structure)?)
    } else if is("is") {
        let (selectors, dropped) = forgiving_list(args, inner);
        PseudoClass::Is { selectors, dropped }
    } else if is("where") {
        let (selectors, dropped) = forgiving_list(args, inner);
        PseudoClass::Where { selectors, dropped }
    } else if is("has") {
        PseudoClass::Has(argument_list(args, true, span, inner, structure)?)
    } else if is("nth-child") || is("nth-last-child") {
        // `An+B of S`: An+B never contains the ident `of`, so the first one
        // is the separator.
        let of = args.iter().position(
            |cv| matches!(&cv.node, ComponentValue::Token(Token::Ident(w)) if w.eq_ignore_ascii_case("of")),
        );
        let (anb, of) = match of {
            Some(k) => (
                &args[..k],
                Some(argument_list(
                    &args[k + 1..],
                    false,
                    span,
                    inner,
                    structure,
                )?),
            ),
            None => (args, None),
        };
        match nth(anb, structure) {
            None => other(),
            Some(nth) if is("nth-child") => PseudoClass::NthChild { nth, of },
            Some(nth) => PseudoClass::NthLastChild { nth, of },
        }
    } else if is("nth-of-type") {
        nth(args, structure).map_or_else(other, PseudoClass::NthOfType)
    } else if is("nth-last-of-type") {
        nth(args, structure).map_or_else(other, PseudoClass::NthLastOfType)
    } else {
        other()
    })
}

/// The inside of an `[ … ]` attribute selector: a (possibly
/// namespace-qualified) name, optionally followed by a matcher and a value,
/// optionally followed by a case-sensitivity flag.
fn attribute<'a>(values: &Values<'a>, bracket: Span) -> Result<AttributeSelector<'a>, SyntaxError> {
    // Index-based over the non-whitespace values: `[ns|href]` needs one
    // token of lookahead to tell a namespace separator from the `|=` matcher,
    // and that reads badly through an iterator.
    let v: Vec<&Spanned<ComponentValue<'a>>> = values
        .iter()
        .filter(|cv| !is_whitespace(&cv.node))
        .collect();
    let ident = |k: usize| match v.get(k).map(|c| &c.node) {
        Some(ComponentValue::Token(Token::Ident(n))) => Some(Spanned::new(n.clone(), v[k].span)),
        _ => None,
    };
    let delim = |k: usize, c: char| matches!(v.get(k).map(|v| &v.node), Some(ComponentValue::Token(Token::Delim(d))) if *d == c);
    let Some(first) = v.first() else {
        // `[]`
        return Err(bad(bracket));
    };

    // The attribute name: `href`, `ns|href`, `*|href`, `|href`. A `|` after
    // a name is a namespace separator only when a name follows it;
    // `[a|="x"]` is the dash-match matcher.
    let (namespace, local, mut i) = if let Some(n) = ident(0) {
        match (delim(1, '|'), ident(2)) {
            (true, Some(local)) => (Some(Namespace::Prefix(n.node)), local, 3),
            _ => (None, n, 1),
        }
    } else if delim(0, '*') && delim(1, '|') {
        (Some(Namespace::Any), ident(2).ok_or(bad(first.span))?, 3)
    } else if delim(0, '|') {
        (
            Some(Namespace::NoNamespace),
            ident(1).ok_or(bad(first.span))?,
            2,
        )
    } else {
        return Err(bad(first.span));
    };
    let name = QualifiedName { namespace, local };

    // Bare `[href]` is complete.
    let Some(matcher) = v.get(i) else {
        return Ok(AttributeSelector { name, value: None });
    };

    // A matcher is `=` alone, or one of ~ | ^ $ * immediately followed by `=`.
    let operator = match &matcher.node {
        ComponentValue::Token(Token::Delim('=')) => AttributeOperator::Equal,
        ComponentValue::Token(Token::Delim(c @ ('~' | '|' | '^' | '$' | '*')))
            if delim(i + 1, '=') =>
        {
            i += 1;
            match c {
                '~' => AttributeOperator::Includes,
                '|' => AttributeOperator::DashMatch,
                '^' => AttributeOperator::Prefix,
                '$' => AttributeOperator::Suffix,
                _ => AttributeOperator::Substring,
            }
        }
        _ => return Err(bad(matcher.span)),
    };
    i += 1;

    // A value must follow: `[href=]` is the classic mistake.
    let value = match v.get(i).map(|c| &c.node) {
        Some(ComponentValue::Token(Token::Ident(s) | Token::String(s))) => s.clone(),
        Some(_) => return Err(bad(v[i].span)),
        None => return Err(bad(matcher.span)),
    };
    i += 1;

    // At most one flag, an ident. Which letter is not checked: that list has
    // grown before.
    let case = match v.get(i) {
        None => AttributeCase::Default,
        Some(_) if i + 1 < v.len() => return Err(bad(v[i + 1].span)),
        Some(_) => match ident(i) {
            Some(f) if f.node.eq_ignore_ascii_case("i") => AttributeCase::Insensitive,
            Some(f) if f.node.eq_ignore_ascii_case("s") => AttributeCase::Sensitive,
            Some(f) => AttributeCase::Other(f.node),
            None => return Err(bad(v[i].span)),
        },
    };
    Ok(AttributeSelector {
        name,
        value: Some(AttributeValue {
            operator,
            value,
            case,
        }),
    })
}

/// `An+B` (CSS Syntax §7.2) from a function's arguments, or `None` if they
/// are not one.
///
/// The grammar is written against tokens, because the tokenizer splits
/// `2n+1` as the dimension `2n` and the number `+1`, `2n-1` as the single
/// dimension `2n-1`, and `-n-1` as one ident. Whitespace is allowed around
/// the sign in `2n + 1`, and nowhere inside a token's worth.
fn an_plus_b(args: &Values<'_>) -> Option<AnB> {
    let toks: Vec<&ComponentValue<'_>> = args
        .iter()
        .map(|cv| &cv.node)
        .filter(|cv| !is_whitespace(cv))
        .collect();
    let first = toks.first()?;
    // A `+` directly before `n…`. `+ n` has whitespace between, and then the
    // `+` is followed by a whitespace token rather than the ident.
    let plus_ident = match (
        first,
        args.iter().map(|cv| &cv.node).nth(skip_ws(args, 0) + 1),
    ) {
        (
            ComponentValue::Token(Token::Delim('+')),
            Some(ComponentValue::Token(Token::Ident(s))),
        ) if !s.starts_with('-') => Some(s),
        _ => None,
    };
    let (a, tail, rest) = match (first, plus_ident) {
        (_, Some(s)) => {
            let (a, tail) = n_ident(s)?;
            (a, tail, &toks[2..])
        }
        (
            ComponentValue::Token(Token::Number {
                value,
                num_type: NumericType::Integer,
                ..
            }),
            _,
        ) => {
            return (toks.len() == 1).then_some(AnB {
                a: 0,
                b: *value as i32,
            });
        }
        (ComponentValue::Token(Token::Ident(s)), _) => {
            if toks.len() == 1 && s.eq_ignore_ascii_case("odd") {
                return Some(AnB { a: 2, b: 1 });
            }
            if toks.len() == 1 && s.eq_ignore_ascii_case("even") {
                return Some(AnB { a: 2, b: 0 });
            }
            let (a, tail) = n_ident(s)?;
            (a, tail, &toks[1..])
        }
        (
            ComponentValue::Token(Token::Dimension {
                value,
                num_type: NumericType::Integer,
                unit,
                ..
            }),
            _,
        ) => {
            let tail = strip_prefix_ignore_case(unit, "n")?;
            (*value as i32, tail, &toks[1..])
        }
        _ => return None,
    };
    let b = match (tail, rest) {
        // `2n`, `n`, `-n`.
        ("", []) => 0,
        // `2n +1`, `n -1`: a signed integer.
        (
            "",
            [
                ComponentValue::Token(Token::Number {
                    value,
                    num_type: NumericType::Integer,
                    repr,
                }),
            ],
        ) if repr.starts_with(['+', '-']) => *value as i32,
        // `2n + 1`, `n - 1`: a sign, then a signless integer.
        ("", [ComponentValue::Token(Token::Delim(sign @ ('+' | '-'))), n]) => {
            let v = signless(n)?;
            if *sign == '-' { -v } else { v }
        }
        // `2n- 1`, `n- 1`.
        ("-", [n]) => -signless(n)?,
        // `2n-1`, `n-1`, `-n-1`: all one token.
        (digits, []) => {
            let d = digits.strip_prefix('-')?;
            if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            d.parse::<i64>()
                .map_or(i32::MIN, |v| i32::try_from(-v).unwrap_or(i32::MIN))
        }
        _ => return None,
    };
    Some(AnB { a, b })
}

/// `n…` or `-n…`: A, and what follows the `n`.
fn n_ident(s: &str) -> Option<(i32, &str)> {
    match strip_prefix_ignore_case(s, "-n") {
        Some(tail) => Some((-1, tail)),
        None => Some((1, strip_prefix_ignore_case(s, "n")?)),
    }
}

/// An integer written without a sign.
fn signless(cv: &ComponentValue<'_>) -> Option<i32> {
    match cv {
        ComponentValue::Token(Token::Number {
            value,
            num_type: NumericType::Integer,
            repr,
        }) if !repr.starts_with(['+', '-']) => Some(*value as i32),
        _ => None,
    }
}

fn strip_prefix_ignore_case<'s>(s: &'s str, prefix: &str) -> Option<&'s str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn bad(span: Span) -> SyntaxError {
    SyntaxError {
        span,
        kind: SyntaxErrorKind::InvalidSelector,
    }
}

fn structure_error(span: Span) -> SyntaxError {
    SyntaxError {
        span,
        kind: SyntaxErrorKind::InvalidSelectorStructure,
    }
}

/// Keep the first structure error a selector has.
fn note(structure: &mut Option<Span>, span: Span) {
    structure.get_or_insert(span);
}

fn is_delim(cv: Option<&Spanned<ComponentValue<'_>>>, c: char) -> bool {
    matches!(cv.map(|v| &v.node), Some(ComponentValue::Token(Token::Delim(d))) if *d == c)
}

fn is_whitespace(cv: &ComponentValue<'_>) -> bool {
    matches!(cv, ComponentValue::Token(Token::Whitespace))
}

fn is_blank(part: &Values<'_>) -> bool {
    part.iter().all(|cv| is_whitespace(&cv.node))
}

fn skip_ws(part: &Values<'_>, mut i: usize) -> usize {
    while part.get(i).is_some_and(|cv| is_whitespace(&cv.node)) {
        i += 1;
    }
    i
}

/// `>`, `+` and `~`. Descendant combination is plain whitespace and is
/// handled by the complex-selector loop, not here.
fn combinator(cv: &ComponentValue<'_>) -> Option<Combinator> {
    match cv {
        ComponentValue::Token(Token::Delim('>')) => Some(Combinator::Child),
        ComponentValue::Token(Token::Delim('+')) => Some(Combinator::NextSibling),
        ComponentValue::Token(Token::Delim('~')) => Some(Combinator::SubsequentSibling),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_list_is_one_the_full_parser_accepts() {
        // Random preludes from pieces near the plain grammar's edges: every
        // one the fast path calls plain must parse with no error, as a
        // plain list and as a relative one.
        const PIECES: &[&str] = &[
            "a", "div", "*", ".", "b", "#x", "#1", " ", ">", "+", "~", ",", "|", ":", "hover", "[",
            "]", "&", "::", "-", "--x", "\\31 ", "u", "1", "(", ")", ".5", "\t", "/**/", "x|",
            "*|", "#-", "#--", ".-a",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let (mut plain, mut tried) = (0, 0);
        for _ in 0..200_000 {
            let len = 1 + next(8);
            let prelude: String = (0..len).map(|_| PIECES[next(PIECES.len())]).collect();
            let css = format!("{prelude} {{}}");
            let sheet = crate::parse_stylesheet(&css).0;
            let [rule] = sheet.rules.as_slice() else {
                continue;
            };
            let crate::Rule::Qualified(q) = &rule.node else {
                continue;
            };
            tried += 1;
            for relative in [false, true] {
                if is_plain_selector_list(&q.prelude, relative) {
                    plain += 1;
                    let full = if relative {
                        parse_relative_selector_list(&q.prelude)
                    } else {
                        parse_selector_list(&q.prelude)
                    };
                    assert!(full.is_ok(), "{prelude:?} relative={relative}: {full:?}");
                }
            }
        }
        assert!(plain > 10_000 && tried > 100_000, "{plain} of {tried}");
    }
    use crate::parser::{SyntaxErrorKind, syntax_errors};

    /// Every selector error the module reports, for one stylesheet.
    fn sel_errors(css: &str) -> usize {
        kinds(css).len()
    }

    /// The kinds of the selector errors reported for one stylesheet.
    fn kinds(css: &str) -> Vec<SyntaxErrorKind> {
        syntax_errors(css)
            .into_iter()
            .map(|e| e.kind)
            .filter(|k| {
                matches!(
                    k,
                    SyntaxErrorKind::InvalidSelector | SyntaxErrorKind::InvalidSelectorStructure
                )
            })
            .collect()
    }

    /// The first rule's prelude, parsed as a selector list.
    fn parse(selector: &str) -> SelectorList<'static> {
        let css: &'static str = Box::leak(format!("{selector} {{}}").into_boxed_str());
        let (sheet, _) = crate::parse_stylesheet(css);
        let crate::Rule::Qualified(q) = &sheet.rules[0].node else {
            panic!("not a qualified rule: {selector}");
        };
        parse_selector_list(&q.prelude).unwrap_or_else(|e| panic!("{selector}: {e:?}"))
    }

    fn spec(selector: &str) -> (u32, u32, u32) {
        let s = parse(selector).specificity();
        (s.ids, s.classes, s.types)
    }

    /// The tree for one everyday selector, node by node.
    #[test]
    fn the_tree_has_compounds_combinators_and_simples() {
        let list = parse("div > p.note + a[href^=\"http\" i]:hover");
        let c = &list.selectors[0].node;
        assert_eq!(c.leading, None);
        assert_eq!(c.combinators, [Combinator::Child, Combinator::NextSibling]);
        let simples: Vec<_> = c.compounds.iter().map(|c| c.node.simples.len()).collect();
        assert_eq!(simples, [1, 2, 3]);
        let a = &c.compounds[2].node.simples;
        let SimpleSelector::Attribute(attr) = &a[1].node else {
            panic!("{:?}", a[1].node)
        };
        assert_eq!(attr.name.local.node, "href");
        let v = attr.value.as_ref().unwrap();
        assert_eq!(v.operator, AttributeOperator::Prefix);
        assert_eq!(v.value, "http");
        assert_eq!(v.case, AttributeCase::Insensitive);
        assert!(matches!(
            &a[2].node,
            SimpleSelector::PseudoClass(PseudoClass::Other { name, args: None }) if name == "hover"
        ));
    }

    #[test]
    fn whitespace_is_a_descendant_combinator_and_nothing_more() {
        let list = parse("  ul   li  ,  ol>li ");
        assert_eq!(list.selectors.len(), 2);
        assert_eq!(list.selectors[0].node.combinators, [Combinator::Descendant]);
        // Whitespace around an explicit combinator is not a second one.
        let list = parse("ol > li");
        assert_eq!(list.selectors[0].node.combinators, [Combinator::Child]);
    }

    #[test]
    fn spans_cover_the_selector_as_written() {
        let css = "h1,  ul li.x , p {}";
        let (sheet, _) = crate::parse_stylesheet(css);
        let list = parse_selector_list(sheet.rules[0].node.prelude()).unwrap();
        let spans: Vec<_> = list.selectors.iter().map(|s| s.span.slice(css)).collect();
        assert_eq!(spans, ["h1", "ul li.x", "p"]);
        let c = &list.selectors[1].node.compounds[1];
        assert_eq!(c.span.slice(css), "li.x");
        assert_eq!(c.node.simples[1].span.slice(css), ".x");
    }

    #[test]
    fn namespaces_are_kept_as_written() {
        let list = parse("svg|rect, *|a, |b, ns|*, [xlink|href]");
        let heads: Vec<_> = list
            .selectors
            .iter()
            .map(|s| s.node.compounds[0].node.simples[0].node.clone())
            .collect();
        assert!(matches!(&heads[0], SimpleSelector::Type(QualifiedName {
            namespace: Some(Namespace::Prefix(p)), local }) if p == "svg" && local.node == "rect"));
        assert!(matches!(
            &heads[1],
            SimpleSelector::Type(QualifiedName {
                namespace: Some(Namespace::Any),
                ..
            })
        ));
        assert!(matches!(
            &heads[2],
            SimpleSelector::Type(QualifiedName {
                namespace: Some(Namespace::NoNamespace),
                ..
            })
        ));
        assert!(
            matches!(&heads[3], SimpleSelector::Universal(Some(Namespace::Prefix(p))) if p == "ns")
        );
        let SimpleSelector::Attribute(attr) = &heads[4] else {
            panic!("{:?}", heads[4])
        };
        assert!(
            matches!(&attr.name.namespace, Some(Namespace::Prefix(p)) if p == "xlink")
                && attr.value.is_none()
        );
    }

    #[test]
    fn legacy_single_colon_pseudo_elements_are_pseudo_elements() {
        for s in ["p:before", "p:after", "p:first-line", "p:FIRST-LETTER"] {
            let list = parse(s);
            assert!(
                matches!(
                    list.selectors[0].node.compounds[0].node.simples[1].node,
                    SimpleSelector::PseudoElement(_)
                ),
                "{s}"
            );
            assert_eq!(spec(s), (0, 0, 2), "{s}");
        }
    }

    /// Selectors Level 4 §17's own examples, and the functional
    /// pseudo-classes whose specificity is not one class.
    #[test]
    fn specificity_follows_the_spec() {
        for (s, want) in [
            ("*", (0, 0, 0)),
            ("li", (0, 0, 1)),
            ("ul li", (0, 0, 2)),
            ("ul ol+li", (0, 0, 3)),
            ("h1 + *[rel=up]", (0, 1, 1)),
            ("ul ol li.red", (0, 1, 3)),
            ("li.red.level", (0, 2, 1)),
            ("#x34y", (1, 0, 0)),
            ("#s12:not(FOO)", (1, 0, 1)),
            (".foo :is(.bar, #baz)", (1, 1, 0)),
            ("a:hover", (0, 1, 1)),
            ("p::first-line", (0, 0, 2)),
            (":where(#a, .b) p", (0, 0, 1)),
            ("li:nth-child(2n+1)", (0, 1, 1)),
            ("li:nth-child(2n of .a, #b)", (1, 1, 1)),
            ("a:has(> img, #x)", (1, 0, 1)),
            ("& .x", (0, 1, 0)),
        ] {
            assert_eq!(spec(s), want, "{s}");
        }
        // The list's specificity is its largest member's.
        assert_eq!(spec("p, #a, .b"), (1, 0, 0));
        // The derived order is the cascade's: one id beats any classes.
        let one_id = Specificity {
            ids: 1,
            classes: 0,
            types: 0,
        };
        let many_classes = Specificity {
            ids: 0,
            classes: 99,
            types: 99,
        };
        assert!(one_id > many_classes);
    }

    fn nth(args: &str) -> Option<AnB> {
        let css: &'static str = Box::leak(format!("x{{a:f({args})}}").into_boxed_str());
        let (sheet, _) = crate::parse_stylesheet(css);
        let items = &sheet.rules[0].node.block().unwrap().node;
        let crate::BlockItem::Declaration(d) = &items[0] else {
            panic!()
        };
        let ComponentValue::Function { args, .. } = &d.node.value[0].node else {
            panic!()
        };
        an_plus_b(args)
    }

    /// Every production of CSS Syntax §7.2, and its near misses.
    #[test]
    fn an_plus_b_reads_every_form() {
        for (s, a, b) in [
            ("odd", 2, 1),
            ("EVEN", 2, 0),
            ("3", 0, 3),
            ("-3", 0, -3),
            ("+3", 0, 3),
            ("n", 1, 0),
            ("N", 1, 0),
            ("-n", -1, 0),
            ("+n", 1, 0),
            ("2n", 2, 0),
            ("2n+1", 2, 1),
            ("2n + 1", 2, 1),
            ("2n +1", 2, 1),
            ("2n+ 1", 2, 1),
            ("2n-1", 2, -1),
            ("2n- 1", 2, -1),
            ("2n -1", 2, -1),
            ("2n - 1", 2, -1),
            ("-n+3", -1, 3),
            ("-n-3", -1, -3),
            ("n-3", 1, -3),
            ("+n-3", 1, -3),
            ("+n- 3", 1, -3),
            ("-n- 3", -1, -3),
            ("0n+0", 0, 0),
            (" 3n ", 3, 0),
        ] {
            assert_eq!(nth(s), Some(AnB { a, b }), "{s}");
        }
        for s in [
            "", "+ n", "+ 3", "1.5n", "2.0", "3n+", "3n + -1", "3n - +1", "n- -1", "2n-1-", "n 1",
            "x", "odd 1", "-n-", "2n -1 1", "--n",
        ] {
            assert_eq!(nth(s), None, "must be rejected: {s:?}");
        }
    }

    #[test]
    fn an_plus_b_positions() {
        let odd = AnB { a: 2, b: 1 };
        assert!(odd.matches(1) && !odd.matches(2) && odd.matches(3));
        let first3 = AnB { a: -1, b: 3 };
        assert!(first3.matches(1) && first3.matches(3) && !first3.matches(4));
        let third = AnB { a: 0, b: 3 };
        assert!(third.matches(3) && !third.matches(6));
        let from5 = AnB { a: 1, b: 5 };
        assert!(!from5.matches(4) && from5.matches(5) && from5.matches(50));
        let never = AnB { a: -2, b: -1 };
        assert!((1..100).all(|i| !never.matches(i)));
    }

    #[test]
    fn forgiving_lists_drop_what_does_not_parse() {
        let list = parse(":is(.a, > b, , .c)");
        let SimpleSelector::PseudoClass(PseudoClass::Is { selectors, dropped }) =
            &list.selectors[0].node.compounds[0].node.simples[0].node
        else {
            panic!()
        };
        assert_eq!(selectors.selectors.len(), 2);
        assert_eq!(*dropped, 1);
        assert_eq!(sel_errors(":where(. x) { color: red }"), 0);
        assert_eq!(sel_errors(":is() { color: red }"), 0);
    }

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
            "a[href=\"x\" s] { color: red }",
            "a[href=\"x\" q] { color: red }",
            "a[ href = \"x\" ] { color: red }",
            // Namespaces.
            "ns|E { color: red }",
            "*|E { color: red }",
            "|E { color: red }",
            "ns|* { color: red }",
            "[ns|href] { color: red }",
            "[*|href] { color: red }",
            "[|href] { color: red }",
            // Pseudo-classes and -elements, including names we don't know.
            "a:hover { color: red }",
            "p::first-line { color: red }",
            "p:first-letter { color: red }",
            "a:hover::before { color: red }",
            "p::before:hover { color: red }",
            "li:nth-child(2n+1) { color: red }",
            "li:nth-of-type(odd) { color: red }",
            "li:nth-last-child(-n+3) { color: red }",
            "p:not(.c) { color: red }",
            "p:not(.a, .b > c) { color: red }",
            "p:is(h1, h2) { color: red }",
            "p:where(.a, .b) { color: red }",
            "a:has(> img) { color: red }",
            "a:has(+ b, ~ c, d) { color: red }",
            "p:lang(ja) { color: red }",
            "p:lang(\"*-CH\") { color: red }",
            ":root { color: red }",
            // Deliberately newer / unknown than this code: must be accepted.
            "::part(label) { color: red }",
            "li:nth-child(2n+1 of .c) { color: red }",
            "input::-webkit-search-cancel-button { color: red }",
            "p:future-pseudo-nobody-invented-yet { color: red }",
            "p:future-function(1 2 3 ? !) { color: red }",
            "& .c { color: red }",
            "&.c { color: red }",
            "&div { color: red }",
            "a:not(:has(> .x)) { color: red }",
        ] {
            assert_eq!(sel_errors(css), 0, "must be accepted: {css}");
        }
    }

    /// Shapes no version of Selectors can produce.
    /// Shapes no version of Selectors can produce, each with the kind it is
    /// reported as.
    #[test]
    fn malformed_selectors_are_reported() {
        use SyntaxErrorKind::{InvalidSelector as T, InvalidSelectorStructure as S};
        for (css, kind) in [
            // Combinator with nothing on one side.
            ("> p", T),
            ("div >", T),
            ("div > > p", T),
            // Empty side of a comma.
            (", p", T),
            ("h1,", T),
            ("h1,, h2", T),
            // A class or namespace separator with no name after it.
            (".", T),
            ("ns|", T),
            ("|", T),
            // A colon with no pseudo name.
            ("a:", T),
            // Attribute selectors.
            ("[]", T),
            ("[=x]", T),
            ("[href=]", T),
            ("[href~]", T),
            ("[*|]", T),
            ("[href=x i s]", T),
            ("[href=x 1]", T),
            // A token that cannot start a simple selector.
            ("\"str\"", T),
            ("42", T),
            // Arguments with a selector grammar: a token error inside is one.
            ("p:not(. a)", T),
            ("p:not(> a)", T),
            ("a:has(. x)", T),
            ("li:nth-child(2n of . a)", T),
            ("p:not()", T),
            ("p:not( )", T),
            // An id has to be an identifier.
            ("#1a", S),
            // Order inside a compound, and the pseudo-element's place.
            (".adiv*", S),
            ("[x]p", S),
            ("p::before.x", S),
            ("p::before span", S),
            // Arguments that are all valid pieces, wrongly put together.
            ("p:not(#1a)", S),
            ("a:has()", S),
            ("li:nth-child(foo)", S),
            ("li:nth-child()", S),
            ("li:nth-child(2n of)", S),
            ("li:nth-of-type(1.5)", S),
        ] {
            assert_eq!(kinds(&format!("{css} {{ color: red }}")), [kind], "{css}");
        }
    }

    /// A selector with both kinds of error reports the token one: that is the
    /// one epubcheck reports too, and the per-selector cap stays one.
    #[test]
    fn a_token_error_wins_over_a_structure_error() {
        use SyntaxErrorKind::{InvalidSelector as T, InvalidSelectorStructure as S};
        assert_eq!(kinds("#1a . b { color: red }"), [T]);
        assert_eq!(kinds("[x]p:not(. b) { color: red }"), [T]);
        assert_eq!(kinds(":nth-child(foo) > > a { color: red }"), [T]);
        // In separate selectors, each reports its own.
        assert_eq!(kinds("#1a, . b { color: red }"), [S, T]);
        // The token error's span, not the structure error's.
        let css = "#1a . b { color: red }";
        assert_eq!(syntax_errors(css)[0].span.slice(css), ".");
    }

    /// The probe shapes epubveri measured against epubcheck 5.4.0
    /// (2026-10-18). The structure kind is exactly the set epubcheck
    /// accepts; the token kind, the set it reports. One exception: epubcheck
    /// reports an empty `:is()`, which takes a forgiving list and is valid.
    #[test]
    fn epubveri_probe_shapes() {
        use SyntaxErrorKind::{InvalidSelector as T, InvalidSelectorStructure as S};
        for (css, want) in [
            ("p.x { color: red }", vec![]),
            (". a { color: red }", vec![T]),
            ("[a=x i] { color: red }", vec![]),
            ("p { & div { color: red } }", vec![]),
            ("#1a { color: red }", vec![S]),
            ("[x]p { color: red }", vec![S]),
            (".a* { color: red }", vec![S]),
            ("p::before.x { color: red }", vec![S]),
            ("p::before span { color: red }", vec![S]),
            ("[a=x i s] { color: red }", vec![T]),
            (":not(. a) { color: red }", vec![T]),
            (":nth-child(foo) { color: red }", vec![S]),
            (":has(. a) { color: red }", vec![T]),
            (":nth-child(2n+) { color: red }", vec![S]),
            // The nine it added for the gate on d993de1.
            (":not() { color: red }", vec![T]),
            (":has() { color: red }", vec![S]),
            (":nth-child(2n of) { color: red }", vec![S]),
            (":nth-of-type(1.5) { color: red }", vec![S]),
            (":not(#1a) { color: red }", vec![S]),
            ("#1a, . b { color: red }", vec![S, T]),
            ("#1a . b { color: red }", vec![T]),
            (":is() { color: red }", vec![]),
            (":nth-child(2n of p) { color: red }", vec![]),
        ] {
            assert_eq!(kinds(css), want, "{css}");
        }
    }

    /// `.a div` is a class then a descendant `div`, which is fine; the bad
    /// shape is a type selector glued after something else.
    #[test]
    fn a_type_selector_after_whitespace_is_a_new_compound() {
        assert_eq!(sel_errors(".a div { color: red }"), 0);
        assert_eq!(sel_errors("[x] p { color: red }"), 0);
        assert_eq!(sel_errors("[x]p { color: red }"), 1);
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
        // Inside `:not()`, the error is the inner one.
        let css = "p:not(a > > b) { color: red }";
        let errs = syntax_errors(css);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].span.start, 10);
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
        let (sheet, _) = crate::parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 2, "@charset and .hello are two rules");
    }

    /// A selector error must not stop the rest of the sheet being parsed -
    /// the parser stays error-recovering, as it is everywhere else.
    #[test]
    fn a_bad_selector_does_not_swallow_the_stylesheet() {
        let (sheet, _) = crate::parse_stylesheet("> bad { color: red } p { color: blue }");
        assert_eq!(sheet.rules.len(), 2, "both rules still parse");
    }

    /// #3: the reporting unit is the comma-separated selector - not the bad
    /// token, and not the whole prelude.
    ///
    /// A stylesheet beginning with a stray declaration is the shape that
    /// exposed it: per CSS Syntax the whole of `text-indent:1.5em;\n@page`
    /// becomes one qualified rule's prelude, so a per-token walk blamed the
    /// `:`, the `1.5em`, the `;` and the `@page` of the *next* rule - four
    /// errors for one mistake, the last of them pointing at well-formed CSS.
    ///
    /// The two-selector case is the invariant in the other direction and the
    /// reason this is not simply "report the first error and stop": `. h-100,
    /// . y-100` really is two independently broken selectors and a caller
    /// fixing them needs both named.
    #[test]
    fn a_broken_selector_is_reported_once_not_once_per_token() {
        assert_eq!(
            sel_errors("text-indent:1.5em;\n@page { margin: 1em; }"),
            1,
            "one stray declaration is one error, not one per token"
        );
        assert_eq!(
            sel_errors(". h-100, . y-100 { height: 99vh; }"),
            2,
            "two separately broken selectors are still two errors"
        );
        assert_eq!(sel_errors("img . portrait { height: 99vh; }"), 1);
        assert_eq!(sel_errors("p { color: red }"), 0, "valid stays silent");
    }

    /// The cap is per part, so an empty comma side is unaffected: it is
    /// reported by `split_on_commas` before any part is walked.
    #[test]
    fn capping_per_selector_leaves_the_comma_errors_alone() {
        assert_eq!(sel_errors("a,, b { color: red }"), 1, "the empty side");
        assert_eq!(sel_errors("a, { color: red }"), 1, "the trailing comma");
        // One bad part next to a good one: still exactly one.
        assert_eq!(sel_errors("p, . q { color: red }"), 1);
    }

    /// Each part is read on its own: two bad attribute selectors in two
    /// parts is two errors.
    #[test]
    fn every_part_is_read() {
        assert_eq!(sel_errors("a[=x], b[=y] { color: red }"), 2);
    }

    /// Past [`MAX_SELECTOR_DEPTH`] arguments are kept unread, not reported
    /// and not recursed into: `:is()` 300 deep parses on a test thread.
    #[test]
    fn deep_selector_arguments_are_kept_unread() {
        let deep = format!("{}a{}", ":is(".repeat(300), ")".repeat(300));
        assert_eq!(sel_errors(&format!("{deep} {{ color: red }}")), 0);
        // `MAX_SELECTOR_DEPTH` levels are read; the one below is `Other`.
        let n = MAX_SELECTOR_DEPTH + 1;
        let list = parse(&format!("{}a{}", ":not(".repeat(n), ")".repeat(n)));
        let mut levels = 0;
        let mut node = &list.selectors[0].node.compounds[0].node.simples[0].node;
        while let SimpleSelector::PseudoClass(PseudoClass::Not(inner)) = node {
            levels += 1;
            node = &inner.selectors[0].node.compounds[0].node.simples[0].node;
        }
        assert_eq!(levels, MAX_SELECTOR_DEPTH);
        assert!(matches!(
            node,
            SimpleSelector::PseudoClass(PseudoClass::Other { .. })
        ));
    }

    #[test]
    fn relative_selectors_only_where_relative() {
        let (sheet, errs) = crate::parse_stylesheet("p { > a { } + b { } ~ c { } }");
        assert!(errs.is_empty(), "{errs:?}");
        let crate::Rule::Qualified(q) = &sheet.rules[0].node else {
            panic!()
        };
        let crate::BlockItem::Rule(r) = &q.block.node[0] else {
            panic!()
        };
        let list = parse_relative_selector_list(r.node.prelude()).unwrap();
        assert_eq!(list.selectors[0].node.leading, Some(Combinator::Child));
        assert!(parse_selector_list(r.node.prelude()).is_err());
        assert_eq!(sel_errors("p { > { } }"), 1, "a lone combinator");
    }
}
