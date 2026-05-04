//! Subcommand dispatch + the core check/resolve workflows that subcommands share.

use std::path::{Path, PathBuf};

use crate::diag::{render_jsonl, render_pretty_json, Diagnostic, DiagnosticKind, Severity};
use crate::directives::{parse_cli_allow, parse_cli_map, parse_cli_skip, parse_inline, Directives};
use crate::frontend::bash::BashFrontend;
use crate::frontend::Frontend;
use crate::ir::{SourceId, SourceUnit};
use crate::resolver::{
    Inputs, MagicVars, MultiFileResolve, Resolver, Solution, UnresolvedKind, DEFAULT_WRAPPERS_DIR,
};
use crate::rewriter;
use crate::safety::{self, SafetyReport};

use super::args::{
    CheckArgs, Cli, Command, DiffArgs, Format, GlobalOpts, Profile, ResolveArgs, SourcesArgs,
};
use super::exit;

/// True if rusholve should print cargo-style stderr progress banners
/// for this invocation. JSON formats own stderr too — keep them clean.
fn show_progress(global: &GlobalOpts) -> bool {
    !global.quiet && matches!(global.format, Format::Human)
}

/// Cargo-style action verb in a 12-char right-aligned column. The
/// reader's eye locks onto the verb position so successive lines
/// (`   Resolving`, `    Resolved`, `   Finished`) read as a single
/// progress stream.
fn action(verb: &str, msg: impl std::fmt::Display) {
    eprintln!("{verb:>12} {msg}");
}

/// One rewrite line: `<original> -> <replacement>` with `original`
/// right-aligned to the same 12-char column the action verbs use,
/// so the arrow lines up under the verb letters and the reader sees
/// a vertical column of replacements.
///
/// Emitted between `Resolving` and `Resolved` banners so the build
/// log shows every concrete edit rusholve makes — defensive signal
/// for "did the right path get spliced?". Suppressed under `--quiet`
/// or any non-human format (same gate as the action banners).
fn log_rewrite(original: &str, replacement: &str) {
    eprintln!("{original:>12} -> {replacement}");
}

/// Print one `log_rewrite` line per [`Solution::Resolved`] in `sols`,
/// in source-order (already sorted upstream). Uses the literal source
/// text at each solution's span (via `multi.map`) for the "from" side,
/// so e.g. `"${BASH_SOURCE%/*}/lib.sh"` shows up as the original even
/// though the resolver's `original` field stores the expanded value.
/// Skips identity edits where the spliced text would not change.
fn log_resolved_rewrites(report: &AuditReport, sols: &[Solution]) {
    for s in sols {
        if let Solution::Resolved {
            source_id,
            initial,
            replacement,
            ..
        } = s
        {
            let text = &report.multi.map.get(*source_id).text;
            let from = text.get(initial.start..initial.end).unwrap_or("<?>").trim();
            if from == replacement {
                continue;
            }
            log_rewrite(from, replacement);
        }
    }
}

/// Mode summary for the start banner. Keeps the verb line short.
fn mode_label(global: &GlobalOpts) -> &'static str {
    if is_strict(global) {
        "strict"
    } else {
        match global.profile {
            Profile::Nixos => "auto",
            Profile::Portable => "portable",
            Profile::Strict => "strict",
        }
    }
}

/// One-line summary of a script's [`Solution`]s. Used as the body of
/// the `Resolved` banner. Counts are stable across runs.
fn solution_summary(sols: &[Solution]) -> String {
    let mut resolved = 0usize;
    let mut in_scope = 0usize;
    let mut allowed = 0usize;
    let mut unresolved = 0usize;
    for s in sols {
        match s {
            Solution::Resolved { .. } => resolved += 1,
            Solution::InScope { .. } => in_scope += 1,
            Solution::Allowed { .. } => allowed += 1,
            Solution::Unresolved { .. } => unresolved += 1,
        }
    }
    let mut parts = Vec::with_capacity(4);
    parts.push(format!("{resolved} resolved"));
    if in_scope > 0 {
        parts.push(format!("{in_scope} in-scope"));
    }
    if allowed > 0 {
        parts.push(format!("{allowed} allowed"));
    }
    if unresolved > 0 {
        parts.push(format!("{unresolved} unresolved"));
    }
    parts.join(", ")
}

