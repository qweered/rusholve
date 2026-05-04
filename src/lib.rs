//! rusholve — clean-slate Rust port of resholve.
//!
//! Resolves and rewrites shell-script command references to absolute paths
//! (typically `/nix/store/...`), with a brush-parser-backed Bash/POSIX frontend
//! and a safety pass that refuses to touch scripts using brush-incompatible
//! constructs.
//!
//! Top-level modules are intentionally crisp so the v0.4+ workspace split is
//! mechanical:
//!
//!   ir         — shell-shaped IR shared across frontends
//!   frontend   — parser → IR lowering (only `bash` in v0.1)
//!   safety     — refuse to rewrite when brush can't faithfully parse
//!   resolver   — CRO + recursive command discovery + source graph
//!   rewriter   — span-keyed sorted edits applied to source text
//!   directives — allow/map/skip parsing and merging
//!   diag       — diagnostic types + human/JSON renderers
//!   lore       — nixpkgs binlore CSV ingestion (v0.3)

pub mod cli;
pub mod diag;
pub mod directives;
pub mod frontend;
pub mod ir;
pub mod lore;
pub mod resolver;
pub mod rewriter;
pub mod safety;
