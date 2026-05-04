//! `allow` / `map` / `skip` directives. Two surfaces share one grammar:
//! CLI flags (`--allow`, `--map`, `--skip`) and inline pragmas
//! (`# rusholve: …`). CLI directives take precedence over inline ones.
//!
//! After the resolver produces solutions, [`Directives::apply`] folds
//! over them, demoting `Unresolved` to `InScope` / `Resolved` /
//! `Allowed` as configured.

mod apply;
mod parse;

use serde::Serialize;

pub use apply::apply;
pub use parse::{parse_cli_allow, parse_cli_map, parse_cli_skip, parse_inline, DirectiveError};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Directives {
    pub allow: Vec<AllowDirective>,
    pub map: Vec<MapDirective>,
    pub skip: Vec<SkipDirective>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllowDirective {
    pub scope: AllowScope,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllowScope {
    Function,
    Alias,
    Builtin,
    SpecialBuiltin,
    Keyword,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MapDirective {
    pub name: String,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkipDirective {
    /// Literal source text the user expects to remain unchanged. The
    /// applier matches this against `&source[unresolved.span]`.
    pub pattern: String,
}

impl Directives {
    pub fn new() -> Self {
        Self::default()
    }

    /// Combine two sets, with `over` taking precedence for `map`
    /// duplicates (later wins). `allow` and `skip` are accumulated.
    pub fn merge(mut self, mut over: Self) -> Self {
        self.allow.append(&mut over.allow);
        // Map: later entries override earlier with same name.
        self.map
            .retain(|m| !over.map.iter().any(|n| n.name == m.name));
        self.map.append(&mut over.map);
        self.skip.append(&mut over.skip);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.map.is_empty() && self.skip.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_concatenates_allow_and_skip() {
        let a = Directives {
            allow: vec![AllowDirective {
                scope: AllowScope::Function,
                name: "foo".into(),
            }],
            map: vec![],
            skip: vec![SkipDirective {
                pattern: "$X".into(),
            }],
        };
        let b = Directives {
            allow: vec![AllowDirective {
                scope: AllowScope::Builtin,
                name: "bar".into(),
            }],
            map: vec![],
            skip: vec![SkipDirective {
                pattern: "$Y".into(),
            }],
        };
        let merged = a.merge(b);
        assert_eq!(merged.allow.len(), 2);
        assert_eq!(merged.skip.len(), 2);
    }

    #[test]
    fn merge_overrides_map_on_name_collision() {
        let a = Directives {
            map: vec![MapDirective {
                name: "jq".into(),
                replacement: "/usr/bin/jq".into(),
            }],
            ..Default::default()
        };
        let b = Directives {
            map: vec![MapDirective {
                name: "jq".into(),
                replacement: "/nix/store/.../jq".into(),
            }],
            ..Default::default()
        };
        let merged = a.merge(b);
        assert_eq!(merged.map.len(), 1);
        assert!(merged.map[0].replacement.starts_with("/nix"));
    }
}
