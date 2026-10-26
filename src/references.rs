//! What a stylesheet points at outside itself: the URLs it names, and the
//! element names, classes, ids and attributes its selectors name.
//!
//! Both lists are in source order, each item with its span, so a tool can
//! check that a referenced file exists, rename an id and its selectors
//! together, or flag a class no document uses. They report what is written;
//! whether a name or a file is real is the caller's question.

use std::borrow::Cow;

use crate::parser::{BlockItem, ComponentValue, Rule, Stylesheet, holds_style_rules, unprefixed};
use crate::selector::{
    PseudoClass, SelectorList, SimpleSelector, parse_selector_list_lenient,
    parse_selector_list_lenient_before_7129,
};
use crate::span::{Span, Spanned};
use crate::token::Token;

type Values<'a> = [Spanned<ComponentValue<'a>>];

/// One URL a stylesheet names.
#[derive(Debug, Clone, PartialEq)]
pub struct UrlReference<'a> {
    /// The URL as CSS reads it, escapes resolved and not made absolute. For
    /// [`UrlForm::BadUrl`], the token's source text instead, since CSS reads
    /// no URL out of it.
    pub url: Cow<'a, str>,
    /// The whole construct: `url(a.png)`, `url("a.png")`, `"a.css"`.
    pub span: Span,
    pub form: UrlForm,
    pub context: UrlContext<'a>,
}

/// How a URL is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrlForm {
    /// `url(a.png)`, unquoted: one token.
    UrlToken,
    /// `url("a.png")`.
    UrlFunction,
    /// `src("a.png")` (CSS Values 4).
    SrcFunction,
    /// A bare string that is a URL by where it stands: `@import "a.css"`,
    /// `@namespace "…"`, a string option of `image-set()`.
    String,
    /// An unquoted `url( … )` that CSS Syntax cannot read (a space, quote or
    /// parenthesis inside): a `<bad-url-token>`, which a browser never
    /// fetches.
    BadUrl,
}

/// Where a URL stands.
#[derive(Debug, Clone, PartialEq)]
pub enum UrlContext<'a> {
    /// The target of an `@import`.
    Import,
    /// An `@namespace` URI: a name, never fetched.
    Namespace,
    /// The value of a declaration. `at_rule` is the at-rule whose block holds
    /// the declaration directly, when it is a descriptor (`font-face` for
    /// `@font-face { src: url(…) }`); `None` in a style rule.
    Declaration {
        property: Cow<'a, str>,
        at_rule: Option<Cow<'a, str>>,
    },
    /// The prelude of some other rule: the at-rule's name, `None` for a
    /// qualified rule.
    Prelude { at_rule: Option<Cow<'a, str>> },
}

/// Every URL in a stylesheet, in source order, at any depth.
pub fn url_references<'a>(sheet: &Stylesheet<'a>) -> Vec<UrlReference<'a>> {
    let mut out = Vec::new();
    for rule in &sheet.rules {
        rule_urls(rule, &mut out);
    }
    out
}

/// Every URL in a block's contents, as read by
/// [`parse_block_contents`](crate::parse_block_contents): a `style`
/// attribute.
pub fn block_url_references<'a>(items: &[BlockItem<'a>]) -> Vec<UrlReference<'a>> {
    let mut out = Vec::new();
    block_urls(items, None, &mut out);
    out
}

