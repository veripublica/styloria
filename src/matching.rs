//! Selector matching against a document tree the caller owns.
//!
//! styloria has no DOM and depends on none. The caller describes its tree
//! through the [`Element`] trait — a handful of questions any DOM can answer
//! cheaply — and asks whether a [`SelectorList`] or [`ComplexSelector`]
//! matches an element.
//!
//! The answer is three-valued, [`Match`]: `Yes`, `No`, or `Unknown` when the
//! selector depends on something a static tree does not say.
//!
//! **The answer is about the element itself**, not its pseudo-elements. A
//! selector ending in a pseudo-element (`img::before`) styles a box the
//! element generates, never the element's own properties, so it answers No.
//! A caller asking "does anything style this element or the boxes it
//! generates" has to look at those selectors separately. A caller that
//! acts only on a certain "no" (a converter deciding whether CSS could
//! override an attribute, say) treats `Unknown` as `Yes`; one that reports
//! can say which it was. Unknown covers:
//!
//! - state and other pseudo-classes this code does not answer (`:hover`,
//!   `:visited`, `:lang()`, `:has()`, vendor and future names);
//! - `&`, whose meaning is the parent rule's selector;
//! - a relative selector's leading combinator (`> a` in a nested rule);
//! - `:is()`/`:where()` arguments the forgiving parse dropped;
//! - `[*|attr]`, which would need every namespace's attribute of that name;
//! - in an HTML document, an attribute value that differs from the selector's
//!   only in ASCII case and has no `i`/`s` flag (HTML makes some attributes
//!   case-insensitive, and this code does not carry that list);
//! - a selector that takes more than [`MAX_MATCH_STEPS`] steps to settle.
//!   Descendant combinators inside nested `:is()` (which is what a resolved
//!   `&` is) multiply the work by the tree's depth per level, so a hostile
//!   stylesheet could otherwise make one answer take hours.
//!
//! Unknown propagates as in three-valued logic: `:not(Unknown)` is Unknown,
//! a compound is No as soon as one part is No, and a combinator is Yes as
//! soon as one path is Yes.

use std::borrow::Cow;
use std::cell::Cell;

use crate::selector::{
    AttributeCase, AttributeOperator, AttributeSelector, Combinator, ComplexSelector,
    CompoundSelector, Namespace, PseudoClass, QualifiedName, SelectorList, SimpleSelector,
};

/// The most steps one call to [`SelectorList::matches`] or
/// [`ComplexSelector::matches`] takes, arguments included; past it the
/// answer is Unknown. A step is one compound tried on one element. Real
/// selectors take far fewer: on 39 real books, a budget of 64 changed no
/// answer.
pub const MAX_MATCH_STEPS: u32 = 1 << 12;

/// The HTML namespace. In an HTML document its elements' names, and their
/// attributes' names, compare ASCII case-insensitively.
pub const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

/// The answer to "does this selector match this element?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Match {
    No,
    Yes,
    /// It depends on something the tree does not say: a state, the parent
    /// rule behind `&`, a construct this code does not evaluate.
    Unknown,
}

impl Match {
    fn from_bool(b: bool) -> Match {
        if b { Match::Yes } else { Match::No }
    }

    /// Both: No if either is No, Yes if both are Yes.
    pub fn and(self, other: Match) -> Match {
        match (self, other) {
            (Match::No, _) | (_, Match::No) => Match::No,
            (Match::Yes, Match::Yes) => Match::Yes,
            _ => Match::Unknown,
        }
    }

    /// Either: Yes if either is Yes, No if both are No.
    pub fn or(self, other: Match) -> Match {
        match (self, other) {
            (Match::Yes, _) | (_, Match::Yes) => Match::Yes,
            (Match::No, Match::No) => Match::No,
            _ => Match::Unknown,
        }
    }

    /// The negation; Unknown stays Unknown.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Match {
        match self {
            Match::No => Match::Yes,
            Match::Yes => Match::No,
            Match::Unknown => Match::Unknown,
        }
    }
}

