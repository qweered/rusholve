//! End-to-end CLI behavior. Drives the built binary via `assert_cmd`
//! to lock in exit codes, stdout/stderr formats, and rewrite output.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn rusholve() -> Command {
    Command::cargo_bin("rusholve").expect("binary built")
}

fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn write_executable(dir: &Path, name: &str) -> std::path::PathBuf {
    write(dir, name, "#!/bin/sh\n")
}

#[test]
fn check_clean_script_exits_zero() {
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "git");
    write_executable(inputs.path(), "jq");

    let script = write(work.path(), "ok.sh", "#!/bin/sh\ngit status\njq .\n");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("check")
        .arg(&script)
        .assert()
        .success();
}

#[test]
fn check_unknown_external_exits_10() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "bad.sh", "nonexistent-tool args\n");

    rusholve()
        .arg("check")
        .arg(&script)
        .assert()
        .code(10)
        .stderr(predicate::str::contains("nonexistent-tool"));
}

#[test]
fn check_select_keyword_exits_14() {
    let work = TempDir::new().unwrap();
    let script = write(
        work.path(),
        "sel.sh",
        "select x in a b c; do echo $x; break; done\n",
    );

    rusholve()
        .arg("check")
        .arg(&script)
        .assert()
        .code(14)
        .stderr(predicate::str::contains("select"));
}

#[test]
fn check_dynamic_command_exits_10_unless_skipped() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "dyn.sh", "$RUNTIME args\n");

    rusholve().arg("check").arg(&script).assert().code(10);

    // With `skip` the same script is clean.
    rusholve()
        .arg("--skip")
        .arg("$RUNTIME")
        .arg("check")
        .arg(&script)
        .assert()
        .success();
}

#[test]
fn resolve_writes_dot_resolved_by_default() {
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "git");
    let script = write(work.path(), "x.sh", "#!/bin/sh\ngit status\n");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg(&script)
        .assert()
        .success();

    let resolved = fs::read_to_string(script.with_extension("sh.resolved")).unwrap();
    assert!(
        resolved.contains(inputs.path().join("git").to_str().unwrap()),
        "expected absolute git path in:\n{resolved}"
    );
    // Original script untouched.
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        "#!/bin/sh\ngit status\n"
    );
}

#[test]
fn resolve_in_place_rewrites_original() {
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    let script = write(work.path(), "x.sh", "jq . input.json\n");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();

    let rewritten = fs::read_to_string(&script).unwrap();
    assert!(rewritten.contains(inputs.path().join("jq").to_str().unwrap()));
    assert!(!script.with_extension("sh.resolved").exists());
}

#[test]
fn map_directive_pins_replacement() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "jq .\n");

    rusholve()
        .arg("--map")
        .arg("jq=/run/wrappers/bin/jq")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();

    let rewritten = fs::read_to_string(&script).unwrap();
    assert!(rewritten.contains("/run/wrappers/bin/jq"));
}

#[test]
fn allow_directive_marks_in_scope_no_rewrite() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "helper arg\n");

    rusholve()
        .arg("--allow")
        .arg("function=helper")
        .arg("check")
        .arg(&script)
        .assert()
        .success();

    rusholve()
        .arg("--allow")
        .arg("function=helper")
        .arg("--no-shebang")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();

    // No rewrite — `helper` is unchanged. (--no-shebang prevents the
    // auto-shebang policy from prepending a #! line.)
    assert_eq!(fs::read_to_string(&script).unwrap(), "helper arg\n");
}

#[test]
fn json_format_emits_machine_readable_diagnostics() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "missing-tool args\n");

    let output = rusholve()
        .arg("--format")
        .arg("json")
        .arg("check")
        .arg(&script)
        .assert()
        .code(10)
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["severity"], "error");
    assert_eq!(arr[0]["name"], "missing-tool");
}

#[test]
fn inline_pragma_skip_works() {
    let work = TempDir::new().unwrap();
    let script = write(
        work.path(),
        "x.sh",
        "# rusholve: skip $RUNTIME\n$RUNTIME args\n",
    );

    rusholve().arg("check").arg(&script).assert().success();
}

#[test]
fn missing_script_argument_is_usage_error() {
    rusholve().arg("check").assert().failure();
}

#[test]
fn invalid_allow_directive_exits_13() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "echo hi\n");
    rusholve()
        .arg("--allow")
        .arg("widget=foo")
        .arg("check")
        .arg(&script)
        .assert()
        .code(13);
}

#[test]
fn wait_dash_n_is_caught_by_ast_safety() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "wait -n\n");
    rusholve()
        .arg("check")
        .arg(&script)
        .assert()
        .code(14)
        .stderr(predicate::str::contains("wait -n"));
}