fn rule_urls<'a>(rule: &Spanned<Rule<'a>>, out: &mut Vec<UrlReference<'a>>) {
    let at_rule = match &rule.node {
        Rule::At(a) => Some(&a.name),
        Rule::Qualified(_) => None,
    };
    let context = match at_rule {
        Some(n) if n.eq_ignore_ascii_case("import") => UrlContext::Import,
        Some(n) if n.eq_ignore_ascii_case("namespace") => UrlContext::Namespace,
        _ => UrlContext::Prelude {
            at_rule: at_rule.cloned(),
        },
    };
    let bare_strings = matches!(context, UrlContext::Import | UrlContext::Namespace);
    let prelude = rule.node.prelude();
    if bare_strings {
        // Only the first value is the URL: `@namespace svg "…"` has its
        // prefix first, and anything after `@import "a.css"` is a condition.
        let first = prelude.iter().filter(|v| !is_whitespace(&v.node));
        let at = if matches!(context, UrlContext::Namespace) && prelude_has_prefix(prelude) {
            1
        } else {
            0
        };
        if let Some(v) = first.clone().nth(at)
            && let ComponentValue::Token(Token::String(s)) = &v.node
        {
            out.push(UrlReference {
                url: s.clone(),
                span: v.span,
                form: UrlForm::String,
                context: context.clone(),
            });
        }
    }
    value_urls(prelude, &context, out);
    if let Some(block) = rule.node.block() {
        let holder = match &rule.node {
            Rule::At(a) if !holds_style_rules(unprefixed(&a.name)) => Some(&a.name),
            _ => None,
        };
        block_urls(&block.node, holder, out);
    }
}

/// Whether an `@namespace` prelude starts with a prefix.
fn prelude_has_prefix(prelude: &Values<'_>) -> bool {
    matches!(
        prelude
            .iter()
            .find(|v| !is_whitespace(&v.node))
            .map(|v| &v.node),
        Some(ComponentValue::Token(Token::Ident(_)))
    )
}

fn block_urls<'a>(
    items: &[BlockItem<'a>],
    at_rule: Option<&Cow<'a, str>>,
    out: &mut Vec<UrlReference<'a>>,
) {
    for item in items {
        match item {
            BlockItem::Declaration(d) => {
                let context = UrlContext::Declaration {
                    property: d.node.name.clone(),
                    at_rule: at_rule.cloned(),
                };
                value_urls(&d.node.value, &context, out);
            }
            BlockItem::Rule(r) => rule_urls(r, out),
        }
    }
}

fn value_urls<'a>(values: &Values<'a>, context: &UrlContext<'a>, out: &mut Vec<UrlReference<'a>>) {
    let mut push = |url: Cow<'a, str>, span: Span, form: UrlForm| {
        out.push(UrlReference {
            url,
            span,
            form,
            context: context.clone(),
        });
    };
    // Depth first, in source order: a function's arguments, then what
    // follows it. The flag marks the arguments of `image-set()`, where a
    // bare string is a URL.
    let mut stack = vec![(values, false)];
    while let Some((values, image_set)) = stack.pop() {
        for (i, v) in values.iter().enumerate() {
            match &v.node {
                ComponentValue::Token(Token::Url(s)) => push(s.clone(), v.span, UrlForm::UrlToken),
                ComponentValue::Token(Token::BadUrl(raw)) => {
                    push(raw.clone(), v.span, UrlForm::BadUrl)
                }
                ComponentValue::Token(Token::String(s)) if image_set => {
                    push(s.clone(), v.span, UrlForm::String)
                }
                ComponentValue::Function { name, args } => {
                    let form = if name.eq_ignore_ascii_case("url") {
                        Some(UrlForm::UrlFunction)
                    } else if name.eq_ignore_ascii_case("src") {
                        Some(UrlForm::SrcFunction)
                    } else {
                        None
                    };
                    if let Some(form) = form {
                        if let Some(s) = first_string(args) {
                            push(s, v.span, form);
                        }
                        continue;
                    }
                    stack.push((&values[i + 1..], image_set));
                    stack.push((args, unprefixed(name).eq_ignore_ascii_case("image-set")));
                    break;
                }
                ComponentValue::Block(b) => {
                    stack.push((&values[i + 1..], image_set));
                    stack.push((&b.values, false));
                    break;
                }
                _ => {}
            }
        }
    }
}

fn first_string<'a>(args: &Values<'a>) -> Option<Cow<'a, str>> {
    match args
        .iter()
        .find(|a| !is_whitespace(&a.node))
        .map(|a| &a.node)
    {
        Some(ComponentValue::Token(Token::String(s))) => Some(s.clone()),
        _ => None,
    }
}