/// An element of the caller's document tree.
///
/// Implement it on a cheap handle (a node reference, an index): it is cloned
/// freely while walking ancestors and siblings. Siblings and parents are
/// **elements only**; text, comments and processing instructions are not
/// siblings for any selector.
pub trait Element: Clone {
    /// The parent element; `None` for the root element.
    fn parent(&self) -> Option<Self>;

    /// The nearest preceding sibling element.
    fn prev_sibling(&self) -> Option<Self>;

    /// The nearest following sibling element.
    fn next_sibling(&self) -> Option<Self>;

    /// Whether the element has any child elements or any text, which is what
    /// `:empty` asks. Browsers count whitespace-only text as content.
    fn has_children(&self) -> bool;

    /// The element's local name, as the document writes it.
    fn local_name(&self) -> &str;

    /// The element's namespace URI; `None` for an element in no namespace.
    fn namespace(&self) -> Option<&str>;

    /// The value of an attribute in no namespace, by local name. For an
    /// HTML element in an HTML document the name asked for is lowercase.
    fn attribute(&self, local_name: &str) -> Option<&str>;

    /// The value of an attribute in the namespace `namespace_uri`.
    fn attribute_ns(&self, namespace_uri: &str, local_name: &str) -> Option<&str>;

    /// Whether the element is in an HTML document, where HTML elements'
    /// names compare case-insensitively. `false` for XML documents, XHTML
    /// included, where every name is case-sensitive.
    fn is_html_document(&self) -> bool;

    /// The element's id. The default reads the `id` attribute.
    fn id(&self) -> Option<&str> {
        self.attribute("id")
    }

    /// Whether `name` is one of the element's classes. The default reads the
    /// `class` attribute as whitespace-separated words.
    fn has_class(&self, name: &str) -> bool {
        self.attribute("class")
            .is_some_and(|c| c.split(is_html_whitespace).any(|w| w == name))
    }
}

/// The namespace prefixes a stylesheet declares with `@namespace`, which a
/// prefixed selector (`svg|rect`) and the default namespace depend on.
///
/// The empty map is a stylesheet without `@namespace`: no default namespace,
/// and every prefix undeclared.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Namespaces<'a> {
    /// The default namespace (`@namespace url(…)`): unprefixed type
    /// selectors match only elements in it.
    pub default: Option<Cow<'a, str>>,
    /// `(prefix, uri)` pairs. Prefixes are case-sensitive. When a prefix is
    /// declared twice the later declaration wins.
    pub prefixes: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

impl Namespaces<'_> {
    /// The URI a prefix stands for. `None` when it was never declared, which
    /// makes a selector using it invalid: a browser drops its rule, and it
    /// matches nothing.
    pub fn resolve(&self, prefix: &str) -> Option<&str> {
        self.prefixes
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.as_ref())
    }
}

impl SelectorList<'_> {
    /// Whether any selector in the list matches `element`.
    pub fn matches<E: Element>(&self, element: &E, namespaces: &Namespaces<'_>) -> Match {
        Matcher::new(namespaces).list(self, element)
    }
}

impl ComplexSelector<'_> {
    /// Whether this selector matches `element`, as its subject (the last
    /// compound).
    pub fn matches<E: Element>(&self, element: &E, namespaces: &Namespaces<'_>) -> Match {
        Matcher::new(namespaces).complex(self, element)
    }
}

struct Matcher<'n, 'a> {
    namespaces: &'n Namespaces<'a>,
    /// Steps left; see [`MAX_MATCH_STEPS`].
    steps: Cell<u32>,
}

impl<'n, 'a> Matcher<'n, 'a> {
    fn new(namespaces: &'n Namespaces<'a>) -> Self {
        Matcher {
            namespaces,
            steps: Cell::new(MAX_MATCH_STEPS),
        }
    }

    fn list<E: Element>(&self, list: &SelectorList<'_>, element: &E) -> Match {
        any(&list.selectors, |s| self.complex(&s.node, element))
    }

    fn complex<E: Element>(&self, selector: &ComplexSelector<'_>, element: &E) -> Match {
        let Some(last) = selector.compounds.len().checked_sub(1) else {
            return Match::No;
        };
        let m = self.from(selector, last, element);
        // `> a` is relative to an element this selector alone does not know.
        if selector.leading.is_some() {
            m.and(Match::Unknown)
        } else {
            m
        }
    }

