//! `Inputs` is the PATH-equivalent table the resolver consults to map
//! a bare command name to an absolute path. v0.1 walks the directories
//! in declaration order and returns the first existing file. v0.2 will
//! consult the binlore CSV for executable-bit + can-exec metadata.

use std::path::{Path, PathBuf};

/// On NixOS, setuid binaries live under `/run/wrappers/bin/`. Resolving
/// `sudo` or `mount` to a `/nix/store/.../bin/` path drops the setuid
/// bit and breaks the resulting script (resholve issue #29). For these
/// well-known wrapper commands, we prefer the wrappers path when it
/// exists and the same name is requested.
///
/// This list mirrors the binaries that NixOS's `security.wrappers`
/// machinery commonly emits. Entries that don't exist on a given system
/// (e.g. `doas` is only there if `security.doas.enable = true`) just
/// fall through to the `--inputs` lookup; listing them is harmless.
///
/// Notably *not* included:
/// - `ping` / `ping6` — on NixOS these use file capabilities
///   (`security.capabilities`), not setuid wrappers.
/// - `unix_chkpwd` — not user-facing.
/// - Custom site-specific wrappers — unknowable from this list.
pub const WRAPPED_COMMANDS: &[&str] = &[
    "chsh",
    "doas",
    "fusermount",
    "fusermount3",
    "mount",
    "newgidmap",
    "newgrp",
    "newuidmap",
    "passwd",
    "pkexec",
    "sg",
    "su",
    "sudo",
    "sudoedit",
    "umount",
];

/// Default directory to search for wrapped (setuid) commands.
pub const DEFAULT_WRAPPERS_DIR: &str = "/run/wrappers/bin";

#[derive(Debug, Default, Clone)]
pub struct Inputs {
    /// Search directories in priority order.
    pub path: Vec<PathBuf>,
    /// Auto-wrappers: when `Some(dir)`, resolving any name in
    /// [`WRAPPED_COMMANDS`] prefers `dir/<name>` if it exists. `None`
    /// disables the lookup (e.g. `--strict` mode).
    pub wrappers_dir: Option<PathBuf>,
}

