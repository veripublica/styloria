//! styloria — a pure-Rust CSS3 parser and serializer.

mod descriptors;
mod known_properties;
pub mod parser;
pub mod serialize;
pub mod span;
pub mod spanned;
pub mod token;
pub mod tokenizer;
pub mod validate;

pub use parser::{
    AtRule, BlockKind, ComponentValue, Declaration, DeclarationListItem, Parser, QualifiedRule,
    Rule, SimpleBlock, Stylesheet,
};
pub use serialize::{serialize_declaration_list, serialize_stylesheet};
pub use span::{Span, Spanned};
pub use spanned::{SyntaxError, SyntaxErrorKind};
pub use token::{NumericType, Token};
pub use tokenizer::{SpannedTokens, Tokenizer};
pub use validate::{Diagnostic, DiagnosticKind, validate_declaration_list, validate_stylesheet};