    /// Whether compounds `0..=k` of `selector` match with compound `k` on
    /// `element`. Right to left: the subject first, then the combinator to
    /// its left, so a mismatch stops the walk before any ancestor is visited.
    fn from<E: Element>(&self, selector: &ComplexSelector<'_>, k: usize, element: &E) -> Match {
        let Some(left) = self.steps.get().checked_sub(1) else {
            return Match::Unknown;
        };
        self.steps.set(left);
        let here = self.compound(&selector.compounds[k].node, element);
        if here == Match::No || k == 0 {
            return here;
        }
        let rest = |e: &E| self.from(selector, k - 1, e);
        let left = match selector.combinators[k - 1] {
            Combinator::Child => element.parent().map_or(Match::No, |p| rest(&p)),
            Combinator::NextSibling => element.prev_sibling().map_or(Match::No, |p| rest(&p)),
            Combinator::Descendant => any_of_chain(element.parent(), E::parent, rest),
            Combinator::SubsequentSibling => {
                any_of_chain(element.prev_sibling(), E::prev_sibling, rest)
            }
        };
        here.and(left)
    }

    fn compound<E: Element>(&self, compound: &CompoundSelector<'_>, element: &E) -> Match {
        let mut m = Match::Yes;
        for s in &compound.simples {
            m = m.and(self.simple(&s.node, element));
            if m == Match::No {
                break;
            }
        }
        m
    }

    fn simple<E: Element>(&self, simple: &SimpleSelector<'_>, element: &E) -> Match {
        match simple {
            SimpleSelector::Type(name) => self.type_name(name, element),
            SimpleSelector::Universal(ns) => self.element_namespace(ns.as_ref(), element),
            SimpleSelector::Id(id) => Match::from_bool(element.id() == Some(id.as_ref())),
            SimpleSelector::Class(class) => Match::from_bool(element.has_class(class)),
            SimpleSelector::Attribute(attr) => self.attribute(attr, element),
            SimpleSelector::PseudoClass(p) => self.pseudo_class(p, element),
            // The selector is about a box the element generates, not the
            // element.
            SimpleSelector::PseudoElement(_) => Match::No,
            SimpleSelector::Nesting => Match::Unknown,
        }
    }

    /// Whether the element is in the namespace a type or universal selector
    /// asks for. No prefix means the default namespace, if one is declared.
    fn element_namespace<E: Element>(&self, ns: Option<&Namespace<'_>>, element: &E) -> Match {
        let want = match ns {
            Some(Namespace::Any) => return Match::Yes,
            Some(Namespace::NoNamespace) => None,
            Some(Namespace::Prefix(p)) => match self.namespaces.resolve(p) {
                Some(uri) => Some(uri),
                None => return Match::No,
            },
            None => match &self.namespaces.default {
                Some(uri) => Some(uri.as_ref()),
                None => return Match::Yes,
            },
        };
        Match::from_bool(element.namespace() == want)
    }

    fn type_name<E: Element>(&self, name: &QualifiedName<'_>, element: &E) -> Match {
        let ns = self.element_namespace(name.namespace.as_ref(), element);
        if ns == Match::No {
            return ns;
        }
        let local = element.local_name();
        let same = if html_rules(element) {
            local.eq_ignore_ascii_case(&name.local.node)
        } else {
            local == name.local.node
        };
        ns.and(Match::from_bool(same))
    }

