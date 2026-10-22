//! Whether a declaration's value fits its property's grammar, or its
//! descriptor's ([`check_descriptor`]).
//!
//! [`check_value`] answers [`Valid`](ValueCheck::Valid),
//! [`Invalid`](ValueCheck::Invalid) or [`Unknown`](ValueCheck::Unknown), and
//! for Unknown says why. The grammars are mdn-data's
//! ([`MDN_DATA_VERSION`]), in CSS Values 4's value definition syntax; the
//! base types (`<length>`, `<number>`, `<ident>`, `<url>`, …) and the math
//! functions (`calc()`, `min()`, …) are read here.
//!
//! **Invalid only when certain.** A browser drops an invalid declaration, so
//! calling a valid one invalid is the worst mistake this can make. The value
//! is matched twice: strictly, where every piece must be read and fit, and
//! leniently, where a type the grammars do not define and a function no
//! grammar names fit anything. Invalid means even the lenient match fails.
//! Before matching, these are Unknown:
//!
//! - a property not in the table behind
//!   [`DiagnosticKind::UnknownProperty`](crate::DiagnosticKind::UnknownProperty),
//!   or one without a grammar, or a descriptor its at-rule does not define;
//!   a vendor-prefixed property;
//! - a value using `var()`, `env()`, `attr()`, `if()` or `inherit()`, whose
//!   meaning is known only when it is computed;
//! - a value holding a bad-url or bad-string token, already a syntax error;
//! - an empty value.
//!
//! After matching, a failed value is Unknown too when it holds
//!
//! - a keyword whose support is up to the UA: one another specification
//!   defines for the property (`display: oeb-page-head`, EPUB 2), or one a
//!   CSS specification leaves optional (`writing-mode: tb-rl`, SVG 1.1's;
//!   `outline-color: invert`);
//! - a vendor-prefixed keyword or function: engines accept their own.
//!
//! A custom property (`--*`) takes any value, and the CSS-wide keywords
//! (`inherit`, `initial`, `unset`, `revert`, `revert-layer`) fit every
//! property.
//!
//! What remains possible is a keyword or function newer than the grammars:
//! mdn-data's version is there so such a case can be traced.

mod grammar;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::OnceLock;

use crate::known_properties::is_known_property;
use crate::parser::{BlockKind, ComponentValue, Declaration, unprefixed};
use crate::span::Spanned;
use crate::token::{NumericType, Token};
use crate::value_syntaxes::{PROPERTY_SYNTAXES, TYPE_SYNTAXES, UNKNOWN_KEYWORDS};
use grammar::{Node, Range};

/// The mdn-data release the grammars come from.
pub const MDN_DATA_VERSION: &str = crate::value_syntaxes::MDN_DATA_VERSION;

/// The most steps one [`check_value`] takes; past it the answer is
/// [`UnknownValue::TooComplex`]. A step is one grammar node tried at one
/// place in the value.
pub const MAX_VALUE_STEPS: u32 = 1 << 16;

/// Steps per component value, nested ones counted: the budget grows with
/// the value, so a short value cannot take long. On the shelf's 88,442
/// declarations the most any value took was 230 per component value.
pub(crate) const STEPS_PER_VALUE: u32 = 1024;

/// The budget for one value: [`STEPS_PER_VALUE`] per component value, at
/// most [`MAX_VALUE_STEPS`].
fn step_budget(items: &[&ComponentValue<'_>], depth: usize) -> u32 {
    fn count(items: &[&ComponentValue<'_>], depth: usize) -> u32 {
        let mut n = 0u32;
        for v in items {
            n = n.saturating_add(1);
            if depth > 0 {
                match v {
                    ComponentValue::Function { args, .. } => {
                        n = n.saturating_add(count(&significant(args), depth - 1));
                    }
                    ComponentValue::Block(b) => {
                        n = n.saturating_add(count(&significant(&b.values), depth - 1));
                    }
                    ComponentValue::Token(_) => {}
                }
            }
        }
        n
    }
    count(items, depth)
        .saturating_add(1)
        .saturating_mul(STEPS_PER_VALUE)
        .min(MAX_VALUE_STEPS)
}

/// How deep functions and blocks in a value are read.
const MAX_VALUE_DEPTH: usize = 32;

/// The answer about one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueCheck {
    Valid,
    /// It cannot fit the property's grammar: a browser drops the
    /// declaration.
    Invalid,
    Unknown(UnknownValue),
}

/// Why a value is neither certainly valid nor certainly invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnknownValue {
    /// The property is not one this crate knows, or the descriptor not one
    /// its at-rule defines.
    UnknownProperty,
    /// The property is vendor-prefixed.
    PrefixedProperty,
    /// The property is known but has no grammar here.
    NoGrammar,
    /// The value is empty.
    Empty,
    /// The value uses `var()`, `env()`, `attr()`, `if()` or `inherit()`.
    Substitution,
    /// The value holds a bad-url or bad-string token.
    BadToken,
    /// It fits no grammar, but holds a keyword whose support is up to the
    /// UA: another specification defines it for this property, or a CSS
    /// specification leaves it optional.
    OtherSpecification,
    /// It fits no grammar, but holds a vendor-prefixed keyword or function.
    PrefixedValue,
    /// It fits only where a piece this crate does not read stands: a type
    /// the grammars leave undefined, a function no grammar names.
    Unread,
    /// Matching took more than [`MAX_VALUE_STEPS`].
    TooComplex,
}

