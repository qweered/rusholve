//! Classify a command name as Function / Alias / Keyword / Builtin /
//! External / Dynamic given the current scope.
//!
//! CRO precedence (alias → keyword → special_builtin → function →
//! builtin → external) is encoded by the order of checks in [`classify`].

use std::collections::HashSet;

use crate::ir::{CommandLike, SourceUnit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Classification {
    Function,
    Alias,
    Keyword,
    SpecialBuiltin,
    Builtin,
    External,
    /// The command word itself was not statically known (e.g. `$cmd`).
    Dynamic,
}

/// Per-scope tables consulted by [`classify`]. Built once per
/// [`SourceUnit`] (and recomputed for nested function-body units).
///
/// We use owned `String` so multiple sources (entry script + sourced
/// files in the auto-source-graph) can be merged without lifetime gymnastics.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub functions: HashSet<String>,
    pub aliases: HashSet<String>,
}

impl Scope {
    /// Build a flat scope from a unit's declaration order. Future-defined
    /// functions are visible to earlier commands too — that matches shell
    /// semantics: shell only fails at *call time* if a function is
    /// missing, but we statically resolve, so any function declared
    /// anywhere in the unit counts.
    pub fn from_unit(unit: &SourceUnit) -> Self {
        let mut s = Self::default();
        s.merge_from_unit(unit);
        s
    }

    /// Add every function/alias defined in `unit` to this scope.
    /// Idempotent (HashSet semantics).
    pub fn merge_from_unit(&mut self, unit: &SourceUnit) {
        for name in &unit.functions_defined {
            self.functions.insert(name.clone());
        }
        for (name, _body) in &unit.aliases_defined {
            self.aliases.insert(name.clone());
        }
        for cmd in &unit.commands {
            if let CommandLike::Function { body, .. } = cmd {
                // Inner function bodies have their own nested scopes,
                // but their *outer* function declaration is visible at
                // this level — already recorded via functions_defined.
                let _ = body;
            }
        }
    }

    pub fn extend(&mut self, other: &Scope) {
        self.functions.extend(other.functions.iter().cloned());
        self.aliases.extend(other.aliases.iter().cloned());
    }
}

/// Classify a name. `None` means the command word was dynamic.
pub fn classify(name: Option<&str>, scope: &Scope) -> Classification {
    let Some(name) = name else {
        return Classification::Dynamic;
    };
    if scope.aliases.contains(name) {
        return Classification::Alias;
    }
    if KEYWORDS.contains(&name) {
        return Classification::Keyword;
    }
    if SPECIAL_BUILTINS.contains(&name) {
        return Classification::SpecialBuiltin;
    }
    if scope.functions.contains(name) {
        return Classification::Function;
    }
    if BUILTINS.contains(&name) {
        return Classification::Builtin;
    }
    Classification::External
}

/// Bash reserved words that can appear at command position. Most of
/// these never reach the classifier (parser handles them) but we cover
/// the few that the parser surfaces as commands (e.g. `time`).
const KEYWORDS: &[&str] = &[
    "!", "[[", "]]", "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if",
    "in", "then", "time", "until", "while",
];

/// POSIX-mandated special builtins. `disown` and `logout` are bash
/// builtins but we exclude them — the safety pass refuses scripts that
/// use them, so they never reach this table.
const SPECIAL_BUILTINS: &[&str] = &[
    ":", ".", "break", "continue", "eval", "exec", "exit", "export", "readonly", "return", "set",
    "shift", "source", "times", "trap", "unset",
];