    fn attribute<E: Element>(&self, attr: &AttributeSelector<'_>, element: &E) -> Match {
        let html = html_rules(element);
        let local = &attr.name.local.node;
        let lowered;
        let local: &str = if html && local.bytes().any(|b| b.is_ascii_uppercase()) {
            lowered = local.to_ascii_lowercase();
            &lowered
        } else {
            local
        };
        let value = match &attr.name.namespace {
            None | Some(Namespace::NoNamespace) => element.attribute(local),
            Some(Namespace::Prefix(p)) => match self.namespaces.resolve(p) {
                Some(uri) => element.attribute_ns(uri, local),
                None => return Match::No,
            },
            Some(Namespace::Any) => return Match::Unknown,
        };
        let Some(value) = value else {
            return Match::No;
        };
        let Some(want) = &attr.value else {
            return Match::Yes;
        };
        let test =
            |insensitive: bool| attribute_test(want.operator, value, &want.value, insensitive);
        match &want.case {
            AttributeCase::Insensitive => Match::from_bool(test(true)),
            AttributeCase::Sensitive => Match::from_bool(test(false)),
            AttributeCase::Other(_) => Match::Unknown,
            AttributeCase::Default => {
                let exact = test(false);
                // Some HTML attributes compare case-insensitively; this
                // code does not carry that list, so a difference in case
                // alone is not a certain answer there.
                if html && exact != test(true) {
                    Match::Unknown
                } else {
                    Match::from_bool(exact)
                }
            }
        }
    }

    fn pseudo_class<E: Element>(&self, p: &PseudoClass<'_>, element: &E) -> Match {
        match p {
            PseudoClass::Root => Match::from_bool(element.parent().is_none()),
            PseudoClass::Empty => Match::from_bool(!element.has_children()),
            PseudoClass::FirstChild => Match::from_bool(element.prev_sibling().is_none()),
            PseudoClass::LastChild => Match::from_bool(element.next_sibling().is_none()),
            PseudoClass::OnlyChild => Match::from_bool(
                element.prev_sibling().is_none() && element.next_sibling().is_none(),
            ),
            PseudoClass::FirstOfType => {
                Match::from_bool(count_of_type(element, E::prev_sibling) == 0)
            }
            PseudoClass::LastOfType => {
                Match::from_bool(count_of_type(element, E::next_sibling) == 0)
            }
            PseudoClass::OnlyOfType => Match::from_bool(
                count_of_type(element, E::prev_sibling) == 0
                    && count_of_type(element, E::next_sibling) == 0,
            ),
            PseudoClass::NthChild { nth, of } => {
                self.nth(element, *nth, of.as_ref(), E::prev_sibling)
            }
            PseudoClass::NthLastChild { nth, of } => {
                self.nth(element, *nth, of.as_ref(), E::next_sibling)
            }
            PseudoClass::NthOfType(nth) => {
                Match::from_bool(nth.matches(index(count_of_type(element, E::prev_sibling))))
            }
            PseudoClass::NthLastOfType(nth) => {
                Match::from_bool(nth.matches(index(count_of_type(element, E::next_sibling))))
            }
            PseudoClass::Not(list) => self.list(list, element).not(),
            PseudoClass::Is { selectors, dropped } | PseudoClass::Where { selectors, dropped } => {
                let m = self.list(selectors, element);
                if *dropped > 0 {
                    m.or(Match::Unknown)
                } else {
                    m
                }
            }
            PseudoClass::Has(_) | PseudoClass::Other { .. } => Match::Unknown,
        }
    }

    /// `:nth-child(An+B [of S])` and `:nth-last-child()`, counting along
    /// `step`. With `of S`, only siblings matching S count, and the element
    /// itself has to match S; a sibling S answers Unknown for makes the
    /// position, and so the answer, Unknown.
    fn nth<E: Element>(
        &self,
        element: &E,
        nth: crate::selector::AnB,
        of: Option<&SelectorList<'_>>,
        step: fn(&E) -> Option<E>,
    ) -> Match {
        let Some(of) = of else {
            let mut n = 0usize;
            let mut cur = step(element);
            while let Some(e) = cur {
                n += 1;
                cur = step(&e);
            }
            return Match::from_bool(nth.matches(index(n)));
        };
        let itself = self.list(of, element);
        if itself == Match::No {
            return Match::No;
        }
        let (mut n, mut unsure) = (0usize, false);
        let mut cur = step(element);
        while let Some(e) = cur {
            match self.list(of, &e) {
                Match::Yes => n += 1,
                Match::Unknown => unsure = true,
                Match::No => {}
            }
            cur = step(&e);
        }
        if unsure {
            Match::Unknown
        } else {
            itself.and(Match::from_bool(nth.matches(index(n))))
        }
    }
}