/// Check a declaration's value against its property.
pub fn check_declaration(d: &Declaration<'_>) -> ValueCheck {
    check_value(&d.name, &d.value)
}

/// Check a value, as the parser gives it (without `!important`), against
/// the grammar of `property`.
pub fn check_value(property: &str, value: &[Spanned<ComponentValue<'_>>]) -> ValueCheck {
    if property.starts_with("--") {
        return ValueCheck::Valid;
    }
    let property: std::borrow::Cow<'_, str> = if property.bytes().any(|b| b.is_ascii_uppercase()) {
        property.to_ascii_lowercase().into()
    } else {
        property.into()
    };
    if property.starts_with('-') {
        return ValueCheck::Unknown(UnknownValue::PrefixedProperty);
    }
    if !is_known_property(&property) {
        return ValueCheck::Unknown(UnknownValue::UnknownProperty);
    }
    let items = significant(value);
    cached(&property, &items)
}

/// Check a declaration inside an at-rule (`@font-face`, `@counter-style`,
/// `@page`, …) against that at-rule's grammar for the descriptor `name`.
/// `at_rule` is the at-rule's name without the `@`; names are ASCII
/// case-insensitive, and a vendor prefix on the at-rule is ignored.
///
/// A descriptor the at-rule does not define is
/// [`UnknownProperty`](UnknownValue::UnknownProperty), except in `@page`,
/// which also takes properties: those are checked as properties.
///
/// `@font-face`'s `src` is a list whose entries a browser reads one by one,
/// skipping those it cannot parse (CSS Fonts 4 §4.3), so it is Invalid
/// only when no entry fits.
pub fn check_descriptor(
    at_rule: &str,
    name: &str,
    value: &[Spanned<ComponentValue<'_>>],
) -> ValueCheck {
    if name.starts_with('-') {
        return ValueCheck::Unknown(UnknownValue::PrefixedProperty);
    }
    let at_rule = unprefixed(at_rule);
    let mut key = String::with_capacity(at_rule.len() + name.len() + 2);
    key.push('@');
    key.push_str(at_rule);
    key.push('/');
    key.push_str(name);
    key.make_ascii_lowercase();
    if !grammars().properties.contains_key(key.as_str()) {
        return if at_rule.eq_ignore_ascii_case("page") {
            check_value(name, value)
        } else {
            ValueCheck::Unknown(UnknownValue::UnknownProperty)
        };
    }
    let items = significant(value);
    if key != "@font-face/src" {
        return cached(&key, &items);
    }
    let mut unknown = None;
    for entry in items.split(|v| matches!(v, ComponentValue::Token(Token::Comma))) {
        match cached(&key, entry) {
            ValueCheck::Valid => return ValueCheck::Valid,
            ValueCheck::Unknown(why) => unknown = unknown.or(Some(why)),
            ValueCheck::Invalid => {}
        }
    }
    match unknown {
        Some(why) => ValueCheck::Unknown(why),
        None => ValueCheck::Invalid,
    }
}

/// The most verdicts [`cached`] keeps per thread; past it, it starts over.
const CACHE_ENTRIES: usize = 8192;

/// Remembered verdicts, and a buffer the next key is written into.
#[derive(Default)]
struct Cache {
    verdicts: HashMap<Box<[u8]>, ValueCheck, Quick>,
    key: Vec<u8>,
}

thread_local! {
    static CACHE: std::cell::RefCell<Cache> = std::cell::RefCell::new(Cache::default());
}

/// [`uncached`], remembered. Stylesheets repeat themselves: the shelf's
/// 88,442 declarations hold about 5,900 distinct values. The key is the
/// property and everything matching reads of the value (see [`encode`]),
/// so a remembered verdict is the one matching would give.
fn cached(property: &str, items: &[&ComponentValue<'_>]) -> ValueCheck {
    let hit = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let Cache { verdicts, key } = &mut *c;
        key.clear();
        key.extend_from_slice(property.as_bytes());
        key.push(0);
        if !encode(items.iter().copied(), key, MAX_VALUE_DEPTH) {
            return Err(false);
        }
        verdicts.get(key.as_slice()).copied().ok_or(true)
    });
    let remember = match hit {
        Ok(v) => return v,
        Err(remember) => remember,
    };
    let verdict = uncached(property, items);
    if remember {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            let Cache { verdicts, key } = &mut *c;
            if verdicts.len() >= CACHE_ENTRIES {
                verdicts.clear();
            }
            verdicts.insert(key.as_slice().into(), verdict);
        });
    }
    verdict
}

