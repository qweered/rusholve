//! Diagnostic types and renderers.
//!
//! v0.1 ships JSON only — it's the format CI tools, editors, and Nix
//! glue actually want. Human (ariadne) rendering is a follow-up;
//! `Diagnostic` is structured enough to render either way.

mod json;
mod suggest;

pub use suggest::nearest;

use std::path::PathBuf;

use serde::Serialize;

use crate::ir::Span;
use crate::resolver::{Solution, UnresolvedKind};
use crate::safety::{HardStopKind, KnownGap, UnsupportedConstruct, WarningKind};

pub use json::{render_jsonl, render_pretty_json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub span: Span,
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub kind: DiagnosticKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "category", content = "code", rename_all = "kebab-case")]
pub enum DiagnosticKind {
    UnsupportedConstruct(HardStopKind),
    KnownGap(WarningKind),
    Unresolved(UnresolvedKind),
}

impl Diagnostic {
    pub fn from_unresolved(file: PathBuf, sol: &Solution, source: &str) -> Option<Self> {
        let Solution::Unresolved {
            span,
            name,
            kind,
            hint,
            ..
        } = sol
        else {
            return None;
        };
        let (line, column) = line_col(source, span.start);
        let message = match (kind, name.as_deref()) {
            (UnresolvedKind::DynamicCommandName, _) => {
                "command name is dynamic; resolve via `--skip` or rewrite as a static call".into()
            }
            (UnresolvedKind::UnknownExternal, Some(n)) => {
                format!("unknown external command `{n}`; add it to --inputs or use --map")
            }
            (UnresolvedKind::UnknownExternal, None) => "unknown external command".into(),
            (UnresolvedKind::DynamicSourcePath, _) => {
                "source path is dynamic; resolve via `--skip` or rewrite as a static path".into()
            }
            (UnresolvedKind::UnreadableSource, Some(n)) => {
                format!("source file `{n}` not found in inputs or relative to the script")
            }
            (UnresolvedKind::UnreadableSource, None) => "source file not found".into(),
        };
        Some(Self {
            file,
            span: *span,
            line,
            column,
            severity: Severity::Error,
            kind: DiagnosticKind::Unresolved(*kind),
            message,
            name: name.clone(),
            hint: hint.clone(),
        })
    }

    pub fn from_unsupported(file: PathBuf, u: &UnsupportedConstruct) -> Self {
        Self {
            file,
            span: u.span,
            line: u.line,
            column: u.column,
            severity: Severity::Error,
            kind: DiagnosticKind::UnsupportedConstruct(u.kind),
            message: u.kind.human().to_string(),
            name: Some(u.snippet.clone()),
            hint: None,
        }
    }

    pub fn from_known_gap(file: PathBuf, w: &KnownGap) -> Self {
        Self {
            file,
            span: w.span,
            line: w.line,
            column: w.column,
            severity: Severity::Warning,
            kind: DiagnosticKind::KnownGap(w.kind),
            message: w.message.clone(),
            name: None,
            hint: None,
        }
    }
}

fn line_col(source: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceId;
    use crate::resolver::UnresolvedKind;

    const SID: SourceId = SourceId::new(0);

    #[test]
    fn unknown_external_message_includes_name() {
        let sol = Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 2),
            name: Some("jq".into()),
            kind: UnresolvedKind::UnknownExternal,
            hint: None,
        };
        let d = Diagnostic::from_unresolved("x.sh".into(), &sol, "jq").unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("`jq`"));
        assert_eq!(d.name.as_deref(), Some("jq"));
    }

    #[test]
    fn from_unresolved_returns_none_for_non_unresolved() {
        let sol = Solution::InScope {
            source_id: SID,
            span: Span::new(0, 4),
            name: "echo".into(),
            kind: crate::resolver::InScopeKind::Builtin,
        };
        assert!(Diagnostic::from_unresolved("x.sh".into(), &sol, "echo").is_none());
    }

    #[test]
    fn from_unsupported_carries_kind() {
        let u = UnsupportedConstruct {
            kind: HardStopKind::SelectStatement,
            span: Span::new(0, 6),
            line: 1,
            column: 1,
            snippet: "select".into(),
        };
        let d = Diagnostic::from_unsupported("x.sh".into(), &u);
        assert_eq!(d.severity, Severity::Error);
        assert!(matches!(
            d.kind,
            DiagnosticKind::UnsupportedConstruct(HardStopKind::SelectStatement)
        ));
    }
}
