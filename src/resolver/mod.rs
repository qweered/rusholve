//! Resolver entry point.
//!
//! Drives the resolution loop over a [`SourceUnit`]. v0.1 produces one
//! [`Solution`] per visited [`CommandLike`], plus a recursive solution
//! for any exec-wrapper inner command. Source-graph traversal is
//! shallow (we do not parse and lower included files in v0.1; the
//! `Source` solution carries the resolved path string but doesn't
//! recurse).

mod auto_skip;
mod classify;
mod execers;
mod inputs;
mod magic_vars;
mod solution;
mod varsub;

pub use classify::{classify, Classification, Scope};
pub use execers::{
    find_wrapped_command, find_wrapped_commands, is_exec_wrapper, is_v01_wrapper, EXEC_WRAPPERS,
};
pub use inputs::{Inputs, DEFAULT_WRAPPERS_DIR, WRAPPED_COMMANDS};
pub use magic_vars::MagicVars;
pub use solution::{InScopeKind, ResolvedKind, Solution, UnresolvedKind};
pub use varsub::parse_var_name;
pub use varsub::VarMap;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::frontend::bash::BashFrontend;
use crate::frontend::Frontend;
use crate::ir::{CommandLike, Invocation, SourceId, SourceMap, SourceUnit, Word, WordPiece};

/// The resolver itself. Stateless — repeat invocations on the same
/// `SourceUnit` are deterministic.
#[derive(Debug, Default, Clone)]
pub struct Resolver {
    pub inputs: Inputs,
    /// Optional directory to resolve `./relative` source paths against.
    pub script_dir: Option<PathBuf>,
    /// Magic variables (`$BASH_SOURCE`, `$HOME`, …) we'll expand when
    /// composing source paths. Empty by default — no expansion.
    pub magic_vars: MagicVars,
    /// If true, behave like v0.1 / resholve: no auto-source-graph harvest,
    /// no auto-skip dynamics, no extended exec-wrapper recursion. The
    /// `MagicVars` and `Inputs.wrappers_dir` configs are still consulted,
    /// so the CLI is responsible for emptying those when strict.
    pub strict: bool,
    /// User-supplied lore overrides for the exec-wrapper table.
    pub lore: crate::lore::Lore,
}

impl Resolver {
    pub fn new(inputs: Inputs) -> Self {
        Self {
            inputs,
            script_dir: None,
            magic_vars: MagicVars::default(),
            strict: false,
            lore: crate::lore::Lore::default(),
        }
    }

    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    pub fn with_lore(mut self, lore: crate::lore::Lore) -> Self {
        self.lore = lore;
        self
    }

    /// Whether `name` should be treated as an exec-wrapper for this
    /// resolver invocation. Lore wins (positive or negative); otherwise
    /// fall back to `is_exec_wrapper` / `is_v01_wrapper` based on mode.
    fn is_active_wrapper(&self, name: &str) -> bool {
        if let Some(verdict) = self.lore.override_for(name) {
            return verdict;
        }
        if self.strict {
            is_v01_wrapper(name)
        } else {
            is_exec_wrapper(name)
        }
    }

    pub fn with_script_dir(mut self, dir: PathBuf) -> Self {
        self.script_dir = Some(dir);
        self
    }

    pub fn with_magic_vars(mut self, magic_vars: MagicVars) -> Self {
        self.magic_vars = magic_vars;
        self
    }

    /// Multi-file resolve: walk the entry script *and* every file it
    /// recursively `source`s, producing a [`MultiFileResolve`] keyed by
    /// [`SourceId`]. Each file is parsed and re-resolved independently
    /// against the merged auto-source-graph scope (so functions/aliases
    /// from a sibling file are visible). The returned [`SourceMap`]
    /// owns every loaded file's text for downstream rewrite/diagnostic.
    ///
    /// `entry_unit` must already have been lowered with `entry_id`. The
    /// frontend should have used a fresh `SourceMap` so subsequent files
    /// the resolver loads can be added without conflict.
    ///
    /// In strict mode this collapses to a single-file resolve — sourced
    /// files stay opaque, mirroring resholve.
    pub fn resolve_with_sources(
        &self,
        entry_unit: SourceUnit,
        entry_text: String,
        entry_path: Option<&Path>,
    ) -> MultiFileResolve {
        let mut map = SourceMap::new();
        let entry_id = map.add(
            entry_path.map(Path::to_path_buf).unwrap_or_default(),
            entry_text,
        );
        debug_assert_eq!(entry_id, entry_unit.source_id);

        let entry_text_ref = map.get(entry_id).text.clone();
        let mut all_solutions = self.resolve_unit(&entry_unit, &entry_text_ref);

        // Strict mode: don't recurse into sourced files. Mirrors resholve.
        if self.strict {
            let mut units = std::collections::HashMap::new();
            units.insert(entry_id, entry_unit);
            return MultiFileResolve {
                map,
                solutions: all_solutions,
                units,
                entry_id,
            };
        }

        // Walk the source graph; for each loaded file, lower it (with a
        // fresh source_id from `map`), resolve its commands against the
        // merged auto-scope, and append the solutions.
        let mut visited: HashSet<PathBuf> = HashSet::new();
        if let Some(p) = entry_path {
            if let Ok(canon) = p.canonicalize() {
                visited.insert(canon);
            } else {
                visited.insert(p.to_path_buf());
            }
        }
        let mut units = std::collections::HashMap::new();
        units.insert(entry_id, entry_unit.clone());
        self.walk_sources_into_map(
            &entry_unit,
            &entry_text_ref,
            entry_path,
            &mut visited,
            &mut map,
            &mut units,
            &mut all_solutions,
        );

        MultiFileResolve {
            map,
            solutions: all_solutions,
            units,
            entry_id,
        }
    }