/// Process a parsed Cli and return an exit code. Errors are surfaced
/// as diagnostics where possible, so this function rarely panics.
pub fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Check(args) => run_check(&cli.global, args),
        Command::Resolve(args) => run_resolve(&cli.global, args),
        Command::Sources(args) => run_sources(&cli.global, args),
        Command::Diff(args) => run_diff(&cli.global, args),
    }
}

fn run_check(global: &GlobalOpts, args: CheckArgs) -> i32 {
    let cli_directives = match build_cli_directives(global) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let mut overall: i32 = exit::SUCCESS;
    let mut all_diags: Vec<Diagnostic> = Vec::new();

    let progress = show_progress(global);
    for script in &args.scripts {
        if progress {
            action(
                "Checking",
                format!("{} ({})", script.display(), mode_label(global)),
            );
        }
        match audit_one(script, global, &cli_directives) {
            Ok(report) => {
                if progress {
                    log_resolved_rewrites(&report, report.solutions());
                    action("Checked", solution_summary(report.solutions()));
                }
                let code = process_report(&report, global);
                if code != exit::SUCCESS && (overall == exit::SUCCESS || code > overall) {
                    overall = code;
                }
                all_diags.extend(report.diagnostics);
            }
            Err(boxed) => {
                let (code, diag) = *boxed;
                if overall == exit::SUCCESS || code > overall {
                    overall = code;
                }
                if let Some(d) = diag {
                    all_diags.push(d);
                }
            }
        }
    }
    emit_diagnostics(&all_diags, global.format);
    overall
}

