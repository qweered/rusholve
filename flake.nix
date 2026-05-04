{
  description = "rusholve — a clean-slate Rust port of resholve";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        rusholve = pkgs.rustPlatform.buildRustPackage {
          pname = "rusholve";
          version = "0.0.1";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # The CLI integration tests shell out to `cargo run` artifacts
          # which we don't expose at check time. Unit + integration tests
          # we *can* run are gated on no-network, so leave doCheck on.
          doCheck = true;
          meta = with pkgs.lib; {
            description = "Rust shell-script command-reference resolver/rewriter for Nix";
            license = licenses.mit;
            mainProgram = "rusholve";
          };
        };

        rusholveLib = import ./nix/lib { inherit pkgs rusholve; };
      in
      {
        packages.default = rusholve;
        packages.rusholve = rusholve;

        # Library output: re-export under self.lib so consumer flakes
        # can do `inherit (rusholve.lib.${system}) writeResolvedShellApplication`.
        legacyPackages.lib = rusholveLib;

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            cargo-nextest
            cargo-insta
            pkg-config
          ];
          RUST_BACKTRACE = "1";
        };

        formatter = pkgs.nixpkgs-fmt;

        # `nix flake check` uses these to validate the package + lib smoke test.
        checks.rusholve = rusholve;
        checks.write-resolved-shell-application-smoke = rusholveLib.tests.smoke;
      });
}
