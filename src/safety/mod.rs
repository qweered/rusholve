//! Refuse-to-rewrite safety pass.
//!
//! The policy: detect brush-incompatible constructs *before* we lower into
//! IR or rewrite. Hard stops produce exit code 14; warnings produce a
//! diagnostic that exits non-zero unless `--allow-known-gaps` is set. We
//! never silently rewrite a script we can't faithfully understand.
//!
//! Two layers:
//!
//! 1. [`scan_tokens`] — regex-based scan over raw source. Catches things
//!    brush can't even tokenize (`select`, `coproc`, `disown`, `logout`,
//!    `$"…"`). Runs *before* parsing so we beat brush to a friendly
//!    diagnostic.
//! 2. [`scan_ast`] — visitor over the brush AST. Catches things that parse
//!    but mean the wrong thing (`wait -n`, signal traps for unsupported
//!    signals, `$BASH_COMMAND` outside trap, deeply-nested `$(…)`, …).
//!    Runs *after* parsing.

mod ast_scan;
mod token_scan;

use serde::Serialize;

use crate::ir::Span;

pub use ast_scan::scan_ast;
pub use token_scan::scan_tokens;

/// Run both safety layers and aggregate the results. Token scan first
/// (cheap, runs even when the AST didn't parse); AST scan second.
pub fn audit(source: &str, ast: &brush_parser::ast::Program) -> SafetyReport {
    let mut report = SafetyReport {
        hard_stops: scan_tokens(source),
        warnings: Vec::new(),
    };
    let (ast_stops, ast_warns) = scan_ast(source, ast);
    report.extend(SafetyReport {
        hard_stops: ast_stops,
        warnings: ast_warns,
    });
    report.hard_stops.sort_by_key(|h| h.span.start);
    report.warnings.sort_by_key(|w| w.span.start);
    report
}

/// Aggregated outcome of the safety pass.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SafetyReport {
    pub hard_stops: Vec<UnsupportedConstruct>,
    pub warnings: Vec<KnownGap>,
}

impl SafetyReport {
    /// True iff there's nothing to report.
    pub fn is_clean(&self) -> bool {
        self.hard_stops.is_empty() && self.warnings.is_empty()
    }

    /// True iff at least one hard stop was found — the caller MUST exit 14.
    pub fn must_refuse(&self) -> bool {
        !self.hard_stops.is_empty()
    }

    pub(crate) fn extend(&mut self, other: SafetyReport) {
        self.hard_stops.extend(other.hard_stops);
        self.warnings.extend(other.warnings);
    }
}

/// A construct we refuse to rewrite. Each carries a span pointer for
/// ariadne and a kind tag for downstream tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnsupportedConstruct {
    pub kind: HardStopKind,
    pub span: Span,
    pub line: usize,
    pub column: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HardStopKind {
    SelectStatement,
    CoprocKeyword,
    DisownBuiltin,
    LogoutBuiltin,
    LocaleQuoted,
    BashCommandOutsideTrap,
    WaitDashN,
    CaseParenInCmdSub,
    DeepNestedCmdSub,
    HeredocQuoteInCmdSub,
}

impl HardStopKind {
    pub fn human(self) -> &'static str {
        match self {
            Self::SelectStatement => "`select` statement (brush \u{1f6a7})",
            Self::CoprocKeyword => "`coproc` keyword (brush issue tracker)",
            Self::DisownBuiltin => "`disown` builtin (brush \u{1f6a7})",
            Self::LogoutBuiltin => "`logout` builtin (brush \u{1f6a7})",
            Self::LocaleQuoted => "locale-aware `$\"\u{2026}\"` quoting",
            Self::BashCommandOutsideTrap => "`$BASH_COMMAND` outside trap (brush \u{1f6a7})",
            Self::WaitDashN => "`wait -n` (brush \u{1f6a7})",
            Self::CaseParenInCmdSub => {
                "case pattern containing `)` inside `$(\u{2026})` (brush issue #1052)"
            }
            Self::DeepNestedCmdSub => {
                "deeply-nested subshells with pipes inside `$(\u{2026})` (brush issue #1040)"
            }
            Self::HeredocQuoteInCmdSub => {
                "unbalanced quote in heredoc inside `\"$(\u{2026})\"` (brush issue #1066)"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnownGap {
    pub kind: WarningKind,
    pub span: Span,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WarningKind {
    UnknownSignalTrap,
    NonWhitespaceIfs,
    PrintfAdvancedFormat,
    SetEReliance,
    UncommonShoptOption,
    AdvancedBindFeature,
    ArithDivByZeroErrexit,
    ComplexAlias,
    /// `eval` with a dynamic argument — what gets eval'd is unknown
    /// at static time, so the resolver can't analyze it. Mirrors
    /// resholve's `QuotedEval` warning.
    QuotedEval,
}

/// Compute (line, column), both 1-based, for a byte offset.
pub(crate) fn line_col_of(source: &str, byte_offset: usize) -> (usize, usize) {
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
