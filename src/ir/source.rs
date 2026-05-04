use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{CommandLike, Span, Word};

/// A `name=value` assignment recovered from a [`SourceUnit`].
///
/// Captures both bare assignments (`cmd=git\n`) and assignments inside
/// declaration commands (`local cmd=git`, `declare cmd=git`,
/// `readonly cmd=git`, `export cmd=git`). The resolver consults these
/// to auto-trace `$cmd` references in v0.3+.
///
/// `literal` is `Some(s)` only when the RHS resolves entirely at parse
/// time — fully literal text, possibly inside single or double quotes
/// with no expansions. Dynamic RHSs (`cmd=$other`, `cmd="$x foo"`) get
/// `None` and the resolver bails on tracing them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarAssign {
    pub name: String,
    pub literal: Option<String>,
    pub span: Span,
    /// Lowered RHS Word. Carried so the resolver can walk the value's
    /// pieces for nested `$(…)` recursion — `path=$(basename "$0")` is
    /// a bare assignment, not a `CommandLike::Simple`, so without this
    /// the inner `basename` would be invisible to the resolver.
    #[serde(default)]
    pub value: Word,
}

/// Stable identifier for a [`SourceFile`] within a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId(u32);

impl SourceId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A single parsed/lowered source file.
///
/// The `SourceMap` keeps owned `text` so the rewriter has a stable buffer
/// to splice against, and so diagnostics can render exact source slices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    pub id: SourceId,
    pub path: PathBuf,
    pub text: String,
}

/// A flat list of [`CommandLike`]s recovered from one [`SourceFile`].
///
/// Function definitions inside this unit carry their own nested
/// `SourceUnit` for the body (recursive shape), but we never recurse into
/// pipelines/subshells/loops at the IR level — they don't change resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUnit {
    pub source_id: SourceId,
    pub commands: Vec<CommandLike>,
    /// Names of functions defined at this scope, in declaration order.
    /// Used by the resolver as the per-scope function table.
    pub functions_defined: Vec<String>,
    /// Aliases declared at this scope (name → body). The resolver looks
    /// here before falling through to the externals table.
    pub aliases_defined: Vec<(String, Word)>,
    /// Variable assignments at this scope, in declaration order. The
    /// resolver builds a `VarMap` from these for auto-trace varsub.
    #[serde(default)]
    pub var_assignments: Vec<VarAssign>,
}

/// Owns every [`SourceFile`] the resolver has touched (entrypoint scripts
/// plus everything recursively reached via `source`/`.` includes).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) -> SourceId {
        let id = SourceId(u32::try_from(self.files.len()).expect("too many source files"));
        self.files.push(SourceFile {
            id,
            path: path.into(),
            text: text.into(),
        });
        id
    }

    pub fn get(&self, id: SourceId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn try_get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    pub fn find_by_path(&self, path: &Path) -> Option<SourceId> {
        self.files.iter().find(|f| f.path == path).map(|f| f.id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_get_round_trip() {
        let mut map = SourceMap::new();
        let id_a = map.add("/a.sh", "echo a");
        let id_b = map.add("/b.sh", "echo b");
        assert_ne!(id_a, id_b);
        assert_eq!(map.get(id_a).text, "echo a");
        assert_eq!(map.get(id_b).path, PathBuf::from("/b.sh"));
    }

    #[test]
    fn find_by_path_returns_first_match() {
        let mut map = SourceMap::new();
        let id = map.add("/x.sh", "true");
        assert_eq!(map.find_by_path(Path::new("/x.sh")), Some(id));
        assert_eq!(map.find_by_path(Path::new("/missing.sh")), None);
    }

    #[test]
    fn empty_map_helpers() {
        let map = SourceMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
    }
}
