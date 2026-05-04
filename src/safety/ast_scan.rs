//! AST-level safety checks. Run after the brush parse so we can inspect
//! AST shape that the token scan can't see.
//!
//! v0.1 covers:
//!
//! - **`wait -n`** — hard stop. Detect SimpleCommand with name `wait`
//!   carrying a `-n` flag in the suffix.
//! - **Signal traps for unsupported signals** — warning. Detect
//!   SimpleCommand with name `trap` whose signal-name args are not in
//!   the set brush fully supports (`DEBUG`, `ERR`, `EXIT`).
//!
//! Deferred to v0.2 (require recursive re-parsing of `$(…)` bodies,
//! which brush's `WordPiece::CommandSubstitution` exposes only as raw
//! strings):
//!
//! - Deeply-nested `$(…)` (brush issue #1040)
//! - `case` pattern containing `)` inside `$(…)` (brush issue #1052)
//! - Unbalanced quote in heredoc inside `"$(…)"` (brush issue #1066)
//! - `$BASH_COMMAND` outside trap context

use brush_parser::ast::{
    AndOr, Command, CompoundCommand, CompoundList, ElseClause, Pipeline, Program, SimpleCommand,
};

use super::{line_col_of, HardStopKind, KnownGap, UnsupportedConstruct, WarningKind};
use crate::ir::Span;

/// Walk the brush AST and surface AST-level safety findings.
pub fn scan_ast(source: &str, ast: &Program) -> (Vec<UnsupportedConstruct>, Vec<KnownGap>) {
    let mut hard_stops = Vec::new();
    let mut warnings = Vec::new();
    for top in &ast.complete_commands {
        walk_compound_list(top, source, &mut hard_stops, &mut warnings);
    }
    (hard_stops, warnings)
}

fn walk_compound_list(
    list: &CompoundList,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    for item in &list.0 {
        let and_or = &item.0;
        walk_pipeline(&and_or.first, source, hs, warns);
        for tail in &and_or.additional {
            let p = match tail {
                AndOr::And(p) | AndOr::Or(p) => p,
            };
            walk_pipeline(p, source, hs, warns);
        }
    }
}

fn walk_pipeline(
    pipeline: &Pipeline,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    for cmd in &pipeline.seq {
        walk_command(cmd, source, hs, warns);
    }
}

fn walk_command(
    cmd: &Command,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    match cmd {
        Command::Simple(simple) => check_simple(simple, source, hs, warns),
        Command::Compound(c, _) => walk_compound(c, source, hs, warns),
        Command::Function(fdef) => walk_compound(&fdef.body.0, source, hs, warns),
        Command::ExtendedTest(_, _) => {}
    }
}

fn walk_compound(
    cc: &CompoundCommand,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    match cc {
        CompoundCommand::BraceGroup(g) => walk_compound_list(&g.list, source, hs, warns),
        CompoundCommand::Subshell(s) => walk_compound_list(&s.list, source, hs, warns),
        CompoundCommand::ForClause(fc) => walk_compound_list(&fc.body.list, source, hs, warns),
        CompoundCommand::ArithmeticForClause(fc) => {
            walk_compound_list(&fc.body.list, source, hs, warns)
        }
        CompoundCommand::WhileClause(w) | CompoundCommand::UntilClause(w) => {
            walk_compound_list(&w.0, source, hs, warns);
            walk_compound_list(&w.1.list, source, hs, warns);
        }
        CompoundCommand::IfClause(ic) => {
            walk_compound_list(&ic.condition, source, hs, warns);
            walk_compound_list(&ic.then, source, hs, warns);
            if let Some(elses) = &ic.elses {
                for ec in elses {
                    walk_else(ec, source, hs, warns);
                }
            }
        }
        CompoundCommand::CaseClause(c) => {
            for item in &c.cases {
                if let Some(body) = &item.cmd {
                    walk_compound_list(body, source, hs, warns);
                }
            }
        }
        CompoundCommand::Coprocess(_) | CompoundCommand::Arithmetic(_) => {}
    }
}

fn walk_else(
    ec: &ElseClause,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    if let Some(cond) = &ec.condition {
        walk_compound_list(cond, source, hs, warns);
    }
    walk_compound_list(&ec.body, source, hs, warns);
}

