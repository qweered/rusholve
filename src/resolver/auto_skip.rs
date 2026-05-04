//! Recognize "well-known dynamic" command words that should silently
//! pass through resolution rather than being flagged as unresolved.
//!
//! These are positional / special-variable forms a script uses to forward
//! its own arguments to whatever it calls. Resholve makes the user spell
//! these out via `--keep`; rusholve v0.2 auto-detects them.
//!
//! Examples that classify as well-known:
//!
//! ```text
//! "$@" rest      # invoke whatever the script was called with
//! $1 args        # invoke first positional arg
//! exec "$@"      # exec into our own args
//! ```
//!
//! When the resolver sees a dynamic command word and the *literal source
//! text* matches one of these patterns, it emits [`Solution::Allowed`]
//! with reason `"well-known-dynamic"` instead of
//! [`UnresolvedKind::DynamicCommandName`].

/// True if `text` is a positional / special-variable form we'll pass
/// through silently. Whitespace is not stripped — caller passes the raw
/// source bytes of the command word, including any wrapping quotes.
pub fn is_well_known_dynamic(text: &str) -> bool {
    let inner = strip_quotes(text.trim());
    matches!(
        inner,
        // The argument-passthrough forms.
        "$@" | "$*"
        // ${@} / ${*} braced forms.
        | "${@}" | "${*}"
        // Positional arguments. We accept the unbraced 0–9 form bash
        // mandates (positional args ≥10 must be braced).
        | "$0" | "$1" | "$2" | "$3" | "$4"
        | "$5" | "$6" | "$7" | "$8" | "$9"
        // Specials: pid, last bg pid, last exit, last arg, #args, shell flags.
        | "$$" | "$!" | "$?" | "$_" | "$#" | "$-"
        // Braced single-digit positionals (less common but valid).
        | "${0}" | "${1}" | "${2}" | "${3}" | "${4}"
        | "${5}" | "${6}" | "${7}" | "${8}" | "${9}"
        // Braced specials.
        | "${$}" | "${!}" | "${?}" | "${_}" | "${#}" | "${-}"
    ) || is_braced_positional(inner)
}

/// `${10}`, `${11}`, etc. — positional args ≥10 in braced form.
fn is_braced_positional(s: &str) -> bool {
    let Some(inner) = s.strip_prefix("${").and_then(|t| t.strip_suffix('}')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit())
}

/// Strip a single layer of matching `"…"` or `'…'` from `s`. The whole
/// thing must be quoted (`"$@"` works; `"x"$@"y"` does not).
fn strip_quotes(s: &str) -> &str {
    if let Some(inner) = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .filter(|t| !t.contains('"'))
    {
        return inner;
    }
    if let Some(inner) = s
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .filter(|t| !t.contains('\''))
    {
        return inner;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_passthrough_forms() {
        assert!(is_well_known_dynamic("$@"));
        assert!(is_well_known_dynamic(r#""$@""#));
        assert!(is_well_known_dynamic("$*"));
        assert!(is_well_known_dynamic(r#""$*""#));
        assert!(is_well_known_dynamic("${@}"));
    }

    #[test]
    fn positionals_unbraced() {
        for n in 0..=9 {
            let s = format!("${n}");
            assert!(is_well_known_dynamic(&s), "expected {s} to be allowed");
        }
    }

    #[test]
    fn positionals_braced_single_digit() {
        for n in 0..=9 {
            let s = format!("${{{n}}}");
            assert!(is_well_known_dynamic(&s), "expected {s} to be allowed");
        }
    }

    #[test]
    fn positionals_braced_two_digit() {
        assert!(is_well_known_dynamic("${10}"));
        assert!(is_well_known_dynamic("${42}"));
    }

    #[test]
    fn special_vars() {
        for s in ["$$", "$!", "$?", "$_", "$#", "$-"] {
            assert!(is_well_known_dynamic(s), "expected {s} allowed");
        }
    }

    #[test]
    fn unrelated_dynamic_is_not_allowed() {
        assert!(!is_well_known_dynamic("$cmd"));
        assert!(!is_well_known_dynamic("${cmd}"));
        assert!(!is_well_known_dynamic("${PATH}"));
        assert!(!is_well_known_dynamic("$BASH_SOURCE"));
    }

    #[test]
    fn double_dollar_one_is_not_allowed() {
        // `$11` is a positional arg in shell parlance only when braced as
        // `${11}` — `$11` means "$1" followed by literal "1". We don't
        // try to be cleverer than that.
        assert!(!is_well_known_dynamic("$11"));
    }

    #[test]
    fn empty_braces_rejected() {
        assert!(!is_well_known_dynamic("${}"));
    }
}
