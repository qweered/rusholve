//! clap-derived CLI grammar.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "rusholve",
    version,
    about = "Rust port of resholve: rewrite shell command references to absolute paths",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse, audit, and resolve scripts; emit diagnostics. No file is
    /// rewritten. Exits non-zero if anything is unresolved (or the
    /// safety pass refuses).
    Check(CheckArgs),

    /// Run `check`, then if clean, rewrite each script to its resolved
    /// form. Output goes to `<script>.resolved` unless `--in-place` is
    /// set.
    Resolve(ResolveArgs),

    /// Print the source graph reachable from each script: the entry
    /// script plus every file it transitively `source`s, with the
    /// functions and aliases each file contributes.
    Sources(SourcesArgs),

    /// Show what `resolve` *would* write, as a unified-style diff,
    /// without modifying any file.
    Diff(DiffArgs),
}

#[derive(Debug, Args)]
pub struct GlobalOpts {
    /// PATH-like search directories for external commands. Repeatable.
    #[arg(
        long,
        value_name = "DIR",
        env = "RUSHOLVE_INPUTS",
        value_delimiter = ':'
    )]
    pub inputs: Vec<PathBuf>,

    /// Treat `<scope>=<name>` as in-scope (skip rewrite). Repeatable.
    /// Scope is one of function|alias|builtin|special-builtin|keyword.
    #[arg(long, value_name = "SCOPE=NAME")]
    pub allow: Vec<String>,

    /// Pin `<name>` to `<replacement>`. Repeatable.
    #[arg(long, value_name = "NAME=REPLACEMENT")]
    pub map: Vec<String>,

    /// Accept the literal source text `<pattern>` unchanged (e.g. a
    /// dynamic command word). Repeatable.
    #[arg(long, value_name = "PATTERN")]
    pub skip: Vec<String>,

    /// Diagnostic format.
    #[arg(long, value_enum, default_value_t = Format::Human, env = "RUSHOLVE_FORMAT")]
    pub format: Format,

    /// Demote safety known-gap warnings to advisory (do not exit
    /// non-zero on them). Hard stops are never demotable.
    #[arg(long)]
    pub allow_known_gaps: bool,

    /// Directory used to resolve `./relative` source paths. Defaults
    /// to the directory of the first script argument.
    #[arg(long, value_name = "DIR")]
    pub script_dir: Option<PathBuf>,

    /// Directory to prefer for setuid wrappers (sudo, mount, …) on
    /// NixOS. Set to a path to override; pass `--no-wrappers` to disable.
    /// Default: `/run/wrappers/bin`.
    #[arg(long, value_name = "DIR", env = "RUSHOLVE_WRAPPERS_DIR")]
    pub wrappers_dir: Option<PathBuf>,

    /// Disable auto-wrappers — never prefer the wrappers directory over
    /// `--inputs` for setuid commands.
    #[arg(long, conflicts_with = "wrappers_dir")]
    pub no_wrappers: bool,

    /// Interpreter to prepend as `#!<path>` when a script lacks a
    /// shebang. Default: `/usr/bin/env bash`. Use a `/nix/store/.../bin/bash`
    /// path for hermetic builds. Pass `--no-shebang` to disable this.
    #[arg(long, value_name = "PATH", env = "RUSHOLVE_INTERPRETER")]
    pub interpreter: Option<String>,

    /// Don't prepend a shebang line — leave the resolved script as-is.
    #[arg(long, conflicts_with = "interpreter")]
    pub no_shebang: bool,

    /// Pre-set bundle. `nixos` (default) is the auto-mode that v0.2
    /// ships. `portable` keeps inferences but disables NixOS-specific
    /// path defaults. `strict` is resholve-style — every reference must
    /// be spelled out.
    #[arg(long, value_enum, default_value_t = Profile::Nixos)]
    pub profile: Profile,

    /// Shortcut for `--profile=strict` — resholve-style discipline.
    /// Disables auto-source-graph, auto-skip dynamics, auto-lore, and
    /// auto-shebang. Equivalent to v0.1 behavior.
    #[arg(long)]
    pub strict: bool,

    /// Read additional exec-wrapper verdicts from a lore file.
    /// Repeatable. See `lore::parse` for accepted formats.
    #[arg(long, value_name = "FILE", env = "RUSHOLVE_LORE")]
    pub lore: Vec<PathBuf>,

    /// Suppress informational stderr progress lines (cargo-style
    /// `Resolving`/`Resolved` banners). Errors and diagnostics are
    /// unaffected. Implied when `--format` is `json` or `jsonl` so
    /// the build log only carries machine-readable output.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Scripts to audit.
    #[arg(value_name = "SCRIPT", required = true)]
    pub scripts: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    /// Scripts to rewrite.
    #[arg(value_name = "SCRIPT", required = true)]
    pub scripts: Vec<PathBuf>,

    /// Rewrite scripts in place instead of writing `<name>.resolved`.
    #[arg(long)]
    pub in_place: bool,

    /// Skip rewriting files the entry script transitively `source`s —
    /// only touch the entry. Default behavior rewrites every sourced
    /// file with at least one resolvable command (full source-tree
    /// resolution; what most Nix builds want). Use this opt-out when
    /// the libraries are read-only inputs you don't want mutated (e.g.
    /// a vendored upstream helper you intend to inspect rather than
    /// transform). Diagnostics still surface either way.
    #[arg(long)]
    pub no_write_sourced: bool,
}