fn is_whitespace(v: &ComponentValue<'_>) -> bool {
    matches!(v, ComponentValue::Token(Token::Whitespace))
}

/// One simple selector that names something in the document: an element
/// type, a class, an id, an attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorReference<'a> {
    /// A [`Type`](SimpleSelector::Type), [`Class`](SimpleSelector::Class),
    /// [`Id`](SimpleSelector::Id) or
    /// [`Attribute`](SimpleSelector::Attribute), its value with escapes
    /// resolved (`#\30 9` is the id `09`). The span covers it as written,
    /// so slicing the stylesheet with it gives the spelling.
    pub selector: Spanned<SimpleSelector<'a>>,
    /// The style rule it is in.
    pub rule: Span,
    /// Inside the argument of `:not()`, `:is()`, `:where()`, `:has()` or
    /// `:nth-child(… of …)`, at any depth.
    pub in_argument: bool,
    /// The rule sits at the top of the stylesheet, not inside an at-rule or
    /// another style rule.
    pub top_level: bool,
    /// The rule's selector list has an error, so a browser drops the whole
    /// rule: `#09 { }` names the id `09` but never styles it. A tool that
    /// renames ids leaves such a reference alone.
    pub dropped: bool,
    /// Whether it hangs on w3c/csswg-drafts#7129, which took some code
    /// points at or above U+0080 out of the ident code points (U+00A0, `‣`):
    /// the reference is there in only one of the two readings (`#◰` names
    /// the id `◰` only before #7129), or its rule is dropped in only one.
    /// [`dropped`](Self::dropped) is then false.
    pub narrowed_ident: bool,
}

/// Every type, class, id and attribute selector in a stylesheet's style
/// rules, at any depth: rules inside `@media`, `@supports` and the like,
/// nested rules, pseudo-class arguments. In source order.
///
/// A selector with a structure error ([`InvalidSelectorStructure`]) still
/// names what it names and is included; one with a token error
/// ([`InvalidSelector`]) is not, since it cannot be read. A selector that
/// reads otherwise before w3c/csswg-drafts#7129 gives the references of both
/// readings, those of only one marked
/// [`narrowed_ident`](SelectorReference::narrowed_ident).
///
/// [`InvalidSelectorStructure`]: crate::SyntaxErrorKind::InvalidSelectorStructure
/// [`InvalidSelector`]: crate::SyntaxErrorKind::InvalidSelector
pub fn selector_references<'a>(sheet: &Stylesheet<'a>) -> Vec<SelectorReference<'a>> {
    /// A block being walked: its rules left, whether they are nested in a
    /// style rule, and whether a rule around them is dropped: as it reads,
    /// and before csswg-drafts#7129.
    struct Frame<'s, 'a> {
        items: &'s [BlockItem<'a>],
        nested: bool,
        dropped: (bool, bool),
    }
    let mut out = Vec::new();
    let mut stack: Vec<Frame<'_, 'a>> = Vec::new();
    let mut top = sheet.rules.iter();
    // Rules at the top, then any block's rules, depth first in source order.
    loop {
        let next = match stack.last_mut() {
            Some(frame) => match frame.items.split_first() {
                Some((BlockItem::Rule(r), rest)) => {
                    frame.items = rest;
                    Some((r, frame.nested, frame.dropped))
                }
                Some((BlockItem::Declaration(_), rest)) => {
                    frame.items = rest;
                    continue;
                }
                None => {
                    stack.pop();
                    continue;
                }
            },
            None => top.next().map(|r| (r, false, (false, false))),
        };
        let Some((rule, nested, (outer_now, outer_before))) = next else {
            break;
        };
        let top_level = stack.is_empty();
        match &rule.node {
            Rule::Qualified(q) => {
                let (list, error) = parse_selector_list_lenient(&q.prelude, nested);
                let before = parse_selector_list_lenient_before_7129(&q.prelude, nested);
                // A dropped rule takes the rules nested in it along.
                let now = outer_now || error;
                let then = outer_before || before.as_ref().map_or(error, |(_, e)| *e);
                let at = Place {
                    rule: rule.span,
                    top_level,
                    dropped: now && then,
                    narrowed: now != then,
                };
                match before {
                    None => walk_list(&list, &at, false, &mut out),
                    Some((old, _)) => {
                        let mut today = Vec::new();
                        walk_list(&list, &at, false, &mut today);
                        let mut then = Vec::new();
                        walk_list(&old, &at, false, &mut then);
                        both_readings(today, then, &mut out);
                    }
                }
                stack.push(Frame {
                    items: &q.block.node,
                    nested: true,
                    dropped: (now, then),
                });
            }
            Rule::At(a) => {
                if let Some(block) = &a.block
                    && holds_style_rules(unprefixed(&a.name))
                {
                    stack.push(Frame {
                        items: &block.node,
                        nested,
                        dropped: (outer_now, outer_before),
                    });
                }
            }
        }
    }
    out
}