#[test]
fn auto_shebang_prepends_default_when_missing() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "true\n");
    rusholve()
        .arg("--allow")
        .arg("builtin=true")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let out = fs::read_to_string(&script).unwrap();
    assert!(
        out.starts_with("#!/usr/bin/env bash\n"),
        "expected default shebang, got: {out:?}"
    );
}

#[test]
fn auto_shebang_respects_existing_shebang() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "#!/bin/sh\ntrue\n");
    rusholve()
        .arg("--allow")
        .arg("builtin=true")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let out = fs::read_to_string(&script).unwrap();
    assert!(
        out.starts_with("#!/bin/sh\n"),
        "shebang must not be replaced: {out:?}"
    );
}

#[test]
fn strict_mode_disables_auto_skip_dynamics() {
    // In auto mode, `"$@" rest` is allowed; in strict mode it's an
    // unresolved-dynamic-command-name error.
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "\"$@\" rest\n");

    rusholve().arg("check").arg(&script).assert().success();

    rusholve()
        .arg("--strict")
        .arg("check")
        .arg(&script)
        .assert()
        .code(10);
}

#[test]
fn strict_mode_disables_auto_shebang() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "true\n");

    rusholve()
        .arg("--strict")
        .arg("--allow")
        .arg("builtin=true")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let out = fs::read_to_string(&script).unwrap();
    assert_eq!(out, "true\n", "strict mode must not prepend a shebang");
}

#[test]
fn profile_strict_is_equivalent_to_strict_flag() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "\"$@\" rest\n");

    rusholve()
        .arg("--profile")
        .arg("strict")
        .arg("check")
        .arg(&script)
        .assert()
        .code(10);
}

#[test]
fn auto_trace_varsub_resolves_single_literal_assignment() {
    let work = TempDir::new().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("git"), "#!/bin/sh\n").unwrap();
    let script = write(work.path(), "x.sh", "#!/bin/sh\ncmd=git\n\"$cmd\" status\n");
    rusholve()
        .arg("--inputs")
        .arg(&bin)
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let out = fs::read_to_string(&script).unwrap();
    assert!(
        out.contains("/git status") && !out.contains("\"$cmd\" status"),
        "varsub should rewrite `\"$cmd\" status` to the git path, got: {out}"
    );
}

#[test]
fn auto_trace_varsub_bails_on_reassignment() {
    let work = TempDir::new().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("git"), "#!/bin/sh\n").unwrap();
    let script = write(
        work.path(),
        "x.sh",
        "#!/bin/sh\ncmd=git\ncmd=hg\n\"$cmd\" status\n",
    );
    rusholve()
        .arg("--inputs")
        .arg(&bin)
        .arg("check")
        .arg(&script)
        .assert()
        .code(10);
}

#[test]
fn lore_noexec_suppresses_wrapper_recursion() {
    // Without lore, `nice -n 19 jq` makes the naive nice-arg parser
    // treat `19` as the inner command — that's a documented gap. With
    // `noexec,nice` in lore, nice is no longer a wrapper, so `19` is
    // not pursued.
    let work = TempDir::new().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("jq"), "#!/bin/sh\n").unwrap();
    fs::write(bin.join("nice"), "#!/bin/sh\n").unwrap();
    let lore = write(work.path(), "lore.csv", "noexec,nice\n");
    let script = write(work.path(), "x.sh", "nice -n 19 jq .\n");

    // With lore: only nice is processed; no spurious `19` error.
    let out = rusholve()
        .arg("--inputs")
        .arg(&bin)
        .arg("--lore")
        .arg(&lore)
        .arg("--format")
        .arg("json")
        .arg("check")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        !s.contains("\"19\""),
        "lore noexec should prevent 19 from being checked: {s}"
    );
}

#[test]
fn lore_exec_adds_custom_wrapper() {
    // With lore exec=foo, foo is treated as a wrapper — its first arg
    // must resolve too.
    let work = TempDir::new().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("foo"), "#!/bin/sh\n").unwrap();
    let lore = write(work.path(), "lore.csv", "exec,foo\n");
    let script = write(work.path(), "x.sh", "foo missing-tool\n");

    rusholve()
        .arg("--inputs")
        .arg(&bin)
        .arg("--lore")
        .arg(&lore)
        .arg("check")
        .arg(&script)
        .assert()
        .code(10)
        .stderr(predicate::str::contains("missing-tool"));
}

#[test]
fn diff_emits_unified_diff_without_writing() {
    let work = TempDir::new().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("git"), "#!/bin/sh\n").unwrap();
    let script = write(work.path(), "x.sh", "#!/bin/sh\ngit status\n");

    let out = rusholve()
        .arg("--inputs")
        .arg(&bin)
        .arg("diff")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("--- "), "expected --- header: {s}");
    assert!(s.contains("+++ "), "expected +++ header: {s}");
    assert!(
        s.contains("-git status"),
        "expected `-git status` line: {s}"
    );
    assert!(s.contains("/git "), "expected resolved git path: {s}");

    // Most importantly: file was not modified.
    let after = fs::read_to_string(&script).unwrap();
    assert_eq!(after, "#!/bin/sh\ngit status\n");
}