/// Write what matching reads of `items` into `out`: each value's kind and
/// content, functions and blocks with their contents, each list closed by an
/// end mark; no spans and no whitespace (which matching skips too). Strings
/// carry their length, so no content can pass for a mark. `false` past
/// `depth`, where the value is not cached.
fn encode<'v, 'a: 'v>(
    items: impl Iterator<Item = &'v ComponentValue<'a>>,
    out: &mut Vec<u8>,
    depth: usize,
) -> bool {
    fn text(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    fn number(out: &mut Vec<u8>, value: f64, num_type: NumericType) {
        out.extend_from_slice(&value.to_bits().to_le_bytes());
        out.push(u8::from(num_type == NumericType::Integer));
    }
    fn significant_iter<'v, 'a>(
        values: &'v [Spanned<ComponentValue<'a>>],
    ) -> impl Iterator<Item = &'v ComponentValue<'a>> {
        values
            .iter()
            .map(|v| &v.node)
            .filter(|v| !matches!(v, ComponentValue::Token(Token::Whitespace)))
    }
    if depth == 0 {
        return false;
    }
    for v in items {
        match v {
            ComponentValue::Function { name, args } => {
                out.push(1);
                text(out, name);
                if !encode(significant_iter(args), out, depth - 1) {
                    return false;
                }
            }
            ComponentValue::Block(b) => {
                out.push(2);
                out.push(b.kind as u8);
                if !encode(significant_iter(&b.values), out, depth - 1) {
                    return false;
                }
            }
            ComponentValue::Token(t) => match t {
                Token::Ident(s) => {
                    out.push(3);
                    text(out, s);
                }
                Token::Function(s) => {
                    out.push(4);
                    text(out, s);
                }
                Token::AtKeyword(s) => {
                    out.push(5);
                    text(out, s);
                }
                Token::Hash { value, is_id } => {
                    out.push(6);
                    out.push(u8::from(*is_id));
                    text(out, value);
                }
                Token::String(s) => {
                    out.push(7);
                    text(out, s);
                }
                Token::Url(s) => {
                    out.push(8);
                    text(out, s);
                }
                Token::BadUrl(s) => {
                    out.push(9);
                    text(out, s);
                }
                Token::Delim(c) => {
                    out.push(10);
                    out.extend_from_slice(&u32::from(*c).to_le_bytes());
                }
                Token::Number {
                    value, num_type, ..
                } => {
                    out.push(11);
                    number(out, *value, *num_type);
                }
                Token::Percentage { value, .. } => {
                    out.push(12);
                    out.extend_from_slice(&value.to_bits().to_le_bytes());
                }
                Token::Dimension {
                    value,
                    num_type,
                    unit,
                    ..
                } => {
                    out.push(13);
                    number(out, *value, *num_type);
                    text(out, unit);
                }
                Token::UnicodeRange { start, end } => {
                    out.push(14);
                    out.extend_from_slice(&start.to_le_bytes());
                    out.extend_from_slice(&end.to_le_bytes());
                }
                Token::BadString => out.push(15),
                Token::Whitespace => out.push(16),
                Token::Cdo => out.push(17),
                Token::Cdc => out.push(18),
                Token::Colon => out.push(19),
                Token::Semicolon => out.push(20),
                Token::Comma => out.push(21),
                Token::LeftSquare => out.push(22),
                Token::RightSquare => out.push(23),
                Token::LeftParen => out.push(24),
                Token::RightParen => out.push(25),
                Token::LeftCurly => out.push(26),
                Token::RightCurly => out.push(27),
            },
        }
    }
    // The end of this list, so `f(a) b` and `f(a b)` differ.
    out.push(0xFF);
    true
}