fn run_resolve(global: &GlobalOpts, args: ResolveArgs) -> i32 {
    let cli_directives = match build_cli_directives(global) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let mut overall: i32 = exit::SUCCESS;
    let mut all_diags: Vec<Diagnostic> = Vec::new();

    let progress = show_progress(global);
    for script in &args.scripts {
        if progress {
            action(
                "Resolving",
                format!("{} ({})", script.display(), mode_label(global)),
            );
        }
        match audit_one(script, global, &cli_directives) {
            Ok(report) => {
                let code = process_report(&report, global);
                if code != exit::SUCCESS {
                    if overall == exit::SUCCESS || code > overall {
                        overall = code;
                    }
                    all_diags.extend(report.diagnostics);
                    continue; // do not rewrite this script
                }
                // Build per-file rewrites. Always rewrite the entry. By
                // default, also rewrite every sourced file with at least
                // one Resolved edit; `--no-write-sourced` flips that off.
                let entry_id = report.multi.entry_id;
                let write_sourced = !args.no_write_sourced;
                match rewrite_files(global, &report, write_sourced) {
                    Ok(rewrites) => {
                        for (sid, final_text) in rewrites {
                            // Entry uses the user-supplied path (with or
                            // without `.resolved` suffix). Sourced files
                            // always rewrite in place — that's the only
                            // unambiguous destination since the user
                            // didn't name them.
                            let dest = if sid == entry_id {
                                if args.in_place {
                                    script.clone()
                                } else {
                                    with_resolved_suffix(script)
                                }
                            } else {
                                report.multi.map.get(sid).path.clone()
                            };
                            if dest.as_os_str().is_empty() {
                                continue;
                            }
                            if let Err(e) = std::fs::write(&dest, &final_text) {
                                eprintln!("rusholve: write {}: {e}", dest.display());
                                overall = exit::GENERIC;
                                continue;
                            }
                            if progress {
                                let sols = report.solutions_for(sid);
                                let owned: Vec<Solution> = sols.into_iter().cloned().collect();
                                log_resolved_rewrites(&report, &owned);
                                action(
                                    "Resolved",
                                    format!("{} ({})", dest.display(), solution_summary(&owned)),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("rusholve: rewrite {}: {e}", script.display());
                        overall = exit::GENERIC;
                    }
                }
                all_diags.extend(report.diagnostics);
            }
            Err(boxed) => {
                let (code, diag) = *boxed;
                if overall == exit::SUCCESS || code > overall {
                    overall = code;
                }
                if let Some(d) = diag {
                    all_diags.push(d);
                }
            }
        }
    }
    emit_diagnostics(&all_diags, global.format);
    overall
}

/// Build the per-file rewrite outputs. The entry is always included
/// (auto-shebang only fires there). Sourced files are included only
/// if `write_sourced` is true and they contributed at least one
/// Resolved edit.
fn rewrite_files(
    global: &GlobalOpts,
    report: &AuditReport,
    write_sourced: bool,
) -> Result<Vec<(SourceId, String)>, rewriter::RewriteError> {
    let entry_id = report.multi.entry_id;
    let mut out: Vec<(SourceId, String)> = Vec::new();

    // Entry always rewrites (and always gets the auto-shebang treatment).
    let entry_text = report.entry_text();
    let entry_sols: Vec<Solution> = report
        .solutions_for(entry_id)
        .into_iter()
        .cloned()
        .collect();
    let final_entry = build_final_text(global, entry_text, &entry_sols)?;
    out.push((entry_id, final_entry));

    if !write_sourced {
        return Ok(out);
    }

    // Pull the SourceMap once; rewriting each non-entry file uses its own
    // text + its own slice of solutions. No auto-shebang on sourced files —
    // they're libraries, not entry points.
    let map = &report.multi.map;
    for file in map.iter() {
        if file.id == entry_id {
            continue;
        }
        let sols: Vec<Solution> = report.solutions_for(file.id).into_iter().cloned().collect();
        // Skip files with no rewrites — touching them would just be a noop
        // copy and risk a needless mtime bump.
        if !sols.iter().any(|s| s.is_resolved()) {
            continue;
        }
        let rewritten = rewriter::rewrite(&file.text, &sols)?;
        out.push((file.id, rewritten));
    }
    Ok(out)
}

/// Compute the final text that `resolve` would write: rewrite-spliced
/// source plus, unless disabled, an auto-shebang line. Centralized so
/// `resolve`, `diff`, and any future preview commands stay in lockstep.
fn build_final_text(
    global: &GlobalOpts,
    source: &str,
    solutions: &[Solution],
) -> Result<String, rewriter::RewriteError> {
    let rewritten = rewriter::rewrite(source, solutions)?;
    let final_text = if global.no_shebang || is_strict(global) {
        rewritten
    } else {
        let interpreter = global.interpreter.as_deref().unwrap_or("/usr/bin/env bash");
        rewriter::ensure_shebang(&rewritten, interpreter)
    };
    Ok(final_text)
}

/// `rusholve diff <script>` — show the unified-style diff of what
/// `resolve` would write. Does not modify any file.
///
/// Exit codes mirror `resolve` so CI gates can reuse the same logic:
/// non-zero on safety hard-stops, unresolved commands, etc.
fn run_diff(global: &GlobalOpts, args: DiffArgs) -> i32 {
    let cli_directives = match build_cli_directives(global) {
        Ok(d) => d,
        Err(code) => return code,
    };
    let mut overall: i32 = exit::SUCCESS;
    let mut all_diags: Vec<Diagnostic> = Vec::new();

    for script in &args.scripts {
        match audit_one(script, global, &cli_directives) {
            Ok(report) => {
                let code = process_report(&report, global);
                if code != exit::SUCCESS {
                    if overall == exit::SUCCESS || code > overall {
                        overall = code;
                    }
                    all_diags.extend(report.diagnostics);
                    continue;
                }
                let entry_text = report.entry_text().to_string();
                let entry_sols: Vec<Solution> = report
                    .solutions_for(report.multi.entry_id)
                    .into_iter()
                    .cloned()
                    .collect();
                match build_final_text(global, &entry_text, &entry_sols) {
                    Ok(final_text) => {
                        if final_text != entry_text {
                            print!("{}", unified_diff(script, &entry_text, &final_text));
                        }
                    }
                    Err(e) => {
                        eprintln!("rusholve: rewrite {}: {e}", script.display());
                        overall = exit::GENERIC;
                    }
                }
                all_diags.extend(report.diagnostics);
            }
            Err(boxed) => {
                let (code, diag) = *boxed;
                if overall == exit::SUCCESS || code > overall {
                    overall = code;
                }
                if let Some(d) = diag {
                    all_diags.push(d);
                }
            }
        }
    }
    emit_diagnostics(&all_diags, global.format);
    overall
}

/// Produce a small unified-diff-style rendering. We rely on rusholve's
/// rewrites preserving line structure: the resolved text is the same
/// number of lines as the source, plus zero or one prepended shebang.
/// That lets us align line-by-line without a full LCS, while still
/// emitting recognizable `@@` hunks.
fn unified_diff(script: &Path, original: &str, resolved: &str) -> String {
    let orig_lines: Vec<&str> = original.split_inclusive('\n').collect();
    let mut new_lines: Vec<&str> = resolved.split_inclusive('\n').collect();

    let mut out = String::new();
    out.push_str(&format!("--- {} (original)\n", script.display()));
    out.push_str(&format!("+++ {} (resolved)\n", script.display()));

    // Auto-shebang inserts a single line at the top. Detect it and emit
    // as a leading `+` line so the rest of the alignment lines up 1:1.
    let mut shebang_insert: Option<&str> = None;
    if new_lines.len() == orig_lines.len() + 1
        && new_lines[0].starts_with("#!")
        && orig_lines.first().is_none_or(|l| !l.starts_with("#!"))
    {
        shebang_insert = Some(new_lines.remove(0));
    }

    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut current: Option<DiffHunk> = None;
    for (i, (o, n)) in orig_lines.iter().zip(new_lines.iter()).enumerate() {
        if o == n {
            if let Some(h) = current.take() {
                hunks.push(h);
            }
            continue;
        }
        let h = current.get_or_insert_with(|| DiffHunk {
            start_line: i + 1,
            old_lines: Vec::new(),
            new_lines: Vec::new(),
        });
        h.old_lines.push((*o).to_string());
        h.new_lines.push((*n).to_string());
    }
    if let Some(h) = current.take() {
        hunks.push(h);
    }

    if let Some(she) = shebang_insert {
        out.push_str("@@ -0,0 +1,1 @@\n");
        out.push('+');
        out.push_str(she);
        if !she.ends_with('\n') {
            out.push('\n');
        }
    }
    for h in &hunks {
        let count = h.old_lines.len();
        let new_offset = h.start_line + shebang_insert.map_or(0, |_| 1);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.start_line, count, new_offset, count
        ));
        for line in &h.old_lines {
            out.push('-');
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
        for line in &h.new_lines {
            out.push('+');
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

#[derive(Debug)]
struct DiffHunk {
    start_line: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

/// `rusholve sources <script>` — print the source graph for each
/// script. Reuses the resolver's source-graph walker (the same one
/// that powers auto-source-graph harvest) without running CRO. Exits
/// 0 unless a parse fails.
fn run_sources(global: &GlobalOpts, args: SourcesArgs) -> i32 {
    let mut overall: i32 = exit::SUCCESS;
    let mut all_graphs: Vec<ScriptGraph> = Vec::new();

    for script in &args.scripts {
        let source = match std::fs::read_to_string(script) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rusholve: read {}: {e}", script.display());
                overall = exit::GENERIC;
                continue;
            }
        };
        let frontend = BashFrontend::default();
        let unit = match frontend.lower(&source, SourceId::new(0)) {
            Ok(out) => out.unit,
            Err(e) => {
                eprintln!("rusholve: parse {}: {e}", script.display());
                overall = exit::PARSE_ERROR;
                continue;
            }
        };
        let resolver = build_resolver(global, script);
        let graph = resolver.source_graph(&unit, &source, Some(script));
        all_graphs.push(ScriptGraph {
            entry: script.clone(),
            nodes: graph,
        });
    }

    match global.format {
        Format::Json => print!("{}", render_pretty_json(&all_graphs)),
        Format::Jsonl => print!("{}", render_jsonl(&all_graphs)),
        Format::Human => render_sources_human(&all_graphs),
    }

    overall
}

/// Pretty `tree`-ish rendering of one or more script graphs.
fn render_sources_human(graphs: &[ScriptGraph]) {
    for (i, sg) in graphs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        for node in &sg.nodes {
            let indent = "  ".repeat(node.depth);
            let path = node
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<entry>".to_string());
            let marker = if node.depth == 0 { "*" } else { "+" };
            println!("{indent}{marker} {path}");
            if !node.functions_defined.is_empty() {
                println!("{indent}  fn: {}", node.functions_defined.join(", "));
            }
            if !node.aliases_defined.is_empty() {
                println!("{indent}  alias: {}", node.aliases_defined.join(", "));
            }
        }
    }
}

/// JSON shape returned by `rusholve sources --format json`.
#[derive(Debug, serde::Serialize)]
struct ScriptGraph {
    entry: PathBuf,
    nodes: Vec<crate::resolver::SourceNode>,
}

/// One script's full audit + resolution, ready to drive both `check`
/// and `resolve`.
///
/// `multi.solutions` is mixed across the source graph; the rewriter
/// partitions by `Solution::source_id`. `multi.map` owns the entry
/// text plus every transitively sourced file. `multi.entry_id` is the
/// id of the entry script in `multi.map`.
struct AuditReport {
    multi: MultiFileResolve,
    diagnostics: Vec<Diagnostic>,
    safety: SafetyReport,
}

impl AuditReport {
    fn entry_text(&self) -> &str {
        &self.multi.map.get(self.multi.entry_id).text
    }

    fn solutions(&self) -> &[Solution] {
        &self.multi.solutions
    }

    fn solutions_for(&self, id: SourceId) -> Vec<&Solution> {
        self.multi
            .solutions
            .iter()
            .filter(|s| s.source_id() == id)
            .collect()
    }
}

fn audit_one(
    script: &Path,
    global: &GlobalOpts,
    cli_directives: &Directives,
) -> Result<AuditReport, Box<(i32, Option<Diagnostic>)>> {
    let source = std::fs::read_to_string(script).map_err(|e| {
        eprintln!("rusholve: read {}: {e}", script.display());
        Box::new((exit::GENERIC, None))
    })?;

    let frontend = BashFrontend::default();
    // Entry's source_id is fixed at 0 — `resolve_with_sources` builds a
    // SourceMap that places the entry first, so this matches.
    let entry_id = SourceId::new(0);
    let lower_result = frontend.lower(&source, entry_id);

    // Run safety token scan first (works even if parse fails).
    let token_stops = safety::scan_tokens(&source);
    let mut report = SafetyReport {
        hard_stops: token_stops,
        warnings: Vec::new(),
    };

    let parsed = match lower_result {
        Ok(out) => Some(out),
        Err(e) => {
            // If we already have a hard-stop token-scan reason, that's
            // the friendlier diagnostic. Otherwise surface the parse error.
            if report.hard_stops.is_empty() {
                let d = Diagnostic {
                    file: script.to_path_buf(),
                    span: crate::ir::Span::new(0, 0),
                    line: 1,
                    column: 1,
                    severity: Severity::Error,
                    kind: crate::diag::DiagnosticKind::Unresolved(UnresolvedKind::UnknownExternal),
                    message: format!("brush parse error: {e}"),
                    name: None,
                    hint: None,
                };
                return Err(Box::new((exit::PARSE_ERROR, Some(d))));
            }
            None
        }
    };

    if let Some(ref out) = parsed {
        if let Some(ast) = &out.bash_ast {
            let (ast_stops, ast_warns) = safety::scan_ast(&source, ast);
            report.hard_stops.extend(ast_stops);
            report.warnings.extend(ast_warns);
        }
    }

    // Inline directives + CLI directives. CLI takes precedence.
    let (inline, parse_errs) = parse_inline(&source);
    if !parse_errs.is_empty() {
        // Surface directive parse errors as diagnostics.
        let mut diags: Vec<Diagnostic> = parse_errs
            .iter()
            .map(|e| Diagnostic {
                file: script.to_path_buf(),
                span: crate::ir::Span::new(0, 0),
                line: 0,
                column: 0,
                severity: Severity::Error,
                kind: crate::diag::DiagnosticKind::Unresolved(UnresolvedKind::UnknownExternal),
                message: e.to_string(),
                name: None,
                hint: None,
            })
            .collect();
        let safety_diags: Vec<Diagnostic> = report
            .hard_stops
            .iter()
            .map(|h| Diagnostic::from_unsupported(script.to_path_buf(), h))
            .collect();
        diags.extend(safety_diags);
        return Err(Box::new((exit::DIRECTIVE_ERROR, diags.into_iter().next())));
    }
    let directives = inline.merge(cli_directives.clone());

    let unit = parsed
        .as_ref()
        .map(|o| &o.unit)
        .cloned()
        .unwrap_or_else(|| empty_unit_with_id(entry_id));
    let resolver = build_resolver(global, script);

    // Multi-file: resolve the entry plus every file it transitively
    // sources. In strict mode this collapses to a single-file resolve.
    let mut multi = resolver.resolve_with_sources(unit, source.clone(), Some(script));

    // Apply directives over each file's solutions independently. Directives
    // act on patterns/names; the same `--map`/`--allow`/`--skip` set applies
    // uniformly to every file in the source graph.
    apply_directives_per_file(&mut multi, &directives, Some(&resolver.inputs));

    let mut diagnostics: Vec<Diagnostic> = report
        .hard_stops
        .iter()
        .map(|h| Diagnostic::from_unsupported(script.to_path_buf(), h))
        .collect();
    diagnostics.extend(
        report
            .warnings
            .iter()
            .map(|w| Diagnostic::from_known_gap(script.to_path_buf(), w)),
    );
    // Auto-suggest: for unresolved external commands, fuzzy-match the
    // typo against every name in `--inputs` and surface the closest as a
    // hint. Computed once per script so a misspelled `git` doesn't
    // re-walk the input dirs for every occurrence. Disabled in strict
    // mode — diagnostics stay verbatim, no inferred guidance.
    let suggest_enabled = !is_strict(global);
    let candidates = if suggest_enabled {
        resolver.inputs.candidate_names()
    } else {
        Vec::new()
    };
    let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    diagnostics.extend(multi.solutions.iter().filter_map(|s| {
        // Diagnostics are keyed by the actual source the solution
        // points into — this is how sourced-file unresolved commands
        // surface with the right path.
        let file = multi.map.get(s.source_id()).path.clone();
        let file = if file.as_os_str().is_empty() {
            script.to_path_buf()
        } else {
            file
        };
        let src_text = &multi.map.get(s.source_id()).text;
        let mut d = Diagnostic::from_unresolved(file, s, src_text)?;
        if suggest_enabled && d.hint.is_none() {
            if let DiagnosticKind::Unresolved(UnresolvedKind::UnknownExternal) = d.kind {
                if let Some(name) = &d.name {
                    if let Some(suggestion) =
                        crate::diag::nearest(name.as_str(), candidate_refs.iter().copied())
                    {
                        d.hint = Some(format!("did you mean `{suggestion}`?"));
                    }
                }
            }
        }
        Some(d)
    }));

    Ok(AuditReport {
        multi,
        diagnostics,
        safety: report,
    })
}

/// Run `directives::apply` independently against each file's solutions.
/// Unresolved entries from sourced files get the same `--map` / `--allow`
/// / `--skip` treatment as the entry script.
fn apply_directives_per_file(
    multi: &mut MultiFileResolve,
    directives: &Directives,
    inputs: Option<&Inputs>,
) {
    // Group indices by source_id so each `apply` call gets the right
    // source text. Then mutate the original Vec in place.
    let mut by_file: std::collections::HashMap<SourceId, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, s) in multi.solutions.iter().enumerate() {
        by_file.entry(s.source_id()).or_default().push(i);
    }
    for (sid, indices) in by_file {
        // Pull out the slice we need to mutate; clone-and-swap because
        // Rust's borrow checker doesn't allow simultaneous mutable
        // borrows of the same Vec.
        let mut subset: Vec<Solution> = indices
            .iter()
            .map(|&i| multi.solutions[i].clone())
            .collect();
        let src_text = multi.map.get(sid).text.clone();
        crate::directives::apply(directives, &mut subset, &src_text, inputs);
        for (slot, sol) in indices.into_iter().zip(subset) {
            multi.solutions[slot] = sol;
        }
    }
    multi.solutions.sort_by_key(|s| (s.source_id(), s.span()));
    multi.solutions.dedup();
}

fn process_report(report: &AuditReport, global: &GlobalOpts) -> i32 {
    if !report.safety.hard_stops.is_empty() {
        return exit::UNSUPPORTED_CONSTRUCT;
    }
    if !report.safety.warnings.is_empty() && !global.allow_known_gaps {
        return exit::GENERIC;
    }
    let mut worst = exit::SUCCESS;
    for s in report.solutions() {
        if let Solution::Unresolved { kind, .. } = s {
            let code = match kind {
                UnresolvedKind::DynamicSourcePath | UnresolvedKind::UnreadableSource => {
                    exit::UNRESOLVED_SOURCE
                }
                _ => exit::UNRESOLVED_COMMAND,
            };
            if code > worst {
                worst = code;
            }
        }
    }
    worst
}

fn build_cli_directives(global: &GlobalOpts) -> Result<Directives, i32> {
    let mut d = Directives::new();
    for raw in &global.allow {
        match parse_cli_allow(raw) {
            Ok(a) => d.allow.push(a),
            Err(e) => {
                eprintln!("rusholve: --allow: {e}");
                return Err(exit::DIRECTIVE_ERROR);
            }
        }
    }
    for raw in &global.map {
        match parse_cli_map(raw) {
            Ok(m) => d.map.push(m),
            Err(e) => {
                eprintln!("rusholve: --map: {e}");
                return Err(exit::DIRECTIVE_ERROR);
            }
        }
    }
    for raw in &global.skip {
        d.skip.push(parse_cli_skip(raw));
    }
    Ok(d)
}

fn build_resolver(global: &GlobalOpts, script: &Path) -> Resolver {
    let strict = is_strict(global);
    // Lore: merge every `--lore <file>` (and the env-equivalent) into a
    // single `Lore` set. Failures here surface as a panic-free warning;
    // the resolver proceeds with the partial set so a single typo'd
    // lore file doesn't block all rewriting.
    let mut lore = crate::lore::Lore::default();
    for path in &global.lore {
        match crate::lore::read_file(path) {
            Ok(l) => lore.merge(l),
            Err(e) => eprintln!("rusholve: --lore: {e}"),
        }
    }
    // Auto-wrappers: prefer `/run/wrappers/bin/<name>` for setuid binaries
    // (sudo, mount, …) unless `--no-wrappers` is set, the active profile
    // disables them, or an override path is given.
    let wrappers_dir = if global.no_wrappers || strict || !global.profile.uses_wrappers() {
        None
    } else {
        Some(
            global
                .wrappers_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(DEFAULT_WRAPPERS_DIR)),
        )
    };
    let inputs = Inputs::new(global.inputs.iter().cloned()).with_wrappers_dir(wrappers_dir);
    let script_dir = global
        .script_dir
        .clone()
        .or_else(|| script.parent().map(Path::to_path_buf));
    // Auto-magic-vars: in v0.2 we expand `$BASH_SOURCE`/`${BASH_SOURCE%/*}`
    // and `$HOME` automatically when composing source paths. Strict mode
    // skips this — every source path must be literal.
    let magic_vars = if strict {
        MagicVars::default()
    } else {
        MagicVars {
            script_path: Some(script.to_path_buf()),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    };
    Resolver {
        inputs,
        script_dir,
        magic_vars,
        strict,
        lore,
    }
}

/// True if the user opted into resholve-style discipline via
/// `--strict` or `--profile=strict`.
fn is_strict(global: &GlobalOpts) -> bool {
    global.strict || global.profile.is_strict()
}

fn empty_unit_with_id(source_id: SourceId) -> SourceUnit {
    SourceUnit {
        source_id,
        commands: Vec::new(),
        functions_defined: Vec::new(),
        aliases_defined: Vec::new(),
        var_assignments: Vec::new(),
    }
}

fn with_resolved_suffix(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".resolved");
    PathBuf::from(s)
}

fn emit_diagnostics(diags: &[Diagnostic], format: Format) {
    if diags.is_empty() {
        return;
    }
    match format {
        Format::Json => print!("{}", render_pretty_json(diags)),
        Format::Jsonl => print!("{}", render_jsonl(diags)),
        Format::Human => {
            for d in diags {
                let sev = match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                };
                eprintln!(
                    "{}:{}:{}: {sev}: {}",
                    d.file.display(),
                    d.line,
                    d.column,
                    d.message
                );
                if let Some(hint) = &d.hint {
                    eprintln!("  help: {hint}");
                }
            }
        }
    }
}
