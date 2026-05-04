{ pkgs, rusholve }:

# Wrap a shell script, resolve every command reference to its
# /nix/store path, and emit a derivation containing `bin/<pname>`.
#
# This mirrors nixpkgs's upstream `writeResolvedShellApplication`
# (in `pkgs/build-support/trivial-builders/default.nix`). Two input
# shapes are supported, in priority order:
#
#   1. `script = ./path.sh` — a real file in the source tree. Copied
#      verbatim and resolved in place. Preferred.
#   2. `text = "..."` — a literal string. Written to a temp file and
#      resolved. A leading `#!...` line in `text` is stripped before
#      we prepend our own shebang, so the output never has two
#      shebang lines (line 3 is inert because the kernel only reads
#      line 1, but it's confusing in build logs).
#
# Defaults match nixpkgs: `strict = true` (resholve-style discipline).
# Set `strict = false` for rusholve's auto/NixOS profile, where
# function calls from sourced libraries, well-known dynamics like
# `"$@"`, and setuid wrapper paths resolve without `--allow`.
#
# Manual escape hatches: `interpreter` for the shebang path,
# `rusholveFlags` for arbitrary CLI overrides (e.g. `--map`,
# `--allow`, `--lore`, `--allow-known-gaps`).

{ pname ? null
, name ? pname
, script ? null
, text ? null
, runtimeInputs ? []
, interpreter ? null
, strict ? true
, rusholveFlags ? []
}:

let
  inherit (pkgs) lib;
  finalName = if name != null then name else throw "writeResolvedShellApplication: must set `pname` (or `name`)";

  # Strip a leading shebang from `text` so we never emit two of them.
  # Kernel only reads line 1, but a stray `#!/usr/bin/env bash` on
  # line 3 reads as a no-op comment to bash and confuses anyone
  # reading the resolved script.
  stripLeadingShebang = s:
    let
      lines = lib.splitString "\n" s;
      first = if lines == [] then "" else builtins.head lines;
    in
      if lib.hasPrefix "#!" first
      then lib.concatStringsSep "\n" (lib.tail lines)
      else s;

  # Source file we'll resolve. `script` wins; `text` is the fallback
  # for inline use (smoke tests, demos).
  scriptFile =
    if script != null then script
    else if text != null then
      pkgs.writeText "${finalName}-source" (stripLeadingShebang text)
    else throw "writeResolvedShellApplication: must set either `script` or `text`";

  inputsFlag = lib.optionals (runtimeInputs != []) [
    "--inputs" (lib.escapeShellArg (lib.makeBinPath runtimeInputs))
  ];
  interpreterFlag = lib.optionals (interpreter != null) [
    "--interpreter" (lib.escapeShellArg interpreter)
  ];
  strictFlag = lib.optional strict "--strict";

  cliArgs = lib.concatStringsSep " " (
    inputsFlag ++ interpreterFlag ++ strictFlag ++ map lib.escapeShellArg rusholveFlags
  );
in
pkgs.runCommand finalName {
  nativeBuildInputs = [ rusholve ];
  passthru = {
    inherit runtimeInputs strict;
  };
  meta.mainProgram = finalName;
} ''
  set -eu
  install -Dm755 ${scriptFile} "$out/bin/${finalName}"
  ${lib.getExe rusholve} ${cliArgs} resolve --in-place "$out/bin/${finalName}"
''
