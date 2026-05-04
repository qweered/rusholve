//! Regex-based scan over raw source. Approximate but conservative:
//! keywords must appear at command position (start-of-line or after a
//! separator), and we strip `#` line comments before matching to suppress
//! the most common false positive. Remaining false positives are accepted
//! in exchange for never silently mis-rewriting.

use std::sync::LazyLock;

use regex::Regex;

use super::{line_col_of, HardStopKind, UnsupportedConstruct};
use crate::ir::Span;

struct Pattern {
    kind: HardStopKind,
    re: Regex,
}

fn cmd_pos(keyword: &str) -> Regex {
    // Match `<keyword>` preceded by start-of-line or a shell separator
    // character. The keyword itself is captured as group 1 so callers can
    // get its own span, not the separator's span.
    let escaped = regex::escape(keyword);
    Regex::new(&format!(r"(?m)(?:^|[ \t;|&{{}}()])({escaped})\b")).expect("static pattern compiles")
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    vec![
        Pattern {
            kind: HardStopKind::SelectStatement,
            re: cmd_pos("select"),
        },
        Pattern {
            kind: HardStopKind::CoprocKeyword,
            re: cmd_pos("coproc"),
        },
        Pattern {
            kind: HardStopKind::DisownBuiltin,
            re: cmd_pos("disown"),
        },
        Pattern {
            kind: HardStopKind::LogoutBuiltin,
            re: cmd_pos("logout"),
        },
        Pattern {
            kind: HardStopKind::LocaleQuoted,
            re: Regex::new(r#"\$""#).expect("static pattern compiles"),
        },
    ]
});

/// Returns the byte offset (relative to `line`) where a `#` line comment
/// begins, or `line.len()` if there is none. Skips `#` inside single- or
/// double-quoted regions.
fn comment_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_single {
            if c == b'\'' {
                in_single = false;
            }
            i += 1;
        } else if in_double {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_double = false;
            }
            i += 1;
        } else {
            match c {
                b'\'' => {
                    in_single = true;
                    i += 1;
                }
                b'"' => {
                    in_double = true;
                    i += 1;
                }
                b'#' if i == 0 || bytes[i - 1].is_ascii_whitespace() => return i,
                _ => i += 1,
            }
        }
    }
    bytes.len()
}

/// Per-line comment ranges (absolute byte offsets) so regex matches inside
/// comments can be filtered.
fn comment_ranges(source: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let bytes = source.as_bytes();
    let mut line_start = 0;
    for i in 0..=bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            let line = &source[line_start..i];
            let cstart_rel = comment_start(line);
            if cstart_rel < line.len() {
                ranges.push((line_start + cstart_rel, i));
            }
            line_start = i + 1;
        }
    }
    ranges
}

/// Token-level scan. Returns hard stops in source-order.
pub fn scan_tokens(source: &str) -> Vec<UnsupportedConstruct> {
    let comments = comment_ranges(source);
    let in_comment = |off: usize| comments.iter().any(|&(s, e)| off >= s && off < e);

    let mut hits = Vec::new();
    for p in PATTERNS.iter() {
        for m in p.re.captures_iter(source) {
            // Keyword patterns capture the keyword as group 1; LocaleQuoted
            // has no capture and we use the whole match.
            let (mstart, mend, snippet) = match m.get(1) {
                Some(g) => (g.start(), g.end(), g.as_str().to_string()),
                None => {
                    let full = m.get(0).expect("regex match has group 0");
                    (full.start(), full.end(), full.as_str().to_string())
                }
            };
            if in_comment(mstart) {
                continue;
            }
            let (line, column) = line_col_of(source, mstart);
            hits.push(UnsupportedConstruct {
                kind: p.kind,
                span: Span::new(mstart, mend),
                line,
                column,
                snippet,
            });
        }
    }
    hits.sort_by_key(|h| h.span.start);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<HardStopKind> {
        scan_tokens(source).into_iter().map(|h| h.kind).collect()
    }

    #[test]
    fn select_at_command_position_is_caught() {
        assert_eq!(
            kinds("select x in a b c; do echo $x; done"),
            vec![HardStopKind::SelectStatement]
        );
    }

    #[test]
    fn select_in_comment_is_skipped() {
        assert_eq!(kinds("# select all rows then process\n"), Vec::new());
    }

    #[test]
    fn select_after_inline_comment_is_skipped() {
        assert_eq!(kinds("ls # select x\n"), Vec::new());
    }

    #[test]
    fn coproc_is_caught() {
        let hits = scan_tokens("coproc { sleep 1; }");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HardStopKind::CoprocKeyword);
        assert_eq!(hits[0].snippet, "coproc");
    }

    #[test]
    fn coprocessing_is_not_a_keyword() {
        assert!(kinds("coprocessing arg").is_empty());
    }

    #[test]
    fn disown_and_logout_are_caught() {
        assert_eq!(
            kinds("disown -a\nlogout"),
            vec![HardStopKind::DisownBuiltin, HardStopKind::LogoutBuiltin]
        );
    }

    #[test]
    fn dollar_double_quote_is_caught_as_locale() {
        let hits = scan_tokens(r#"echo $"hello""#);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HardStopKind::LocaleQuoted);
    }

    #[test]
    fn ansi_c_quote_is_not_locale() {
        assert!(kinds(r"var=$'\n'").is_empty());
    }

    #[test]
    fn double_quoted_dollar_is_not_locale() {
        assert!(kinds(r#"echo "$x""#).is_empty());
    }

    #[test]
    fn span_points_at_keyword_not_separator() {
        let src = "  ;  select foo";
        let hits = scan_tokens(src);
        assert_eq!(hits.len(), 1);
        assert_eq!(&src[hits[0].span.start..hits[0].span.end], "select");
    }

    #[test]
    fn line_col_is_one_based() {
        let src = "echo a\nselect b";
        let hits = scan_tokens(src);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].column, 1);
    }

    #[test]
    fn multiple_hits_are_sorted() {
        let src = "logout\nselect x\ndisown";
        let hits = scan_tokens(src);
        let starts: Vec<usize> = hits.iter().map(|h| h.span.start).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn comment_inside_single_quotes_is_not_a_comment() {
        assert_eq!(
            comment_start("echo '# not a comment'"),
            "echo '# not a comment'".len()
        );
    }

    #[test]
    fn comment_inside_double_quotes_is_not_a_comment() {
        let line = r##"echo "# not a comment""##;
        assert_eq!(comment_start(line), line.len());
    }

    #[test]
    fn hash_at_start_of_line_starts_comment() {
        assert_eq!(comment_start("#!/usr/bin/env bash"), 0);
    }

    #[test]
    fn hash_after_word_without_space_is_not_a_comment() {
        // Bash: `foo#bar` is a single word; `#` is only a comment when
        // preceded by start-of-line or whitespace.
        assert_eq!(comment_start("foo#bar"), "foo#bar".len());
    }
}
