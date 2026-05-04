//! JSON renderers for [`Diagnostic`]. Two flavors:
//!
//! - `render_jsonl`: one line per diagnostic, suitable for `cargo build`-
//!   style streaming consumers.
//! - `render_pretty_json`: array of diagnostics, indented; suitable for
//!   editor integrations and human inspection.

use serde::Serialize;
use serde_json::ser::PrettyFormatter;
use serde_json::Serializer;

/// Emit one JSON object per item, separated by `\n`. Suitable for
/// `cargo build`-style streaming consumers.
pub fn render_jsonl<T: Serialize>(items: &[T]) -> String {
    let mut out = String::new();
    for item in items {
        let line = serde_json::to_string(item).expect("item serializes");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Emit a single indented JSON array. Suitable for editor integrations
/// and human inspection.
pub fn render_pretty_json<T: Serialize>(items: &[T]) -> String {
    let mut bytes = Vec::new();
    let formatter = PrettyFormatter::with_indent(b"  ");
    let mut ser = Serializer::with_formatter(&mut bytes, formatter);
    items.serialize(&mut ser).expect("array serializes");
    String::from_utf8(bytes).expect("serde_json emits UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Diagnostic, DiagnosticKind, Severity};
    use crate::ir::Span;
    use crate::resolver::UnresolvedKind;

    fn sample() -> Diagnostic {
        Diagnostic {
            file: "x.sh".into(),
            span: Span::new(0, 2),
            line: 1,
            column: 1,
            severity: Severity::Error,
            kind: DiagnosticKind::Unresolved(UnresolvedKind::UnknownExternal),
            message: "unknown external command `jq`".into(),
            name: Some("jq".into()),
            hint: None,
        }
    }

    #[test]
    fn jsonl_emits_one_line_per_diagnostic() {
        let out = render_jsonl(&[sample(), sample()]);
        assert_eq!(out.lines().count(), 2);
        for line in out.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["severity"], "error");
        }
    }

    #[test]
    fn jsonl_omits_none_fields() {
        let out = render_jsonl(&[sample()]);
        assert!(!out.contains("\"hint\""));
    }

    #[test]
    fn pretty_json_round_trips() {
        let out = render_pretty_json(&[sample()]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["name"], "jq");
    }
}