/// The references of one rule as it reads and as it read before
/// csswg-drafts#7129, merged in source order; one found in only one reading
/// is marked.
fn both_readings<'a>(
    today: Vec<SelectorReference<'a>>,
    then: Vec<SelectorReference<'a>>,
    out: &mut Vec<SelectorReference<'a>>,
) {
    let key = |r: &SelectorReference<'_>| (r.selector.span.start, r.selector.span.end);
    let seen: std::collections::HashMap<_, _> =
        today.iter().map(|r| (key(r), &r.selector)).collect();
    let kept: std::collections::HashSet<_> = then
        .iter()
        .filter(|r| seen.get(&key(r)) == Some(&&r.selector))
        .map(key)
        .collect();
    let start = out.len();
    for mut r in then {
        r.narrowed_ident |= !kept.contains(&key(&r));
        out.push(r);
    }
    for mut r in today {
        if !kept.contains(&key(&r)) {
            r.narrowed_ident = true;
            out.push(r);
        }
    }
    out[start..].sort_by_key(|r| r.selector.span.start);
}

/// The rule a selector list belongs to.
struct Place {
    rule: Span,
    top_level: bool,
    /// In both readings, before csswg-drafts#7129 and after.
    dropped: bool,
    /// In only one of them.
    narrowed: bool,
}