#[derive(Debug, Args)]
pub struct SourcesArgs {
    /// Scripts to analyze.
    #[arg(value_name = "SCRIPT", required = true)]
    pub scripts: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Scripts to preview.
    #[arg(value_name = "SCRIPT", required = true)]
    pub scripts: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Format {
    Human,
    Json,
    Jsonl,
}

/// Behavior preset. Bundles several flags so the most common postures
/// are one-flag changes from default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Profile {
    /// Auto-mode tuned for NixOS: prefer `/run/wrappers/bin/`, expand
    /// `${BASH_SOURCE%/*}`, harvest sourced functions, etc. (default).
    Nixos,
    /// Auto-mode tuned for non-NixOS hosts: same inferences, but
    /// `--no-wrappers` (no `/run/wrappers/bin/` preference).
    Portable,
    /// Resholve-style discipline — every reference must be spelled out.
    /// Equivalent to `--strict`.
    Strict,
}

impl Profile {
    /// True when this profile implies `--strict`.
    pub fn is_strict(self) -> bool {
        matches!(self, Profile::Strict)
    }

    /// True when this profile uses NixOS wrapper paths.
    pub fn uses_wrappers(self) -> bool {
        matches!(self, Profile::Nixos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse ok")
    }

    #[test]
    fn check_subcommand_parses() {
        let cli = parse(&["rusholve", "check", "script.sh"]);
        assert!(matches!(cli.command, Command::Check(_)));
    }

    #[test]
    fn resolve_in_place_parses() {
        let cli = parse(&["rusholve", "resolve", "--in-place", "script.sh"]);
        match cli.command {
            Command::Resolve(args) => assert!(args.in_place),
            _ => unreachable!(),
        }
    }

    #[test]
    fn inputs_split_on_colon() {
        let cli = parse(&["rusholve", "--inputs", "/a:/b:/c", "check", "x.sh"]);
        assert_eq!(cli.global.inputs.len(), 3);
    }

    #[test]
    fn directives_accumulate() {
        let cli = parse(&[
            "rusholve",
            "--allow",
            "function=foo",
            "--map",
            "jq=/usr/bin/jq",
            "--skip",
            "$RUNTIME",
            "check",
            "x.sh",
        ]);
        assert_eq!(cli.global.allow.len(), 1);
        assert_eq!(cli.global.map.len(), 1);
        assert_eq!(cli.global.skip.len(), 1);
    }

    #[test]
    fn format_default_is_human() {
        let cli = parse(&["rusholve", "check", "x.sh"]);
        assert_eq!(cli.global.format, Format::Human);
    }

    #[test]
    fn format_json_parses() {
        let cli = parse(&["rusholve", "--format", "json", "check", "x.sh"]);
        assert_eq!(cli.global.format, Format::Json);
    }
}
