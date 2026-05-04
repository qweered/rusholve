//! What the resolver hands the rewriter (or, on failure, the diagnostic
//! renderer). Every `CommandLike` the resolver visits produces exactly
//! one [`Solution`].

use serde::Serialize;

use crate::ir::{SourceId, Span};

/// What the resolver emits per visited reference, attributed back to the
/// originating source file.
///
/// `source_id` (added in v0.3 for multi-file resolution) tells the
/// rewriter/diagnostics which file in the [`SourceMap`](crate::ir::SourceMap)
/// the span addresses. Within a single-file resolve (the common case),
/// every solution carries the entry script's id; multi-file resolves
/// produce solutions across the source graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Solution {
    /// Splice `replacement` into source at `initial`.
    Resolved {
        source_id: SourceId,
        initial: Span,
        original: String,
        replacement: String,
        kind: ResolvedKind,
    },

    /// The reference is internal (function, builtin, keyword, alias body).
    /// The rewriter ignores these; diagnostics may surface them with
    /// `--explain`.
    InScope {
        source_id: SourceId,
        span: Span,
        name: String,
        kind: InScopeKind,
    },

    /// User explicitly accepted this reference via a `skip` or `allow`
    /// directive. The rewriter treats this like InScope (no rewrite),
    /// but diagnostics surface it differently — the user took
    /// responsibility, the resolver did not infer it.
    Allowed {
        source_id: SourceId,
        span: Span,
        name: Option<String>,
        reason: String,
    },

    /// We could not resolve this reference. Caller decides whether to
    /// fail or emit a warning, depending on directive context.
    Unresolved {
        source_id: SourceId,
        span: Span,
        name: Option<String>,
        kind: UnresolvedKind,
        hint: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolvedKind {
    /// External command found in inputs PATH.
    External,
    /// `source X` whose target was statically resolvable.
    SourceFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InScopeKind {
    Function,
    Alias,
    Builtin,
    SpecialBuiltin,
    Keyword,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnresolvedKind {
    /// The command name itself is dynamic — `$cmd args` or `${var} args`.
    DynamicCommandName,
    /// The name resolved as External but no input directory contains it.
    UnknownExternal,
    /// `source $x` — the source target is dynamic, can't follow.
    DynamicSourcePath,
    /// `source X` where X is static but the file isn't reachable in inputs.
    UnreadableSource,
}

impl Solution {
    pub fn span(&self) -> Span {
        match self {
            Self::Resolved { initial, .. } => *initial,
            Self::InScope { span, .. }
            | Self::Allowed { span, .. }
            | Self::Unresolved { span, .. } => *span,
        }
    }

    /// Source file this solution's span addresses.
    pub fn source_id(&self) -> SourceId {
        match self {
            Self::Resolved { source_id, .. }
            | Self::InScope { source_id, .. }
            | Self::Allowed { source_id, .. }
            | Self::Unresolved { source_id, .. } => *source_id,
        }
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved { .. })
    }

    pub fn is_unresolved(&self) -> bool {
        matches!(self, Self::Unresolved { .. })
    }
}
