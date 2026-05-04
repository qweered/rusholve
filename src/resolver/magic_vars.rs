//! Recognize and expand a tiny, hand-picked set of "magic" parameter
//! expansions that we can statically resolve at audit time.
//!
//! The whole point of this module is to make
//!
//! ```bash
//! source "${BASH_SOURCE%/*}/lib.sh"
//! source "$HOME/.dotfiles/init.sh"
//! ```
//!
//! resolvable without the user having to spell out a `--skip` or `--map`.
//! Anything we don't recognize stays dynamic — we never guess.
//!
//! The `text` we expand is the *raw source bytes* of a single
//! `WordPiece::Dynamic` (typically a `ParamExp` or `VarSub` piece). The
//! caller — the resolver — is responsible for splicing the recognized
//! pieces into a final string.
//!
//! Recognized forms (and only these forms — tightening over time is fine,
//! loosening is a compatibility risk):
//!
//! | Form                         | Expansion                       |
//! |------------------------------|---------------------------------|
//! | `$BASH_SOURCE`               | script path                     |
//! | `${BASH_SOURCE}`             | script path                     |
//! | `${BASH_SOURCE[0]}`          | script path                     |
//! | `${BASH_SOURCE%/*}`          | parent dir of script            |
//! | `${BASH_SOURCE%%/*}`         | parent dir of script            |
//! | `$0` / `${0}` / `${0%/*}`    | script path / parent dir        |
//! | `$HOME` / `${HOME}`          | user home                       |
//! | `${HOME%/*}`                 | parent of user home (rare)      |

use std::path::{Path, PathBuf};

use crate::ir::{Word, WordPiece};

#[derive(Debug, Default, Clone)]
pub struct MagicVars {
    /// Absolute path to the entry script. Used to expand `$BASH_SOURCE` /
    /// `$0` and the `${BASH_SOURCE%/*}` directory form.
    pub script_path: Option<PathBuf>,
    /// User home directory. Used to expand `$HOME` / `${HOME}`.
    pub home: Option<PathBuf>,
}

impl MagicVars {
    /// Try to expand a single dynamic piece. `text` is the literal source
    /// substring of the piece (e.g. `${BASH_SOURCE%/*}` or `$HOME`).
    /// Returns `None` if the piece is not a recognized magic var, or if
    /// the corresponding context value is missing.
    pub fn expand(&self, text: &str) -> Option<String> {
        let inner = strip_dollar_braces(text)?;
        match inner {
            "BASH_SOURCE" | "BASH_SOURCE[0]" | "0" => {
                self.script_path()?.to_str().map(String::from)
            }
            "BASH_SOURCE%/*" | "BASH_SOURCE%%/*" | "BASH_SOURCE[0]%/*" | "BASH_SOURCE[0]%%/*"
            | "0%/*" | "0%%/*" => self
                .script_path()
                .and_then(Path::parent)
                .and_then(|p| p.to_str())
                .map(String::from),
            "HOME" => self.home()?.to_str().map(String::from),
            "HOME%/*" | "HOME%%/*" => self
                .home()
                .and_then(Path::parent)
                .and_then(|p| p.to_str())
                .map(String::from),
            _ => None,
        }
    }

    fn script_path(&self) -> Option<&Path> {
        self.script_path.as_deref()
    }

    fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }
}

/// Walk a [`Word`]'s pieces and produce a static string by combining each
/// piece's literal contribution with any magic-var expansion. Returns
/// `None` the moment we hit a piece we can't statically realize — we
/// never partially-expand and never guess.
///
/// `source` is the original source text the word's spans index into; used
/// to look up the literal text of dynamic pieces.
pub fn expand_word(word: &Word, source: &str, magic: &MagicVars) -> Option<String> {
    let mut out = String::new();
    for piece in &word.pieces {
        expand_piece(piece, source, magic, &mut out)?;
    }
    Some(out)
}