impl Inputs {
    pub fn new<I, P>(dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            path: dirs.into_iter().map(Into::into).collect(),
            wrappers_dir: Some(PathBuf::from(DEFAULT_WRAPPERS_DIR)),
        }
    }

    pub fn with_wrappers_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.wrappers_dir = dir;
        self
    }

    /// First directory in `path` that contains a regular file named `name`,
    /// or — for known setuid wrappers — the wrappers directory if it has
    /// that file. The wrappers path is preferred over `--inputs` because
    /// invoking the wrapper preserves the setuid bit.
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        if let Some(p) = self.resolve_wrapped(name) {
            return Some(p);
        }
        for dir in &self.path {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Wrappers-only lookup: returns `Some(path)` iff `name` is in
    /// [`WRAPPED_COMMANDS`] and `wrappers_dir/<name>` exists.
    fn resolve_wrapped(&self, name: &str) -> Option<PathBuf> {
        let dir = self.wrappers_dir.as_ref()?;
        if !WRAPPED_COMMANDS.contains(&name) {
            return None;
        }
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    }

    /// Enumerate the names of every regular file across all input
    /// directories (and the wrappers dir, if set). Used by the
    /// "did-you-mean?" hint to fuzzy-match unresolved command names.
    /// Order isn't guaranteed; duplicates are not deduplicated (the
    /// caller's matcher handles that naturally).
    pub fn candidate_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for dir in self.path.iter().chain(self.wrappers_dir.iter()) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else { continue };
                if !ft.is_file() && !ft.is_symlink() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        out
    }

    /// Resolve a relative source target (e.g. `./lib/common.sh`) against
    /// a base directory.
    pub fn resolve_source(&self, target: &str, relative_to: Option<&Path>) -> Option<PathBuf> {
        if Path::new(target).is_absolute() {
            return Path::new(target).is_file().then(|| PathBuf::from(target));
        }
        if target.starts_with("./") || target.starts_with("../") {
            let base = relative_to?;
            let candidate = base.join(target);
            return candidate.is_file().then_some(candidate);
        }
        // Bare name: search inputs.
        self.resolve(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_returns_first_match_in_path() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir1.path().join("git"), "first").unwrap();
        fs::write(dir2.path().join("git"), "second").unwrap();
        let inputs = Inputs::new([dir1.path(), dir2.path()]);
        let resolved = inputs.resolve("git").unwrap();
        assert!(resolved.starts_with(dir1.path()));
    }

    #[test]
    fn resolve_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let inputs = Inputs::new([dir.path()]);
        assert!(inputs.resolve("nonexistent").is_none());
    }

    #[test]
    fn auto_wrappers_prefers_wrappers_dir_for_setuid_commands() {
        // sudo is in WRAPPED_COMMANDS, so resolution should prefer the
        // wrappers path even if --inputs has its own copy.
        let inputs_dir = TempDir::new().unwrap();
        let wrappers_dir = TempDir::new().unwrap();
        fs::write(inputs_dir.path().join("sudo"), "store-copy").unwrap();
        fs::write(wrappers_dir.path().join("sudo"), "wrapper-copy").unwrap();

        let inputs = Inputs::new([inputs_dir.path()])
            .with_wrappers_dir(Some(wrappers_dir.path().to_path_buf()));
        let resolved = inputs.resolve("sudo").unwrap();
        assert!(
            resolved.starts_with(wrappers_dir.path()),
            "expected wrappers path, got {resolved:?}"
        );
    }

    #[test]
    fn auto_wrappers_falls_back_to_inputs_when_wrapper_missing() {
        // If wrappers dir doesn't have the binary, still find it via inputs.
        let inputs_dir = TempDir::new().unwrap();
        let wrappers_dir = TempDir::new().unwrap();
        fs::write(inputs_dir.path().join("sudo"), "store-copy").unwrap();
        // wrappers_dir is empty.

        let inputs = Inputs::new([inputs_dir.path()])
            .with_wrappers_dir(Some(wrappers_dir.path().to_path_buf()));
        let resolved = inputs.resolve("sudo").unwrap();
        assert!(resolved.starts_with(inputs_dir.path()));
    }

    #[test]
    fn auto_wrappers_does_not_apply_to_non_wrapped_commands() {
        // `git` isn't in WRAPPED_COMMANDS; even if a wrappers/git existed
        // we wouldn't prefer it. (No wrapper/git here, but we test the
        // table membership by ensuring resolve still uses inputs first.)
        let inputs_dir = TempDir::new().unwrap();
        let wrappers_dir = TempDir::new().unwrap();
        fs::write(inputs_dir.path().join("git"), "real").unwrap();
        fs::write(wrappers_dir.path().join("git"), "shouldnt-be-used").unwrap();

        let inputs = Inputs::new([inputs_dir.path()])
            .with_wrappers_dir(Some(wrappers_dir.path().to_path_buf()));
        let resolved = inputs.resolve("git").unwrap();
        assert!(
            resolved.starts_with(inputs_dir.path()),
            "git is not setuid; should not be diverted to wrappers, got {resolved:?}"
        );
    }

    #[test]
    fn auto_wrappers_disabled_when_dir_is_none() {
        let inputs_dir = TempDir::new().unwrap();
        fs::write(inputs_dir.path().join("sudo"), "store-copy").unwrap();
        let inputs = Inputs::new([inputs_dir.path()]).with_wrappers_dir(None);
        let resolved = inputs.resolve("sudo").unwrap();
        assert!(resolved.starts_with(inputs_dir.path()));
    }

    #[test]
    fn resolve_source_handles_absolute_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, "echo hi").unwrap();
        let inputs = Inputs::default();
        assert_eq!(
            inputs.resolve_source(path.to_str().unwrap(), None),
            Some(path)
        );
    }

    #[test]
    fn resolve_source_handles_dot_relative_path() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("lib.sh"), "true").unwrap();
        let inputs = Inputs::default();
        let resolved = inputs.resolve_source("./lib.sh", Some(dir.path())).unwrap();
        assert_eq!(resolved, dir.path().join("./lib.sh"));
    }

    #[test]
    fn resolve_source_falls_back_to_inputs_for_bare_name() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("common.sh"), "#").unwrap();
        let inputs = Inputs::new([dir.path()]);
        let resolved = inputs.resolve_source("common.sh", None).unwrap();
        assert_eq!(resolved, dir.path().join("common.sh"));
    }

    #[test]
    fn resolve_source_returns_none_when_relative_lacks_base() {
        let inputs = Inputs::default();
        assert!(inputs.resolve_source("./lib.sh", None).is_none());
    }
}
