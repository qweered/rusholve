//! Auto-trace local variable substitution.
//!
//! Resholve calls the general case "intractable" — and they're right.
//! `cmd=$(curl -fsSL ...)` followed by `$cmd args` is genuinely
//! impossible to resolve statically. But the *common* case — a single
//! literal-RHS assignment in scope, no reassignments — is easy and
//! covers the most common variable-as-command pattern in nixpkgs build
//! scripts:
//!
//! ```bash
//! cmd=git
//! "$cmd" status
//! ```
//!
//! This module builds a [`VarMap`] from a [`SourceUnit`]'s assignments
//! and exposes a single [`VarMap::lookup`] entry point. The resolver
//! consults it when classifying a dynamic command word; if the word is
//! `$NAME` / `${NAME}` and the map has a unique literal value, we
//! re-classify as if the name *were* that value.
//!
//! Conservative subset (everything outside this stays dynamic):
//!
//! - **Literal RHS only.** `cmd=git` works; `cmd=$other`, `cmd="$x foo"`
//!   don't (we get `None` for `literal` from the frontend).
//! - **Single binding per name.** Two assignments to `cmd` → drop from
//!   the map. We can't statically prove control-flow ordering.
//! - **Single SourceUnit.** Function-body assignments don't cross into
//!   the outer scope (different `SourceUnit`). Outer assignments do
//!   cross into function bodies — that's a separate v0.3+ extension if
//!   needed; for now we keep it scope-local.
//!
//! Together with the rewriter, the substituted name's *original* dynamic
//! span gets replaced with the resolved path — same machinery as a plain
//! external command.

use std::collections::HashMap;

use crate::ir::SourceUnit;

/// A static-time view of `name → literal value` assignments harvested
/// from a [`SourceUnit`]. Lookup returns `None` for any name that wasn't
/// uniquely + literally bound — caller must treat such names as dynamic.
#[derive(Debug, Default, Clone)]
pub struct VarMap {
    values: HashMap<String, String>,
}

impl VarMap {
    /// Build a `VarMap` by folding `unit.var_assignments`. Names that
    /// appear with non-literal RHS *or* more than one assignment are
    /// excluded; the result contains only names safely substitutable.
    pub fn from_unit(unit: &SourceUnit) -> Self {
        // First pass: count assignments per name, tracking the (only)
        // literal value if exactly one assignment — and only if it had a
        // literal RHS.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for a in &unit.var_assignments {
            *counts.entry(a.name.as_str()).or_insert(0) += 1;
        }
        let mut values = HashMap::new();
        for a in &unit.var_assignments {
            // Only single-assignment names with a literal RHS qualify.
            if counts.get(a.name.as_str()).copied().unwrap_or(0) != 1 {
                continue;
            }
            if let Some(literal) = &a.literal {
                values.insert(a.name.clone(), literal.clone());
            }
        }
        Self { values }
    }

    /// Returns the literal value bound to `name`, if any.
    pub fn lookup(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Extract the variable name from a dynamic command word's source text.
/// Accepts `$NAME` and `${NAME}` (no parameter-expansion operators); any
/// other shape returns `None`. Also works for the quoted-then-substituted
/// shapes `"$NAME"` and `"${NAME}"` — those parse as a `DoubleQuoted`
/// piece wrapping a single `Dynamic`, but the resolver passes us only
/// the Dynamic piece's text, so the quotes are already stripped.
pub fn parse_var_name(text: &str) -> Option<&str> {
    let body = text.strip_prefix('$')?;
    if let Some(inner) = body.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        if !is_valid_name(inner) {
            return None;
        }
        return Some(inner);
    }
    if !is_valid_name(body) {
        return None;
    }
    Some(body)
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Span, VarAssign};

    fn unit_with(assigns: Vec<(&str, Option<&str>)>) -> SourceUnit {
        SourceUnit {
            source_id: crate::ir::SourceId::new(0),
            commands: Vec::new(),
            functions_defined: Vec::new(),
            aliases_defined: Vec::new(),
            var_assignments: assigns
                .into_iter()
                .map(|(n, v)| VarAssign {
                    name: n.to_string(),
                    literal: v.map(str::to_string),
                    span: Span::new(0, 0),
                    value: crate::ir::Word::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn single_literal_assignment_lookups() {
        let m = VarMap::from_unit(&unit_with(vec![("cmd", Some("git"))]));
        assert_eq!(m.lookup("cmd"), Some("git"));
    }

    #[test]
    fn dynamic_rhs_excluded() {
        let m = VarMap::from_unit(&unit_with(vec![("cmd", None)]));
        assert!(m.lookup("cmd").is_none());
    }

    #[test]
    fn two_assignments_drop_the_name() {
        let m = VarMap::from_unit(&unit_with(vec![("cmd", Some("git")), ("cmd", Some("hg"))]));
        assert!(m.lookup("cmd").is_none());
    }

    #[test]
    fn unrelated_names_dont_interfere() {
        let m = VarMap::from_unit(&unit_with(vec![
            ("cmd", Some("git")),
            ("path", Some("/tmp")),
        ]));
        assert_eq!(m.lookup("cmd"), Some("git"));
        assert_eq!(m.lookup("path"), Some("/tmp"));
    }

    #[test]
    fn parse_var_name_handles_dollar_forms() {
        assert_eq!(parse_var_name("$cmd"), Some("cmd"));
        assert_eq!(parse_var_name("${cmd}"), Some("cmd"));
        assert_eq!(parse_var_name("${PATH}"), Some("PATH"));
    }

    #[test]
    fn parse_var_name_rejects_paramexp_and_garbage() {
        assert!(parse_var_name("${cmd:-default}").is_none());
        assert!(parse_var_name("${cmd%/*}").is_none());
        assert!(parse_var_name("$1").is_none()); // positional, not a name
        assert!(parse_var_name("$@").is_none());
        assert!(parse_var_name("plain").is_none());
        assert!(parse_var_name("$").is_none());
    }
}
