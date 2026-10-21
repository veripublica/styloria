//! styloria — a pure-Rust CSS parser and serializer, following CSS Syntax
//! Level 3 as of the 1 October 2026 Candidate Recommendation Draft
//! (<https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/>).
//!
//! [`parse_stylesheet`] returns the whole tree — rules, the declarations and
//! nested rules in their blocks, component values — with a byte [`Span`] on
//! every node, and every [`SyntaxError`] it recovered from.
//! [`parse_block_contents`] does the same for a `style="…"` attribute.
//!
//! On top of the tree:
//!
//! - [`decode`]: bytes to text in CSS Syntax's order, and back
//!   ([`decode_stylesheet`], [`encode_stylesheet`]).
//! - [`selector`] and [`matching`]: selector trees with specificity, matched
//!   three-valued against the caller's tree through [`Element`].
//! - [`cascade`]: which declarations could set a property on an element, and
//!   which decides ([`Cascade`]).
//! - [`validate`] and [`values`]: unknown properties and descriptors, and
//!   values that cannot fit their grammar ([`validate_parsed_stylesheet`],
//!   [`check_value`], [`check_descriptor`]).
//! - [`references`]: the URLs and selector names a stylesheet points at.
//! - [`at_rule`]: the preludes of `@namespace`, `@import`, `@layer`, `@media`.
//! - [`serialize`]: the tree back to CSS.

pub mod at_rule;
pub mod cascade;
pub mod decode;
mod descriptors;
mod encodings;
mod known_properties;
pub mod matching;
pub mod parser;
pub mod properties;
pub mod references;
pub mod selector;
pub mod serialize;
mod shorthands;
pub mod span;
pub mod token;
pub mod tokenizer;
pub mod validate;
mod value_syntaxes;
pub mod values;

pub use at_rule::{
    ImportLayer, ImportRule, LayerName, LayerRule, MediaQualifier, MediaQuery, MediaQueryList,
    NamespaceRule, parse_import_rule, parse_layer_rule, parse_media_query_list,
    parse_namespace_rule,
};
pub use cascade::{
    Candidate, Cascade, ElementStyle, MAX_IMPORT_DEPTH, PropertyAnswer, Setter, Sheet,
    UnknownReason, UnknownReasons,
};
pub use decode::{
    DecodeError, Decoded, EncodeError, Encoding, LabelSource, charset_rule, decode_stylesheet,
    encode_stylesheet,
};
pub use matching::{Element, HTML_NAMESPACE, MAX_MATCH_STEPS, Match, Namespaces};
pub use parser::{
    AtRule, Block, BlockItem, BlockKind, ComponentValue, Declaration, MAX_NESTING_DEPTH,
    QualifiedRule, Rule, SimpleBlock, Stylesheet, SyntaxError, SyntaxErrorKind,
    declaration_removal_span, parse_block_contents, parse_stylesheet, syntax_errors,
};
pub use properties::longhands;
pub use references::{
    SelectorReference, UrlContext, UrlForm, UrlReference, block_url_references,
    selector_references, url_references,
};
pub use selector::{
    AnB, AttributeCase, AttributeOperator, AttributeSelector, AttributeValue, Combinator,
    ComplexSelector, CompoundSelector, MAX_RESOLVED_SIMPLES, MAX_SELECTOR_DEPTH, Namespace,
    PseudoClass, PseudoElement, QualifiedName, SelectorList, SimpleSelector, Specificity,
    parse_relative_selector_list, parse_selector_list,
};
pub use serialize::{serialize_block_contents, serialize_stylesheet};
pub use span::{Span, Spanned};
pub use token::{NumericType, Token};
pub use tokenizer::{SpannedTokens, Tokenizer};
pub use validate::{
    Diagnostic, DiagnosticKind, validate_declaration_list, validate_parsed_block,
    validate_parsed_stylesheet, validate_stylesheet,
};
pub use values::{
    MAX_VALUE_STEPS, MDN_DATA_VERSION, UnknownValue, ValueCheck, check_declaration,
    check_descriptor, check_value,
};
