//! Bash/POSIX frontend: brush_parser AST → rusholve IR.
//!
//! The walker is *flattening* — it descends through pipelines, subshells,
//! brace groups, control flow, and case clauses, emitting every nested
//! [`SimpleCommand`] / [`FunctionDefinition`] into a single flat
//! [`SourceUnit`] in source order. Function bodies become their own
//! nested [`SourceUnit`] so the resolver can scope CRO correctly.
//!
//! v0.1 deliberately ignores: `select` (safety hard-stop), `coproc`
//! (safety hard-stop), arithmetic standalone commands (no command
//! references), redirects/process-substitutions (carried in spans only).

use brush_parser::ast::{
    Command, CompoundCommand, CompoundList, ElseClause, FunctionDefinition, Program, SimpleCommand,
};
use brush_parser::word::WordPieceWithSource;
use brush_parser::{ParserOptions, SourceSpan};
use thiserror::Error;

use super::{Frontend, FrontendOutput};
use crate::ir::{
    CommandLike, DynamicKind, Invocation, InvocationContext, SourceId, SourceUnit, Span, VarAssign,
    Word, WordPiece,
};

#[derive(Debug, Error)]
pub enum BashFrontendError {
    #[error("parse error: {0}")]
    Parse(#[from] brush_parser::ParseError),
}

/// The Bash frontend.
#[derive(Default)]
pub struct BashFrontend {
    pub options: ParserOptions,
}

impl Frontend for BashFrontend {
    type Error = BashFrontendError;

