//! Fold directives over the resolver's solution stream.
//!
//! Precedence (per Unresolved): `map` first (most specific — pins a
//! replacement), then `allow` (reclassifies as in-scope), then `skip`
//! (last-resort acceptance via source-text match). First match wins
//! per directive type; multiple directive types compose because they
//! target different solutions.

use crate::resolver::{parse_var_name, InScopeKind, Inputs, Solution, UnresolvedKind};

use super::{AllowScope, Directives};

/// Apply user directives over the resolver's solution stream.
///
/// `inputs` is consulted for `--map $VAR=name` directives whose
/// replacement is a bare command name (no leading `/`); we resolve
/// it through the same PATH-equivalent the resolver uses, so the
/// replacement spliced into the rewritten script is an absolute
/// `/nix/store/.../bin/<name>` path. Pass `None` to disable that
/// inputs lookup (useful in unit tests that don't care).
pub fn apply(
    directives: &Directives,
    solutions: &mut [Solution],
    source: &str,
    inputs: Option<&Inputs>,
) {
    for s in solutions.iter_mut() {
        let Solution::Unresolved {
            source_id,
            span,
            name,
            kind,
            ..
        } = s
        else {
            continue;
        };
        let source_id = *source_id;

        // map: precedence is (1) `$VAR=value` patterns matched by
        // source text against dynamic command words, (2) bare-name
        // patterns matched against `name` for unknown externals.
        if let Some(replacement) = lookup_dynamic_map(directives, source, *span, inputs) {
            *s = Solution::Resolved {
                source_id,
                initial: *span,
                original: source[span.start..span.end].to_string(),
                replacement,
                kind: crate::resolver::ResolvedKind::External,
            };
            continue;
        }
        if matches!(kind, UnresolvedKind::UnknownExternal) {
            if let Some(n) = name.as_deref() {
                if let Some(m) = directives
                    .map
                    .iter()
                    .find(|m| !is_dollar_pattern(&m.name) && m.name == n)
                {
                    *s = Solution::Resolved {
                        source_id,
                        initial: *span,
                        original: n.to_string(),
                        replacement: m.replacement.clone(),
                        kind: crate::resolver::ResolvedKind::External,
                    };
                    continue;
                }
            }
        }

        // allow: reclassify by user-asserted scope.
        if matches!(kind, UnresolvedKind::UnknownExternal) {
            if let Some(n) = name.as_deref() {
                if let Some(a) = directives.allow.iter().find(|a| a.name == n) {
                    let in_scope = match a.scope {
                        AllowScope::Function => InScopeKind::Function,
                        AllowScope::Alias => InScopeKind::Alias,
                        AllowScope::Builtin => InScopeKind::Builtin,
                        AllowScope::SpecialBuiltin => InScopeKind::SpecialBuiltin,
                        AllowScope::Keyword => InScopeKind::Keyword,
                    };
                    *s = Solution::InScope {
                        source_id,
                        span: *span,
                        name: n.to_string(),
                        kind: in_scope,
                    };
                    continue;
                }
            }
        }

        // skip: literal source-text match against the unresolved span.
        let span_text = &source[span.start..span.end];
        if let Some(sk) = directives.skip.iter().find(|sk| sk.pattern == span_text) {
            *s = Solution::Allowed {
                source_id,
                span: *span,
                name: name.clone(),
                reason: format!("user `skip {pat}`", pat = sk.pattern),
            };
        }
    }
}

/// True if a map directive's `name` targets a dynamic command word
/// (e.g. `$AAXTOMP3`, `${VAR}`) rather than a static command name.
fn is_dollar_pattern(name: &str) -> bool {
    name.starts_with('$')
}

