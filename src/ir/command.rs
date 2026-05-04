use serde::{Deserialize, Serialize};

use super::{Span, Word};

/// A single command invocation (one `SimpleCommand` in brush terms).
///
/// `words[0]` (when present and static) is the command name; the rest are
/// its arguments. Empty `words` is legal (e.g. a bare assignment or
/// redirect-only command); the resolver skips those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    pub words: Vec<Word>,
    pub span: Span,
    pub context: InvocationContext,
}

impl Invocation {
    /// First word, if any. Convenience for the common case.
    pub fn name(&self) -> Option<&Word> {
        self.words.first()
    }

    /// Static command name, if known. `None` for empty invocations or
    /// dynamic command names (e.g. `$cmd args`).
    pub fn static_name(&self) -> Option<&str> {
        self.name().and_then(Word::as_static)
    }

    pub fn args(&self) -> &[Word] {
        if self.words.is_empty() {
            &[]
        } else {
            &self.words[1..]
        }
    }
}

/// The CRO-relevant context an invocation appears in.
///
/// Most invocations are `Default`. The other variants exist because they
/// change Command Resolution Order: e.g. inside `command foo`, alias and
/// function lookup is skipped. The frontend tags invocations as it lowers
/// them; the resolver consults the tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvocationContext {
    /// Top-level or any "normal" command position.
    Default,

    /// Argument of the `command` builtin — skip function and alias lookup.
    InsideCommand,

    /// Argument of `exec` — like Default, but the resolved command replaces
    /// the shell process; the resolver still resolves it normally.
    InsideExec,

    /// Argument of `eval` (unquoted). The resolver treats the inner words as
    /// a fresh command line.
    InsideEval,

    /// Inside a function body — the function's own name is in scope.
    /// (We carry the body name on the surrounding `CommandLike::Function`,
    /// not here, to keep `Invocation` `Copy`-ish.)
    InsideFunctionBody,
}

/// The resolver's unit of work. Everything the rewriter cares about lives
/// inside one of these variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandLike {
    /// A simple command: `git status -sb`.
    Simple(Invocation),

    /// A function definition: `name() { … }` or `function name { … }`.
    Function {
        name: String,
        name_span: Span,
        /// Function body lowered as its own [`SourceUnit`](super::SourceUnit).
        /// Boxed because this tree node is recursive.
        body: Box<super::SourceUnit>,
        span: Span,
    },

    /// A `source` / `.` include. `target` is the file argument (may be
    /// dynamic — the resolver handles that).
    Source { target: Word, span: Span },

    /// An alias definition: `alias name=value`.
    Alias {
        name: String,
        name_span: Span,
        definition: Word,
        span: Span,
    },
}

impl CommandLike {
    pub fn span(&self) -> Span {
        match self {
            CommandLike::Simple(inv) => inv.span,
            CommandLike::Function { span, .. }
            | CommandLike::Source { span, .. }
            | CommandLike::Alias { span, .. } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::WordPiece;

    fn static_word(text: &str, start: usize) -> Word {
        Word {
            span: Span::new(start, start + text.len()),
            pieces: vec![WordPiece::Literal {
                text: text.to_string(),
                span: Span::new(start, start + text.len()),
            }],
            static_value: Some(text.to_string()),
        }
    }

    #[test]
    fn invocation_name_and_args() {
        let inv = Invocation {
            words: vec![
                static_word("git", 0),
                static_word("status", 4),
                static_word("-sb", 11),
            ],
            span: Span::new(0, 14),
            context: InvocationContext::Default,
        };
        assert_eq!(inv.static_name(), Some("git"));
        assert_eq!(inv.args().len(), 2);
        assert_eq!(inv.args()[0].as_static(), Some("status"));
    }

    #[test]
    fn empty_invocation_has_no_name() {
        let inv = Invocation {
            words: vec![],
            span: Span::new(0, 0),
            context: InvocationContext::Default,
        };
        assert!(inv.name().is_none());
        assert!(inv.static_name().is_none());
        assert!(inv.args().is_empty());
    }

    #[test]
    fn command_like_span_dispatches() {
        let inv = Invocation {
            words: vec![],
            span: Span::new(5, 10),
            context: InvocationContext::Default,
        };
        assert_eq!(CommandLike::Simple(inv).span(), Span::new(5, 10));
    }
}