fn walk_list<'a>(
    list: &SelectorList<'a>,
    at: &Place,
    in_argument: bool,
    out: &mut Vec<SelectorReference<'a>>,
) {
    for s in list
        .selectors
        .iter()
        .flat_map(|c| &c.node.compounds)
        .flat_map(|c| &c.node.simples)
    {
        match &s.node {
            SimpleSelector::Type(_)
            | SimpleSelector::Class(_)
            | SimpleSelector::Id(_)
            | SimpleSelector::Attribute(_) => out.push(SelectorReference {
                selector: s.clone(),
                rule: at.rule,
                in_argument,
                top_level: at.top_level,
                dropped: at.dropped,
                narrowed_ident: at.narrowed,
            }),
            SimpleSelector::PseudoClass(p) => {
                let arg = match p {
                    PseudoClass::Not(l)
                    | PseudoClass::Has(l)
                    | PseudoClass::Is { selectors: l, .. }
                    | PseudoClass::Where { selectors: l, .. }
                    | PseudoClass::NthChild { of: Some(l), .. }
                    | PseudoClass::NthLastChild { of: Some(l), .. } => Some(l),
                    _ => None,
                };
                if let Some(l) = arg {
                    walk_list(l, at, true, out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::AttributeOperator;

    fn urls(css: &str) -> Vec<(String, UrlForm, String)> {
        let sheet = crate::parse_stylesheet(css).0;
        url_references(&sheet)
            .into_iter()
            .map(|u| {
                let ctx = match &u.context {
                    UrlContext::Import => "import".to_string(),
                    UrlContext::Namespace => "namespace".to_string(),
                    UrlContext::Declaration { property, at_rule } => match at_rule {
                        Some(a) => format!("@{a} {property}"),
                        None => property.to_string(),
                    },
                    UrlContext::Prelude { at_rule } => {
                        format!("prelude {}", at_rule.as_deref().unwrap_or("-"))
                    }
                };
                (u.url.into_owned(), u.form, ctx)
            })
            .collect()
    }

    #[test]
    fn every_url_with_its_context() {
        use UrlForm::*;
        let css = r#"@import "a.css" screen;
@import url(b.css);
@namespace svg "http://www.w3.org/2000/svg";
@namespace url(http://www.w3.org/1999/xhtml);
@font-face { font-family: X; src: url("x.woff2") format("woff2"), local(X), url(x.ttf) }
p { background: image-set("p1.png" 1x, url(p2.png) 2x) no-repeat, url( "p3.png" ) }
@media print { q { --icon: url(\61 .svg) } }
div { cursor: src("c.cur"), auto; & span { content: url(n.png) } }"#;
        let s = |u: &str, f, c: &str| (u.to_string(), f, c.to_string());
        assert_eq!(
            urls(css),
            [
                s("a.css", String, "import"),
                s("b.css", UrlToken, "import"),
                s("http://www.w3.org/2000/svg", String, "namespace"),
                s("http://www.w3.org/1999/xhtml", UrlToken, "namespace"),
                s("x.woff2", UrlFunction, "@font-face src"),
                s("x.ttf", UrlToken, "@font-face src"),
                s("p1.png", String, "background"),
                s("p2.png", UrlToken, "background"),
                s("p3.png", UrlFunction, "background"),
                s("a.svg", UrlToken, "--icon"),
                s("c.cur", SrcFunction, "cursor"),
                s("n.png", UrlToken, "content"),
            ]
        );
    }

    #[test]
    fn spans_cover_the_construct_and_bad_urls_keep_their_text() {
        let css = "p { background: url( \"a.png\" ) } q { background: url(a b.png) }\
                   r { background: image-set(url(r1.png) 1x, \"r2.png\" 2x) }";
        let sheet = crate::parse_stylesheet(css).0;
        let all = url_references(&sheet);
        assert_eq!(all[0].span.slice(css), "url( \"a.png\" )");
        assert_eq!(all[1].form, UrlForm::BadUrl);
        assert_eq!(all[1].url, "url(a b.png)");
        assert_eq!(all[1].span.slice(css), "url(a b.png)");
        // Source order inside `image-set()` too.
        assert_eq!(
            (all[2].url.as_ref(), all[3].url.as_ref()),
            ("r1.png", "r2.png")
        );
        // A string that is not a URL by where it stands is not one.
        assert!(urls("p { content: \"a.png\"; font-family: \"x.ttf\" }").is_empty());
        assert!(urls("@font-face { src: local(\"x.ttf\") }").is_empty());
        // A `style` attribute.
        let inline = crate::parse_block_contents("background: url(i.png)").0;
        assert_eq!(block_url_references(&inline)[0].url, "i.png");
    }

    fn refs(css: &str) -> Vec<(String, bool, bool)> {
        let sheet = crate::parse_stylesheet(css).0;
        selector_references(&sheet)
            .into_iter()
            .map(|r| {
                (
                    r.selector.span.slice(css).to_string(),
                    r.in_argument,
                    r.top_level,
                )
            })
            .collect()
    }

    #[test]
    fn every_name_a_selector_uses() {
        let s = |t: &str, arg, top| (t.to_string(), arg, top);
        assert_eq!(
            refs("h4.note > em#x[lang|=en]:hover::before, svg|circle { }"),
            [
                s("h4", false, true),
                s(".note", false, true),
                s("em", false, true),
                s("#x", false, true),
                s("[lang|=en]", false, true),
                s("svg|circle", false, true),
            ]
        );
        assert_eq!(
            refs("@media print { p:not(.a, :is(#b)) { } }"),
            [
                s("p", false, false),
                s(".a", true, false),
                s("#b", true, false),
            ]
        );
        assert_eq!(
            refs("div { & > .k { } img { } } * { }"),
            [
                s("div", false, true),
                s(".k", false, false),
                s("img", false, false)
            ]
        );
        // A structure error still names; a token error does not.
        assert_eq!(
            refs("#1a h4a, . x { }"),
            [s("#1a", false, true), s("h4a", false, true)]
        );
        // A dropped rule takes its nested rules along.
        let sheet = crate::parse_stylesheet("#09 { .in { } @media print { .deep { } } } #a { }").0;
        let dropped: Vec<bool> = selector_references(&sheet)
            .iter()
            .map(|r| r.dropped)
            .collect();
        assert_eq!(dropped, [true, true, true, false]);
        // Nothing in `@font-face` or `@keyframes` is a selector.
        assert!(refs("@keyframes k { from { } } @font-face { }").is_empty());
    }

    #[test]
    fn ids_come_resolved_with_their_spelling() {
        let css = "#\\30 9, [id^=\"bookmark\"], a:has([id=x i]) { }";
        let sheet = crate::parse_stylesheet(css).0;
        let all = selector_references(&sheet);
        let SimpleSelector::Id(id) = &all[0].selector.node else {
            panic!("{:?}", all[0]);
        };
        assert_eq!(
            (id.as_ref(), all[0].selector.span.slice(css)),
            ("09", "#\\30 9")
        );
        let SimpleSelector::Attribute(a) = &all[1].selector.node else {
            panic!("{:?}", all[1]);
        };
        let v = a.value.as_ref().unwrap();
        assert_eq!(
            (a.name.local.node.as_ref(), v.operator, v.value.as_ref()),
            ("id", AttributeOperator::Prefix, "bookmark")
        );
        assert!(all[3].in_argument);
        assert_eq!(all[3].selector.span.slice(css), "[id=x i]");
    }

    /// Before csswg-drafts#7129 these code points were name code points; a
    /// reference found in only one reading, or in a rule dropped in only
    /// one, is marked.
    #[test]
    fn narrowed_ident_references_are_marked() {
        let marks = |css: &str| -> Vec<(String, bool, bool)> {
            let sheet = crate::parse_stylesheet(css).0;
            selector_references(&sheet)
                .into_iter()
                .map(|r| {
                    let spelling = r.selector.span.slice(css).to_string();
                    (spelling, r.narrowed_ident, r.dropped)
                })
                .collect()
        };
        let m = |t: &str, narrowed, dropped| (t.to_string(), narrowed, dropped);
        // `#◰` is an id only in the old reading; the rule is dropped in the
        // new one, so `p` hangs on #7129 too.
        assert_eq!(
            marks("p, #◰ { } .k { }"),
            [
                m("p", true, false),
                m("#◰", true, false),
                m(".k", false, false)
            ]
        );
        let sheet = crate::parse_stylesheet("#◰ { }").0;
        let SimpleSelector::Id(id) = &selector_references(&sheet)[0].selector.node else {
            panic!();
        };
        assert_eq!(id.as_ref(), "◰");
        // A forgiving list keeps the rule either way: only `.a‣b` is marked.
        assert_eq!(
            marks("p:is(.a\u{A0}b, .c) { }"),
            [
                m("p", false, false),
                m(".a\u{A0}b", true, false),
                m(".c", false, false)
            ]
        );
        // Nested rules follow their parent's two readings.
        assert_eq!(
            marks(".‣ { .in { } }"),
            [m(".‣", true, false), m(".in", true, false)]
        );
        // Dropped in both readings: dropped, not marked.
        assert_eq!(marks(".‣ . b, p { }"), [m("p", false, true)]);
    }
}
