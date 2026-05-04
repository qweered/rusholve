//! Parsers for `allow=` / `map=` / `skip=` directive grammars, shared
//! across CLI flags and inline `# rusholve: …` pragmas.

use thiserror::Error;

use super::{AllowDirective, AllowScope, Directives, MapDirective, SkipDirective};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DirectiveError {
    #[error("invalid allow directive `{raw}`: expected `<scope>=<name>` where scope is one of function|alias|builtin|special-builtin|keyword")]
    InvalidAllow { raw: String },
    #[error("invalid map directive `{raw}`: expected `<name>=<replacement>`")]
    InvalidMap { raw: String },
    #[error("invalid pragma on line {line}: {detail}")]
    InvalidPragma { line: usize, detail: String },
}

pub fn parse_cli_allow(raw: &str) -> Result<AllowDirective, DirectiveError> {
    let (scope_str, name) = raw
        .split_once('=')
        .ok_or_else(|| DirectiveError::InvalidAllow {
            raw: raw.to_string(),
        })?;
    let scope = match scope_str {
        "function" => AllowScope::Function,
        "alias" => AllowScope::Alias,
        "builtin" => AllowScope::Builtin,
        "special-builtin" | "special_builtin" => AllowScope::SpecialBuiltin,
        "keyword" => AllowScope::Keyword,
        _ => {
            return Err(DirectiveError::InvalidAllow {
                raw: raw.to_string(),
            })
        }
    };
    if name.is_empty() {
        return Err(DirectiveError::InvalidAllow {
            raw: raw.to_string(),
        });
    }
    Ok(AllowDirective {
        scope,
        name: name.to_string(),
    })
}

pub fn parse_cli_map(raw: &str) -> Result<MapDirective, DirectiveError> {
    let (name, replacement) = raw
        .split_once('=')
        .ok_or_else(|| DirectiveError::InvalidMap {
            raw: raw.to_string(),
        })?;
    if name.is_empty() || replacement.is_empty() {
        return Err(DirectiveError::InvalidMap {
            raw: raw.to_string(),
        });
    }
    Ok(MapDirective {
        name: name.to_string(),
        replacement: replacement.to_string(),
    })
}

pub fn parse_cli_skip(raw: &str) -> SkipDirective {
    SkipDirective {
        pattern: raw.to_string(),
    }
}

/// Scan source for `# rusholve: <verb> <arg>` lines. Pragmas can appear
/// anywhere; they are accumulated in source order. Errors are reported
/// alongside successful directives so the caller can report all
/// problems in one pass.
pub fn parse_inline(source: &str) -> (Directives, Vec<DirectiveError>) {
    let mut directives = Directives::new();
    let mut errors = Vec::new();

    for (idx, line) in source.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("rusholve:") else {
            continue;
        };
        let rest = rest.trim();

        let (verb, arg) = match rest.split_once(char::is_whitespace) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };

        match verb {
            "allow" => match parse_cli_allow(arg) {
                Ok(d) => directives.allow.push(d),
                Err(e) => errors.push(DirectiveError::InvalidPragma {
                    line: line_no,
                    detail: e.to_string(),
                }),
            },
            "map" => match parse_cli_map(arg) {
                Ok(d) => directives.map.push(d),
                Err(e) => errors.push(DirectiveError::InvalidPragma {
                    line: line_no,
                    detail: e.to_string(),
                }),
            },
            "skip" => {
                if arg.is_empty() {
                    errors.push(DirectiveError::InvalidPragma {
                        line: line_no,
                        detail: "skip requires a pattern argument".into(),
                    });
                } else {
                    directives.skip.push(parse_cli_skip(arg));
                }
            }
            other => errors.push(DirectiveError::InvalidPragma {
                line: line_no,
                detail: format!("unknown verb `{other}` (expected allow|map|skip)"),
            }),
        }
    }

    (directives, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_parses_each_scope() {
        for (text, expected) in [
            ("function=foo", AllowScope::Function),
            ("alias=ll", AllowScope::Alias),
            ("builtin=echo", AllowScope::Builtin),
            ("special-builtin=eval", AllowScope::SpecialBuiltin),
            ("keyword=time", AllowScope::Keyword),
        ] {
            let d = parse_cli_allow(text).unwrap();
            assert_eq!(d.scope, expected);
        }
    }

    #[test]
    fn allow_rejects_unknown_scope() {
        assert!(parse_cli_allow("widget=foo").is_err());
        assert!(parse_cli_allow("function=").is_err());
        assert!(parse_cli_allow("missing-equals").is_err());
    }

    #[test]
    fn map_parses_name_replacement() {
        let d = parse_cli_map("jq=/run/wrappers/bin/jq").unwrap();
        assert_eq!(d.name, "jq");
        assert_eq!(d.replacement, "/run/wrappers/bin/jq");
    }

    #[test]
    fn map_replacement_can_contain_equals_signs() {
        let d = parse_cli_map("foo=KEY=value").unwrap();
        assert_eq!(d.replacement, "KEY=value");
    }

    #[test]
    fn map_rejects_empty_sides() {
        assert!(parse_cli_map("=value").is_err());
        assert!(parse_cli_map("name=").is_err());
        assert!(parse_cli_map("missing-equals").is_err());
    }

    #[test]
    fn skip_accepts_arbitrary_pattern() {
        assert_eq!(parse_cli_skip("$VAR").pattern, "$VAR");
        assert_eq!(parse_cli_skip("./local").pattern, "./local");
    }

    #[test]
    fn parse_inline_collects_pragmas() {
        let src = r#"#!/usr/bin/env bash
# rusholve: allow function=helper
# rusholve: map jq=/usr/bin/jq
# rusholve: skip $RUNTIME

helper
"#;
        let (d, errs) = parse_inline(src);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(d.allow.len(), 1);
        assert_eq!(d.allow[0].name, "helper");
        assert_eq!(d.map.len(), 1);
        assert_eq!(d.map[0].name, "jq");
        assert_eq!(d.skip.len(), 1);
        assert_eq!(d.skip[0].pattern, "$RUNTIME");
    }

    #[test]
    fn parse_inline_ignores_unrelated_comments() {
        let src = "# regular comment\n# todo: nothing\necho hi\n";
        let (d, errs) = parse_inline(src);
        assert!(d.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn parse_inline_reports_errors_with_line_numbers() {
        let src = "# rusholve: allow widget=foo\n# rusholve: nope\n# rusholve: skip\n";
        let (_d, errs) = parse_inline(src);
        assert_eq!(errs.len(), 3);
        for e in &errs {
            assert!(matches!(e, DirectiveError::InvalidPragma { .. }));
        }
    }

    #[test]
    fn parse_inline_handles_indented_pragmas() {
        let src = "    # rusholve: allow function=foo\n";
        let (d, errs) = parse_inline(src);
        assert!(errs.is_empty());
        assert_eq!(d.allow.len(), 1);
    }
}