    /// Walks every `source`/`.` reachable from `unit`. For each loaded
    /// file: register in `map` (allocating a fresh [`SourceId`]), lower
    /// it, resolve its commands, and recurse into its own source graph.
    /// Cycle-safe via `visited`.
    #[allow(clippy::too_many_arguments)]
    fn walk_sources_into_map(
        &self,
        unit: &SourceUnit,
        source: &str,
        script_path: Option<&Path>,
        visited: &mut HashSet<PathBuf>,
        map: &mut SourceMap,
        units: &mut std::collections::HashMap<SourceId, SourceUnit>,
        out: &mut Vec<Solution>,
    ) {
        let magic = MagicVars {
            script_path: script_path.map(Path::to_path_buf),
            home: self.magic_vars.home.clone(),
        };
        // Collect children first so we don't borrow `unit` while mutating.
        let mut children: Vec<(PathBuf, String)> = Vec::new();
        for cmd in &unit.commands {
            if let CommandLike::Source { target, .. } = cmd {
                let Some(path_str) = target
                    .as_static()
                    .map(str::to_string)
                    .or_else(|| magic_vars::expand_word(target, source, &magic))
                else {
                    continue;
                };
                let base_dir = script_path
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .or_else(|| self.script_dir.clone());
                let Some(abs) = self.inputs.resolve_source(&path_str, base_dir.as_deref()) else {
                    continue;
                };
                let canon = abs.canonicalize().unwrap_or(abs);
                if !visited.insert(canon.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&canon) else {
                    continue;
                };
                children.push((canon, text));
            }
        }
        // Recurse into nested function bodies' Source directives too —
        // matches `walk_sources` semantics.
        for cmd in &unit.commands {
            if let CommandLike::Function { body, .. } = cmd {
                self.walk_sources_into_map(body, source, script_path, visited, map, units, out);
            }
        }
        for (canon, text) in children {
            let id = map.add(canon.clone(), text.clone());
            let Ok(out_lower) = BashFrontend::default().lower(&text, id) else {
                continue;
            };
            // Resolve this file's commands. The unit's solutions carry
            // its own source_id (via `walk_unit`).
            let sols = self.resolve_unit(&out_lower.unit, &text);
            out.extend(sols);
            units.insert(id, out_lower.unit.clone());
            // Recurse: resolve files this one sources, too.
            self.walk_sources_into_map(
                &out_lower.unit,
                &text,
                Some(&canon),
                visited,
                map,
                units,
                out,
            );
        }
    }

    /// Resolve every command-like in `unit`. `source` is the original
    /// source text; spans on words are byte offsets into it. We need it
    /// to look up the literal text of dynamic pieces for magic-var
    /// expansion. Pass `""` if magic-var expansion isn't needed.
    pub fn resolve_unit(&self, unit: &SourceUnit, source: &str) -> Vec<Solution> {
        let mut out = Vec::new();
        let mut scope = Scope::from_unit(unit);
        let varmap = if self.strict {
            VarMap::default()
        } else {
            VarMap::from_unit(unit)
        };

        // Auto-source-graph: harvest function and alias names from every
        // statically resolvable `source`/`.` directive (recursively) and
        // merge them into the entry script's scope. Disabled in strict
        // mode — matches resholve's behavior, where you must spell out
        // `--allow function=name` for every function defined in a sourced
        // file.
        if !self.strict {
            let mut harvest = Scope::default();
            let mut visited: HashSet<PathBuf> = HashSet::new();
            if let Some(p) = &self.magic_vars.script_path {
                if let Ok(canon) = p.canonicalize() {
                    visited.insert(canon);
                } else {
                    visited.insert(p.clone());
                }
            }
            self.harvest_sourced_decls(
                unit,
                source,
                self.magic_vars.script_path.as_deref(),
                &mut visited,
                &mut harvest,
            );
            scope.extend(&harvest);
        }

        self.walk_unit(unit, &scope, &varmap, source, &mut out);
        out.sort_by_key(Solution::span);
        out.dedup();
        out
    }

    fn walk_unit(
        &self,
        unit: &SourceUnit,
        scope: &Scope,
        varmap: &VarMap,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        let source_id = unit.source_id;
        for cmd in &unit.commands {
            self.resolve_command(cmd, source_id, scope, varmap, source, out);
        }
        // Bare assignments (`path=$(basename …)`) don't surface as
        // CommandLike::Simple, so the cmdsub-recursion that runs in
        // `resolve_invocation` would miss them. Walk the harvested
        // assignments' RHS Words here so any `$(…)` inside resolves.
        if !self.strict {
            for assign in &unit.var_assignments {
                self.recurse_into_cmd_subs(&assign.value, source_id, scope, varmap, source, out);
            }
        }
    }

    fn resolve_command(
        &self,
        cmd: &CommandLike,
        source_id: SourceId,
        scope: &Scope,
        varmap: &VarMap,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        match cmd {
            CommandLike::Simple(inv) => {
                self.resolve_invocation(inv, source_id, scope, varmap, source, out)
            }
            CommandLike::Function { body, .. } => {
                // Bash function names share a global table — every
                // outer function/alias is visible inside this body.
                // (A nested function defined *inside* this body stays
                // local to it, which falls out naturally because we
                // only merge the body's own decls.)
                let mut inner = scope.clone();
                inner.merge_from_unit(body);
                // Function bodies start with their own VarMap; outer
                // assignments don't yet propagate (conservative scope).
                let inner_vars = if self.strict {
                    VarMap::default()
                } else {
                    VarMap::from_unit(body)
                };
                self.walk_unit(body, &inner, &inner_vars, source, out);
            }
            CommandLike::Source { target, .. } => {
                self.resolve_source(target, source_id, source, out)
            }
            CommandLike::Alias { definition, .. } => {
                // v0.1: surface the alias body as InScope; full alias
                // expansion (resolving the commands *inside* the body) is
                // v0.2 work.
                if let Some(name) = definition.as_static() {
                    out.push(Solution::InScope {
                        source_id,
                        span: definition.span,
                        name: name.to_string(),
                        kind: InScopeKind::Alias,
                    });
                }
            }
        }
    }

