use serde::{Deserialize, Serialize};

use super::{SourceUnit, Span};

/// A single shell *word* (token at the level a `[a-zA-Z_]+` argument occupies).
///
/// `static_value` is `Some(s)` when the entire word resolves to a fixed string
/// at parse time — that is, every piece is a literal or single-quoted run with
/// no variable, command-substitution, or other dynamic expansion. The resolver
/// uses this as the "is this command name knowable?" signal.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub span: Span,
    pub pieces: Vec<WordPiece>,
    pub static_value: Option<String>,
}

impl Word {
    pub fn is_static(&self) -> bool {
        self.static_value.is_some()
    }

    pub fn as_static(&self) -> Option<&str> {
        self.static_value.as_deref()
    }
}

/// A sub-element of a [`Word`].
///
/// We model only the distinctions the resolver and rewriter need. We do *not*
/// preserve enough information to evaluate parameter expansion or arithmetic
/// — for our purposes those are just dynamic markers on a span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordPiece {
    /// Bare literal text.
    Literal { text: String, span: Span },

    /// A single-quoted run. Body is verbatim (no escapes interpreted).
    SingleQuoted { text: String, span: Span },

    /// A double-quoted run. Inner pieces may include dynamic expansions.
    DoubleQuoted { pieces: Vec<WordPiece>, span: Span },

    /// A `$(…)` command substitution we *did* re-parse. The inner
    /// [`SourceUnit`] is a fully-lowered nested unit whose spans are
    /// absolute offsets into the *outer* source, so the rewriter can
    /// splice resolutions for inner commands without coordinate
    /// translation.
    ///
    /// Backquoted command substitutions stay opaque (`Dynamic` with
    /// `kind: CmdSub`) because backslash-escape processing breaks the
    /// span-into-outer-source invariant.
    CommandSub { inner: Box<SourceUnit>, span: Span },

    /// Any dynamic expansion we are not going to evaluate here.
    Dynamic { kind: DynamicKind, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DynamicKind {
    /// `$VAR` or `${VAR}` (plain).
    VarSub,
    /// `${VAR:-default}`, `${VAR/x/y}`, `${!prefix*}`, etc.
    ParamExp,
    /// `$(…)` or backticks.
    CmdSub,
    /// `$((…))`.
    ArithExp,
    /// `<(…)` or `>(…)`.
    ProcSub,
    /// `~user` or bare `~`.
    Tilde,
    /// `$'…'` (ANSI-C quoting). Statically resolvable in principle but we
    /// don't bother in v0.1.
    AnsiC,
    /// Brace expansion `{a,b,c}` or `{1..10}`. Evaluated by the shell.
    BraceExp,
    /// Glob characters `*`, `?`, `[`. Resolved by the shell at runtime.
    Glob,
}

impl WordPiece {
    pub fn span(&self) -> Span {
        match self {
            WordPiece::Literal { span, .. }
            | WordPiece::SingleQuoted { span, .. }
            | WordPiece::DoubleQuoted { span, .. }
            | WordPiece::CommandSub { span, .. }
            | WordPiece::Dynamic { span, .. } => *span,
        }
    }

    /// True if this piece does not introduce runtime evaluation.
    pub fn is_static(&self) -> bool {
        match self {
            WordPiece::Literal { .. } | WordPiece::SingleQuoted { .. } => true,
            WordPiece::DoubleQuoted { pieces, .. } => pieces.iter().all(WordPiece::is_static),
            WordPiece::CommandSub { .. } | WordPiece::Dynamic { .. } => false,
        }
    }

    /// Concatenates the static contribution of this piece. Returns `None` if
    /// any sub-piece is dynamic.
    pub fn static_text(&self) -> Option<String> {
        match self {
            WordPiece::Literal { text, .. } | WordPiece::SingleQuoted { text, .. } => {
                Some(text.clone())
            }
            WordPiece::DoubleQuoted { pieces, .. } => {
                let mut out = String::new();
                for p in pieces {
                    out.push_str(&p.static_text()?);
                }
                Some(out)
            }
            WordPiece::CommandSub { .. } | WordPiece::Dynamic { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str, start: usize) -> WordPiece {
        WordPiece::Literal {
            text: text.to_string(),
            span: Span::new(start, start + text.len()),
        }
    }

    #[test]
    fn literal_is_static() {
        assert!(lit("git", 0).is_static());
        assert_eq!(lit("git", 0).static_text().as_deref(), Some("git"));
    }

    #[test]
    fn dynamic_is_not_static() {
        let p = WordPiece::Dynamic {
            kind: DynamicKind::VarSub,
            span: Span::new(0, 4),
        };
        assert!(!p.is_static());
        assert_eq!(p.static_text(), None);
    }

    #[test]
    fn double_quoted_with_only_literals_is_static() {
        let p = WordPiece::DoubleQuoted {
            pieces: vec![lit("hello ", 1), lit("world", 7)],
            span: Span::new(0, 13),
        };
        assert!(p.is_static());
        assert_eq!(p.static_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn double_quoted_with_dynamic_is_not_static() {
        let p = WordPiece::DoubleQuoted {
            pieces: vec![
                lit("hello ", 1),
                WordPiece::Dynamic {
                    kind: DynamicKind::VarSub,
                    span: Span::new(7, 12),
                },
            ],
            span: Span::new(0, 13),
        };
        assert!(!p.is_static());
        assert_eq!(p.static_text(), None);
    }

    #[test]
    fn word_is_static_helper() {
        let w = Word {
            span: Span::new(0, 3),
            pieces: vec![lit("git", 0)],
            static_value: Some("git".into()),
        };
        assert!(w.is_static());
        assert_eq!(w.as_static(), Some("git"));
    }
}
