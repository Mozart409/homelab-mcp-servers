{
  description = "Development environment for homelab-mcp-servers";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [rust-overlay.overlays.default];
      };
      rust = pkgs.rust-bin.stable."1.95.0".default.override {
        extensions = ["rustfmt" "clippy" "rust-src"];
      };
    in {
      # to use other shells, run:
      # nix develop . --command fish
      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          # keep-sorted start
          act
          cargo-audit
          cargo-deny
          cargo-watch
          cargo-workspaces
          claude-code
          cocogitto
          just
          keep-sorted
          lazydocker
          lefthook
          opencode
          podman
          podman-compose
          rust
          sqlx-cli
          tailwindcss_4
          trivy
          # keep-sorted end
        ];
        shellHook = ''
          lefthook install
        '';
      };
    });
}
