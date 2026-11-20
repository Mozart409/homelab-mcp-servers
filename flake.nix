{
  description = "MCP servers for a personal homelab (standalone flake for the GitHub export)";

  # In the yggdrasil monorepo this is a subflake of the root flake, which makes
  # every input below follow its own, so the toolchain is the monorepo's one
  # stable Rust (rust/toolchain.nix `build` there). The monorepo itself builds
  # the project by importing ./nix directly with its shared toolchain; this file
  # exists so the exported GitHub repo (release.yml, nix-checks.yml) can build
  # and check without the monorepo around it.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux"];

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        # The pinned toolchain: ./rust-toolchain.toml is a byte-identical copy
        # of the yggdrasil root's; its channel is resolved with the same manifest
        # sha256 as the monorepo's rust/toolchain.nix (the root's
        # `rust-toolchain-sync` asserts the store paths agree). Keep the component
        # list equal to `build` in yggdrasil's rust/toolchain.nix.
        manifestFor = system:
          inputs.fenix.packages.${system}.fromToolchainName {
            name = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain.channel;
            sha256 = "sha256-MFHoWVcPZKjFrLnQnodCXbUsfZcofGfo9tV9fTTvklg=";
          };
        toolchain = (manifestFor system).withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];

        mcp = import ./nix {
          inherit pkgs toolchain;
          inherit (inputs) crane;
        };
      in {
        # `packages` is the server registry: release.yml enumerates it, so it
        # holds the server binaries, `homelab-mcp-servers-all` and `toolchain`.
        packages = mcp.packages // {inherit toolchain;};

        inherit (mcp) checks;

        # Named so CI can give the compiled dependency tree a GC root; it is a
        # build input of the checks, not part of their closure.
        legacyPackages.cargo-artifacts = mcp.cargoArtifacts;

        # What `cargo deny check` needs (it runs `cargo metadata`), held small
        # so a cold CI run realises little.
        devShells.ci = pkgs.mkShell {
          packages = [pkgs.cargo-deny toolchain];
        };
      };
    };
}