/// Resolve a `--map $VAR=value` directive to a concrete replacement
/// string for the dynamic command word at `span`. Matching is
/// shell-aware: pattern `$VAR` matches both `$VAR` and `${VAR}` in
/// source. If the replacement is a bare name (no `/`), look it up in
/// `inputs`. Returns `None` if no map directive matches or the inputs
/// lookup fails.
fn lookup_dynamic_map(
    directives: &Directives,
    source: &str,
    span: crate::ir::Span,
    inputs: Option<&Inputs>,
) -> Option<String> {
    let span_text = source.get(span.start..span.end)?;
    // Strip outer double quotes if the dynamic word came from `"$VAR"`.
    let unquoted = span_text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(span_text);
    let span_var = parse_var_name(unquoted)?;
    for m in &directives.map {
        if !is_dollar_pattern(&m.name) {
            continue;
        }
        let map_var = parse_var_name(&m.name)?;
        if map_var != span_var {
            continue;
        }
        // Replacement: absolute path → use as-is. Bare name → look
        // up in inputs (PATH-equivalent).
        if m.replacement.starts_with('/') {
            return Some(m.replacement.clone());
        }
        if let Some(ins) = inputs {
            if let Some(abs) = ins.resolve(&m.replacement) {
                return Some(abs.to_string_lossy().into_owned());
            }
        }
        // Bare name, no inputs available or not found: leave it as the
        // raw replacement so the user at least sees what was attempted.
        return Some(m.replacement.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directives::{AllowDirective, AllowScope, MapDirective, SkipDirective};
    use crate::ir::{SourceId, Span};
    use crate::resolver::{ResolvedKind, Solution, UnresolvedKind};

    /// Test sentinel — every test in this module uses a single source.
    const SID: SourceId = SourceId::new(0);

    fn unresolved(name: &str, span: Span) -> Solution {
        Solution::Unresolved {
            source_id: SID,
            span,
            name: Some(name.to_string()),
            kind: UnresolvedKind::UnknownExternal,
            hint: None,
        }
    }

    #[test]
    fn map_replaces_unresolved_with_resolved() {
        let mut sols = vec![unresolved("jq", Span::new(0, 2))];
        let directives = Directives {
            map: vec![MapDirective {
                name: "jq".into(),
                replacement: "/usr/bin/jq".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, "jq", None);
        match &sols[0] {
            Solution::Resolved {
                replacement, kind, ..
            } => {
                assert_eq!(replacement, "/usr/bin/jq");
                assert_eq!(*kind, ResolvedKind::External);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn allow_demotes_to_in_scope() {
        let mut sols = vec![unresolved("helper", Span::new(0, 6))];
        let directives = Directives {
            allow: vec![AllowDirective {
                scope: AllowScope::Function,
                name: "helper".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, "helper", None);
        assert!(matches!(
            &sols[0],
            Solution::InScope {
                kind: InScopeKind::Function,
                ..
            }
        ));
    }

    #[test]
    fn skip_matches_source_text_and_marks_allowed() {
        let src = "$RUNTIME args";
        let mut sols = vec![Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 8),
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        }];
        let directives = Directives {
            skip: vec![SkipDirective {
                pattern: "$RUNTIME".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, src, None);
        assert!(matches!(&sols[0], Solution::Allowed { .. }));
    }

    #[test]
    fn map_takes_precedence_over_allow() {
        let mut sols = vec![unresolved("foo", Span::new(0, 3))];
        let directives = Directives {
            allow: vec![AllowDirective {
                scope: AllowScope::Function,
                name: "foo".into(),
            }],
            map: vec![MapDirective {
                name: "foo".into(),
                replacement: "/path/foo".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, "foo", None);
        assert!(matches!(&sols[0], Solution::Resolved { .. }));
    }

    #[test]
    fn directives_dont_touch_already_resolved() {
        let mut sols = vec![Solution::Resolved {
            source_id: SID,
            initial: Span::new(0, 3),
            original: "git".into(),
            replacement: "/bin/git".into(),
            kind: ResolvedKind::External,
        }];
        let directives = Directives {
            map: vec![MapDirective {
                name: "git".into(),
                replacement: "/other/git".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, "git", None);
        match &sols[0] {
            Solution::Resolved { replacement, .. } => assert_eq!(replacement, "/bin/git"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn skip_only_matches_exact_text() {
        let src = "$RUNTIMEX args";
        let mut sols = vec![Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 9),
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        }];
        let directives = Directives {
            skip: vec![SkipDirective {
                pattern: "$RUNTIME".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, src, None);
        assert!(matches!(&sols[0], Solution::Unresolved { .. }));
    }

    #[test]
    fn dollar_map_with_absolute_path_resolves_dynamic_word() {
        // `--map $AAXTOMP3=/nix/store/.../bin/aaxtomp3` should rewrite
        // a `$AAXTOMP3` command word to the literal absolute path.
        let src = "$AAXTOMP3 -i input";
        let mut sols = vec![Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 9),
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        }];
        let directives = Directives {
            map: vec![MapDirective {
                name: "$AAXTOMP3".into(),
                replacement: "/nix/store/aaa-aaxtomp3/bin/aaxtomp3".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, src, None);
        match &sols[0] {
            Solution::Resolved { replacement, .. } => {
                assert_eq!(replacement, "/nix/store/aaa-aaxtomp3/bin/aaxtomp3");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn dollar_map_matches_brace_form_too() {
        // `--map $X=…` should also match `${X}` in source.
        let src = "${X} args";
        let mut sols = vec![Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 4),
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        }];
        let directives = Directives {
            map: vec![MapDirective {
                name: "$X".into(),
                replacement: "/abs/path".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, src, None);
        assert!(matches!(&sols[0], Solution::Resolved { .. }));
    }

    #[test]
    fn dollar_map_with_bare_name_falls_back_when_no_inputs() {
        // Without an Inputs lookup, a bare-name replacement is spliced
        // verbatim. (CLI integration test covers the inputs path.)
        let src = "$FIND .";
        let mut sols = vec![Solution::Unresolved {
            source_id: SID,
            span: Span::new(0, 5),
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        }];
        let directives = Directives {
            map: vec![MapDirective {
                name: "$FIND".into(),
                replacement: "find".into(),
            }],
            ..Default::default()
        };
        apply(&directives, &mut sols, src, None);
        match &sols[0] {
            Solution::Resolved { replacement, .. } => assert_eq!(replacement, "find"),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }
}