    fn resolve_invocation(
        &self,
        inv: &Invocation,
        source_id: SourceId,
        scope: &Scope,
        varmap: &VarMap,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        // Auto-recurse into `$(…)` substitutions inside *any* word
        // (command name or argument). Disabled in strict mode to
        // mirror resholve, which only sees the outer `$(…)` opaquely.
        if !self.strict {
            for w in &inv.words {
                self.recurse_into_cmd_subs(w, source_id, scope, varmap, source, out);
            }
        }

        let Some(first_word) = inv.words.first() else {
            return;
        };
        let name = first_word.as_static();

        // Auto-trace varsub: if the command word is dynamic, see if it's
        // a `$NAME` / `${NAME}` reference to a single-binding literal
        // assignment. If so, re-classify with the bound value and emit a
        // Resolved (or whatever the bound name classifies as).
        let traced_name = if name.is_none() {
            self.trace_var(first_word, varmap, source)
        } else {
            None
        };
        let effective_name: Option<&str> = name.or(traced_name.as_deref());

        let cls = classify(effective_name, scope);
        match cls {
            Classification::Dynamic => {
                self.push_dynamic(first_word, source_id, source, out);
            }
            Classification::Function => {
                self.push_in_scope(
                    effective_name,
                    source_id,
                    first_word,
                    InScopeKind::Function,
                    out,
                );
            }
            Classification::Alias => {
                self.push_in_scope(
                    effective_name,
                    source_id,
                    first_word,
                    InScopeKind::Alias,
                    out,
                );
            }
            Classification::Keyword => {
                self.push_in_scope(
                    effective_name,
                    source_id,
                    first_word,
                    InScopeKind::Keyword,
                    out,
                );
            }
            Classification::SpecialBuiltin => {
                self.push_in_scope(
                    effective_name,
                    source_id,
                    first_word,
                    InScopeKind::SpecialBuiltin,
                    out,
                );
            }
            Classification::Builtin => {
                self.push_in_scope(
                    effective_name,
                    source_id,
                    first_word,
                    InScopeKind::Builtin,
                    out,
                );
            }
            Classification::External => {
                self.resolve_external(first_word, source_id, effective_name.unwrap(), out);
            }
        }

        // Recurse into wrapper commands regardless of how the wrapper
        // itself resolved (builtin for `command`/`exec`, external for
        // `sudo`/`env`/...). The active wrapper set respects the lore
        // overrides and strict mode. Use the effective name here so a
        // traced `$cmd → sudo` still recurses.
        if let Some(name) = effective_name {
            if self.is_active_wrapper(name) {
                for inner in find_wrapped_commands(&inv.words) {
                    self.resolve_inner_word(inner, source_id, scope, source, out);
                }
            }
        }
    }

    /// Walk every piece of `word` and, for each `$(…)` substitution
    /// that the frontend re-parsed into a nested [`SourceUnit`],
    /// resolve the commands inside it. The inner unit shares the
    /// surrounding scope (commands inside `$(…)` see outer functions
    /// and aliases) and the surrounding varmap (so `cmd=git; "$(…)"`
    /// flows through). Spans inside the inner unit are absolute
    /// offsets into the outer source, so the rewriter splices
    /// inner-command resolutions transparently.
    ///
    /// Recursion happens automatically because the inner unit's own
    /// invocations re-enter `resolve_invocation`, which calls back
    /// into this helper.
    fn recurse_into_cmd_subs(
        &self,
        word: &Word,
        source_id: SourceId,
        scope: &Scope,
        varmap: &VarMap,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        for piece in &word.pieces {
            self.recurse_piece(piece, source_id, scope, varmap, source, out);
        }
    }

    fn recurse_piece(
        &self,
        piece: &WordPiece,
        source_id: SourceId,
        scope: &Scope,
        varmap: &VarMap,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        match piece {
            WordPiece::CommandSub { inner, .. } => {
                // Inner unit's own source_id (set by the frontend on
                // re-parse) already matches the outer file — its spans
                // are absolute outer-source byte offsets.
                let _ = source_id;
                self.walk_unit(inner, scope, varmap, source, out);
            }
            WordPiece::DoubleQuoted { pieces, .. } => {
                for p in pieces {
                    self.recurse_piece(p, source_id, scope, varmap, source, out);
                }
            }
            _ => {}
        }
    }

    /// Try to trace a dynamic command word back to its literal value via
    /// the in-scope `VarMap`. Returns `Some(value)` on a hit (the
    /// resolver re-classifies as if the source said that name); `None`
    /// otherwise — the caller falls back to push_dynamic.
    fn trace_var(&self, word: &Word, varmap: &VarMap, source: &str) -> Option<String> {
        if varmap.is_empty() {
            return None;
        }
        let text = source.get(word.span.start..word.span.end)?;
        // Strip a single layer of double-quotes if present: `"$cmd"`
        // shows up as a `Word` whose source slice includes them.
        let unquoted = text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(text);
        let name = varsub::parse_var_name(unquoted)?;
        varmap.lookup(name).map(str::to_string)
    }

    /// Common handler for dynamic command words. If the literal source
    /// text is a positional / special variable (`$@`, `$1`, `$$`, …),
    /// auto-allow it instead of erroring — the script is forwarding its
    /// own arguments and there's nothing meaningful to rewrite. Strict
    /// mode skips this check and always emits Unresolved.
    fn push_dynamic(
        &self,
        word: &Word,
        source_id: SourceId,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        if !self.strict {
            let text = source.get(word.span.start..word.span.end);
            if let Some(text) = text {
                if auto_skip::is_well_known_dynamic(text) {
                    out.push(Solution::Allowed {
                        source_id,
                        span: word.span,
                        name: Some(text.to_string()),
                        reason: "well-known-dynamic".to_string(),
                    });
                    return;
                }
            }
        }
        out.push(Solution::Unresolved {
            source_id,
            span: word.span,
            name: None,
            kind: UnresolvedKind::DynamicCommandName,
            hint: None,
        });
    }

