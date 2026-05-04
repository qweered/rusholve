{ pkgs, rusholve }:

let
  writeResolvedShellApplication = import ./writeResolvedShellApplication.nix { inherit pkgs rusholve; };

  # Smoke test for `nix flake check` — wraps a tiny script that uses
  # `jq` and `curl`, runs the resolver, asserts the resolved script
  # contains absolute /nix/store paths.
  smokeTest = pkgs.runCommand "rusholve-smoke" { } ''
    set -eu
    app=${
      writeResolvedShellApplication {
        pname = "rusholve-smoke-tool";
        runtimeInputs = [ pkgs.jq pkgs.curl ];
        # Note the leading shebang in `text`: this exercises the
        # double-shebang strip in writeResolvedShellApplication.
        text = ''
          #!/usr/bin/env bash
          jq --version
          curl --version
        '';
        strict = false;
      }
    }
    grep -q "${pkgs.jq}/bin/jq" $app/bin/rusholve-smoke-tool
    grep -q "${pkgs.curl}/bin/curl" $app/bin/rusholve-smoke-tool
    # The resolved script must have exactly one shebang line.
    shebang_count=$(grep -c '^#!' $app/bin/rusholve-smoke-tool)
    if [ "$shebang_count" -ne 1 ]; then
      echo "expected 1 shebang line, got $shebang_count" >&2
      exit 1
    fi
    : > $out
  '';
in
{
  inherit writeResolvedShellApplication;
  tests = {
    smoke = smokeTest;
  };
}
