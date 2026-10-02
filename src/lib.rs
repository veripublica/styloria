//! styloria — a pure-Rust CSS parser and serializer, following CSS Syntax
//! Level 3 as of the 1 October 2026 Candidate Recommendation Draft
//! (<https://www.w3.org/TR/2026/CRD-css-syntax-3-20261001/>).
//!
//! [`parse_stylesheet`] returns the whole tree — rules, the declarations and
//! nested rules in their blocks, component values — with a byte [`Span`] on
//! every node, and every [`SyntaxError`] it recovered from.
//! [`parse_block_contents`] does the same for a `style="…"` attribute.

mod descriptors;
mod known_properties;
pub mod parser;
pub mod selector;
pub mod serialize;
pub mod span;
pub mod token;
pub mod tokenizer;
pub mod validate;

pub use parser::{
    AtRule, Block, BlockItem, BlockKind, ComponentValue, Declaration, MAX_NESTING_DEPTH,
    QualifiedRule, Rule, SimpleBlock, Stylesheet, SyntaxError, SyntaxErrorKind,
    parse_block_contents, parse_stylesheet, syntax_errors,
};
pub use selector::{type_selector_names, validate_relative_selector_list, validate_selector_list};
pub use serialize::{serialize_block_contents, serialize_stylesheet};
pub use span::{Span, Spanned};
pub use token::{NumericType, Token};
pub use tokenizer::{SpannedTokens, Tokenizer};
pub use validate::{
    Diagnostic, DiagnosticKind, validate_declaration_list, validate_parsed_block,
    validate_parsed_stylesheet, validate_stylesheet,
};
