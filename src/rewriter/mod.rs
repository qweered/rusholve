//! Span-keyed source rewriter.
//!
//! Given the original source plus a `Vec<Solution>`, produces the
//! resolved source. Only [`Solution::Resolved`] entries contribute
//! edits; the others are advisory. Edits are sorted by span start and
//! must be non-overlapping (debug-asserted; an overlap indicates an IR
//! bug, not a recoverable input).

use crate::ir::Span;
use crate::resolver::Solution;

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("overlapping edits at byte ranges [{a_start}..{a_end}] and [{b_start}..{b_end}]")]
    OverlappingEdits {
        a_start: usize,
        a_end: usize,
        b_start: usize,
        b_end: usize,
    },
    #[error("edit span [{start}..{end}] is out of bounds for source of length {len}")]
    OutOfBounds {
        start: usize,
        end: usize,
        len: usize,
    },
}

/// Apply every `Resolved` edit in `solutions` to `source`. Returns the
/// rewritten source. Solutions are sorted internally; ordering of the
/// input does not matter.
pub fn rewrite(source: &str, solutions: &[Solution]) -> Result<String, RewriteError> {
    let mut edits: Vec<(Span, &str)> = solutions
        .iter()
        .filter_map(|s| match s {
            Solution::Resolved {
                initial,
                replacement,
                ..
            } => Some((*initial, replacement.as_str())),
            _ => None,
        })
        .collect();
    edits.sort_by_key(|(span, _)| *span);

    // Validate ranges.
    for (span, _) in &edits {
        if span.end > source.len() {
            return Err(RewriteError::OutOfBounds {
                start: span.start,
                end: span.end,
                len: source.len(),
            });
        }
    }
    for w in edits.windows(2) {
        if w[0].0.end > w[1].0.start {
            return Err(RewriteError::OverlappingEdits {
                a_start: w[0].0.start,
                a_end: w[0].0.end,
                b_start: w[1].0.start,
                b_end: w[1].0.end,
            });
        }
    }

    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    for (span, replacement) in edits {
        out.push_str(&source[cursor..span.start]);
        out.push_str(replacement);
        cursor = span.end;
    }
    out.push_str(&source[cursor..]);
    Ok(out)
}

/// Auto-shebang: if `source` starts with `#!` (any line ending), return
/// it unchanged. Otherwise, prepend `#!<interpreter>\n` so the resolved
/// script is executable. The interpreter argument is treated literally —
/// pass `/usr/bin/env bash` for portability or a `/nix/store/.../bin/bash`
/// for hermetic builds.
pub fn ensure_shebang(source: &str, interpreter: &str) -> String {
    if source.starts_with("#!") {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len() + interpreter.len() + 4);
    out.push_str("#!");
    out.push_str(interpreter);
    out.push('\n');
    out.push_str(source);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourceId;
    use crate::resolver::ResolvedKind;

    const SID: SourceId = SourceId::new(0);

    fn resolved(start: usize, end: usize, replacement: &str) -> Solution {
        Solution::Resolved {
            source_id: SID,
            initial: Span::new(start, end),
            original: String::new(),
            replacement: replacement.into(),
            kind: ResolvedKind::External,
        }
    }

    #[test]
    fn no_edits_returns_source_unchanged() {
        let src = "echo hi";
        assert_eq!(rewrite(src, &[]).unwrap(), src);
    }

    #[test]
    fn single_edit_splices_correctly() {
        let src = "git status";
        let out = rewrite(src, &[resolved(0, 3, "/bin/git")]).unwrap();
        assert_eq!(out, "/bin/git status");
    }

    #[test]
    fn multiple_edits_apply_in_source_order() {
        let src = "git push && jq .";
        let out = rewrite(
            src,
            &[resolved(12, 14, "/bin/jq"), resolved(0, 3, "/bin/git")],
        )
        .unwrap();
        assert_eq!(out, "/bin/git push && /bin/jq .");
    }

    #[test]
    fn ignores_non_resolved_solutions() {
        let src = "echo hi";
        let sols = vec![Solution::InScope {
            source_id: SID,
            span: Span::new(0, 4),
            name: "echo".into(),
            kind: crate::resolver::InScopeKind::Builtin,
        }];
        assert_eq!(rewrite(src, &sols).unwrap(), src);
    }

    #[test]
    fn rejects_overlapping_edits() {
        let src = "abcdef";
        let result = rewrite(src, &[resolved(0, 3, "X"), resolved(2, 5, "Y")]);
        assert!(matches!(result, Err(RewriteError::OverlappingEdits { .. })));
    }

    #[test]
    fn rejects_out_of_bounds_edits() {
        let src = "abc";
        let result = rewrite(src, &[resolved(0, 100, "X")]);
        assert!(matches!(result, Err(RewriteError::OutOfBounds { .. })));
    }

    #[test]
    fn replacement_can_be_longer_than_original() {
        let src = "j .";
        let out = rewrite(src, &[resolved(0, 1, "/very/long/path/jq")]).unwrap();
        assert_eq!(out, "/very/long/path/jq .");
    }

    #[test]
    fn ensure_shebang_prepends_when_missing() {
        let out = ensure_shebang("echo hi\n", "/usr/bin/env bash");
        assert_eq!(out, "#!/usr/bin/env bash\necho hi\n");
    }

    #[test]
    fn ensure_shebang_leaves_existing_shebang_alone() {
        let src = "#!/bin/bash\necho hi\n";
        assert_eq!(ensure_shebang(src, "/usr/bin/env bash"), src);
    }

    #[test]
    fn ensure_shebang_handles_empty_source() {
        let out = ensure_shebang("", "/bin/sh");
        assert_eq!(out, "#!/bin/sh\n");
    }

    #[test]
    fn replacement_can_be_empty() {
        let src = "rm -f file";
        let out = rewrite(src, &[resolved(3, 5, "")]).unwrap();
        // The space between -f and file collapses to a single space.
        assert_eq!(out, "rm  file");
    }
}