    fn push_in_scope(
        &self,
        name: Option<&str>,
        source_id: SourceId,
        word: &Word,
        kind: InScopeKind,
        out: &mut Vec<Solution>,
    ) {
        if let Some(name) = name {
            out.push(Solution::InScope {
                source_id,
                span: word.span,
                name: name.to_string(),
                kind,
            });
        }
    }

    fn resolve_external(
        &self,
        word: &Word,
        source_id: SourceId,
        name: &str,
        out: &mut Vec<Solution>,
    ) {
        match self.inputs.resolve(name) {
            Some(abs) => out.push(Solution::Resolved {
                source_id,
                initial: word.span,
                original: name.to_string(),
                replacement: abs.to_string_lossy().into_owned(),
                kind: ResolvedKind::External,
            }),
            None => out.push(Solution::Unresolved {
                source_id,
                span: word.span,
                name: Some(name.to_string()),
                kind: UnresolvedKind::UnknownExternal,
                hint: None,
            }),
        }
    }

    fn resolve_inner_word(
        &self,
        word: &Word,
        source_id: SourceId,
        scope: &Scope,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        // The wrapped command undergoes the same CRO as a top-level
        // simple command (modulo InvocationContext, deferred to v0.2).
        match classify(word.as_static(), scope) {
            Classification::Dynamic => {
                self.push_dynamic(word, source_id, source, out);
            }
            Classification::External => {
                self.resolve_external(word, source_id, word.as_static().unwrap(), out);
            }
            cls => {
                let kind = match cls {
                    Classification::Function => InScopeKind::Function,
                    Classification::Alias => InScopeKind::Alias,
                    Classification::Keyword => InScopeKind::Keyword,
                    Classification::SpecialBuiltin => InScopeKind::SpecialBuiltin,
                    Classification::Builtin => InScopeKind::Builtin,
                    Classification::Dynamic | Classification::External => unreachable!(),
                };
                self.push_in_scope(word.as_static(), source_id, word, kind, out);
            }
        }
    }

    fn resolve_source(
        &self,
        target: &Word,
        source_id: SourceId,
        source: &str,
        out: &mut Vec<Solution>,
    ) {
        // Pure-static path is the easy case.
        let static_path = target
            .as_static()
            .map(str::to_string)
            .or_else(|| self.expand_with_magic_vars(target, source));

        let Some(s) = static_path else {
            out.push(Solution::Unresolved {
                source_id,
                span: target.span,
                name: None,
                kind: UnresolvedKind::DynamicSourcePath,
                hint: None,
            });
            return;
        };
        match self.inputs.resolve_source(&s, self.script_dir.as_deref()) {
            Some(abs) => out.push(Solution::Resolved {
                source_id,
                initial: target.span,
                original: s,
                replacement: abs.to_string_lossy().into_owned(),
                kind: ResolvedKind::SourceFile,
            }),
            None => out.push(Solution::Unresolved {
                source_id,
                span: target.span,
                name: Some(s),
                kind: UnresolvedKind::UnreadableSource,
                hint: None,
            }),
        }
    }

    fn expand_with_magic_vars(&self, word: &Word, source: &str) -> Option<String> {
        magic_vars::expand_word(word, source, &self.magic_vars)
    }

    /// Walk every `Source` directive in `unit` (recursively), parse each
    /// sourced file, and merge its top-level function and alias names
    /// into `harvest`. Cycle-safe via `visited` — each canonicalized path
    /// is loaded at most once.
    ///
    /// We deliberately do *not* re-resolve commands inside sourced files;
    /// that's deferred to v0.3. v0.2's job is the cheap, high-value win:
    /// make function calls from sourced libraries resolve without `--allow`.
    fn harvest_sourced_decls(
        &self,
        unit: &SourceUnit,
        source: &str,
        script_path: Option<&Path>,
        visited: &mut HashSet<PathBuf>,
        harvest: &mut Scope,
    ) {
        self.walk_sources(unit, source, script_path, visited, 0, &mut |loaded| {
            for name in &loaded.unit.functions_defined {
                harvest.functions.insert(name.clone());
            }
            for (name, _def) in &loaded.unit.aliases_defined {
                harvest.aliases.insert(name.clone());
            }
        });
    }

    /// Build the source graph rooted at `unit`: a flat list of every file
    /// reachable via `source`/`.`, in DFS visit order. The first entry is
    /// the entry script itself (depth 0). Each subsequent entry records
    /// the canonical path, the depth, and the function/alias names that
    /// file contributes to the global scope.
    ///
    /// Used by the `rusholve sources` subcommand and as a debug aid.
    pub fn source_graph(
        &self,
        unit: &SourceUnit,
        source: &str,
        script_path: Option<&Path>,
    ) -> Vec<SourceNode> {
        let mut out = Vec::new();
        // Entry node — depth 0. We don't have decls aggregated here in
        // the same shape, but they're directly on the unit.
        out.push(SourceNode {
            path: script_path.map(Path::to_path_buf),
            depth: 0,
            functions_defined: unit.functions_defined.clone(),
            aliases_defined: unit
                .aliases_defined
                .iter()
                .map(|(n, _)| n.clone())
                .collect(),
        });

        let mut visited: HashSet<PathBuf> = HashSet::new();
        if let Some(p) = script_path {
            if let Ok(canon) = p.canonicalize() {
                visited.insert(canon);
            } else {
                visited.insert(p.to_path_buf());
            }
        }
        self.walk_sources(unit, source, script_path, &mut visited, 1, &mut |loaded| {
            out.push(SourceNode {
                path: Some(loaded.canonical_path.clone()),
                depth: loaded.depth,
                functions_defined: loaded.unit.functions_defined.clone(),
                aliases_defined: loaded
                    .unit
                    .aliases_defined
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect(),
            });
        });
        out
    }

