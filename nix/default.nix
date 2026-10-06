# Packages and crane checks for the homelab-mcp-servers workspace, called from
# the root flake (which used to consume this as the `homelab-mcp` flake input):
#
#   import ./rust/homelab-mcp-servers/nix {inherit pkgs crane toolchain;}
#
# `toolchain` is the monorepo's one stable Rust (rust/toolchain.nix `build`),
# so this file carries no toolchain of its own.
#
# Returns `packages` (one per server binary, plus `homelab-mcp-servers-all`),
# `checks` (fmt/clippy/test/actionlint/toolchain-pin), `cargoArtifacts` and
# `toolchain`. The root flake exposes the packages as
# `packages.<system>.<server>` and the rest under
# `legacyPackages.<system>.homelab-mcp-servers`.
{
  pkgs,
  crane,
  toolchain,
}: let
  inherit (pkgs) lib;

  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

  # Only this project's own build inputs, so an edit elsewhere in the monorepo
  # (or to this project's docs) does not rebuild the servers.
  #
  # `clippy.toml` is kept because without it the workspace's deny-level
  # `unwrap_used`/`expect_used` lints apply to test code too (the
  # `allow-*-in-tests` relaxations live in that file), so every test module
  # would fail clippy. `deny.toml` is kept so the source a check sees matches
  # the source a developer sees.
  #
  # `crates/` is taken whole: it holds only *.rs, Cargo.toml, the per-crate
  # README.md files (each server `include_str!`s its README and serves it as an
  # MCP doc resource) and the E2E snapshots/fixtures under `tests/` that the
  # test check compares against.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../clippy.toml
      ../deny.toml
      ../crates
    ];
  };

  commonArgs = {
    inherit src;
    pname = "homelab-mcp-servers";
    version = (lib.importTOML ../Cargo.toml).workspace.package.version;
    strictDeps = true;
    buildInputs = [pkgs.openssl];
    nativeBuildInputs = [pkgs.pkg-config];
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # Helper to build one server binary from the shared workspace.
  mkServer = bin:
    craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        pname = bin;
        cargoExtraArgs = "--bin ${bin}";
      });

  # All server binaries in the workspace. This list is the server registry
  # (docs/adr/0002-flake-is-the-server-registry.md); keep it in step with
  # `knownServers` in ./module.nix.
  serverPkgs = {
    pbsmcp-server = mkServer "pbsmcp-server";
    pgmcp-server = mkServer "pgmcp-server";
    prommcp-server = mkServer "prommcp-server";
    lokimcp-server = mkServer "lokimcp-server";
    hamcp-server = mkServer "hamcp-server";
    wpmcp-server = mkServer "wpmcp-server";
    alertmanagermcp-server = mkServer "alertmanagermcp-server";
    tempomcp-server = mkServer "tempomcp-server";
  };

  # Nix-native lint/test checks, sharing `cargoArtifacts` with the package
  # builds above, so the compiled dependency tree is a cacheable store path.
  #
  # NOT INCLUDED: cargo-deny. It fetches the RustSec advisory database over
  # the network, and Nix builds run in a sandbox with no network access.
  checks = {
    clippy = craneLib.cargoClippy (commonArgs
      // {
        inherit cargoArtifacts;
        pname = "homelab-mcp-servers-clippy";
        # Kept byte-identical to `just clippy` so a local run and a CI run
        # fail on exactly the same lints.
        cargoClippyExtraArgs = "--all-targets --all-features -- -D warnings -D clippy::pedantic";
      });

    test = craneLib.cargoTest (commonArgs
      // {
        inherit cargoArtifacts;
        pname = "homelab-mcp-servers-test";
        cargoTestExtraArgs = "--workspace";

        # `reqwest`'s `rustls` feature loads the SYSTEM root store; a Nix build
        # sandbox has no /etc/ssl/certs, so hand it nixpkgs' CA bundle.
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        nativeCheckInputs = [pkgs.cacert pkgs.postgresql];

        # pgmcp's suite runs against a real, throwaway Postgres in the
        # sandbox's TMPDIR (unix socket only), the same script `just test`
        # uses.
        preCheck = ''
          eval "$(bash ${../scripts/test-pg.sh} start)"
        '';
        postCheck = ''
          bash ${../scripts/test-pg.sh} stop
        '';

        # A snapshot mismatch or a missing snapshot fails the check; it must
        # never be "accepted" inside a build.
        INSTA_UPDATE = "no";
      });

    fmt = craneLib.cargoFmt {
      inherit src;
      pname = "homelab-mcp-servers-fmt";
    };

    # actionlint over the GitHub Actions workflows, including the shellcheck
    # and pyflakes passes it bundles for the `run:` blocks.
    actionlint = pkgs.runCommand "actionlint" {nativeBuildInputs = [pkgs.actionlint];} ''
      cp -r ${../.github} .github
      # Named explicitly: with no arguments actionlint looks for a `.git`
      # directory, which a build sandbox does not have.
      actionlint .github/workflows/*.yml
      touch $out
    '';

    # The Containerfile's RUST_TOOLCHAIN must equal the rustc built here; see
    # scripts/check-toolchain-pin.sh.
    toolchain-pin = pkgs.runCommand "toolchain-pin" {nativeBuildInputs = [toolchain pkgs.gawk];} ''
      bash ${../scripts/check-toolchain-pin.sh} ${../Containerfile}
      touch $out
    '';
  };
in {
  inherit checks cargoArtifacts toolchain;

  packages =
    serverPkgs
    // {
      homelab-mcp-servers-all = pkgs.linkFarm "all-mcp-servers" (
        lib.mapAttrsToList (name: pkg: {
          inherit name;
          path = "${pkg}/bin/${name}";
        })
        serverPkgs
      );
    };
}
