//! Shell-shaped intermediate representation.
//!
//! The IR is intentionally **flat**: a [`SourceUnit`] is a `Vec<CommandLike>`,
//! not a tree of pipelines/subshells/loops. We do not need AST shape for
//! rewriting — every node carries a [`Span`] into its source text, and the
//! rewriter splices replacements at those spans. The frontend's job is to
//! walk the parser's tree and emit a flat sequence of `CommandLike`s in
//! traversal order.
//!
//! Function bodies nest as their own [`SourceUnit`] because functions create
//! a new CRO scope.

mod command;
mod source;
mod span;
mod visitor;
mod word;

pub use command::{CommandLike, Invocation, InvocationContext};
pub use source::{SourceFile, SourceId, SourceMap, SourceUnit, VarAssign};
pub use span::Span;
pub use visitor::Visitor;
pub use word::{DynamicKind, Word, WordPiece};
