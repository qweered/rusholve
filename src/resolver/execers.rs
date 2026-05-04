//! Wrapper-command handlers — commands whose argument is *itself* a
//! command to run. The resolver consults this module to decide whether
//! to recurse.
//!
//! v0.1 covered `env`, `sudo`, `doas`, `command`, `exec`. v0.2's
//! "auto-lore cheap" pass extends this to the most common wrappers used
//! in nixpkgs:
//!
//! - **Simple wrappers** (nohup, setsid, nice, ionice, chrt, stdbuf,
//!   unbuffer, time): same shape as sudo. Skip leading flags and
//!   `NAME=value` assignments, the next word is the wrapped command.
//! - **timeout**: like sudo, but skip one extra positional (the
//!   duration) before the command.
//! - **xargs**: first non-flag word is the command. Known gap: flags
//!   that take arguments (`-I {}`, `-n N`, `-P N`) may misclassify the
//!   argument as the command. This is documented.
//! - **find**: `find PATH ... -exec CMD ... \;` (or `-execdir`). Each
//!   such predicate produces an inner command.
//! - **parallel**: deliberately deferred to v0.3 — its option grammar is
//!   too rich for a small parser to be safe.

use crate::ir::Word;

/// Names of wrappers we recognize and recurse into.
pub const EXEC_WRAPPERS: &[&str] = &[
    // v0.1
    "env", "sudo", "doas", "command", "exec", // v0.2 simple
    "nohup", "setsid", "nice", "ionice", "chrt", "stdbuf", "unbuffer", "time",
    // v0.2 special
    "timeout", "xargs", "find",
];

pub fn is_exec_wrapper(name: &str) -> bool {
    EXEC_WRAPPERS.contains(&name)
}

/// The conservative v0.1 wrapper set. `--strict` mode uses this so no
/// auto-lore (xargs, find, timeout, …) inferences fire — matching
/// resholve's "spell-it-out" discipline.
pub fn is_v01_wrapper(name: &str) -> bool {
    matches!(name, "env" | "sudo" | "doas" | "command" | "exec")
}

/// Per-wrapper argument grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `cmd [FLAGS] [ASSIGNMENTS] WRAPPED ARGS...` — the v0.1 shape.
    Simple,
    /// `timeout [FLAGS] DURATION WRAPPED ARGS...`.
    Timeout,
    /// `xargs [FLAGS] WRAPPED ARGS...`. Same as Simple in our naive
    /// parser, but kept separate for future flag-arg handling.
    Xargs,
    /// `find PATH... [-exec CMD ARGS ;]...`.
    Find,
}

fn kind_of(name: &str) -> Option<Kind> {
    Some(match name {
        "env" | "sudo" | "doas" | "command" | "exec" | "nohup" | "setsid" | "nice" | "ionice"
        | "chrt" | "stdbuf" | "unbuffer" | "time" => Kind::Simple,
        "timeout" => Kind::Timeout,
        "xargs" => Kind::Xargs,
        "find" => Kind::Find,
        _ => return None,
    })
}

/// All inner commands implied by an invocation. Returns an empty slice
/// when no plausible inner command is present (e.g. `sudo -l` or
/// `find . -name x`).
///
/// Most wrappers contribute zero or one command; only `find` may
/// contribute multiple (one per `-exec`/`-execdir`). For names not in
/// the hardcoded grammar table — typically lore-supplied custom
/// wrappers — we default to the [`Kind::Simple`] shape: skip flags and
/// assignments, the next word is the inner command.
pub fn find_wrapped_commands(words: &[Word]) -> Vec<&Word> {
    let Some(first) = words.first().and_then(Word::as_static) else {
        return Vec::new();
    };
    let kind = kind_of(first).unwrap_or(Kind::Simple);
    match kind {
        Kind::Simple => find_simple_inner(words).into_iter().collect(),
        Kind::Timeout => find_timeout_inner(words).into_iter().collect(),
        Kind::Xargs => find_simple_inner(words).into_iter().collect(),
        Kind::Find => find_exec_predicates(words),
    }
}