#[test]
fn diff_is_silent_when_nothing_changes() {
    // A script with only builtins resolves to itself (modulo shebang).
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "#!/bin/sh\necho hi\n");
    let out = rusholve()
        .arg("diff")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(out.is_empty(), "expected no diff for unchanged script");
}

#[test]
fn sources_lists_entry_and_transitively_loaded_files() {
    let work = TempDir::new().unwrap();
    fs::write(work.path().join("lib.sh"), "helper() { :; }\n").unwrap();
    let script = write(work.path(), "x.sh", "source ./lib.sh\nhelper\n");

    let out = rusholve()
        .arg("sources")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("x.sh"), "entry script should be listed: {s}");
    assert!(s.contains("lib.sh"), "sourced lib.sh should be listed: {s}");
    assert!(
        s.contains("helper"),
        "harvested function should be shown: {s}"
    );
}

#[test]
fn sources_json_format_is_valid() {
    let work = TempDir::new().unwrap();
    fs::write(work.path().join("lib.sh"), "f() { :; }\n").unwrap();
    let script = write(work.path(), "x.sh", "source ./lib.sh\n");

    let out = rusholve()
        .arg("--format")
        .arg("json")
        .arg("sources")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(v.is_array());
    assert!(v[0]["nodes"].is_array());
}

#[test]
fn auto_shebang_uses_custom_interpreter_flag() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "true\n");
    rusholve()
        .arg("--allow")
        .arg("builtin=true")
        .arg("--interpreter")
        .arg("/nix/store/zzz-bash/bin/bash")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let out = fs::read_to_string(&script).unwrap();
    assert!(
        out.starts_with("#!/nix/store/zzz-bash/bin/bash\n"),
        "expected custom interpreter, got: {out:?}"
    );
}

#[test]
fn resolve_emits_progress_banner_on_stderr() {
    // Cargo-style `Resolving … (auto)` banner before, `Resolved …`
    // after, both on stderr. stdout stays empty for a clean script.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    let script = write(work.path(), "x.sh", "jq .\n");

    let out = rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Resolving"),
        "expected Resolving banner on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("Resolved"),
        "expected Resolved summary on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("(auto)"),
        "expected mode label on stderr, got: {stderr:?}"
    );
}

#[test]
fn resolve_quiet_flag_suppresses_progress_banner() {
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    let script = write(work.path(), "x.sh", "jq .\n");

    let out = rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("--quiet")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Resolving"),
        "--quiet should suppress banner, got stderr: {stderr:?}"
    );
}

#[test]
fn resolve_json_format_suppresses_progress_banner() {
    // JSON output owns the streams; progress logging would muddy it.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    let script = write(work.path(), "x.sh", "jq .\n");

    let out = rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("--format")
        .arg("json")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Resolving"),
        "--format=json should suppress banner, got stderr: {stderr:?}"
    );
}

#[test]
fn map_dollar_var_resolves_dynamic_command_via_inputs() {
    // `--map '$FIND=find'` makes `$FIND` resolve to whichever
    // `find` ends up in --inputs. End-to-end variant: the bare
    // replacement name goes through the same PATH lookup as a
    // top-level external command.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "find");
    let script = write(work.path(), "x.sh", "$FIND .\n");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("--map")
        .arg("$FIND=find")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();

    let rewritten = fs::read_to_string(&script).unwrap();
    assert!(
        rewritten.contains(inputs.path().join("find").to_str().unwrap()),
        "expected $FIND rewritten to find's /nix/store path, got: {rewritten}"
    );
}

#[test]
fn map_dollar_var_with_absolute_path_inlines_verbatim() {
    let work = TempDir::new().unwrap();
    let script = write(work.path(), "x.sh", "$TOOL --version\n");

    rusholve()
        .arg("--map")
        .arg("$TOOL=/nix/store/aaa/bin/tool")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();
    let rewritten = fs::read_to_string(&script).unwrap();
    assert!(rewritten.contains("/nix/store/aaa/bin/tool"));
}