fn expand_piece(
    piece: &WordPiece,
    source: &str,
    magic: &MagicVars,
    out: &mut String,
) -> Option<()> {
    match piece {
        WordPiece::Literal { text, .. } | WordPiece::SingleQuoted { text, .. } => {
            out.push_str(text);
            Some(())
        }
        WordPiece::DoubleQuoted { pieces, .. } => {
            for inner in pieces {
                expand_piece(inner, source, magic, out)?;
            }
            Some(())
        }
        WordPiece::Dynamic { span, .. } => {
            let text = source.get(span.start..span.end)?;
            let expanded = magic.expand(text)?;
            out.push_str(&expanded);
            Some(())
        }
        WordPiece::CommandSub { .. } => {
            // Magic-var expansion gives source-graph paths a static
            // form. A `$(…)` mid-path is genuinely runtime-only; we
            // can't pre-evaluate it here.
            None
        }
    }
}

/// Strip the leading `$` and any matching `{}` from a parameter expansion
/// text so the body can be matched against known forms.
///
/// Inputs we accept: `$NAME`, `${NAME}`, `${NAME%/*}`, `${NAME[0]}`.
/// Inputs we reject: anything not starting with `$`, anything with
/// unbalanced braces, anything we'd be guessing about.
fn strip_dollar_braces(text: &str) -> Option<&str> {
    let body = text.strip_prefix('$')?;
    if let Some(inner) = body.strip_prefix('{') {
        // Must end with a single `}` that closes the opening brace.
        let inner = inner.strip_suffix('}')?;
        // Reject anything with nested `${` — we don't recurse.
        if inner.contains("${") {
            return None;
        }
        Some(inner)
    } else {
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> MagicVars {
        MagicVars {
            script_path: Some(PathBuf::from("/etc/profile.d/foo.sh")),
            home: Some(PathBuf::from("/home/alice")),
        }
    }

    #[test]
    fn bash_source_unbraced() {
        assert_eq!(
            vars().expand("$BASH_SOURCE").as_deref(),
            Some("/etc/profile.d/foo.sh")
        );
    }

    #[test]
    fn bash_source_braced() {
        assert_eq!(
            vars().expand("${BASH_SOURCE}").as_deref(),
            Some("/etc/profile.d/foo.sh")
        );
    }

    #[test]
    fn bash_source_dir_form() {
        assert_eq!(
            vars().expand("${BASH_SOURCE%/*}").as_deref(),
            Some("/etc/profile.d")
        );
        assert_eq!(
            vars().expand("${BASH_SOURCE%%/*}").as_deref(),
            Some("/etc/profile.d")
        );
    }

    #[test]
    fn bash_source_array_index_zero() {
        assert_eq!(
            vars().expand("${BASH_SOURCE[0]}").as_deref(),
            Some("/etc/profile.d/foo.sh")
        );
        assert_eq!(
            vars().expand("${BASH_SOURCE[0]%/*}").as_deref(),
            Some("/etc/profile.d")
        );
    }

    #[test]
    fn dollar_zero() {
        assert_eq!(
            vars().expand("$0").as_deref(),
            Some("/etc/profile.d/foo.sh")
        );
        assert_eq!(vars().expand("${0%/*}").as_deref(), Some("/etc/profile.d"));
    }

    #[test]
    fn home_variants() {
        assert_eq!(vars().expand("$HOME").as_deref(), Some("/home/alice"));
        assert_eq!(vars().expand("${HOME}").as_deref(), Some("/home/alice"));
    }

    #[test]
    fn unknown_var_is_none() {
        assert_eq!(vars().expand("$NOPE"), None);
        assert_eq!(vars().expand("${ALSO_NOT_KNOWN}"), None);
    }

    #[test]
    fn unsupported_paramexp_form_is_none() {
        // `${BASH_SOURCE:-default}` is a defaulting form we don't model.
        assert_eq!(vars().expand("${BASH_SOURCE:-x}"), None);
        // Quoted-default and slash-replace also unsupported.
        assert_eq!(vars().expand("${BASH_SOURCE/x/y}"), None);
    }

    #[test]
    fn missing_context_returns_none() {
        let m = MagicVars {
            script_path: None,
            home: None,
        };
        assert_eq!(m.expand("$BASH_SOURCE"), None);
        assert_eq!(m.expand("$HOME"), None);
    }

    #[test]
    fn nested_braces_rejected() {
        // We don't recurse — bail out so we don't expand wrongly.
        assert_eq!(vars().expand("${${INNER}}"), None);
    }

    #[test]
    fn missing_dollar_rejected() {
        assert_eq!(vars().expand("BASH_SOURCE"), None);
    }
}