    /// Generic source-graph traversal. For every `source`/`.` whose path
    /// resolves to an existing file we haven't visited, parse the file
    /// and invoke `cb` once with the loaded unit + its canonical path.
    /// Recurses into the loaded file's own `source` directives.
    ///
    /// Cycle-safe via `visited`. `script_path` is the *current* file
    /// (used to set `BASH_SOURCE` for magic-var expansion in this file's
    /// source paths). `depth` is forwarded to the callback.
    fn walk_sources(
        &self,
        unit: &SourceUnit,
        source: &str,
        script_path: Option<&Path>,
        visited: &mut HashSet<PathBuf>,
        depth: usize,
        cb: &mut dyn FnMut(LoadedSource<'_>),
    ) {
        let magic = MagicVars {
            script_path: script_path.map(Path::to_path_buf),
            home: self.magic_vars.home.clone(),
        };
        for cmd in &unit.commands {
            match cmd {
                CommandLike::Source { target, .. } => {
                    let Some(path_str) = target
                        .as_static()
                        .map(str::to_string)
                        .or_else(|| magic_vars::expand_word(target, source, &magic))
                    else {
                        continue;
                    };
                    let base_dir = script_path
                        .and_then(Path::parent)
                        .map(Path::to_path_buf)
                        .or_else(|| self.script_dir.clone());
                    let Some(abs) = self.inputs.resolve_source(&path_str, base_dir.as_deref())
                    else {
                        continue;
                    };
                    let canon = abs.canonicalize().unwrap_or(abs);
                    if !visited.insert(canon.clone()) {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(&canon) else {
                        continue;
                    };
                    let Ok(out) = BashFrontend::default().lower(&text, SourceId::new(0)) else {
                        continue;
                    };
                    cb(LoadedSource {
                        canonical_path: canon.clone(),
                        depth,
                        unit: &out.unit,
                    });
                    self.walk_sources(&out.unit, &text, Some(&canon), visited, depth + 1, cb);
                }
                CommandLike::Function { body, .. } => {
                    self.walk_sources(body, source, script_path, visited, depth, cb);
                }
                _ => {}
            }
        }
    }
}

/// Multi-file resolution result returned by [`Resolver::resolve_with_sources`].
///
/// `solutions` is mixed across the source graph; partition by
/// [`Solution::source_id`] to get per-file edits. `map` owns every
/// loaded file's text — the rewriter splices against `map.get(id).text`.
#[derive(Debug, Clone)]
pub struct MultiFileResolve {
    pub map: SourceMap,
    pub solutions: Vec<Solution>,
    /// Lowered units per source. Useful for downstream re-walks (e.g.
    /// re-running `apply` directives on each unit's solutions).
    pub units: std::collections::HashMap<SourceId, SourceUnit>,
    pub entry_id: SourceId,
}

impl MultiFileResolve {
    /// Group solutions by their attributed `source_id`.
    pub fn solutions_by_file(&self) -> std::collections::HashMap<SourceId, Vec<Solution>> {
        let mut out: std::collections::HashMap<SourceId, Vec<Solution>> =
            std::collections::HashMap::new();
        for s in &self.solutions {
            out.entry(s.source_id()).or_default().push(s.clone());
        }
        out
    }
}

/// One entry in the source graph returned by [`Resolver::source_graph`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceNode {
    /// Canonical path to the file. `None` only for the entry node when
    /// the caller didn't supply a script path.
    pub path: Option<PathBuf>,
    /// 0 for the entry script, 1 for files it directly sources, 2 for
    /// files those source, and so on.
    pub depth: usize,
    /// Top-level function names defined in this file.
    pub functions_defined: Vec<String>,
    /// Top-level alias names defined in this file.
    pub aliases_defined: Vec<String>,
}

/// Internal: passed to the [`Resolver::walk_sources`] callback for each
/// loaded file.
struct LoadedSource<'a> {
    canonical_path: PathBuf,
    depth: usize,
    unit: &'a SourceUnit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{bash::BashFrontend, Frontend};
    use crate::ir::SourceId;
    use std::fs;
    use tempfile::TempDir;

    fn lower(source: &str) -> SourceUnit {
        BashFrontend::default()
            .lower(source, SourceId::new(0))
            .expect("parses")
            .unit
    }

    fn make_inputs(setup: &TempDir, names: &[&str]) -> Inputs {
        for name in names {
            fs::write(setup.path().join(name), "#!/bin/sh\n").unwrap();
        }
        // Disable auto-wrappers in unit tests so resolution doesn't depend
        // on the test host having `/run/wrappers/bin/<name>` populated.
        // The auto-wrappers behavior is exercised in `inputs::tests`.
        Inputs::new([setup.path()]).with_wrappers_dir(None)
    }

    #[test]
    fn resolves_a_single_external() {
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["jq"]);
        let resolver = Resolver::new(inputs);
        let src = "jq .";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        match &sols[0] {
            Solution::Resolved {
                replacement, kind, ..
            } => {
                assert!(replacement.ends_with("/jq"));
                assert_eq!(*kind, ResolvedKind::External);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn unknown_external_is_unresolved() {
        let resolver = Resolver::default();
        let src = "nonexistent-tool arg";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::Unresolved {
                kind: UnresolvedKind::UnknownExternal,
                ..
            }
        ));
    }