    fn lower(&self, source: &str, source_id: SourceId) -> Result<FrontendOutput, Self::Error> {
        let program = parse(source, &self.options)?;
        let unit = lower_program(&program, source, source_id, &self.options);
        Ok(FrontendOutput {
            unit,
            bash_ast: Some(program),
        })
    }
}

fn parse(source: &str, opts: &ParserOptions) -> Result<Program, brush_parser::ParseError> {
    let mut parser = brush_parser::Parser::new(source.as_bytes(), opts);
    parser.parse_program()
}

fn lower_program(
    prog: &Program,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) -> SourceUnit {
    let mut unit = SourceUnit {
        source_id,
        commands: Vec::new(),
        functions_defined: Vec::new(),
        aliases_defined: Vec::new(),
        var_assignments: Vec::new(),
    };
    for top_list in &prog.complete_commands {
        walk_compound_list(top_list, &mut unit, source, source_id, opts);
    }
    unit
}

fn walk_compound_list(
    list: &CompoundList,
    unit: &mut SourceUnit,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) {
    for item in &list.0 {
        // CompoundListItem(AndOrList, SeparatorOperator)
        let and_or = &item.0;
        walk_pipeline(&and_or.first, unit, source, source_id, opts);
        for tail in &and_or.additional {
            // AndOr::And(Pipeline) | AndOr::Or(Pipeline)
            let pipeline = match tail {
                brush_parser::ast::AndOr::And(p) | brush_parser::ast::AndOr::Or(p) => p,
            };
            walk_pipeline(pipeline, unit, source, source_id, opts);
        }
    }
}

fn walk_pipeline(
    pipeline: &brush_parser::ast::Pipeline,
    unit: &mut SourceUnit,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) {
    for cmd in &pipeline.seq {
        walk_command(cmd, unit, source, source_id, opts);
    }
}

fn walk_command(
    cmd: &Command,
    unit: &mut SourceUnit,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) {
    match cmd {
        Command::Simple(simple) => {
            harvest_assignments(simple, source, opts, unit);
            if let Some(lowered) = lower_simple(simple, source, opts) {
                record_alias_or_function(&lowered, unit);
                unit.commands.push(lowered);
            }
        }
        Command::Compound(compound, _redirects) => {
            walk_compound(compound, unit, source, source_id, opts);
        }
        Command::Function(fdef) => {
            let lowered = lower_function(fdef, source, source_id, opts);
            record_alias_or_function(&lowered, unit);
            unit.commands.push(lowered);
        }
        Command::ExtendedTest(_, _) => {
            // [[ … ]] doesn't introduce external command references at
            // the resolver level.
        }
    }
}

fn walk_compound(
    cc: &CompoundCommand,
    unit: &mut SourceUnit,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) {
    match cc {
        CompoundCommand::BraceGroup(g) => {
            walk_compound_list(&g.list, unit, source, source_id, opts)
        }
        CompoundCommand::Subshell(s) => walk_compound_list(&s.list, unit, source, source_id, opts),
        CompoundCommand::ForClause(fc) => {
            // The iteration values aren't command references; skip them.
            walk_compound_list(&fc.body.list, unit, source, source_id, opts);
        }
        CompoundCommand::ArithmeticForClause(fc) => {
            walk_compound_list(&fc.body.list, unit, source, source_id, opts);
        }
        CompoundCommand::WhileClause(w) | CompoundCommand::UntilClause(w) => {
            // tuple struct: (CompoundList, DoGroupCommand, SourceSpan)
            walk_compound_list(&w.0, unit, source, source_id, opts);
            walk_compound_list(&w.1.list, unit, source, source_id, opts);
        }
        CompoundCommand::IfClause(ic) => {
            walk_compound_list(&ic.condition, unit, source, source_id, opts);
            walk_compound_list(&ic.then, unit, source, source_id, opts);
            if let Some(elses) = &ic.elses {
                for ec in elses {
                    walk_else(ec, unit, source, source_id, opts);
                }
            }
        }
        CompoundCommand::CaseClause(cc) => {
            for item in &cc.cases {
                if let Some(body) = &item.cmd {
                    walk_compound_list(body, unit, source, source_id, opts);
                }
            }
        }
        CompoundCommand::Coprocess(_) | CompoundCommand::Arithmetic(_) => {
            // Coproc is a safety hard-stop and the script will already
            // have been refused before we lower. Arithmetic standalone
            // commands have no command-reference content.
        }
    }
}

fn walk_else(
    ec: &ElseClause,
    unit: &mut SourceUnit,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) {
    if let Some(cond) = &ec.condition {
        walk_compound_list(cond, unit, source, source_id, opts);
    }
    walk_compound_list(&ec.body, unit, source, source_id, opts);
}

fn lower_simple(cmd: &SimpleCommand, source: &str, opts: &ParserOptions) -> Option<CommandLike> {
    use brush_parser::ast::CommandPrefixOrSuffixItem as Item;

    let mut words = Vec::new();
    // Prefix: only Word items are command-line words. AssignmentWord items
    // here are env-variable assignments (`FOO=bar cmd …`), not arguments.
    if let Some(prefix) = &cmd.prefix {
        for item in &prefix.0 {
            if let Item::Word(w) = item {
                words.push(lower_word(w, source, opts));
            }
        }
    }
    if let Some(name) = &cmd.word_or_name {
        words.push(lower_word(name, source, opts));
    }
    // Suffix: both Word and AssignmentWord items are arguments. The
    // assignment-shaped form is how `alias ll='ls -la'` appears in the AST.
    if let Some(suffix) = &cmd.suffix {
        for item in &suffix.0 {
            match item {
                Item::Word(w) | Item::AssignmentWord(_, w) => {
                    words.push(lower_word(w, source, opts));
                }
                _ => {}
            }
        }
    }
    if words.is_empty() {
        return None;
    }

    let span = brush_parser::ast::SourceLocation::location(cmd)
        .as_ref()
        .map(|s| span_from(s, source))
        .unwrap_or_else(|| Span::new(0, 0));

    // Specialize source/. and alias.
    if let Some(name) = words.first().and_then(Word::as_static) {
        if matches!(name, "source" | ".") {
            if let Some(target) = words.get(1).cloned() {
                return Some(CommandLike::Source { target, span });
            }
        }
        if name == "alias" && words.len() >= 2 {
            if let Some(spec) = words.get(1) {
                if let Some(spec_str) = spec.as_static() {
                    if let Some(eq) = spec_str.find('=') {
                        let alias_name = spec_str[..eq].to_string();
                        let value = spec_str[eq + 1..].to_string();
                        let definition = Word {
                            span: spec.span,
                            pieces: Vec::new(),
                            static_value: Some(value),
                        };
                        return Some(CommandLike::Alias {
                            name: alias_name,
                            name_span: spec.span,
                            definition,
                            span,
                        });
                    }
                }
            }
        }
    }

    let context = invocation_context(&words);
    Some(CommandLike::Simple(Invocation {
        words,
        span,
        context,
    }))
}

/// Detect well-known exec wrappers from the leading word so the resolver
/// can apply the right CRO. (Resolution itself happens later; here we
/// just tag.)
fn invocation_context(words: &[Word]) -> InvocationContext {
    match words.first().and_then(Word::as_static) {
        Some("command") => InvocationContext::InsideCommand,
        Some("exec") => InvocationContext::InsideExec,
        Some("eval") => InvocationContext::InsideEval,
        _ => InvocationContext::Default,
    }
}

fn lower_function(
    fdef: &FunctionDefinition,
    source: &str,
    source_id: SourceId,
    opts: &ParserOptions,
) -> CommandLike {
    let name = fdef.fname.value.clone();
    let name_span = span_from_word(&fdef.fname, source);
    let span = brush_parser::ast::SourceLocation::location(fdef)
        .as_ref()
        .map(|s| span_from(s, source))
        .unwrap_or(name_span);

    let mut body_unit = SourceUnit {
        source_id,
        commands: Vec::new(),
        functions_defined: Vec::new(),
        aliases_defined: Vec::new(),
        var_assignments: Vec::new(),
    };
    walk_compound(&fdef.body.0, &mut body_unit, source, source_id, opts);

    CommandLike::Function {
        name,
        name_span,
        body: Box::new(body_unit),
        span,
    }
}

/// Walk a `SimpleCommand`'s prefix and (declaration-style) suffix to
/// collect every `name=value` assignment and push it onto
/// `unit.var_assignments`. Two shapes are recognized:
///
/// 1. **Bare assignment** — `cmd=git` with no command word. Prefix
///    contains the `AssignmentWord`, suffix is empty, `word_or_name`
///    is `None`.
/// 2. **Declaration command** — `local cmd=git`, `export PATH=…`,
///    `declare cmd=git`, `readonly cmd=git`, `typeset cmd=git`. The
///    `word_or_name` is the declaration keyword and the suffix carries
///    one or more `AssignmentWord`s.
///
/// Other suffix `AssignmentWord`s (e.g. `alias ll='ls -la'`) are
/// *not* harvested here — they're command arguments that happen to be
/// assignment-shaped, not name-binding assignments.
fn harvest_assignments(
    simple: &SimpleCommand,
    source: &str,
    opts: &ParserOptions,
    unit: &mut SourceUnit,
) {
    use brush_parser::ast::CommandPrefixOrSuffixItem as Item;

    // Prefix `AssignmentWord`s are name bindings unless there's a
    // command word — in which case they're env-vars-for-command and
    // should not survive past the command's invocation. Bash semantics:
    // `FOO=1 cmd` doesn't change FOO in the surrounding shell.
    let has_command_word = simple.word_or_name.is_some();
    if !has_command_word {
        if let Some(prefix) = &simple.prefix {
            for item in &prefix.0 {
                if let Item::AssignmentWord(assign, value) = item {
                    push_assign(assign, value, source, opts, unit);
                }
            }
        }
    }

    // Suffix assignments are bindings only for declaration commands.
    if let Some(name) = simple.word_or_name.as_ref().map(|w| w.value.as_str()) {
        if matches!(
            name,
            "local" | "declare" | "export" | "readonly" | "typeset"
        ) {
            if let Some(suffix) = &simple.suffix {
                for item in &suffix.0 {
                    if let Item::AssignmentWord(assign, value) = item {
                        push_assign(assign, value, source, opts, unit);
                    }
                }
            }
        }
    }
}

fn push_assign(
    assign: &brush_parser::ast::Assignment,
    raw_word: &brush_parser::ast::Word,
    source: &str,
    opts: &ParserOptions,
    unit: &mut SourceUnit,
) {
    use brush_parser::ast::{AssignmentName, AssignmentValue};

    // Only scalar `name=value`. Array assignments (`a=(x y z)`) are
    // currently out of scope for varsub tracing.
    let name = match &assign.name {
        AssignmentName::VariableName(n) => n.clone(),
        AssignmentName::ArrayElementName(_, _) => return,
    };
    let value_word = match &assign.value {
        AssignmentValue::Scalar(w) => w,
        AssignmentValue::Array(_) => return,
    };

    // brush attaches `loc` to the raw `name=value` word but *not* to
    // the inner `value` word, so spans inside the value would otherwise
    // start at 0 instead of pointing at the outer source. Recover the
    // outer base offset as raw_word.start + len(name) + 1 (for `=`).
    let value_base = raw_word
        .loc
        .as_ref()
        .map(|l| byte_offset(source, l.start.line, l.start.column) + name.len() + 1)
        .unwrap_or(0);
    let lowered = lower_assignment_value(value_word, value_base, source, opts);

    let span = lowered.span;
    let literal = lowered.static_value.clone();
    unit.var_assignments.push(VarAssign {
        name,
        literal,
        span,
        value: lowered,
    });
}

/// Like [`lower_word`] but uses an explicit outer-source base offset
/// instead of relying on `brush_word.loc` (which brush leaves unset
/// on assignment-value Words).
fn lower_assignment_value(
    brush_word: &brush_parser::ast::Word,
    base: usize,
    source: &str,
    opts: &ParserOptions,
) -> Word {
    let span = Span::new(base, base + brush_word.value.len());
    let pieces = match brush_parser::word::parse(&brush_word.value, opts) {
        Ok(brush_pieces) => lower_pieces(&brush_pieces, base, source, opts),
        Err(_) => vec![WordPiece::Dynamic {
            kind: DynamicKind::ParamExp,
            span,
        }],
    };
    let static_value = compute_static(&pieces);
    Word {
        span,
        pieces,
        static_value,
    }
}

fn record_alias_or_function(cl: &CommandLike, unit: &mut SourceUnit) {
    match cl {
        CommandLike::Function { name, .. } => {
            unit.functions_defined.push(name.clone());
        }
        CommandLike::Alias {
            name, definition, ..
        } => {
            unit.aliases_defined
                .push((name.clone(), definition.clone()));
        }
        _ => {}
    }
}

fn lower_word(brush_word: &brush_parser::ast::Word, source: &str, opts: &ParserOptions) -> Word {
    let span = span_from_word(brush_word, source);

    let pieces = match brush_parser::word::parse(&brush_word.value, opts) {
        Ok(brush_pieces) => lower_pieces(&brush_pieces, span.start, source, opts),
        Err(_) => vec![WordPiece::Dynamic {
            kind: DynamicKind::ParamExp,
            span,
        }],
    };
    let static_value = compute_static(&pieces);
    Word {
        span,
        pieces,
        static_value,
    }
}

fn lower_pieces(
    pieces: &[WordPieceWithSource],
    base: usize,
    source: &str,
    opts: &ParserOptions,
) -> Vec<WordPiece> {
    pieces
        .iter()
        .map(|p| lower_piece(p, base, source, opts))
        .collect()
}

fn lower_piece(
    p: &WordPieceWithSource,
    base: usize,
    source: &str,
    opts: &ParserOptions,
) -> WordPiece {
    let span = Span::new(base + p.start_index, base + p.end_index);
    use brush_parser::word::WordPiece as B;
    match &p.piece {
        B::Text(s) => WordPiece::Literal {
            text: s.clone(),
            span,
        },
        B::SingleQuotedText(s) => WordPiece::SingleQuoted {
            text: s.clone(),
            span,
        },
        B::EscapeSequence(s) => WordPiece::Literal {
            text: s.clone(),
            span,
        },
        B::DoubleQuotedSequence(inner) => WordPiece::DoubleQuoted {
            pieces: lower_pieces(inner, base, source, opts),
            span,
        },
        B::AnsiCQuotedText(_) => WordPiece::Dynamic {
            kind: DynamicKind::AnsiC,
            span,
        },
        B::GettextDoubleQuotedSequence(_) => WordPiece::Dynamic {
            kind: DynamicKind::AnsiC,
            span,
        },
        B::TildeExpansion(_) => WordPiece::Dynamic {
            kind: DynamicKind::Tilde,
            span,
        },
        B::ParameterExpansion(_) => WordPiece::Dynamic {
            kind: DynamicKind::ParamExp,
            span,
        },
        B::CommandSubstitution(_) => match lower_cmd_sub(span, source, opts) {
            Some(inner) => WordPiece::CommandSub {
                inner: Box::new(inner),
                span,
            },
            None => WordPiece::Dynamic {
                kind: DynamicKind::CmdSub,
                span,
            },
        },
        B::BackquotedCommandSubstitution(_) => WordPiece::Dynamic {
            // Backticks rewrite escape sequences inside the body, so
            // brush's parsed text doesn't byte-align with the outer
            // source — and we splice rewrites by outer-source offsets.
            // Stay opaque until we have a span-preserving inner parser
            // for backticks.
            kind: DynamicKind::CmdSub,
            span,
        },
        B::ArithmeticExpression(_) => WordPiece::Dynamic {
            kind: DynamicKind::ArithExp,
            span,
        },
    }
}

/// Re-parse a `$(...)` command substitution and lower it as a nested
/// [`SourceUnit`]. The returned unit's spans are absolute byte-offsets
/// into the *outer* `source` — we achieve this by parsing a string
/// padded with `outer.start + 2` leading spaces, so brush's own
/// 0-based span arithmetic naturally lands on outer-source offsets.
///
/// Returns `None` if the inner content doesn't parse cleanly (so the
/// caller can fall back to opaque [`WordPiece::Dynamic`]).
fn lower_cmd_sub(outer: Span, source: &str, opts: &ParserOptions) -> Option<SourceUnit> {
    // Skip past `$(` at the start and `)` at the end. Both must fit
    // inside `outer` for us to be looking at a real command sub.
    if outer.end < outer.start + 3 {
        return None;
    }
    let inner_start = outer.start + 2;
    let inner_end = outer.end - 1;
    let inner = source.get(inner_start..inner_end)?;

    // Pad with `inner_start` leading spaces so brush's 0-based byte
    // offsets translate directly to outer-source offsets when the
    // inner content begins. Spaces are safe anywhere bash whitespace
    // is allowed (between commands, before the first command).
    let mut padded = String::with_capacity(inner_start + inner.len());
    padded.extend(std::iter::repeat_n(' ', inner_start));
    padded.push_str(inner);

    let prog = parse(&padded, opts).ok()?;
    Some(lower_program(&prog, &padded, SourceId::new(0), opts))
}

fn compute_static(pieces: &[WordPiece]) -> Option<String> {
    let mut out = String::new();
    for p in pieces {
        out.push_str(&p.static_text()?);
    }
    Some(out)
}

/// Translate a brush `SourceSpan` into a byte-offset [`Span`] over `source`.
///
/// brush_parser 0.4.0 has a bug where `loc.start.index` / `loc.end.index`
/// are computed as **character counts**, not byte offsets — so any
/// multi-byte UTF-8 character above a token shifts every subsequent token's
/// reported index by `byte_len - 1` per char. brush's `line` and `column`
/// (1-based char counts) *are* correct, so we re-derive byte offsets
/// from those. Tracking upstream at
/// <https://github.com/reubeno/brush/issues/1127>; drop this workaround
/// when the upstream fix lands and we bump `brush-parser`.
///
/// brush's `end` line/col is *also* sometimes inconsistent for compound
/// spans (we've seen `end < start` on multi-line constructs). Clamp the
/// end up to `start` so [`Span::new`]'s invariant holds; the resulting
/// zero-width span is a degraded-but-safe diagnostic anchor.
fn span_from(span: &SourceSpan, source: &str) -> Span {
    let start = byte_offset(source, span.start.line, span.start.column);
    let end = byte_offset(source, span.end.line, span.end.column).max(start);
    Span::new(start, end)
}

/// Word-specific span helper. Trusts brush's `start` (after coordinate
/// translation) and recomputes the end as `start + value.len()`, since
/// `brush_word.value` is reliable and gives an exact byte length even
/// when brush's `end` is bogus.
fn span_from_word(brush_word: &brush_parser::ast::Word, source: &str) -> Span {
    let Some(loc) = brush_word.loc.as_ref() else {
        return Span::new(0, 0);
    };
    let start = byte_offset(source, loc.start.line, loc.start.column);
    Span::new(start, start + brush_word.value.len())
}

/// 1-based (line, column) → byte offset. `column` counts UTF-8 chars
/// (not bytes), matching brush's convention. Returns `source.len()` if
/// the position is past EOF.
fn byte_offset(source: &str, line: usize, column: usize) -> usize {
    if line == 0 || column == 0 {
        return 0;
    }
    let mut cur_line = 1usize;
    let mut cur_col = 1usize;
    for (byte_idx, ch) in source.char_indices() {
        if cur_line == line && cur_col == column {
            return byte_idx;
        }
        if ch == '\n' {
            cur_line += 1;
            cur_col = 1;
        } else {
            cur_col += 1;
        }
    }
    source.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(source: &str) -> SourceUnit {
        let frontend = BashFrontend::default();
        frontend
            .lower(source, SourceId::new(0))
            .expect("test source parses")
            .unit
    }

    fn names(unit: &SourceUnit) -> Vec<String> {
        unit.commands
            .iter()
            .filter_map(|c| match c {
                CommandLike::Simple(inv) => inv.static_name().map(str::to_string),
                CommandLike::Function { name, .. } => Some(format!("fn:{name}")),
                CommandLike::Source { target, .. } => {
                    target.as_static().map(|s| format!("src:{s}"))
                }
                CommandLike::Alias { name, .. } => Some(format!("alias:{name}")),
            })
            .collect()
    }

    #[test]
    fn lowers_single_simple_command() {
        let unit = lower("git status");
        assert_eq!(unit.commands.len(), 1);
        assert_eq!(names(&unit), vec!["git"]);
    }

    #[test]
    fn lowers_pipeline_into_two_commands() {
        let unit = lower("git status | grep modified");
        assert_eq!(names(&unit), vec!["git", "grep"]);
    }

    #[test]
    fn lowers_if_branches() {
        let unit = lower("if test -f a; then cat a; else echo nope; fi");
        let n = names(&unit);
        assert!(n.contains(&"test".to_string()));
        assert!(n.contains(&"cat".to_string()));
        assert!(n.contains(&"echo".to_string()));
    }

    #[test]
    fn lowers_for_loop_body() {
        let unit = lower("for f in a b c; do cat \"$f\"; done");
        assert!(names(&unit).contains(&"cat".to_string()));
    }

    #[test]
    fn function_definition_creates_nested_unit() {
        let unit = lower("greet() { echo hi; }\ngreet");
        let mut found_fn_body_echo = false;
        for cmd in &unit.commands {
            if let CommandLike::Function { name, body, .. } = cmd {
                assert_eq!(name, "greet");
                assert!(names(body).contains(&"echo".to_string()));
                found_fn_body_echo = true;
            }
        }
        assert!(found_fn_body_echo, "expected function body to contain echo");
        assert_eq!(unit.functions_defined, vec!["greet".to_string()]);
    }

    #[test]
    fn source_command_specializes_to_source_variant() {
        let unit = lower("source ./lib/common.sh");
        match &unit.commands[0] {
            CommandLike::Source { target, .. } => {
                assert_eq!(target.as_static(), Some("./lib/common.sh"));
            }
            other => panic!("expected Source variant, got {other:?}"),
        }
    }

    #[test]
    fn dot_specializes_to_source_variant() {
        let unit = lower(". ./lib/common.sh");
        assert!(matches!(&unit.commands[0], CommandLike::Source { .. }));
    }

    #[test]
    fn alias_definition_specializes_to_alias_variant() {
        let unit = lower("alias ll='ls -la'");
        match &unit.commands[0] {
            CommandLike::Alias {
                name, definition, ..
            } => {
                assert_eq!(name, "ll");
                assert_eq!(definition.as_static(), Some("ls -la"));
            }
            other => panic!("expected Alias variant, got {other:?}"),
        }
    }

    #[test]
    fn bare_assignment_is_harvested_not_emitted_as_command() {
        let unit = lower("cmd=git\n");
        // A bare assignment must NOT show up as a CommandLike::Simple;
        // otherwise the resolver would try to look up `cmd=git` as a
        // command name.
        assert!(
            unit.commands.is_empty(),
            "bare assignment should not produce a CommandLike, got {:?}",
            unit.commands
        );
        assert_eq!(unit.var_assignments.len(), 1, "{:?}", unit.var_assignments);
        let a = &unit.var_assignments[0];
        assert_eq!(a.name, "cmd");
        assert_eq!(a.literal.as_deref(), Some("git"));
    }

    #[test]
    fn local_declaration_harvests_assignment() {
        let unit = lower("local cmd=git\n");
        // The `local` command itself stays as a Simple, but the
        // assignment is harvested.
        assert_eq!(unit.var_assignments.len(), 1);
        assert_eq!(unit.var_assignments[0].name, "cmd");
        assert_eq!(unit.var_assignments[0].literal.as_deref(), Some("git"));
    }

    #[test]
    fn env_prefix_assignment_is_not_harvested() {
        // `FOO=1 cmd ...` is an env binding for the command, not a
        // surrounding-shell binding. Don't harvest.
        let unit = lower("FOO=1 echo hi\n");
        assert!(
            unit.var_assignments.is_empty(),
            "{:?}",
            unit.var_assignments
        );
    }

    #[test]
    fn variable_command_is_dynamic() {
        let unit = lower("$cmd args");
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                assert!(inv.static_name().is_none());
            }
            other => panic!("expected Simple variant, got {other:?}"),
        }
    }

    #[test]
    fn double_quoted_literal_is_static() {
        let unit = lower(r#"echo "hello world""#);
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                assert_eq!(inv.static_name(), Some("echo"));
                assert_eq!(inv.args()[0].as_static(), Some("hello world"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn double_quoted_with_var_is_dynamic() {
        let unit = lower(r#"echo "hello $name""#);
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                assert_eq!(inv.static_name(), Some("echo"));
                assert!(inv.args()[0].as_static().is_none());
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn span_points_at_the_command_word() {
        let src = "echo hello";
        let unit = lower(src);
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                let name_span = inv.name().unwrap().span;
                assert_eq!(&src[name_span.start..name_span.end], "echo");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn command_wrapper_tags_context() {
        let unit = lower("command git status");
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                assert_eq!(inv.context, InvocationContext::InsideCommand);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn exec_wrapper_tags_context() {
        let unit = lower("exec bash other.sh");
        match &unit.commands[0] {
            CommandLike::Simple(inv) => {
                assert_eq!(inv.context, InvocationContext::InsideExec);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn cmd_sub_lowers_to_inner_unit_with_outer_spans() {
        // `$(getopt -o ab)` — inner content must lower to a nested
        // SourceUnit whose `getopt` Word's span points at the outer
        // source.
        let src = r#"foo=$(getopt -o ab)"#;
        let unit = lower(src);
        // Find the `foo=...` assignment's value, then dig into pieces.
        // Easier: walk all words in all commands and find a CommandSub.
        let mut found = false;
        for cmd in &unit.commands {
            if let CommandLike::Simple(inv) = cmd {
                for word in &inv.words {
                    for piece in &word.pieces {
                        if let crate::ir::WordPiece::CommandSub { inner, span } = piece {
                            // span covers `$(getopt -o ab)`
                            assert_eq!(&src[span.start..span.end], "$(getopt -o ab)");
                            // Inner unit holds `getopt -o ab` and its
                            // `getopt` word maps to the outer source.
                            assert_eq!(inner.commands.len(), 1);
                            if let CommandLike::Simple(inner_inv) = &inner.commands[0] {
                                let getopt_span = inner_inv.name().unwrap().span;
                                assert_eq!(&src[getopt_span.start..getopt_span.end], "getopt");
                            }
                            found = true;
                        }
                    }
                }
            }
        }
        // The bare assignment case lowers to var_assignments, not a
        // Simple command. Re-check: var_assignments[0].literal is None
        // because `$()` is dynamic; but we can look at the assign's
        // span by re-lowering with a different shape:
        if !found {
            // Try `cmd $(getopt -o ab)` instead — `cmd` is the outer
            // Simple, `$(...)` is in args.
            let src2 = r#"cmd $(getopt -o ab)"#;
            let unit2 = lower(src2);
            for cmd in &unit2.commands {
                if let CommandLike::Simple(inv) = cmd {
                    for word in &inv.words {
                        for piece in &word.pieces {
                            if let crate::ir::WordPiece::CommandSub { inner, span } = piece {
                                assert_eq!(&src2[span.start..span.end], "$(getopt -o ab)");
                                if let CommandLike::Simple(inner_inv) = &inner.commands[0] {
                                    let getopt_span = inner_inv.name().unwrap().span;
                                    assert_eq!(&src2[getopt_span.start..getopt_span.end], "getopt");
                                }
                                found = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(found, "expected a CommandSub piece");
    }

    #[test]
    fn word_spans_survive_multibyte_utf8_above() {
        // Regression: brush_parser 0.4.0 returns `loc.start.index` as a
        // char count, so any multi-byte UTF-8 above a token shifts the
        // reported byte offset. We re-derive byte offsets from
        // (line, column). Verify a command after a UTF-8-heavy comment
        // still has a span pointing at the actual command bytes.
        let src = "# café résumé naïve coöperate\nmv a b\n";
        let unit = lower(src);
        let inv = match &unit.commands[0] {
            CommandLike::Simple(i) => i,
            other => panic!("expected Simple, got {other:?}"),
        };
        let name_word = inv.name().unwrap();
        let span = name_word.span;
        assert_eq!(
            &src[span.start..span.end],
            "mv",
            "span must point at `mv`, not at random bytes shifted by UTF-8 above"
        );
    }

    #[test]
    fn nested_cmd_subs_lower_recursively() {
        // `$(echo $(date))` — outer cmdsub contains inner cmdsub.
        let src = r#"x=$(echo $(date))"#;
        let unit = lower(src);
        let mut depth = 0;
        fn walk(piece: &crate::ir::WordPiece, depth: &mut usize) {
            match piece {
                crate::ir::WordPiece::CommandSub { inner, .. } => {
                    *depth += 1;
                    for cmd in &inner.commands {
                        if let CommandLike::Simple(inv) = cmd {
                            for w in &inv.words {
                                for p in &w.pieces {
                                    walk(p, depth);
                                }
                            }
                        }
                    }
                }
                crate::ir::WordPiece::DoubleQuoted { pieces, .. } => {
                    for p in pieces {
                        walk(p, depth);
                    }
                }
                _ => {}
            }
        }
        // Bare assignment: cmdsub lives on var_assignments[0] but the
        // *piece* representation is on the lowered Word inside the
        // SourceUnit's harvested assignments. We don't keep the Word
        // there. Use suffixed form instead.
        let src2 = r#"cmd $(echo $(date))"#;
        let unit2 = lower(src2);
        for cmd in &unit2.commands {
            if let CommandLike::Simple(inv) = cmd {
                for w in &inv.words {
                    for p in &w.pieces {
                        walk(p, &mut depth);
                    }
                }
            }
        }
        let _ = unit;
        assert!(
            depth >= 2,
            "expected at least 2 levels of CommandSub, got {depth}"
        );
    }
}