/// Inspect a SimpleCommand for known wait/trap shapes.
fn check_simple(
    cmd: &SimpleCommand,
    source: &str,
    hs: &mut Vec<UnsupportedConstruct>,
    warns: &mut Vec<KnownGap>,
) {
    let name = cmd.word_or_name.as_ref().map(|w| w.value.as_str());
    let suffix_words: Vec<&brush_parser::ast::Word> = cmd
        .suffix
        .as_ref()
        .map(|s| {
            s.0.iter()
                .filter_map(|item| match item {
                    brush_parser::ast::CommandPrefixOrSuffixItem::Word(w) => Some(w),
                    brush_parser::ast::CommandPrefixOrSuffixItem::AssignmentWord(_, w) => Some(w),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    match name {
        Some("wait") => {
            if let Some(arg) = suffix_words.iter().find(|w| w.value == "-n") {
                if let Some(span) = brush_span(arg) {
                    let (line, column) = line_col_of(source, span.start);
                    hs.push(UnsupportedConstruct {
                        kind: HardStopKind::WaitDashN,
                        span,
                        line,
                        column,
                        snippet: arg.value.clone(),
                    });
                }
            }
        }
        Some("eval") => {
            // QuotedEval: warn when *any* eval argument contains a
            // dynamic expansion — the eval'd content is unknowable
            // statically, so we can't audit what it does.
            //
            // Exempt the canonical `eval set -- "$args"` idiom (and
            // any `eval set …` shape): `set` is a special builtin
            // that only rebinds positional parameters and shell
            // options; it can't execute a command name from its
            // arguments. The dynamic content is argv data, not code,
            // so there's nothing static analysis could tell us that
            // a runtime read of the same value wouldn't.
            if is_eval_set_idiom(&suffix_words) {
                return;
            }
            for arg in &suffix_words {
                if arg_has_dynamic_expansion(arg) {
                    if let Some(span) = brush_span(arg) {
                        let (line, column) = line_col_of(source, span.start);
                        warns.push(KnownGap {
                            kind: WarningKind::QuotedEval,
                            span,
                            line,
                            column,
                            message: "`eval` with a dynamic argument: the eval'd command is \
                                 unknown statically; consider rewriting as a direct call \
                                 or `--allow-known-gaps` to suppress"
                                .into(),
                        });
                        // One warning per eval invocation is enough.
                        break;
                    }
                }
            }
        }
        Some("trap") => {
            // First arg is the handler (a string of commands or `-`);
            // subsequent args are signal names.
            let mut iter = suffix_words.iter();
            let _handler = iter.next();
            for sig in iter {
                let s = sig.value.as_str();
                if s.is_empty() || s.starts_with('-') {
                    continue;
                }
                if !is_supported_trap_signal(s) {
                    if let Some(span) = brush_span(sig) {
                        let (line, column) = line_col_of(source, span.start);
                        warns.push(KnownGap {
                            kind: WarningKind::UnknownSignalTrap,
                            span,
                            line,
                            column,
                            message: format!(
                                "signal `{s}` is in brush 🔷 partial-support territory; \
                                 v0.1 only DEBUG, ERR, EXIT are fully supported"
                            ),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

/// Brush fully supports these trap "signals" (they're not real signals
/// but special trap conditions). Real OS signals are in 🔷 partial.
#[cfg(test)]
mod quoted_eval_tests {
    use super::*;

    fn warns_for(src: &str) -> Vec<KnownGap> {
        let opts = brush_parser::ParserOptions::default();
        let mut p = brush_parser::Parser::new(src.as_bytes(), &opts);
        let prog = p.parse_program().expect("test source parses");
        let (_, w) = scan_ast(src, &prog);
        w
    }

    #[test]
    fn dynamic_arg_to_evaluator_warns() {
        let w = warns_for("eval \"$cmd\"\n");
        assert!(w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }

    #[test]
    fn static_arg_to_evaluator_does_not_warn() {
        let w = warns_for("eval 'echo hi'\n");
        assert!(!w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }

    #[test]
    fn evaluator_with_cmd_substitution_warns() {
        let w = warns_for("eval \"$(echo hi)\"\n");
        assert!(w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }

    #[test]
    fn non_evaluator_dynamic_does_not_trigger_quoted_eval() {
        let w = warns_for("echo \"$x\"\n");
        assert!(!w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }

    #[test]
    fn evaluator_set_dashdash_is_exempt() {
        // The canonical getopt idiom — must not warn.
        let w = warns_for(r#"eval set -- "$PARSED_ARGUMENTS""#);
        assert!(
            !w.iter().any(|g| g.kind == WarningKind::QuotedEval),
            "`eval set -- ...` is the getopt idiom, must not warn: {w:?}"
        );
    }

    #[test]
    fn evaluator_set_without_dashdash_is_exempt() {
        // Other `eval set …` shapes (without `--`) are equally safe —
        // `set` doesn't execute argv content, regardless of flags.
        let w = warns_for(r#"eval set "$args""#);
        assert!(!w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }

    #[test]
    fn evaluator_with_other_first_word_still_warns() {
        // Make sure the exemption is narrow: `eval $cmd args` still warns.
        let w = warns_for(r#"eval "$cmd" args"#);
        assert!(w.iter().any(|g| g.kind == WarningKind::QuotedEval));
    }
}

fn is_supported_trap_signal(s: &str) -> bool {
    let upper = s.trim_start_matches("SIG").to_ascii_uppercase();
    matches!(upper.as_str(), "DEBUG" | "ERR" | "EXIT" | "RETURN")
}

/// True for `eval set …` — the canonical positional-parameter
/// rebinding idiom (`eval set -- "$(getopt …)"` and friends). `set`
/// is a special builtin that never executes a command from its
/// args, so dynamic content there is argv data, not code.
///
/// We accept any first-arg shape whose `value` is exactly `set`,
/// including the very common `eval set -- …`. Quoted (`'set'` or
/// `"set"`) variants resolve to the same `value` after brush
/// tokenization.
fn is_eval_set_idiom(suffix: &[&brush_parser::ast::Word]) -> bool {
    suffix.first().is_some_and(|w| w.value == "set")
}

/// True iff this word contains *any* dynamic expansion — variable
/// substitution, command substitution, parameter expansion, arithmetic.
/// Single/double-quoted *literals* don't count, but a double-quoted
/// `"$x"` does (the `$x` is dynamic).
fn arg_has_dynamic_expansion(w: &brush_parser::ast::Word) -> bool {
    use brush_parser::word::WordPiece as B;
    let opts = brush_parser::ParserOptions::default();
    let Ok(pieces) = brush_parser::word::parse(&w.value, &opts) else {
        // If we can't tokenize, assume dynamic — better to warn
        // spuriously than silently accept.
        return true;
    };
    fn any_dynamic(pieces: &[brush_parser::word::WordPieceWithSource]) -> bool {
        pieces.iter().any(|p| match &p.piece {
            B::Text(_) | B::SingleQuotedText(_) | B::EscapeSequence(_) => false,
            B::DoubleQuotedSequence(inner) => any_dynamic(inner),
            B::ParameterExpansion(_)
            | B::CommandSubstitution(_)
            | B::BackquotedCommandSubstitution(_)
            | B::ArithmeticExpression(_)
            | B::TildeExpansion(_)
            | B::AnsiCQuotedText(_)
            | B::GettextDoubleQuotedSequence(_) => true,
        })
    }
    any_dynamic(&pieces)
}

fn brush_span(w: &brush_parser::ast::Word) -> Option<Span> {
    w.loc
        .as_ref()
        .map(|s| Span::new(s.start.index, s.end.index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brush_parser::ParserOptions;

    fn parse(src: &str) -> Program {
        let mut p = brush_parser::Parser::new(src.as_bytes(), &ParserOptions::default());
        p.parse_program().expect("parses")
    }

    #[test]
    fn empty_ast_yields_no_findings() {
        let src = "";
        let ast = parse(src);
        let (stops, warns) = scan_ast(src, &ast);
        assert!(stops.is_empty());
        assert!(warns.is_empty());
    }

    #[test]
    fn wait_n_is_caught() {
        let src = "wait -n\n";
        let ast = parse(src);
        let (stops, _) = scan_ast(src, &ast);
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].kind, HardStopKind::WaitDashN);
    }

    #[test]
    fn wait_without_n_is_fine() {
        let src = "wait $!\n";
        let ast = parse(src);
        let (stops, _) = scan_ast(src, &ast);
        assert!(stops.is_empty());
    }

    #[test]
    fn trap_on_supported_signals_no_warning() {
        let src = r#"trap 'echo bye' EXIT
trap 'echo err' ERR
trap 'echo dbg' DEBUG
"#;
        let ast = parse(src);
        let (_, warns) = scan_ast(src, &ast);
        assert!(warns.is_empty(), "{warns:?}");
    }

    #[test]
    fn trap_on_sigint_warns() {
        let src = "trap 'echo got SIGINT' SIGINT\n";
        let ast = parse(src);
        let (_, warns) = scan_ast(src, &ast);
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarningKind::UnknownSignalTrap);
    }

    #[test]
    fn trap_with_multiple_signals_warns_per_unsupported() {
        let src = "trap 'cleanup' EXIT SIGTERM SIGHUP\n";
        let ast = parse(src);
        let (_, warns) = scan_ast(src, &ast);
        assert_eq!(warns.len(), 2); // SIGTERM and SIGHUP
    }

    #[test]
    fn trap_signal_is_normalized_with_or_without_sig_prefix() {
        // bash accepts both 'EXIT' and 'SIGEXIT'? Actually only EXIT.
        // We accept both forms with-or-without SIG prefix for the
        // supported trap conditions.
        let src = "trap 'cleanup' SIGEXIT\n";
        let ast = parse(src);
        let (_, warns) = scan_ast(src, &ast);
        assert!(warns.is_empty());
    }

    #[test]
    fn nested_wait_n_inside_function_is_caught() {
        let src = "f() { wait -n; }\n";
        let ast = parse(src);
        let (stops, _) = scan_ast(src, &ast);
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].kind, HardStopKind::WaitDashN);
    }
}
