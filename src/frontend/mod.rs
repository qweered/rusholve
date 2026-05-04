//! Frontends — parser-specific lowerers that emit our shell IR.
//!
//! v0.1 ships only [`bash`]. The trait shape exists so v0.4+ multi-shell
//! work can drop in additional implementations without churning the
//! resolver or rewriter.

pub mod bash;

use crate::ir::{SourceId, SourceUnit};

/// A parser-specific lowering frontend.
///
/// Implementors take raw source text plus the [`SourceId`] it was
/// registered under and produce a fully-lowered [`SourceUnit`]. They are
/// expected to surface their own parser's errors via the associated
/// `Error` type.
pub trait Frontend {
    type Error: std::error::Error + 'static;

    fn lower(&self, source: &str, source_id: SourceId) -> Result<FrontendOutput, Self::Error>;
}

/// What a frontend hands back: the lowered IR plus enough parser-specific
/// state for downstream passes (notably the safety AST scan) to consult.
#[non_exhaustive]
pub struct FrontendOutput {
    pub unit: SourceUnit,
    /// Brush-specific raw AST. v0.1 wires this through to the AST safety
    /// scan; v0.4+ frontends may carry a different payload.
    pub bash_ast: Option<brush_parser::ast::Program>,
}