/// [`check_value`] past the property checks, on the value's significant
/// component values.
fn uncached(property: &str, items: &[&ComponentValue<'_>]) -> ValueCheck {
    let Some(first) = items.first() else {
        return ValueCheck::Unknown(UnknownValue::Empty);
    };
    if let Some(reason) = unknown_before_matching(items, MAX_VALUE_DEPTH) {
        return ValueCheck::Unknown(reason);
    }
    if let (1, ComponentValue::Token(Token::Ident(word))) = (items.len(), first)
        && CSS_WIDE.iter().any(|k| word.eq_ignore_ascii_case(k))
    {
        return ValueCheck::Valid;
    }
    let g = grammars();
    let Some(Some(node)) = g.properties.get(property) else {
        return ValueCheck::Unknown(UnknownValue::NoGrammar);
    };
    let budget = step_budget(items, MAX_VALUE_DEPTH);
    let strict = Matcher::new(g, false, budget);
    let fits = strict.full(node, items, 0);
    if strict.exhausted.get() {
        return ValueCheck::Unknown(UnknownValue::TooComplex);
    }
    if fits {
        return ValueCheck::Valid;
    }
    let lenient = Matcher::new(g, true, budget);
    let fits = lenient.full(node, items, 0);
    if lenient.exhausted.get() {
        return ValueCheck::Unknown(UnknownValue::TooComplex);
    }
    if fits {
        ValueCheck::Unknown(UnknownValue::Unread)
    } else if has_optional_keyword(property, items) {
        ValueCheck::Unknown(UnknownValue::OtherSpecification)
    } else if has_prefixed(items, MAX_VALUE_DEPTH) {
        ValueCheck::Unknown(UnknownValue::PrefixedValue)
    } else {
        ValueCheck::Invalid
    }
}

const CSS_WIDE: [&str; 5] = ["inherit", "initial", "unset", "revert", "revert-layer"];

/// Functions whose result is known only at computed-value time.
const SUBSTITUTION: [&str; 5] = ["var", "env", "attr", "if", "inherit"];

/// Math functions (CSS Values 4 §10), which stand for a number, dimension or
/// percentage wherever one may go.
const MATH: [&str; 22] = [
    "calc", "min", "max", "clamp", "round", "mod", "rem", "sin", "cos", "tan", "asin", "acos",
    "atan", "atan2", "pow", "sqrt", "hypot", "log", "exp", "abs", "sign", "progress",
];

/// The component values that count: whitespace dropped.
fn significant<'v, 'a>(values: &'v [Spanned<ComponentValue<'a>>]) -> Vec<&'v ComponentValue<'a>> {
    values
        .iter()
        .map(|v| &v.node)
        .filter(|v| !matches!(v, ComponentValue::Token(Token::Whitespace)))
        .collect()
}

