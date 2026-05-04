# rusholve

[![ci](https://github.com/qweered/rusholve/actions/workflows/ci.yml/badge.svg)](https://github.com/qweered/rusholve/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rusholve.svg)](https://crates.io/crates/rusholve)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A clean-slate Rust port of [resholve](https://github.com/abathur/resholve): a shell-script command-reference resolver and rewriter for Nix.

`rusholve` reads a shell script, finds every command reference in it (`jq`, `curl`, `awk`, etc.), and rewrites them to absolute `/nix/store/...` paths so the resulting script is hermetic and survives `PATH` perturbation. It refuses to touch scripts that use Bash constructs it can't faithfully parse, so a successful rewrite is a strong correctness guarantee.

## Why

Resholve has long been the standard for resolving shell-command references in nixpkgs derivations, but it's a Python tool wrapping `oilshell/oil` and is awkward to integrate into Rust-based tooling. `rusholve` is a from-scratch reimplementation in Rust on top of [`brush-parser`](https://crates.io/crates/brush-parser), with:

- A **clean Rust library API** (`use rusholve::...`) for embedding in build tools.
- A **safety pass** that hard-stops on Bash features the parser can't handle, instead of silently producing a wrong rewrite.
- An **auto mode** ("nixos" profile) that infers the things resholve normally requires you to spell out: `/run/wrappers/bin/` for setuid binaries, `${BASH_SOURCE%/*}` expansion, function names harvested from sourced files, well-known dynamic words.
- Cargo-style structured diagnostics with `--format=json` and `--format=jsonl` for build-tool consumption.

## Install

### From crates.io

```sh
cargo install rusholve
```

### From source

```sh
cargo install --git https://github.com/qweered/rusholve --tag v0.0.1
```

### Nix flake

```nix
{
  inputs.rusholve.url = "github:qweered/rusholve";

  outputs = { self, nixpkgs, rusholve, ... }: {
    # CLI binary
    packages.x86_64-linux.default = rusholve.packages.x86_64-linux.default;

    # Helper to wrap shell scripts:
    #   inherit (rusholve.legacyPackages.x86_64-linux.lib) writeResolvedShellApplication;
  };
}
```

## Usage

```sh
# Audit a script — emit diagnostics, exit non-zero if anything is unresolved.
rusholve check script.sh

# Rewrite to script.sh.resolved (default) or in-place.
rusholve resolve --in-place script.sh

# Show the unified diff of what `resolve` would write, without touching anything.
rusholve diff script.sh

# Print the source graph (entry script + every transitively-sourced file,
# with the functions and aliases each contributes).
rusholve sources script.sh
```

### Profiles

| Profile | Behavior |
|---|---|
| `nixos` (default) | Auto mode for NixOS: prefers `/run/wrappers/bin/`, expands `${BASH_SOURCE%/*}`, harvests sourced functions, applies binlore verdicts. |
| `portable` | Same auto inferences as `nixos`, but no `/run/wrappers/bin/` preference (for non-NixOS hosts). |
| `strict` | Resholve-style discipline — every reference must be spelled out. Equivalent to `--strict`. |

### Common flags

```sh
rusholve --inputs $PATH --allow function=mybuild --map jq=/usr/bin/jq \
         --skip '$RUNTIME' --format json check script.sh
```

- `--inputs` — `PATH`-like search directories. Repeatable, colon-separated, or via `RUSHOLVE_INPUTS`.
- `--allow scope=name` — treat `scope=name` as in-scope and don't rewrite it. Scope is `function|alias|builtin|special-builtin|keyword`.
- `--map name=replacement` — pin a name to a specific replacement.
- `--skip pattern` — accept the literal source text unchanged (e.g. a dynamic word).
- `--lore FILE` — read additional exec-wrapper verdicts from a binlore CSV.
- `--allow-known-gaps` — demote known-gap warnings to advisory.

### Exit codes

`rusholve` uses semantic exit codes so build tooling can pattern-match failures:

| Code | Meaning |
|---|---|
| `0`  | success |
| `1`  | generic error (I/O, etc.) |
| `2`  | CLI usage error |
| `10` | unresolved command |
| `11` | unresolved source |
| `12` | parse error |
| `13` | directive error |
| `14` | unsupported construct (safety hard stop) |

## Using rusholve from Nix

The flake exposes a `writeResolvedShellApplication` builder that mirrors the upstream nixpkgs helper of the same name:

```nix
{
  rusholve.legacyPackages.${system}.lib.writeResolvedShellApplication {
    pname = "my-tool";
    runtimeInputs = [ pkgs.jq pkgs.curl ];
    text = ''
      #!/usr/bin/env bash
      jq --version
      curl --version
    '';
    strict = false;  # opt into rusholve's auto/NixOS profile
  }
}
```

The result is a derivation containing `bin/my-tool` with every `jq`/`curl` reference rewritten to its `/nix/store/.../bin/...` path.

## Development

```sh
# Inside the repo
nix develop                  # rust toolchain + cargo-nextest + cargo-insta
cargo test                   # 43 tests across cli, safety_corpus, and unit tests
cargo clippy --all-targets   # CI gate
nix flake check              # builds the package + runs the writeResolvedShellApplication smoke test
```

## Status

`v0.0.1` is the first tagged release. The core resolve/check/diff/sources surface is feature-complete for Bash; the auto-mode profile suite (`nixos`/`portable`/`strict`) and `writeResolvedShellApplication` are stable. Known gaps:

- POSIX-only and Zsh frontends are not yet implemented (only `brush-parser` Bash).
- A handful of constructs trigger a safety hard stop instead of resolving — see `tests/fixtures/unsupported/` for the current set (`coproc`, `select`, `disown`, `logout`, locale-quoted strings).
- Variable substitution inside command words (`${prefix}_jq`) and `eval`-style indirection are conservative — they may require `--skip` or `--allow`.

See the changelog (and tagged release notes) for per-version details.

## Acknowledgements

`rusholve` would not exist without [resholve](https://github.com/abathur/resholve) by [@abathur](https://github.com/abathur) — both for proving the design space and for the binlore data model that `--lore` consumes. The IR shape and CLI grammar borrow heavily from resholve's vocabulary; the implementation is independent.

## License

MIT — see [LICENSE](LICENSE).
