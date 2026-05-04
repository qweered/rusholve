//! Integration test: every fixture under tests/fixtures/unsupported/
//! must produce at least one hard stop from the safety pass. This is the
//! brush-version pin guard described in the plan: if brush starts handling
//! one of these constructs cleanly and we forget to drop the corresponding
//! detector, the fixture loudly says so (the snapshot will diff).

use std::fs;
use std::path::Path;

use rusholve::safety::{scan_tokens, HardStopKind};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/unsupported")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

#[test]
fn select_fixture_is_refused() {
    let hits = scan_tokens(&fixture("select.sh"));
    assert!(hits.iter().any(|h| h.kind == HardStopKind::SelectStatement));
}

#[test]
fn coproc_fixture_is_refused() {
    let hits = scan_tokens(&fixture("coproc.sh"));
    assert!(hits.iter().any(|h| h.kind == HardStopKind::CoprocKeyword));
}

#[test]
fn disown_fixture_is_refused() {
    let hits = scan_tokens(&fixture("disown.sh"));
    assert!(hits.iter().any(|h| h.kind == HardStopKind::DisownBuiltin));
}

#[test]
fn logout_fixture_is_refused() {
    let hits = scan_tokens(&fixture("logout.sh"));
    assert!(hits.iter().any(|h| h.kind == HardStopKind::LogoutBuiltin));
}

#[test]
fn locale_quoted_fixture_is_refused() {
    let hits = scan_tokens(&fixture("locale_quoted.sh"));
    assert!(hits.iter().any(|h| h.kind == HardStopKind::LocaleQuoted));
}
