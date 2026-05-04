//! CLI surface. Binary entry point lives in `src/main.rs`; this module
//! defines the argument grammar and the per-subcommand workflows.
//!
//! Exit-code policy (matches the v0.1 plan):
//!
//! - `0`  success
//! - `1`  generic error (I/O, etc.)
//! - `2`  CLI usage error (clap-managed)
//! - `10` unresolved-command
//! - `11` unresolved-source
//! - `12` parse error
//! - `13` directive error
//! - `14` unsupported-construct (safety hard stop)

mod args;
mod check;
mod resolve;
mod run;

pub use args::{Cli, Command, GlobalOpts};
pub use run::run;

/// Exit codes used across subcommands. Single source of truth.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const GENERIC: i32 = 1;
    pub const _USAGE: i32 = 2;
    pub const UNRESOLVED_COMMAND: i32 = 10;
    pub const UNRESOLVED_SOURCE: i32 = 11;
    pub const PARSE_ERROR: i32 = 12;
    pub const DIRECTIVE_ERROR: i32 = 13;
    pub const UNSUPPORTED_CONSTRUCT: i32 = 14;
}