fn unknown_before_matching(items: &[&ComponentValue<'_>], depth: usize) -> Option<UnknownValue> {
    if depth == 0 {
        return Some(UnknownValue::TooComplex);
    }
    for v in items {
        match v {
            ComponentValue::Token(Token::BadUrl(_) | Token::BadString) => {
                return Some(UnknownValue::BadToken);
            }
            ComponentValue::Function { name, args } => {
                if SUBSTITUTION.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                    return Some(UnknownValue::Substitution);
                }
                if let Some(r) = unknown_before_matching(&significant(args), depth - 1) {
                    return Some(r);
                }
            }
            ComponentValue::Block(b) => {
                if let Some(r) = unknown_before_matching(&significant(&b.values), depth - 1) {
                    return Some(r);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether a keyword whose support is up to the UA for `property` stands
/// among `items` (not inside functions or blocks).
fn has_optional_keyword(property: &str, items: &[&ComponentValue<'_>]) -> bool {
    let Some((_, keywords)) = UNKNOWN_KEYWORDS.iter().find(|(p, _)| *p == property) else {
        return false;
    };
    items.iter().any(|v| {
        matches!(v, ComponentValue::Token(Token::Ident(w))
            if keywords.iter().any(|k| w.eq_ignore_ascii_case(k)))
    })
}

fn has_prefixed(items: &[&ComponentValue<'_>], depth: usize) -> bool {
    let prefixed = |s: &str| s.starts_with('-') && !s.starts_with("--") && unprefixed(s) != s;
    depth > 0
        && items.iter().any(|v| match v {
            ComponentValue::Token(Token::Ident(s)) => prefixed(s),
            ComponentValue::Function { name, args } => {
                prefixed(name) || has_prefixed(&significant(args), depth - 1)
            }
            ComponentValue::Block(b) => has_prefixed(&significant(&b.values), depth - 1),
            _ => false,
        })
}

/// Every grammar, read once.
struct Grammars {
    properties: HashMap<&'static str, Option<Node>, Quick>,
    types: HashMap<&'static str, Option<Node>, Quick>,
    /// Every function a grammar names, and the math functions.
    functions: HashSet<String, Quick>,
}

fn grammars() -> &'static Grammars {
    static G: OnceLock<Grammars> = OnceLock::new();
    G.get_or_init(|| {
        let read = |list: &'static [(&'static str, &'static str)]| {
            list.iter()
                .map(|(n, s)| (*n, grammar::parse(s)))
                .collect::<HashMap<_, _, Quick>>()
        };
        let properties = read(PROPERTY_SYNTAXES);
        let types = read(TYPE_SYNTAXES);
        let mut functions: HashSet<String, Quick> = MATH.iter().map(|m| m.to_string()).collect();
        for name in TYPE_SYNTAXES
            .iter()
            .filter_map(|(n, _)| n.strip_suffix("()"))
        {
            functions.insert(name.to_ascii_lowercase());
        }
        for node in properties.values().chain(types.values()).flatten() {
            collect_functions(node, &mut functions);
        }
        Grammars {
            properties,
            types,
            functions,
        }
    })
}

fn collect_functions(node: &Node, out: &mut HashSet<String, Quick>) {
    match node {
        Node::Function(name, inner) => {
            out.insert(name.to_ascii_lowercase());
            collect_functions(inner, out);
        }
        Node::SquareBlock(n) | Node::ParenBlock(n) | Node::NonEmpty(n) => collect_functions(n, out),
        Node::Repeat { node, .. } => collect_functions(node, out),
        Node::Seq(v) | Node::AllOf(v) | Node::AnyOf(v) | Node::OneOf(v) => {
            for n in v {
                collect_functions(n, out);
            }
        }
        Node::Keyword(_) | Node::Literal(_) | Node::Type { .. } | Node::Property(_) => {}
    }
}

/// A multiply-rotate hash for the matcher's own tables, whose keys are
/// grammar-node addresses and positions. The tables are bounded by the step
/// budget, so they need no protection against chosen collisions, and
/// SipHash was half the time of checking a stylesheet.
#[derive(Default)]
struct QuickHasher(u64);

impl Hasher for QuickHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.write_u64(u64::from_le_bytes(*w));
        }
        for &b in rest {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

type Quick = BuildHasherDefault<QuickHasher>;

/// The most alternatives `&&` and `||` take (one bit each).
const MAX_ANY_ORDER: usize = 32;

struct Matcher<'g> {
    g: &'g Grammars,
    lenient: bool,
    steps: Cell<u32>,
    exhausted: Cell<bool>,
}

/// One list of component values being matched, with what is known so far:
/// for a grammar node at a position, the positions it can end at.
struct List<'l, 'v, 'a> {
    items: &'l [&'v ComponentValue<'a>],
    memo: HashMap<(usize, usize), Vec<usize>, Quick>,
    depth: usize,
}

impl<'g> Matcher<'g> {
    fn new(g: &'g Grammars, lenient: bool, budget: u32) -> Self {
        Matcher {
            g,
            lenient,
            steps: Cell::new(budget),
            exhausted: Cell::new(false),
        }
    }

    /// Whether `node` matches all of `items`.
    fn full(&self, node: &Node, items: &[&ComponentValue<'_>], depth: usize) -> bool {
        if depth > MAX_VALUE_DEPTH {
            self.exhausted.set(true);
            return false;
        }
        let mut list = List {
            items,
            memo: HashMap::with_capacity_and_hasher(64, Quick::default()),
            depth,
        };
        self.ends(&mut list, node, 0).contains(&items.len())
    }

    /// The positions `node` can end at, starting at `pos`; sorted, no
    /// repeats.
    fn ends(&self, l: &mut List<'_, '_, '_>, node: &Node, pos: usize) -> Vec<usize> {
        // A keyword or a literal is one comparison: cheaper than the memo,
        // and not worth a step. `<color>` alone offers 148 keywords.
        if matches!(node, Node::Keyword(_) | Node::Literal(_)) {
            return self.compute(l, node, pos);
        }
        let Some(left) = self.steps.get().checked_sub(1) else {
            self.exhausted.set(true);
            return Vec::new();
        };
        // Only what can be reached twice at one place is remembered: a named
        // type or property (which is also how a grammar reaching itself is
        // stopped), and the alternatives of `&&`, `||` and repetitions.
        let shared = matches!(
            node,
            Node::Type { .. }
                | Node::Property(_)
                | Node::AnyOf(_)
                | Node::AllOf(_)
                | Node::Repeat { .. }
        );
        if !shared {
            self.steps.set(left);
            let mut out = self.compute(l, node, pos);
            out.sort_unstable();
            out.dedup();
            return out;
        }
        let key = (std::ptr::from_ref(node) as usize, pos);
        if let Some(e) = l.memo.get(&key) {
            return e.clone();
        }
        self.steps.set(left);
        // A grammar that reaches itself without consuming anything ends
        // nowhere on the inner visit.
        l.memo.insert(key, Vec::new());
        let mut out = self.compute(l, node, pos);
        out.sort_unstable();
        out.dedup();
        l.memo.insert(key, out.clone());
        out
    }

    fn compute(&self, l: &mut List<'_, '_, '_>, node: &Node, pos: usize) -> Vec<usize> {
        let item = l.items.get(pos).copied();
        let one = |ok: bool| if ok { vec![pos + 1] } else { Vec::new() };
        match node {
            Node::Keyword(k) => one(matches!(item,
                Some(ComponentValue::Token(Token::Ident(w))) if w.eq_ignore_ascii_case(k))),
            Node::Literal(c) => one(match (item, c) {
                (Some(ComponentValue::Token(Token::Comma)), ',') => true,
                (Some(ComponentValue::Token(Token::Colon)), ':') => true,
                (Some(ComponentValue::Token(Token::Semicolon)), ';') => true,
                (Some(ComponentValue::Token(Token::Delim(d))), c) => d == c,
                _ => false,
            }),
            Node::Type { name, range } => self.typed(l, name, *range, pos),
            Node::Property(p) => match self.g.properties.get(p.as_str()) {
                Some(Some(n)) => self.ends(l, n, pos),
                _ => self.unread(l, pos),
            },
            Node::Function(name, args) => match item {
                Some(ComponentValue::Function { name: f, args: a })
                    if f.eq_ignore_ascii_case(name) =>
                {
                    one(self.full(args, &significant(a), l.depth + 1))
                }
                _ => Vec::new(),
            },
            Node::SquareBlock(inner) | Node::ParenBlock(inner) => {
                let kind = if matches!(node, Node::SquareBlock(_)) {
                    BlockKind::Square
                } else {
                    BlockKind::Paren
                };
                match item {
                    Some(ComponentValue::Block(b)) if b.kind == kind => {
                        one(self.full(inner, &significant(&b.values), l.depth + 1))
                    }
                    _ => Vec::new(),
                }
            }
            Node::Seq(items) => self.seq(l, items, pos),
            Node::OneOf(alts) => {
                // Keywords, often most of the list, compared in place.
                let ident = match item {
                    Some(ComponentValue::Token(Token::Ident(w))) => Some(w),
                    _ => None,
                };
                let mut out = Vec::new();
                for a in alts {
                    match a {
                        Node::Keyword(k) => {
                            if ident.is_some_and(|w| w.eq_ignore_ascii_case(k)) {
                                out.push(pos + 1);
                            }
                        }
                        _ => out.extend(self.ends(l, a, pos)),
                    }
                }
                out
            }
            Node::AnyOf(alts) => self.any_order(l, alts, pos, false),
            Node::AllOf(alts) => self.any_order(l, alts, pos, true),
            Node::Repeat {
                node,
                min,
                max,
                comma,
            } => self.repeat(l, node, *min, *max, *comma, pos),
            Node::NonEmpty(inner) => self
                .ends(l, inner, pos)
                .into_iter()
                .filter(|&e| e > pos)
                .collect(),
        }
    }

    /// A piece this crate does not read: nothing strictly, anything
    /// leniently (any run, the empty one included).
    fn unread(&self, l: &List<'_, '_, '_>, pos: usize) -> Vec<usize> {
        if self.lenient {
            (pos..=l.items.len()).collect()
        } else {
            Vec::new()
        }
    }

    fn typed(
        &self,
        l: &mut List<'_, '_, '_>,
        name: &str,
        range: Option<Range>,
        pos: usize,
    ) -> Vec<usize> {
        let item = l.items.get(pos).copied();
        // A function no grammar names may be a newer one: leniently, it
        // fits any type.
        if self.lenient
            && let Some(ComponentValue::Function { name: f, .. }) = item
            && !self.g.functions.contains(&f.to_ascii_lowercase())
        {
            let mut out = vec![pos + 1];
            if let Some(n) = self.g.types.get(name).and_then(Option::as_ref) {
                out.extend(self.ends(l, n, pos));
            }
            return out;
        }
        if matches!(name, "any-value" | "declaration-value") {
            return (pos + 1..=l.items.len()).collect();
        }
        if let Some(fits) = item.and_then(|v| base_type(name, range, v)) {
            return if fits { vec![pos + 1] } else { Vec::new() };
        }
        if is_base_type(name) {
            return Vec::new();
        }
        // A range on a named type bounds the base types it is made of:
        // `<length-percentage [0,∞]>`.
        if let (Some(r), Some(v)) = (range, item)
            && let Some(fits) = self.ranged(name, r, v, 0)
        {
            return if fits { vec![pos + 1] } else { Vec::new() };
        }
        match self.g.types.get(name) {
            Some(Some(n)) => self.ends(l, n, pos),
            _ => self.unread(l, pos),
        }
    }

    /// Whether one value fits a named type made of base types alone, each
    /// bounded by `range`; `None` when the type is not that simple.
    fn ranged(
        &self,
        name: &str,
        range: Range,
        v: &ComponentValue<'_>,
        depth: usize,
    ) -> Option<bool> {
        if depth > 4 {
            return None;
        }
        if is_base_type(name) {
            return base_type(name, Some(range), v);
        }
        let node = self.g.types.get(name)?.as_ref()?;
        let alts = match node {
            Node::OneOf(alts) => alts.as_slice(),
            other => std::slice::from_ref(other),
        };
        let mut any = false;
        for alt in alts {
            let Node::Type { name, .. } = alt else {
                return None;
            };
            any |= self.ranged(name, range, v, depth + 1)?;
        }
        Some(any)
    }

    /// Juxtaposition, with CSS Values 4 §2.6's omitted commas: a comma in
    /// the grammar is left out when everything before it in the sequence
    /// was, when everything after it is, or when it would follow another
    /// comma.
    fn seq(&self, l: &mut List<'_, '_, '_>, items: &[Node], pos: usize) -> Vec<usize> {
        // (position, anything consumed yet, everything after must be empty)
        let mut states = vec![(pos, false, false)];
        for item in items {
            let mut next = Vec::new();
            for &(p, consumed, must_be_empty) in &states {
                if matches!(item, Node::Literal(',')) {
                    let is_comma =
                        matches!(l.items.get(p), Some(ComponentValue::Token(Token::Comma)));
                    if is_comma && !must_be_empty {
                        next.push((p + 1, true, false));
                    }
                    let after_comma = p > 0
                        && matches!(
                            l.items.get(p - 1),
                            Some(ComponentValue::Token(Token::Comma))
                        );
                    if !consumed || after_comma {
                        next.push((p, consumed, must_be_empty));
                    } else {
                        next.push((p, consumed, true));
                    }
                    continue;
                }
                for e in self.ends(l, item, p) {
                    if must_be_empty && e != p {
                        continue;
                    }
                    next.push((e, consumed || e > p, must_be_empty));
                }
            }
            next.sort_unstable();
            next.dedup();
            if next.is_empty() {
                return Vec::new();
            }
            states = next;
        }
        states.into_iter().map(|(p, ..)| p).collect()
    }

    /// `&&` (`all`) and `||`: the alternatives in any order, each at most
    /// once. Only an alternative that consumes something moves on, so the
    /// states stay as few as the value is long; under `&&`, one never used
    /// must be able to match nothing where the value ends.
    fn any_order(
        &self,
        l: &mut List<'_, '_, '_>,
        alts: &[Node],
        pos: usize,
        all: bool,
    ) -> Vec<usize> {
        if alts.len() > MAX_ANY_ORDER {
            self.exhausted.set(true);
            return Vec::new();
        }
        let mut seen: HashSet<(u32, usize), Quick> = HashSet::default();
        let mut todo = vec![(0u32, pos)];
        let mut out = Vec::new();
        while let Some((mask, p)) = todo.pop() {
            if !seen.insert((mask, p)) {
                continue;
            }
            let done = if all {
                let mut rest_empty = true;
                for (i, alt) in alts.iter().enumerate() {
                    if mask & (1 << i) == 0 && !self.ends(l, alt, p).contains(&p) {
                        rest_empty = false;
                        break;
                    }
                }
                rest_empty
            } else {
                mask != 0
            };
            if done {
                out.push(p);
            }
            for (i, alt) in alts.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    continue;
                }
                for e in self.ends(l, alt, p) {
                    if e > p {
                        todo.push((mask | (1 << i), e));
                    }
                }
            }
            if self.exhausted.get() {
                return Vec::new();
            }
        }
        out
    }

    fn repeat(
        &self,
        l: &mut List<'_, '_, '_>,
        node: &Node,
        min: u32,
        max: Option<u32>,
        comma: bool,
        pos: usize,
    ) -> Vec<usize> {
        let mut out = Vec::new();
        if min == 0 {
            out.push(pos);
        }
        let mut frontier = vec![pos];
        let mut reached: HashSet<usize, Quick> = HashSet::default();
        let cap = max.unwrap_or(u32::MAX);
        let mut count = 0u32;
        while count < cap && !frontier.is_empty() {
            count += 1;
            let mut next = Vec::new();
            for &p in &frontier {
                let start = if comma && count > 1 {
                    match l.items.get(p) {
                        Some(ComponentValue::Token(Token::Comma)) => p + 1,
                        _ => continue,
                    }
                } else {
                    p
                };
                next.extend(self.ends(l, node, start));
            }
            next.sort_unstable();
            next.dedup();
            if count >= min {
                out.extend(&next);
            }
            // Past the minimum, a repetition that only reaches places
            // already reached adds nothing.
            if count >= min && max.is_none() {
                next.retain(|p| reached.insert(*p));
            }
            frontier = next;
            if self.exhausted.get() {
                return Vec::new();
            }
        }
        out
    }
}

/// The base types read here, by name.
fn is_base_type(name: &str) -> bool {
    matches!(
        name,
        "length"
            | "percentage"
            | "number"
            | "integer"
            | "angle"
            | "time"
            | "frequency"
            | "resolution"
            | "flex"
            | "dimension"
            | "zero"
            | "ident"
            | "custom-ident"
            | "dashed-ident"
            | "custom-property-name"
            | "string"
            | "url"
            | "hex-color"
            | "ident-token"
            | "function-token"
            | "hash-token"
            | "string-token"
            | "number-token"
            | "dimension-token"
            | "percentage-token"
            | "url-token"
            | "unicode-range-token"
    )
}

const LENGTH_UNITS: &[&str] = &[
    "em", "rem", "ex", "rex", "cap", "rcap", "ch", "rch", "ic", "ric", "lh", "rlh", "vw", "vh",
    "vi", "vb", "vmin", "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw", "lvh", "lvi",
    "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh", "cqi",
    "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc", "px",
];
const ANGLE_UNITS: &[&str] = &["deg", "grad", "rad", "turn"];
const TIME_UNITS: &[&str] = &["s", "ms"];
const FREQUENCY_UNITS: &[&str] = &["hz", "khz"];
const RESOLUTION_UNITS: &[&str] = &["dpi", "dpcm", "dppx", "x"];

/// Whether one component value is of a base type: `None` when `name` is
/// not one, or the value is a math function and the type not numeric.
fn base_type(name: &str, range: Option<Range>, v: &ComponentValue<'_>) -> Option<bool> {
    let in_range = |n: f64| range.is_none_or(|r| n >= r.min && n <= r.max);
    let unit_of = |units: &[&str]| match v {
        ComponentValue::Token(Token::Dimension { value, unit, .. }) => {
            units.iter().any(|u| unit.eq_ignore_ascii_case(u)) && in_range(*value)
        }
        _ => false,
    };
    let numeric = matches!(
        name,
        "length"
            | "percentage"
            | "number"
            | "integer"
            | "angle"
            | "time"
            | "frequency"
            | "resolution"
            | "flex"
            | "dimension"
    );
    if numeric
        && let ComponentValue::Function { name: f, .. } = v
        && MATH.iter().any(|m| f.eq_ignore_ascii_case(m))
    {
        return Some(true);
    }
    let token = match v {
        ComponentValue::Token(t) => Some(t),
        _ => None,
    };
    Some(match name {
        "length" => {
            unit_of(LENGTH_UNITS)
                || matches!(token, Some(Token::Number { value, .. }) if *value == 0.0)
        }
        "percentage" => matches!(token, Some(Token::Percentage { value, .. }) if in_range(*value)),
        "number" => matches!(token, Some(Token::Number { value, .. }) if in_range(*value)),
        "integer" => {
            matches!(token, Some(Token::Number { value, num_type: NumericType::Integer, .. })
            if in_range(*value))
        }
        "angle" => unit_of(ANGLE_UNITS),
        "time" => unit_of(TIME_UNITS),
        "frequency" => unit_of(FREQUENCY_UNITS),
        "resolution" => unit_of(RESOLUTION_UNITS),
        "flex" => unit_of(&["fr"]),
        "dimension" | "dimension-token" => matches!(token, Some(Token::Dimension { .. })),
        "zero" => matches!(token, Some(Token::Number { value, .. }) if *value == 0.0),
        "ident" | "ident-token" => matches!(token, Some(Token::Ident(_))),
        "custom-ident" => matches!(token, Some(Token::Ident(w))
            if !CSS_WIDE.iter().chain(&["default"]).any(|k| w.eq_ignore_ascii_case(k))),
        "dashed-ident" | "custom-property-name" => {
            matches!(token, Some(Token::Ident(w)) if w.starts_with("--"))
        }
        "string" | "string-token" => matches!(token, Some(Token::String(_))),
        "url" => match v {
            ComponentValue::Token(Token::Url(_)) => true,
            ComponentValue::Function { name, .. } => {
                name.eq_ignore_ascii_case("url") || name.eq_ignore_ascii_case("src")
            }
            _ => false,
        },
        "url-token" => matches!(token, Some(Token::Url(_))),
        "hex-color" => matches!(token, Some(Token::Hash { value, .. })
            if matches!(value.len(), 3 | 4 | 6 | 8) && value.bytes().all(|b| b.is_ascii_hexdigit())),
        "hash-token" => matches!(token, Some(Token::Hash { .. })),
        "unicode-range-token" => matches!(token, Some(Token::UnicodeRange { .. })),
        "number-token" => matches!(token, Some(Token::Number { .. })),
        "percentage-token" => matches!(token, Some(Token::Percentage { .. })),
        "function-token" => matches!(v, ComponentValue::Function { .. }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