#[test]
fn resolve_recurses_into_command_substitution() {
    // The migration-from-resholve case: `path="$(basename "$0")"` and
    // `out="$(getopt -o foo)"` should rewrite the inner `basename` and
    // `getopt` to absolute paths.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "basename");
    write_executable(inputs.path(), "getopt");
    let script = write(
        work.path(),
        "x.sh",
        r#"path="$(basename "$0")"
out="$(getopt -o foo)"
"#,
    );

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success();

    let rewritten = fs::read_to_string(&script).unwrap();
    let basename_path = inputs.path().join("basename");
    let getopt_path = inputs.path().join("getopt");
    assert!(
        rewritten.contains(basename_path.to_str().unwrap()),
        "expected basename rewritten in $(...), got: {rewritten}"
    );
    assert!(
        rewritten.contains(getopt_path.to_str().unwrap()),
        "expected getopt rewritten in $(...), got: {rewritten}"
    );
}

#[test]
fn check_surfaces_unresolved_in_sourced_file() {
    // Multi-file v0.3: when the entry sources a library, the library's
    // own commands should also be classified. An unknown external in
    // lib.sh must surface as a diagnostic — not be silently ignored.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    let lib = write(work.path(), "lib.sh", "missing-tool foo\n");
    let entry = write(
        work.path(),
        "entry.sh",
        "#!/usr/bin/env bash\nsource ./lib.sh\n",
    );

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("check")
        .arg(&entry)
        .assert()
        .code(10) // unresolved command
        .stderr(predicate::str::contains("missing-tool"))
        .stderr(predicate::str::contains(
            lib.file_name().unwrap().to_string_lossy().as_ref(),
        ));
}

#[test]
fn resolve_rewrites_sourced_files_by_default() {
    // Default behavior: every sourced file with a Resolved edit is also
    // rewritten in place. lib.sh's `jq` becomes the absolute path. No
    // auto-shebang on libs — that's only for the entry.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    write_executable(inputs.path(), "grep");
    let lib = write(work.path(), "lib.sh", "jq .\n");
    let entry = write(
        work.path(),
        "entry.sh",
        "#!/usr/bin/env bash\nsource ./lib.sh\ngrep foo bar\n",
    );

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg(&entry)
        .assert()
        .success();

    let lib_after = fs::read_to_string(&lib).unwrap();
    let jq_path = inputs.path().join("jq");
    assert!(
        lib_after.contains(jq_path.to_str().unwrap()),
        "lib.sh should have jq rewritten by default, got: {lib_after}"
    );
    assert!(
        !lib_after.starts_with("#!"),
        "sourced libs must not get an auto-shebang prepended"
    );

    let entry_after = fs::read_to_string(&entry).unwrap();
    let grep_path = inputs.path().join("grep");
    assert!(
        entry_after.contains(grep_path.to_str().unwrap()),
        "entry.sh should have grep rewritten"
    );
}

#[test]
fn resolve_no_write_sourced_leaves_library_files_alone() {
    // Opt-out: `--no-write-sourced` flips the default off. lib.sh keeps
    // its original text on disk; only the entry script is rewritten.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    write_executable(inputs.path(), "grep");
    let lib = write(work.path(), "lib.sh", "jq .\n");
    let entry = write(
        work.path(),
        "entry.sh",
        "#!/usr/bin/env bash\nsource ./lib.sh\ngrep foo bar\n",
    );

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg("--no-write-sourced")
        .arg(&entry)
        .assert()
        .success();

    let lib_after = fs::read_to_string(&lib).unwrap();
    assert_eq!(
        lib_after, "jq .\n",
        "lib.sh must be untouched under --no-write-sourced"
    );

    let entry_after = fs::read_to_string(&entry).unwrap();
    let grep_path = inputs.path().join("grep");
    assert!(
        entry_after.contains(grep_path.to_str().unwrap()),
        "entry.sh should still have grep rewritten"
    );
}

#[test]
fn resolve_logs_each_rewrite_by_default() {
    // Spammy by default: every Resolved edit emits a `<from> -> <to>`
    // line on stderr between the Resolving/Resolved banners.
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    write_executable(inputs.path(), "curl");
    let script = write(work.path(), "x.sh", "#!/bin/sh\njq . | curl -d @-\n");

    let jq_path = inputs.path().join("jq");
    let curl_path = inputs.path().join("curl");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "jq -> {}",
            jq_path.display()
        )))
        .stderr(predicate::str::contains(format!(
            "curl -> {}",
            curl_path.display()
        )));
}

#[test]
fn resolve_quiet_suppresses_per_rewrite_lines() {
    let work = TempDir::new().unwrap();
    let inputs = TempDir::new().unwrap();
    write_executable(inputs.path(), "jq");
    let script = write(work.path(), "x.sh", "#!/bin/sh\njq .\n");
    let jq_path = inputs.path().join("jq");

    rusholve()
        .arg("--inputs")
        .arg(inputs.path())
        .arg("--quiet")
        .arg("resolve")
        .arg("--in-place")
        .arg(&script)
        .assert()
        .success()
        .stderr(predicate::str::contains(format!("jq -> {}", jq_path.display())).not())
        .stderr(predicate::str::contains("Resolving").not());
}