/// Regular bash builtins (POSIX + bashref). Excludes `disown`, `logout`,
/// `select`, `coproc` — those are safety hard-stops.
const BUILTINS: &[&str] = &[
    "alias",
    "bg",
    "bind",
    "builtin",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "declare",
    "dirs",
    "echo",
    "enable",
    "false",
    "fg",
    "getopts",
    "hash",
    "help",
    "history",
    "jobs",
    "kill",
    "let",
    "local",
    "mapfile",
    "popd",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "suspend",
    "test",
    "[",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "wait",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Scope {
        Scope::default()
    }

    fn with_function(name: &str) -> Scope {
        let mut s = Scope::default();
        s.functions.insert(name.to_string());
        s
    }

    fn with_alias(name: &str) -> Scope {
        let mut s = Scope::default();
        s.aliases.insert(name.to_string());
        s
    }

    #[test]
    fn dynamic_when_name_is_none() {
        assert_eq!(classify(None, &empty()), Classification::Dynamic);
    }

    #[test]
    fn external_default() {
        assert_eq!(classify(Some("git"), &empty()), Classification::External);
        assert_eq!(classify(Some("jq"), &empty()), Classification::External);
    }

    #[test]
    fn keyword_recognized() {
        assert_eq!(classify(Some("time"), &empty()), Classification::Keyword);
    }

    #[test]
    fn special_builtin_takes_precedence_over_external() {
        assert_eq!(
            classify(Some("eval"), &empty()),
            Classification::SpecialBuiltin
        );
        assert_eq!(
            classify(Some("exec"), &empty()),
            Classification::SpecialBuiltin
        );
        assert_eq!(
            classify(Some("source"), &empty()),
            Classification::SpecialBuiltin
        );
    }

    #[test]
    fn builtin_recognized() {
        assert_eq!(classify(Some("echo"), &empty()), Classification::Builtin);
        assert_eq!(classify(Some("printf"), &empty()), Classification::Builtin);
        assert_eq!(classify(Some("test"), &empty()), Classification::Builtin);
    }

    #[test]
    fn alias_beats_everything_else() {
        // `echo` is a builtin but if shadowed by an alias, alias wins.
        assert_eq!(
            classify(Some("echo"), &with_alias("echo")),
            Classification::Alias
        );
    }

    #[test]
    fn keyword_beats_function_per_cro() {
        // `time` is a keyword; even if user defined a function called
        // `time`, the parser treats `time foo` as the time keyword.
        assert_eq!(
            classify(Some("time"), &with_function("time")),
            Classification::Keyword
        );
    }

    #[test]
    fn function_beats_external_but_not_special_builtin() {
        assert_eq!(
            classify(Some("git"), &with_function("git")),
            Classification::Function
        );
        // `eval` is a special builtin — function shadowing it doesn't matter for CRO.
        assert_eq!(
            classify(Some("eval"), &with_function("eval")),
            Classification::SpecialBuiltin
        );
    }

    #[test]
    fn function_beats_regular_builtin() {
        // bash CRO: function looks up *before* regular builtins.
        assert_eq!(
            classify(Some("echo"), &with_function("echo")),
            Classification::Function
        );
    }

    #[test]
    fn brush_hard_stops_are_external_when_they_slip_through() {
        // safety pass should refuse first; if they slip through we fall
        // back to External, which will fail to resolve and surface the
        // confusion via diagnostics rather than misclassifying.
        assert_eq!(classify(Some("disown"), &empty()), Classification::External);
        assert_eq!(classify(Some("logout"), &empty()), Classification::External);
        assert_eq!(classify(Some("select"), &empty()), Classification::External);
        assert_eq!(classify(Some("coproc"), &empty()), Classification::External);
    }

    #[test]
    fn scope_from_unit_collects_function_names() {
        use crate::ir::{CommandLike, SourceId, Span};
        let inner = SourceUnit {
            source_id: SourceId::new(0),
            commands: Vec::new(),
            functions_defined: Vec::new(),
            aliases_defined: Vec::new(),
            var_assignments: Vec::new(),
        };
        let unit = SourceUnit {
            source_id: SourceId::new(0),
            commands: vec![CommandLike::Function {
                name: "greet".into(),
                name_span: Span::new(0, 5),
                body: Box::new(inner),
                span: Span::new(0, 20),
            }],
            functions_defined: vec!["greet".into()],
            aliases_defined: Vec::new(),
            var_assignments: Vec::new(),
        };
        let scope = Scope::from_unit(&unit);
        assert!(scope.functions.contains("greet"));
    }

    #[test]
    fn scope_merge_combines_function_sets() {
        let mut a = Scope::default();
        a.functions.insert("foo".into());
        let mut b = Scope::default();
        b.functions.insert("bar".into());
        a.extend(&b);
        assert!(a.functions.contains("foo"));
        assert!(a.functions.contains("bar"));
    }
}