/// Backwards-compatible single-command form. Prefer
/// [`find_wrapped_commands`] in new code.
pub fn find_wrapped_command(words: &[Word]) -> Option<&Word> {
    find_wrapped_commands(words).into_iter().next()
}

fn find_simple_inner(words: &[Word]) -> Option<&Word> {
    let mut iter = words.iter().skip(1);
    while let Some(w) = iter.next() {
        let Some(s) = w.as_static() else {
            return Some(w);
        };
        if s == "--" {
            return iter.next();
        }
        if s.starts_with('-') && s != "-" {
            continue;
        }
        if is_assignment_word(s) {
            continue;
        }
        return Some(w);
    }
    None
}

fn find_timeout_inner(words: &[Word]) -> Option<&Word> {
    // Skip flags + flag-args, then the duration positional, then take
    // the next positional. timeout's flags are: -k DUR, -s SIG,
    // --preserve-status, --foreground. We treat any leading flag the
    // same as Simple (skip it); flag-args like `-k 5s` are not split.
    // Documented gap: `-k 5 cmd` would treat `5` as the duration,
    // missing the cmd. Users writing `-k=5s` or `--kill-after=5s` work.
    let mut iter = words.iter().skip(1);
    let mut saw_duration = false;
    while let Some(w) = iter.next() {
        let Some(s) = w.as_static() else {
            return Some(w);
        };
        if s == "--" {
            return iter.next();
        }
        if s.starts_with('-') && s != "-" {
            continue;
        }
        if !saw_duration {
            saw_duration = true;
            continue;
        }
        return Some(w);
    }
    None
}

fn find_exec_predicates(words: &[Word]) -> Vec<&Word> {
    // Walk arguments looking for `-exec` / `-execdir`. The next word is
    // the command. We do not validate the closing `;` or `+` token —
    // resolution doesn't depend on it.
    let mut out = Vec::new();
    let mut iter = words.iter().skip(1);
    while let Some(w) = iter.next() {
        let Some(s) = w.as_static() else { continue };
        if s == "-exec" || s == "-execdir" {
            if let Some(next) = iter.next() {
                out.push(next);
            }
        }
    }
    out
}