    #[test]
    fn dynamic_command_is_unresolved() {
        let resolver = Resolver::default();
        let src = "$cmd args";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::Unresolved {
                kind: UnresolvedKind::DynamicCommandName,
                ..
            }
        ));
    }

    #[test]
    fn auto_skip_dollar_at_is_allowed() {
        // `"$@" rest` — passing positional args through. Don't flag this.
        let resolver = Resolver::default();
        let src = r#""$@" rest"#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        match &sols[0] {
            Solution::Allowed { reason, name, .. } => {
                assert_eq!(reason, "well-known-dynamic");
                assert_eq!(name.as_deref(), Some(r#""$@""#));
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn auto_skip_dollar_one_is_allowed() {
        let resolver = Resolver::default();
        let src = "$1 args";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert!(
            matches!(sols[0], Solution::Allowed { .. }),
            "expected Allowed for $1, got {sols:?}"
        );
    }

    #[test]
    fn auto_skip_through_exec_wrapper() {
        // `exec "$@"` — exec is a special builtin (InScope), and the
        // inner $@ should auto-allow rather than emit DynamicCommandName.
        let resolver = Resolver::default();
        let src = r#"exec "$@""#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        // exec is InScope::SpecialBuiltin; "$@" is Allowed.
        let inner_allowed = sols.iter().any(|s| matches!(s, Solution::Allowed { .. }));
        assert!(
            inner_allowed,
            "expected exec-inner $@ to be Allowed: {sols:?}"
        );
    }

    #[test]
    fn builtin_is_in_scope() {
        let resolver = Resolver::default();
        let src = "echo hi";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::InScope {
                kind: InScopeKind::Builtin,
                ..
            }
        ));
    }

    #[test]
    fn function_shadowing_external_resolves_in_scope() {
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["greet"]);
        let resolver = Resolver::new(inputs);
        let src = "greet() { echo hi; }\ngreet\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        // We expect: function definition body has `echo` (InScope builtin),
        // and the top-level `greet` invocation is InScope::Function.
        let function_call = sols
            .iter()
            .find(|s| matches!(s, Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "greet"));
        assert!(
            function_call.is_some(),
            "expected greet call to resolve as function: {sols:?}"
        );
    }

    #[test]
    fn env_wrapper_resolves_inner_command() {
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["env", "jq"]);
        let resolver = Resolver::new(inputs);
        let src = "env FOO=1 jq .";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        // Two Resolved solutions expected: env itself and jq.
        let mut originals: Vec<&str> = sols
            .iter()
            .filter_map(|s| match s {
                Solution::Resolved { original, .. } => Some(original.as_str()),
                _ => None,
            })
            .collect();
        originals.sort();
        assert_eq!(originals, vec!["env", "jq"]);
    }

    #[test]
    fn sudo_wrapper_resolves_inner_command() {
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["sudo", "systemctl"]);
        let resolver = Resolver::new(inputs);
        let src = "sudo -E systemctl restart nginx";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let originals: Vec<&str> = sols
            .iter()
            .filter_map(|s| match s {
                Solution::Resolved { original, .. } => Some(original.as_str()),
                _ => None,
            })
            .collect();
        assert!(originals.contains(&"sudo"));
        assert!(originals.contains(&"systemctl"));
    }

    #[test]
    fn command_wrapper_resolves_inner() {
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["git"]);
        let resolver = Resolver::new(inputs);
        let src = "command git status";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        // `command` is a builtin (InScope), `git` is External (Resolved).
        assert!(sols.iter().any(|s| matches!(
            s,
            Solution::InScope {
                kind: InScopeKind::SpecialBuiltin,
                name,
                ..
            } if name == "command"
        ) || matches!(
            s,
            Solution::InScope {
                kind: InScopeKind::Builtin,
                name,
                ..
            } if name == "command"
        )));
        assert!(sols.iter().any(|s| matches!(
            s,
            Solution::Resolved { original, .. } if original == "git"
        )));
    }

    #[test]
    fn source_with_relative_path_resolves() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("lib.sh"), "true").unwrap();
        let resolver = Resolver::default().with_script_dir(dir.path().to_path_buf());
        let src = "source ./lib.sh";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        match &sols[0] {
            Solution::Resolved {
                kind, replacement, ..
            } => {
                assert_eq!(*kind, ResolvedKind::SourceFile);
                assert!(replacement.ends_with("lib.sh"));
            }
            other => panic!("expected Resolved source, got {other:?}"),
        }
    }

    #[test]
    fn dynamic_source_path_is_unresolved() {
        let resolver = Resolver::default();
        let src = r#"source "$LIB""#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::Unresolved {
                kind: UnresolvedKind::DynamicSourcePath,
                ..
            }
        ));
    }

    #[test]
    fn auto_magic_var_bash_source_dir() {
        // `source "${BASH_SOURCE%/*}/lib.sh"` resolves when we know
        // the entry-script path. This is the most common nixpkgs pattern.
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("entry.sh");
        fs::write(&script, "true").unwrap();
        let lib = dir.path().join("lib.sh");
        fs::write(&lib, "true").unwrap();

        let resolver = Resolver::default()
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(script.clone()),
                home: None,
            });
        let src = r#"source "${BASH_SOURCE%/*}/lib.sh""#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        match &sols[0] {
            Solution::Resolved {
                kind, replacement, ..
            } => {
                assert_eq!(*kind, ResolvedKind::SourceFile);
                assert!(
                    replacement.ends_with("lib.sh"),
                    "expected lib.sh, got {replacement}"
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn auto_magic_var_home() {
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("init.sh");
        fs::write(&lib, "true").unwrap();

        let resolver = Resolver::default().with_magic_vars(MagicVars {
            script_path: None,
            home: Some(dir.path().to_path_buf()),
        });
        let src = r#"source "$HOME/init.sh""#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::Resolved {
                kind: ResolvedKind::SourceFile,
                ..
            }
        ));
    }

    #[test]
    fn auto_source_graph_harvests_function_from_sourced_file() {
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("lib.sh");
        fs::write(&lib, "helper() { :; }\n").unwrap();
        let entry = dir.path().join("entry.sh");
        fs::write(&entry, "").unwrap();

        let resolver = Resolver::default()
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let src = "source ./lib.sh\nhelper\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);

        // We expect: source resolves, and `helper` resolves as a function
        // (not an unknown external).
        assert!(
            sols.iter().any(|s| matches!(
                s,
                Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "helper"
            )),
            "expected helper to be harvested as function: {sols:?}"
        );
    }

    #[test]
    fn auto_source_graph_harvests_alias_from_sourced_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("lib.sh"), "alias ll='ls -la'\n").unwrap();
        let entry = dir.path().join("entry.sh");
        fs::write(&entry, "").unwrap();

        let resolver = Resolver::default()
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let src = "source ./lib.sh\nll\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);

        assert!(
            sols.iter().any(|s| matches!(
                s,
                Solution::InScope { kind: InScopeKind::Alias, name, .. } if name == "ll"
            )),
            "expected ll to be harvested as alias: {sols:?}"
        );
    }

    #[test]
    fn auto_source_graph_is_cycle_safe() {
        // a.sh sources b.sh which sources a.sh — must terminate.
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.sh");
        let b = dir.path().join("b.sh");
        fs::write(&a, "source ./b.sh\nfn_a() { :; }\n").unwrap();
        fs::write(&b, "source ./a.sh\nfn_b() { :; }\n").unwrap();

        let resolver = Resolver::default()
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(a.clone()),
                home: None,
            });
        let src = "source ./b.sh\nfn_b\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert!(sols.iter().any(|s| matches!(
            s,
            Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "fn_b"
        )));
    }

    #[test]
    fn auto_source_graph_recurses_transitively() {
        // entry.sh → a.sh → b.sh; a function defined in b.sh should be
        // visible from entry.sh's scope.
        let dir = TempDir::new().unwrap();
        let entry = dir.path().join("entry.sh");
        let a = dir.path().join("a.sh");
        let b = dir.path().join("b.sh");
        fs::write(&entry, "").unwrap();
        fs::write(&a, "source ./b.sh\n").unwrap();
        fs::write(&b, "deep_helper() { :; }\n").unwrap();

        let resolver = Resolver::default()
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let src = "source ./a.sh\ndeep_helper\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert!(
            sols.iter().any(|s| matches!(
                s,
                Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "deep_helper"
            )),
            "expected transitively-sourced function to resolve: {sols:?}"
        );
    }

    #[test]
    fn auto_source_graph_does_not_panic_on_unreadable_source() {
        // Missing file: harvest silently skips, and the main resolution
        // pass surfaces UnreadableSource via resolve_source.
        let dir = TempDir::new().unwrap();
        let resolver = Resolver::default().with_script_dir(dir.path().to_path_buf());
        let src = "source ./does-not-exist.sh\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert!(matches!(
            sols[0],
            Solution::Unresolved {
                kind: UnresolvedKind::UnreadableSource,
                ..
            }
        ));
    }

    #[test]
    fn outer_function_is_visible_inside_another_function_body() {
        // Bash semantics: function names share a global table, so a
        // function defined at top level is callable from inside any
        // other function's body. The resolver must not treat the
        // inner body as a fresh scope that loses outer decls.
        let resolver = Resolver::default();
        let src = "info() { echo hi; }\nrun() { info \"starting\"; }\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let info_call = sols.iter().any(|s| {
            matches!(
                s,
                Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "info"
            )
        });
        assert!(
            info_call,
            "outer `info` must resolve as Function inside `run`'s body: {sols:?}"
        );
    }

    #[test]
    fn nested_function_does_not_leak_to_outer_scope() {
        // Negative: a function defined *inside* another body is not
        // visible at the outer level (statically — runtime visibility
        // depends on whether the outer was called, but we can't model
        // that). Outer call to `inner` should classify as External
        // (or Unresolved) — definitely not Function.
        let resolver = Resolver::default();
        let src = "outer() { inner() { :; }; }\ninner foo\n";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let leaked = sols.iter().any(|s| {
            matches!(
                s,
                Solution::InScope { kind: InScopeKind::Function, name, .. } if name == "inner"
            )
        });
        assert!(
            !leaked,
            "nested `inner` must not leak to outer scope: {sols:?}"
        );
    }

    #[test]
    fn auto_recurse_resolves_command_inside_cmd_sub() {
        // The migration-from-resholve case: `path="$(basename "$0")"`
        // and `command="$(getopt -o foo)"`. Both must resolve their
        // inner command (basename, getopt) to absolute paths.
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["basename", "getopt"]);
        let resolver = Resolver::new(inputs);
        let src = r#"path="$(basename "$0")"
