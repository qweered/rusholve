//! Lore-file ingestion.
//!
//! Lore is a per-binary "can this command exec other commands?" verdict.
//! resholve relies on the [binlore](https://github.com/abathur/binlore)
//! tool to scan a Nix build closure and emit verdicts; rusholve's v0.2
//! shipped with a hardcoded table for the most common wrappers, and
//! v0.3 adds this module so users (or upstream binlore output) can
//! supply additional verdicts.
//!
//! ## File format
//!
//! Two-column CSV without a header, no quoting required for the simple
//! shapes we accept:
//!
//! ```text
//! exec,my-runner
//! noexec,jq
//! # comments and blank lines are ignored
//! ```
//!
//! For interop with the resholve / binlore convention, single-column
//! `can:NAME` / `cannot:NAME` rows are also accepted.
//!
//! `exec` (or `can:`) means: when this command is at command position,
//! recurse into its arguments looking for an inner command — same as the
//! built-in `sudo`/`xargs`/`find` handling.
//!
//! `noexec` (or `cannot:`) means: even if rusholve's hardcoded table
//! would treat this name as a wrapper, the lore overrides — don't
//! recurse. Useful for `nice`-flavored commands the user *knows* their
//! script doesn't use as wrappers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Default, Clone)]
pub struct Lore {
    /// Names the user marked as additional exec-wrappers.
    pub execers: HashSet<String>,
    /// Names the user explicitly said are NOT exec-wrappers; this
    /// suppresses any built-in wrapper handling for them.
    pub non_execers: HashSet<String>,
}

impl Lore {
    pub fn merge(&mut self, other: Lore) {
        self.execers.extend(other.execers);
        self.non_execers.extend(other.non_execers);
    }

    /// Returns `Some(true)` if the user explicitly added this name as an
    /// exec-wrapper, `Some(false)` if they explicitly excluded it, and
    /// `None` if the lore has no opinion (caller falls back to the
    /// built-in table).
    pub fn override_for(&self, name: &str) -> Option<bool> {
        if self.non_execers.contains(name) {
            return Some(false);
        }
        if self.execers.contains(name) {
            return Some(true);
        }
        None
    }
}

#[derive(Debug, Error)]
pub enum LoreError {
    #[error("lore file `{path}`: I/O error: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("lore file `{path}` line {line}: malformed row `{raw}`")]
    Malformed {
        path: PathBuf,
        line: usize,
        raw: String,
    },
    #[error("lore file `{path}` line {line}: unknown verb `{verb}` (expected `exec`/`noexec`/`can:`/`cannot:`)")]
    UnknownVerb {
        path: PathBuf,
        line: usize,
        verb: String,
    },
}

/// Parse a lore file from disk into a [`Lore`] set.
pub fn read_file(path: &Path) -> Result<Lore, LoreError> {
    let text = std::fs::read_to_string(path).map_err(|e| LoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse(&text, path)
}

/// Parse lore text. The path is used only for diagnostics.
pub fn parse(text: &str, path: &Path) -> Result<Lore, LoreError> {
    let mut lore = Lore::default();
    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Single-column form: `can:NAME` / `cannot:NAME`.
        if let Some(rest) = trimmed.strip_prefix("can:") {
            insert_one(&mut lore, true, rest);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("cannot:") {
            insert_one(&mut lore, false, rest);
            continue;
        }
        // Two-column form: `verb,name`. We accept any single ASCII
        // separator: comma, tab, whitespace.
        let mut parts = trimmed.splitn(2, |c: char| c == ',' || c == '\t' || c.is_whitespace());
        let verb = parts.next().unwrap_or("");
        let Some(name) = parts.next().map(str::trim) else {
            return Err(LoreError::Malformed {
                path: path.to_path_buf(),
                line,
                raw: trimmed.to_string(),
            });
        };
        match verb.to_ascii_lowercase().as_str() {
            "exec" => insert_one(&mut lore, true, name),
            "noexec" | "cant_exec" | "cannot" => insert_one(&mut lore, false, name),
            other => {
                return Err(LoreError::UnknownVerb {
                    path: path.to_path_buf(),
                    line,
                    verb: other.to_string(),
                });
            }
        }
    }
    Ok(lore)
}

fn insert_one(lore: &mut Lore, is_execer: bool, name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    // If `name` is a path (e.g. `/nix/store/.../bin/foo`), index by
    // basename; that's what command-position lookups will compare.
    let basename = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed)
        .to_string();
    if is_execer {
        lore.execers.insert(basename);
    } else {
        lore.non_execers.insert(basename);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("test.lore")
    }

    #[test]
    fn parses_two_column_form() {
        let l = parse("exec,my-runner\nnoexec,jq\n", &p()).unwrap();
        assert!(l.execers.contains("my-runner"));
        assert!(l.non_execers.contains("jq"));
    }

    #[test]
    fn parses_single_column_can_form() {
        let l = parse("can:my-runner\ncannot:jq\n", &p()).unwrap();
        assert!(l.execers.contains("my-runner"));
        assert!(l.non_execers.contains("jq"));
    }

    #[test]
    fn whitespace_separator_works() {
        let l = parse("exec  my-runner\nnoexec\tjq\n", &p()).unwrap();
        assert!(l.execers.contains("my-runner"));
        assert!(l.non_execers.contains("jq"));
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let l = parse("# this is a comment\n\nexec,foo\n", &p()).unwrap();
        assert_eq!(l.execers.len(), 1);
        assert!(l.execers.contains("foo"));
    }

    #[test]
    fn store_paths_are_indexed_by_basename() {
        // binlore typically emits absolute paths.
        let l = parse("can:/nix/store/abc-coreutils/bin/cat\n", &p()).unwrap();
        assert!(l.execers.contains("cat"));
        assert!(!l.execers.contains("/nix/store/abc-coreutils/bin/cat"));
    }

    #[test]
    fn unknown_verb_is_rejected() {
        let err = parse("maybe,foo\n", &p()).unwrap_err();
        assert!(matches!(err, LoreError::UnknownVerb { .. }));
    }

    #[test]
    fn malformed_two_col_row_is_rejected() {
        let err = parse("exec\n", &p()).unwrap_err();
        assert!(matches!(err, LoreError::Malformed { .. }));
    }

    #[test]
    fn override_for_returns_lore_verdict() {
        let l = parse("exec,foo\nnoexec,bar\n", &p()).unwrap();
        assert_eq!(l.override_for("foo"), Some(true));
        assert_eq!(l.override_for("bar"), Some(false));
        assert_eq!(l.override_for("baz"), None);
    }

    #[test]
    fn merge_combines_two_sources() {
        let mut a = parse("exec,a\n", &p()).unwrap();
        let b = parse("noexec,b\n", &p()).unwrap();
        a.merge(b);
        assert!(a.execers.contains("a"));
        assert!(a.non_execers.contains("b"));
    }
}