/// Whether the HTML rules for names apply: an HTML element in an HTML
/// document.
fn html_rules<E: Element>(element: &E) -> bool {
    element.is_html_document() && element.namespace() == Some(HTML_NAMESPACE)
}

/// `[attr op value]` on one value.
fn attribute_test(op: AttributeOperator, have: &str, want: &str, insensitive: bool) -> bool {
    let eq = |a: &str, b: &str| {
        if insensitive {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    let n = want.len();
    match op {
        AttributeOperator::Equal => eq(have, want),
        AttributeOperator::Includes => {
            !want.is_empty()
                && !want.contains(is_html_whitespace)
                && have.split(is_html_whitespace).any(|w| eq(w, want))
        }
        AttributeOperator::DashMatch => {
            eq(have, want)
                || (have.len() > n
                    && have.as_bytes()[n] == b'-'
                    && have.get(..n).is_some_and(|h| eq(h, want)))
        }
        // An empty value matches nothing for these three.
        AttributeOperator::Prefix => n > 0 && have.get(..n).is_some_and(|h| eq(h, want)),
        AttributeOperator::Suffix => {
            n > 0 && have.len() >= n && have.get(have.len() - n..).is_some_and(|h| eq(h, want))
        }
        AttributeOperator::Substring => {
            n > 0
                && if insensitive {
                    have.to_ascii_lowercase()
                        .contains(&want.to_ascii_lowercase())
                } else {
                    have.contains(want)
                }
        }
    }
}

fn is_html_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')
}

/// How many siblings along `step` share the element's name and namespace.
fn count_of_type<E: Element>(element: &E, step: fn(&E) -> Option<E>) -> usize {
    let (name, ns) = (element.local_name(), element.namespace());
    let mut n = 0;
    let mut cur = step(element);
    while let Some(e) = cur {
        if e.local_name() == name && e.namespace() == ns {
            n += 1;
        }
        cur = step(&e);
    }
    n
}

/// The 1-based position after `before` siblings.
fn index(before: usize) -> i32 {
    i32::try_from(before).map_or(i32::MAX, |n| n.saturating_add(1))
}

/// Yes as soon as one item matches; Unknown if none did but one might.
fn any<T>(items: &[T], mut f: impl FnMut(&T) -> Match) -> Match {
    let mut m = Match::No;
    for item in items {
        m = m.or(f(item));
        if m == Match::Yes {
            break;
        }
    }
    m
}

/// [`any`] along a chain of elements: the ancestors, or the preceding
/// siblings.
fn any_of_chain<E: Element>(
    first: Option<E>,
    step: fn(&E) -> Option<E>,
    mut f: impl FnMut(&E) -> Match,
) -> Match {
    let mut m = Match::No;
    let mut cur = first;
    while let Some(e) = cur {
        m = m.or(f(&e));
        if m == Match::Yes {
            break;
        }
        cur = step(&e);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny arena DOM: enough to exercise every trait method.
    #[derive(Default)]
    struct Doc {
        nodes: Vec<Node>,
        html: bool,
    }

    #[derive(Default)]
    struct Node {
        name: String,
        ns: Option<String>,
        attrs: Vec<(Option<String>, String, String)>,
        parent: Option<usize>,
        children: Vec<usize>,
        text: bool,
    }

    #[derive(Clone, Copy)]
    struct El<'d> {
        doc: &'d Doc,
        i: usize,
    }

    impl<'d> El<'d> {
        fn node(&self) -> &'d Node {
            &self.doc.nodes[self.i]
        }
        fn sibling(&self, d: isize) -> Option<Self> {
            let p = self.node().parent?;
            let kids = &self.doc.nodes[p].children;
            let at = kids.iter().position(|&k| k == self.i)? as isize + d;
            let k = *kids.get(usize::try_from(at).ok()?)?;
            Some(El {
                doc: self.doc,
                i: k,
            })
        }
    }

    impl Element for El<'_> {
        fn parent(&self) -> Option<Self> {
            self.node().parent.map(|i| El { doc: self.doc, i })
        }
        fn prev_sibling(&self) -> Option<Self> {
            self.sibling(-1)
        }
        fn next_sibling(&self) -> Option<Self> {
            self.sibling(1)
        }
        fn has_children(&self) -> bool {
            self.node().text || !self.node().children.is_empty()
        }
        fn local_name(&self) -> &str {
            &self.node().name
        }
        fn namespace(&self) -> Option<&str> {
            self.node().ns.as_deref()
        }
        fn attribute(&self, local_name: &str) -> Option<&str> {
            self.node()
                .attrs
                .iter()
                .find(|(ns, n, _)| ns.is_none() && n == local_name)
                .map(|(_, _, v)| v.as_str())
        }
        fn attribute_ns(&self, uri: &str, local_name: &str) -> Option<&str> {
            self.node()
                .attrs
                .iter()
                .find(|(ns, n, _)| ns.as_deref() == Some(uri) && n == local_name)
                .map(|(_, _, v)| v.as_str())
        }
        fn is_html_document(&self) -> bool {
            self.doc.html
        }
    }

    const SVG: &str = "http://www.w3.org/2000/svg";
    const XLINK: &str = "http://www.w3.org/1999/xlink";

    /// Build from a compact description: `(name, parent, attrs)`; names
    /// starting `svg:` are in the SVG namespace, others in HTML's.
    /// `(name, parent, attributes)`.
    type Spec<'s> = (&'s str, Option<usize>, &'s [(&'s str, &'s str)]);

    fn doc(html: bool, spec: &[Spec<'_>]) -> Doc {
        let mut d = Doc {
            html,
            ..Doc::default()
        };
        for (i, (name, parent, attrs)) in spec.iter().enumerate() {
            let (ns, name) = match name.strip_prefix("svg:") {
                Some(n) => (SVG, n),
                None => (HTML_NAMESPACE, *name),
            };
            let attrs = attrs
                .iter()
                .map(|(k, v)| match k.strip_prefix("xlink:") {
                    Some(k) => (Some(XLINK.to_string()), k.to_string(), v.to_string()),
                    None => (None, k.to_string(), v.to_string()),
                })
                .collect();
            d.nodes.push(Node {
                name: name.to_string(),
                ns: Some(ns.to_string()),
                attrs,
                parent: *parent,
                ..Node::default()
            });
            if let Some(p) = parent {
                d.nodes[*p].children.push(i);
            }
        }
        d
    }

    fn list(selector: &str) -> SelectorList<'static> {
        let css: &'static str = Box::leak(format!("{selector} {{}}").into_boxed_str());
        let (sheet, _) = crate::parse_stylesheet(css);
        crate::parse_selector_list(sheet.rules[0].node.prelude())
            .unwrap_or_else(|e| panic!("{selector}: {e:?}"))
    }

    fn m(d: &Doc, i: usize, selector: &str) -> Match {
        list(selector).matches(&El { doc: d, i }, &Namespaces::default())
    }

    /// html > body > (p.a#x, p, div > (img[width], span), p.b[lang=en-GB])
    fn page(html: bool) -> Doc {
        doc(
            html,
            &[
                ("html", None, &[]),
                ("body", Some(0), &[]),
                ("p", Some(1), &[("class", "a  intro"), ("id", "x")]),
                ("p", Some(1), &[]),
                ("div", Some(1), &[("title", "Hello World")]),
                ("img", Some(4), &[("width", "46"), ("src", "a.PNG")]),
                ("span", Some(4), &[]),
                ("p", Some(1), &[("class", "b"), ("lang", "en-GB")]),
            ],
        )
    }

    #[test]
    fn type_class_id_and_combinators() {
        let d = page(false);
        use Match::*;
        for (i, s, want) in [
            (2, "p", Yes),
            (2, "P", No), // XHTML: case-sensitive
            (2, ".a", Yes),
            (2, ".intro", Yes),
            (2, ".A", No),
            (2, "#x", Yes),
            (2, "p.a#x", Yes),
            (2, "p.b", No),
            (5, "div img", Yes),
            (5, "body img", Yes),
            (5, "body > img", No),
            (5, "html div > img", Yes),
            (5, "p img", No),
            (6, "img + span", Yes),
            (6, "img ~ span", Yes),
            (7, "p + p", No),
            (7, "div + p", Yes),
            (7, ".a ~ p", Yes),
            (7, ".a + p", No),
            (5, "*", Yes),
            (5, "div *", Yes),
            (5, "p, img", Yes),
        ] {
            assert_eq!(m(&d, i, s), want, "{s} on #{i}");
        }
    }

    #[test]
    fn html_documents_compare_html_names_without_case() {
        let d = page(true);
        assert_eq!(m(&d, 2, "P"), Match::Yes);
        assert_eq!(m(&d, 5, "IMG[WIDTH]"), Match::Yes);
        // Classes and ids stay case-sensitive.
        assert_eq!(m(&d, 2, ".A"), Match::No);
    }

    #[test]
    fn attribute_operators() {
        use Match::*;
        for (html, s, want) in [
            (false, "[width]", Yes),
            (false, "[height]", No),
            (false, "[width=\"46\"]", Yes),
            (false, "[src$=\".PNG\"]", Yes),
            (false, "[src$=\".png\"]", No),
            (false, "[src$=\".png\" i]", Yes),
            (false, "[src$=\".png\" s]", No),
            (false, "[src^=a]", Yes),
            (false, "[src*=\".\"]", Yes),
            (false, "[src^=\"\"]", No),
            (false, "[src=x q]", Unknown),
            (true, "[src$=\".png\"]", Unknown),
            (true, "[src$=\".PNG\"]", Yes),
            (true, "[src$=\".gif\"]", No),
        ] {
            assert_eq!(m(&page(html), 5, s), want, "{s} (html: {html})");
        }
        let d = page(false);
        assert_eq!(m(&d, 4, "[title~=World]"), Match::Yes);
        assert_eq!(m(&d, 4, "[title~=Wor]"), Match::No);
        assert_eq!(m(&d, 4, "[title~=\"\"]"), Match::No);
        assert_eq!(m(&d, 7, "[lang|=en]"), Match::Yes);
        assert_eq!(m(&d, 7, "[lang|=en-GB]"), Match::Yes);
        assert_eq!(m(&d, 7, "[lang|=e]"), Match::No);
    }

    #[test]
    fn structural_pseudo_classes() {
        let d = page(false);
        use Match::*;
        for (i, s, want) in [
            (0, ":root", Yes),
            (1, ":root", No),
            (6, ":empty", Yes),
            (4, ":empty", No),
            (2, ":first-child", Yes),
            (3, ":first-child", No),
            (7, ":last-child", Yes),
            (5, ":only-child", No),
            (1, ":only-child", Yes),
            (2, "p:first-of-type", Yes),
            (4, "div:first-of-type", Yes),
            (4, ":only-of-type", Yes),
            (7, "p:last-of-type", Yes),
            (3, "p:last-of-type", No),
            (3, ":nth-child(2)", Yes),
            (3, ":nth-child(odd)", No),
            (7, ":nth-child(2n)", Yes),
            (7, ":nth-last-child(1)", Yes),
            (7, ":nth-of-type(3)", Yes),
            (3, ":nth-last-of-type(2)", Yes),
            (7, ":nth-child(2 of p)", No),
            (7, ":nth-child(3 of p)", Yes),
            (7, ":nth-child(1 of .b)", Yes),
            (4, ":nth-child(1 of p)", No),
        ] {
            assert_eq!(m(&d, i, s), want, "{s} on #{i}");
        }
    }

    #[test]
    fn logical_pseudo_classes_and_unknowns() {
        let d = page(false);
        use Match::*;
        for (i, s, want) in [
            (2, "p:not(.b)", Yes),
            (7, "p:not(.b)", No),
            (2, ":is(.a, .b)", Yes),
            (3, ":is(.a, .b)", No),
            (3, ":where(.a, div p, p)", Yes),
            (3, "p:hover", Unknown),
            (3, "div:hover", No), // No wins over Unknown in a compound
            (3, "p:not(:hover)", Unknown),
            (3, "p::before", No),
            (3, "p:hover::before", No),
            (3, "p::part(x)", No),
            (3, "p:has(img)", Unknown),
            (3, "p:lang(en)", Unknown),
            (3, ":is(p, > x)", Yes),
            (5, ":is(p, > x)", Unknown),
            (5, ":hover img", Unknown),
            (5, "a:hover img", No),
            (5, ":hover > img, img", Yes),
            (3, "& p", Unknown),
            (3, ":nth-child(1 of :hover)", Unknown),
        ] {
            assert_eq!(m(&d, i, s), want, "{s} on #{i}");
        }
    }

    #[test]
    fn namespaces_resolve_through_the_map() {
        let d = doc(
            false,
            &[
                ("body", None, &[]),
                ("svg:svg", Some(0), &[]),
                (
                    "svg:image",
                    Some(1),
                    &[("xlink:href", "a.png"), ("width", "10")],
                ),
            ],
        );
        let none = Namespaces::default();
        let declared = Namespaces {
            default: None,
            prefixes: vec![("svg".into(), SVG.into()), ("xl".into(), XLINK.into())],
        };
        let html_default = Namespaces {
            default: Some(HTML_NAMESPACE.into()),
            prefixes: vec![],
        };
        let el = El { doc: &d, i: 2 };
        let at = |s: &str, ns: &Namespaces<'_>| list(s).matches(&el, ns);
        assert_eq!(at("image", &none), Match::Yes);
        assert_eq!(at("svg|image", &declared), Match::Yes);
        assert_eq!(at("svg|image", &none), Match::No, "undeclared prefix");
        assert_eq!(at("*|image", &none), Match::Yes);
        assert_eq!(at("|image", &none), Match::No);
        assert_eq!(at("image", &html_default), Match::No, "default namespace");
        assert_eq!(at("svg *", &html_default), Match::No);
        assert_eq!(at("[xl|href]", &declared), Match::Yes);
        assert_eq!(at("[href]", &declared), Match::No, "no-namespace attribute");
        assert_eq!(at("[*|href]", &declared), Match::Unknown);
        assert_eq!(at("[width]", &none), Match::Yes);
    }

    #[test]
    fn relative_selectors_are_unknown_at_the_top() {
        let d = page(false);
        let css = "p { > img { } }";
        let (sheet, _) = crate::parse_stylesheet(css);
        let crate::BlockItem::Rule(r) = &sheet.rules[0].node.block().unwrap().node[0] else {
            panic!()
        };
        let rel = crate::parse_relative_selector_list(r.node.prelude()).unwrap();
        let el = El { doc: &d, i: 5 };
        assert_eq!(rel.matches(&el, &Namespaces::default()), Match::Unknown);
        let el = El { doc: &d, i: 6 };
        assert_eq!(rel.matches(&el, &Namespaces::default()), Match::No);
    }

    #[test]
    fn three_valued_logic() {
        use Match::*;
        assert_eq!(Yes.and(Unknown), Unknown);
        assert_eq!(No.and(Unknown), No);
        assert_eq!(Yes.or(Unknown), Yes);
        assert_eq!(No.or(Unknown), Unknown);
        assert_eq!(Unknown.not(), Unknown);
    }

    #[test]
    fn a_selector_past_the_step_budget_is_unknown() {
        // A chain of 64 `div`s. A class nobody has, then eleven descendant
        // `*`: every way of placing the `*`s on the ancestors is tried
        // before the class fails, far past the budget, where the answer
        // would be No.
        let spec: Vec<Spec<'_>> = (0..64usize)
            .map(|i| ("div", i.checked_sub(1), &[][..]))
            .collect();
        let d = doc(false, &spec);
        let deepest = El { doc: &d, i: 63 };
        let hard = format!(".nope{}", " *".repeat(11));
        assert_eq!(
            list(&hard).matches(&deepest, &Namespaces::default()),
            Match::Unknown
        );
        // The budget is per call: a cheap selector after it is answered.
        assert_eq!(
            list("div div").matches(&deepest, &Namespaces::default()),
            Match::Yes
        );
        assert_eq!(
            list(".nope div").matches(&deepest, &Namespaces::default()),
            Match::No
        );
    }
}