out="$(getopt -o foo)"
"#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let originals: Vec<&str> = sols
            .iter()
            .filter_map(|s| match s {
                Solution::Resolved { original, .. } => Some(original.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            originals.contains(&"basename"),
            "expected basename inside $(...) to resolve: {sols:?}"
        );
        assert!(
            originals.contains(&"getopt"),
            "expected getopt inside $(...) to resolve: {sols:?}"
        );
    }

    #[test]
    fn auto_recurse_inner_span_points_at_outer_source() {
        // The whole point of the padding trick: the rewriter must be
        // able to splice an inner-command rewrite using the inner
        // span as an outer-source byte range.
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["getopt"]);
        let resolver = Resolver::new(inputs);
        let src = "x=$(getopt -o ab)";
        // bare assignment — but the cmdsub Word lives on the
        // var_assignments[0]; we instead test via a real argument:
        let src = format!("foo {src}");
        let _ = src; // shadow; keep simple form below
        let src = "foo $(getopt -o ab)";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let getopt_sol = sols.iter().find_map(|s| match s {
            Solution::Resolved {
                original, initial, ..
            } if original == "getopt" => Some(*initial),
            _ => None,
        });
        let span = getopt_sol.expect("expected getopt to be Resolved");
        assert_eq!(&src[span.start..span.end], "getopt");
    }

    #[test]
    fn strict_mode_does_not_recurse_into_cmd_sub() {
        // strict mode mirrors resholve — `$(...)` stays opaque, the
        // inner `getopt` is invisible.
        let dir = TempDir::new().unwrap();
        let inputs = make_inputs(&dir, &["getopt"]);
        let resolver = Resolver::new(inputs).with_strict(true);
        let src = "foo $(getopt -o ab)";
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        let getopt_resolved = sols.iter().any(|s| {
            matches!(
                s,
                Solution::Resolved { original, .. } if original == "getopt"
            )
        });
        assert!(
            !getopt_resolved,
            "strict mode must not recurse into $(...): {sols:?}"
        );
    }

    #[test]
    fn multi_file_resolve_resolves_commands_in_sourced_files() {
        // The whole point of v0.3 multi-file spans: when entry.sh sources
        // lib.sh, lib.sh's commands are also resolved (against the merged
        // scope) and surface with their own source_id. The rewriter can
        // then route splices to the right file.
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("lib.sh");
        fs::write(&lib, "jq .\nhelper() { :; }\n").unwrap();
        let entry = dir.path().join("entry.sh");
        fs::write(&entry, "source ./lib.sh\nhelper\ngrep foo bar\n").unwrap();

        let inputs = make_inputs(&dir, &["jq", "grep"]);
        let resolver = Resolver::new(inputs)
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let entry_text = fs::read_to_string(&entry).unwrap();
        let unit = BashFrontend::default()
            .lower(&entry_text, SourceId::new(0))
            .unwrap()
            .unit;
        let multi = resolver.resolve_with_sources(unit, entry_text, Some(&entry));

        // grep is in entry.sh, jq is in lib.sh. Both should resolve.
        let grep_src = multi.solutions.iter().find_map(|s| match s {
            Solution::Resolved {
                source_id,
                original,
                ..
            } if original == "grep" => Some(*source_id),
            _ => None,
        });
        let jq_src = multi.solutions.iter().find_map(|s| match s {
            Solution::Resolved {
                source_id,
                original,
                ..
            } if original == "jq" => Some(*source_id),
            _ => None,
        });
        let grep_src = grep_src.expect("grep should be resolved");
        let jq_src = jq_src.expect("jq inside lib.sh should be resolved");
        assert_eq!(grep_src, multi.entry_id);
        assert_ne!(jq_src, multi.entry_id, "jq must be attributed to lib.sh");
        // Map should know about both files.
        assert_eq!(multi.map.len(), 2);
    }

    #[test]
    fn multi_file_resolve_strict_mode_skips_sourced_files() {
        // Strict mode: mirror resholve. Sourced files stay opaque, no
        // re-resolution. Only the entry's solutions appear.
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("lib.sh");
        fs::write(&lib, "jq .\n").unwrap();
        let entry = dir.path().join("entry.sh");
        fs::write(&entry, "source ./lib.sh\n").unwrap();

        let inputs = make_inputs(&dir, &["jq"]);
        let resolver = Resolver::new(inputs)
            .with_script_dir(dir.path().to_path_buf())
            .with_strict(true)
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let entry_text = fs::read_to_string(&entry).unwrap();
        let unit = BashFrontend::default()
            .lower(&entry_text, SourceId::new(0))
            .unwrap()
            .unit;
        let multi = resolver.resolve_with_sources(unit, entry_text, Some(&entry));

        // jq from lib.sh must NOT appear — strict mode is opaque.
        let jq_resolved = multi.solutions.iter().any(|s| {
            matches!(
                s, Solution::Resolved { original, .. } if original == "jq"
            )
        });
        assert!(!jq_resolved);
    }

    #[test]
    fn multi_file_resolve_solutions_by_file_partitions_correctly() {
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("lib.sh");
        fs::write(&lib, "jq .\n").unwrap();
        let entry = dir.path().join("entry.sh");
        fs::write(&entry, "source ./lib.sh\ngrep foo bar\n").unwrap();

        let inputs = make_inputs(&dir, &["jq", "grep"]);
        let resolver = Resolver::new(inputs)
            .with_script_dir(dir.path().to_path_buf())
            .with_magic_vars(MagicVars {
                script_path: Some(entry.clone()),
                home: None,
            });
        let entry_text = fs::read_to_string(&entry).unwrap();
        let unit = BashFrontend::default()
            .lower(&entry_text, SourceId::new(0))
            .unwrap()
            .unit;
        let multi = resolver.resolve_with_sources(unit, entry_text, Some(&entry));

        let by_file = multi.solutions_by_file();
        // Both files must show up as keys; entry contains source-resolve +
        // grep, lib contains jq.
        assert_eq!(by_file.len(), 2);
        let entry_sols = &by_file[&multi.entry_id];
        assert!(entry_sols.iter().any(|s| matches!(
            s, Solution::Resolved { original, .. } if original == "grep"
        )));
    }

    #[test]
    fn auto_magic_var_unknown_var_stays_dynamic() {
        // We must not invent expansions for unknown vars.
        let resolver = Resolver::default().with_magic_vars(MagicVars {
            script_path: Some("/tmp/x.sh".into()),
            home: Some("/tmp".into()),
        });
        let src = r#"source "$UNRELATED/lib.sh""#;
        let unit = lower(src);
        let sols = resolver.resolve_unit(&unit, src);
        assert_eq!(sols.len(), 1);
        assert!(matches!(
            sols[0],
            Solution::Unresolved {
                kind: UnresolvedKind::DynamicSourcePath,
                ..
            }
        ));
    }
}