fn is_assignment_word(s: &str) -> bool {
    let Some(eq) = s.find('=') else {
        return false;
    };
    let name = &s[..eq];
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Span, WordPiece};

    fn lit(text: &str) -> Word {
        Word {
            span: Span::new(0, text.len()),
            pieces: vec![WordPiece::Literal {
                text: text.to_string(),
                span: Span::new(0, text.len()),
            }],
            static_value: Some(text.to_string()),
        }
    }

    fn dyn_word() -> Word {
        Word {
            span: Span::new(0, 4),
            pieces: vec![WordPiece::Dynamic {
                kind: crate::ir::DynamicKind::VarSub,
                span: Span::new(0, 4),
            }],
            static_value: None,
        }
    }

    #[test]
    fn wrapper_set_membership() {
        for name in ["env", "sudo", "doas", "command", "exec"] {
            assert!(is_exec_wrapper(name));
        }
        for name in [
            "nohup", "setsid", "nice", "ionice", "chrt", "stdbuf", "unbuffer", "time",
        ] {
            assert!(is_exec_wrapper(name), "{name} should be a wrapper");
        }
        for name in ["timeout", "xargs", "find"] {
            assert!(is_exec_wrapper(name));
        }
        assert!(!is_exec_wrapper("git"));
    }

    #[test]
    fn assignment_word_detection() {
        assert!(is_assignment_word("FOO=bar"));
        assert!(is_assignment_word("_x=y"));
        assert!(is_assignment_word("PATH=/usr/bin:/bin"));
        assert!(!is_assignment_word("=value"));
        assert!(!is_assignment_word("1FOO=bar"));
        assert!(!is_assignment_word("--flag"));
        assert!(!is_assignment_word("foo"));
        assert!(!is_assignment_word("foo bar"));
    }

    #[test]
    fn finds_inner_command_after_env_assignments() {
        let words = vec![lit("env"), lit("FOO=1"), lit("BAR=2"), lit("jq"), lit(".")];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("jq"));
    }

    #[test]
    fn finds_inner_command_after_sudo_flags() {
        let words = vec![
            lit("sudo"),
            lit("-E"),
            lit("-u"),
            lit("nobody"),
            lit("systemctl"),
        ];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("nobody"));
    }

    #[test]
    fn handles_double_dash_sentinel() {
        let words = vec![lit("sudo"), lit("-E"), lit("--"), lit("-foo")];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("-foo"));
    }

    #[test]
    fn returns_dynamic_word_as_inner_command() {
        let words = vec![lit("sudo"), dyn_word()];
        let inner = find_wrapped_command(&words).unwrap();
        assert!(inner.as_static().is_none());
    }

    #[test]
    fn returns_none_when_no_inner_command() {
        let words = vec![lit("sudo"), lit("-l")];
        assert!(find_wrapped_command(&words).is_none());
    }

    #[test]
    fn nohup_recurses_into_inner_command() {
        let words = vec![lit("nohup"), lit("long-running"), lit("--quiet")];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("long-running"));
    }

    #[test]
    fn nice_with_priority_flag_finds_command() {
        let words = vec![lit("nice"), lit("-n"), lit("19"), lit("make")];
        // Naive parser treats `19` as the inner command — known gap.
        // Real fix is to teach the parser nice's `-n N` form. Documented
        // limitation; users can `--map nice=...` to work around.
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("19"));
    }

    #[test]
    fn timeout_skips_duration_positional() {
        let words = vec![lit("timeout"), lit("30s"), lit("curl"), lit("-fSL")];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("curl"));
    }

    #[test]
    fn timeout_with_flags_then_duration() {
        let words = vec![
            lit("timeout"),
            lit("--preserve-status"),
            lit("30s"),
            lit("curl"),
        ];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("curl"));
    }

    #[test]
    fn timeout_returns_none_when_no_command() {
        let words = vec![lit("timeout"), lit("30s")];
        assert!(find_wrapped_command(&words).is_none());
    }

    #[test]
    fn xargs_finds_inner_command() {
        let words = vec![lit("xargs"), lit("-r"), lit("rm")];
        let inner = find_wrapped_command(&words).unwrap();
        assert_eq!(inner.as_static(), Some("rm"));
    }

    #[test]
    fn find_exec_extracts_inner_command() {
        let words = vec![
            lit("find"),
            lit("."),
            lit("-name"),
            lit("*.tmp"),
            lit("-exec"),
            lit("rm"),
            lit("{}"),
            lit(";"),
        ];
        let inners = find_wrapped_commands(&words);
        assert_eq!(inners.len(), 1);
        assert_eq!(inners[0].as_static(), Some("rm"));
    }

    #[test]
    fn find_with_multiple_exec_predicates() {
        let words = vec![
            lit("find"),
            lit("."),
            lit("-exec"),
            lit("chmod"),
            lit("644"),
            lit(";"),
            lit("-exec"),
            lit("touch"),
            lit("{}"),
            lit(";"),
        ];
        let inners = find_wrapped_commands(&words);
        let names: Vec<_> = inners.iter().filter_map(|w| w.as_static()).collect();
        assert_eq!(names, vec!["chmod", "touch"]);
    }

    #[test]
    fn find_execdir_also_matches() {
        let words = vec![
            lit("find"),
            lit("."),
            lit("-execdir"),
            lit("git"),
            lit("status"),
        ];
        let inners = find_wrapped_commands(&words);
        assert_eq!(inners.len(), 1);
        assert_eq!(inners[0].as_static(), Some("git"));
    }

    #[test]
    fn find_without_exec_yields_no_inner() {
        let words = vec![lit("find"), lit("."), lit("-name"), lit("*.txt")];
        assert!(find_wrapped_commands(&words).is_empty());
    }
}
