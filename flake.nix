{
  description = "homelab-mcp-servers — Cargo workspace of MCP servers for a personal homelab";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    fenix,
    crane,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [fenix.overlays.default];
      };

      # Latest stable Rust, pinned by flake.lock rather than a literal version.
      toolchain = pkgs.fenix.stable.withComponents [
        "cargo"
        "clippy"
        "rust-src"
        "rustc"
        "rustfmt"
      ];

      craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

      # `craneLib.cleanCargoSource` keeps only Cargo.toml/Cargo.lock/*.rs, which
      # would drop `clippy.toml` — and without it the workspace's deny-level
      # `unwrap_used`/`expect_used` lints apply to test code too (the
      # `allow-*-in-tests` relaxations live in that file), so every test module
      # would fail clippy. `deny.toml` is kept for the same reason: so the source
      # a check sees matches the source a developer sees.
      #
      # `README.md` is kept because each server `include_str!`s its crate README
      # and serves it as an MCP doc resource. Filtering it out compiles fine on a
      # developer's checkout and then fails only under `nix build`, which is the
      # worst place to discover it — the file is a build input now, not just docs.
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          (craneLib.filterCargoSources path type)
          || (builtins.elem (builtins.baseNameOf path) ["clippy.toml" "deny.toml" "README.md"]);
      };

      commonArgs = {
        inherit src;
        pname = "homelab-mcp-servers";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
        strictDeps = true;
        buildInputs =
          [pkgs.openssl]
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
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

      # All server binaries in the workspace.
      serverPkgs = {
        pbsmcp-server = mkServer "pbsmcp-server";
        pgmcp-server = mkServer "pgmcp-server";
        prommcp-server = mkServer "prommcp-server";
        lokimcp-server = mkServer "lokimcp-server";
        hamcp-server = mkServer "hamcp-server";
        wpmcp-server = mkServer "wpmcp-server";
        alertmanagermcp-server = mkServer "alertmanagermcp-server";
      };

      # Nix-native lint/test checks, sharing `cargoArtifacts` with the package
      # builds above.
      #
      # WHY THESE EXIST: `just ci` shells out to cargo directly, so every CI run
      # recompiled all ~1580 dependency crates from scratch — `target/` and
      # `~/.cargo` live in a container that is destroyed when the step ends, and
      # nothing outside the Nix store can be served by the Attic cache. Routing
      # the same checks through crane makes the compiled dependency tree a store
      # path, so it is cached once and substituted thereafter, leaving only the
      # ~11 workspace crates to build.
      #
      # `cargoArtifacts` invalidates when Cargo.lock changes, so a dependency
      # bump pays the full cost once — the price of never paying it otherwise.
      #
      # NOT INCLUDED: cargo-deny. It fetches the RustSec advisory database over
      # the network, and Nix builds run in a sandbox with no network access, so
      # it cannot work here by construction. CI runs it in the dev shell instead.
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

            # `reqwest`'s `rustls` feature loads the SYSTEM root store when
            # `Client::builder().build()` runs — it is `rustls` (native roots),
            # not `rustls-tls-webpki-roots` (bundled roots). A Nix build sandbox
            # has no /etc/ssl/certs, so every `*Client::new()` fails with
            # `ClientCreationFailed("builder error")` and every test that
            # constructs a client dies instantly. Only the pure serde tests
            # survived. Handing it nixpkgs' CA bundle fixes all of them.
            #
            # This is a sandbox artefact, not a product bug: the shipped
            # containers inherit certs from the distroless base.
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            nativeCheckInputs = [pkgs.cacert];
          });

        fmt = craneLib.cargoFmt {
          inherit src;
          pname = "homelab-mcp-servers-fmt";
        };

        # actionlint over the GitHub Actions workflows, with the shellcheck and
        # pyflakes passes it bundles — so the `run:` blocks in those files (awk,
        # jq, the tag/version guards) are linted, not just the YAML schema.
        #
        # WHY THIS IS A CHECK AND NOT A devShells.ci PACKAGE: nixpkgs' actionlint
        # carries shellcheck and pyflakes, a 236 MiB closure. The CI shell is
        # deliberately held to what `just ci` invokes, because a lint runner has
        # to realise that whole closure before it can run anything. As a check
        # derivation the cost is paid only when this actually runs, and it is
        # substituted from cache.nixos.org rather than built.
        #
        # WHY IT MATTERS HERE: a malformed workflow does not fail loudly on
        # GitHub — it just never triggers, which is indistinguishable from a
        # misconfigured repository and cost a release to work out once already.
        # Nothing else in the tree looks at these files.
        actionlint = pkgs.runCommand "actionlint" {nativeBuildInputs = [pkgs.actionlint];} ''
          cp -r ${./.github} .github
          # Named explicitly rather than letting actionlint discover them: with
          # no arguments it locates the project by walking up for a `.git`
          # directory, which a build sandbox does not have.
          actionlint .github/workflows/*.yml
          touch $out
        '';
      };
    in {
      inherit checks;

      # Exposed so CI can build and push it to the binary cache by name.
      #
      # `cargoArtifacts` is a build INPUT of the checks, not part of any check's
      # runtime closure, so pushing the check outputs would not carry it. Without
      # an explicit handle there is no way to name the one derivation that makes
      # the whole crane arrangement worthwhile — the compiled dependency tree.
      legacyPackages.cargo-artifacts = cargoArtifacts;

      packages =
        serverPkgs
        // {
          default = serverPkgs.pbsmcp-server;
          all = pkgs.linkFarm "all-mcp-servers" (
            pkgs.lib.mapAttrsToList (name: pkg: {
              inherit name;
              path = "${pkg}/bin/${name}";
            })
            serverPkgs
          );
        };

      # to use other shells, run:
      # nix develop . --command fish
      devShells.default = pkgs.mkShell {
        buildInputs =
          (with pkgs; [
            # keep-sorted start
            act
            actionlint
            cargo-audit
            cargo-deny
            cargo-edit
            cargo-watch
            cargo-workspaces
            claude-code
            cocogitto
            just
            keep-sorted
            lazydocker
            lefthook
            mold
            opencode
            podman
            podman-compose
            sccache
            sqlx-cli
            tailwindcss_4
            toolchain
            trivy
            # keep-sorted end
          ])
          ++ [
            # rust-analyzer from the same fenix channel as `toolchain`, so the
            # editor and the CLI agree on rustc — pedantic clippy and the
            # analyzer disagreeing about a lint is a miserable way to spend an
            # afternoon.
            #
            # Deliberately NOT a component of `toolchain` itself: devShells.ci
            # consumes `toolchain`, and rust-analyzer is ~100 MB of closure a
            # lint runner would realise and never invoke.
            pkgs.fenix.stable.rust-analyzer
          ];

        # --- Build-time tuning; see docs/build-performance.md for the numbers --
        #
        # sccache caches rustc invocations for the ~300 registry dependencies.
        # It does nothing for the warm edit-compile loop by design: proc-macros
        # and anything that invokes the linker are excluded, and workspace
        # crates carry `-C incremental`, which sccache refuses outright.
        #
        # WHAT IT ACTUALLY BUYS, measured: a rebuild after `cargo clean` in the
        # SAME directory drops 92s -> 52s at a 50% Rust hit rate.
        #
        # WHAT IT DOES NOT BUY, also measured: reuse in a DIFFERENT target
        # directory. A second, freshly created target dir got a 0% Rust hit rate
        # and ran 19% slower (100s -> 119s) — sccache's cache keys here are
        # sensitive to the absolute paths cargo passes. So this does not
        # subsidise rust-analyzer's separate `target/rust-analyzer`, and any
        # argument for that directory has to stand on lock contention alone.
        # Do not restore the "reuse across target dirs" claim without
        # re-measuring; it was believed here once and was wrong.
        #
        # Setting this changes every fingerprint in `target/`, so the first
        # build after adopting it recompiles the world exactly once.
        RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";
        # Default is 10G; the dependency tree here is large enough, across
        # enough target dirs and branches, to evict itself at that size.
        SCCACHE_CACHE_SIZE = "20G";

        # mold instead of GNU ld. Linking is the measured bottleneck of the dev
        # loop — `cargo test --workspace --no-run` links a dozen test binaries,
        # and `cargo check` on the same edit is ~100x faster because it links
        # nothing at all.
        #
        # `build.rustflags` (not `target.<triple>.rustflags`, and not a
        # committed `.cargo/config.toml`) is deliberate: crane's source filter
        # globs every `*.toml`, so a `.cargo/config.toml` would silently become
        # a build input of `nix build` and of the cargo-zigbuild container,
        # neither of which has mold installed. Keeping it an env var of THIS
        # shell means the flake's derivations and the musl cross-build keep
        # their stock linker and stay reproducible.
        #
        # An explicitly set RUSTFLAGS overrides this, which is the right
        # precedence for a one-off.
        CARGO_BUILD_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
        # Installing the git hooks is a developer-workstation concern. In CI the
        # checkout is throwaway and `.git/hooks` is never consulted, so skip it —
        # it would only add noise (or fail) on a bare clone.
        shellHook = ''
          if [ -z "''${CI:-}" ]; then
            lefthook install
          fi
        '';
      };

      # Minimal shell for CI: exactly what `just ci` (fmt + clippy + deny +
      # test) invokes, and nothing else.
      #
      # This exists because `devShells.default` carries the whole workstation
      # toolbox — editors' agents, podman, trivy, sqlx-cli, tailwind. A CI
      # runner would have to realise that entire closure before it could run a
      # single lint, and every one of those inputs is a cache miss waiting to
      # happen on an unrelated version bump. Keeping the CI closure small is
      # what makes a cold pipeline (empty binary cache) merely slow rather than
      # unusable.
      #
      # `toolchain` is the same fenix derivation the default shell uses, so CI and
      # the workstation run byte-identical rustc/clippy/rustfmt — which matters
      # because clippy's pedantic set shifts between toolchain releases.
      devShells.ci = pkgs.mkShell {
        buildInputs = [
          pkgs.cargo-deny
          pkgs.just
          toolchain
        ];
      };
    })
    // {
      # Generalized NixOS module for the homelab-mcp-servers workspace.
      # Each server is configured under `services.homelab-mcp.servers.<name>`.
      nixosModules.default = {
        config,
        lib,
        pkgs,
        ...
      }: let
        cfg = config.services.homelab-mcp;

        # Known server defaults: name -> { prefix, port, hasToken, tokenVar? }
        # tokenVar overrides the env var the secret is exported as (defaults
        # to "<prefix>_TOKEN") for servers whose config reads a different name.
        knownServers = {
          pbsmcp-server = {
            prefix = "PBS";
            port = 8080;
            hasToken = true;
            # pbsmcp reads PBS_API_KEY, not PBS_TOKEN
            tokenVar = "PBS_API_KEY";
          };
          pgmcp-server = {
            prefix = "PG";
            port = 8081;
            # The "token" here is the full connection URL — it embeds the
            # password, so it must travel via tokenFile/LoadCredential and
            # never through the world-readable systemd environment.
            hasToken = true;
            tokenVar = "PG_DATABASE_URL";
          };
          prommcp-server = {
            prefix = "PROM";
            port = 8082;
            hasToken = true;
          };
          lokimcp-server = {
            prefix = "LOKI";
            port = 8083;
            hasToken = true;
          };
          hamcp-server = {
            prefix = "HA";
            port = 8084;
            hasToken = true;
          };
          wpmcp-server = {
            # WP_, not WOODPECKER_ — the agent injects WOODPECKER_* into every
            # pipeline step, so the two namespaces are kept disjoint.
            prefix = "WP";
            port = 8085;
            hasToken = true;
          };
          alertmanagermcp-server = {
            # Spelled out rather than AM_: unlike WP_ there is no namespace to
            # avoid, and AM_ reads as an abbreviation of nothing in particular.
            prefix = "ALERTMANAGER";
            port = 8086;
            hasToken = true;
          };
        };

        # Turn a list of strings into a comma-separated string, or null if empty.
        mkAllowedHosts = hosts:
          if hosts == [] || hosts == null
          then null
          else lib.concatStringsSep "," hosts;

        # Resolve the known-server defaults (prefix, port, token handling) for
        # an instance, keyed on its `serverType` (which defaults to the
        # instance name). Keying on serverType — not the instance name — is
        # what lets you run several instances of the same binary: a second
        # Postgres MCP named `pg-warehouse` with `serverType = "pgmcp-server"`
        # picks up the `PG` prefix instead of a nonsensical `PG-WAREHOUSE` one.
        serverDefaults = type:
          knownServers.${
            type
          } or {
            prefix = lib.toUpper type;
            port = 8080;
            hasToken = true;
          };

        # Build the environment attrset for one server instance.
        mkServerEnv = srv: let
          defaults = serverDefaults srv.serverType;
          p = defaults.prefix;
          bind = srv.bind;
          env =
            {
              "${p}_BIND" = bind;
              "${p}_INSECURE" = lib.boolToString srv.insecure;
            }
            // (lib.optionalAttrs (srv.host != null) {"${p}_HOST" = srv.host;})
            // (lib.optionalAttrs (srv.allowedHosts != []) {
              "${p}_ALLOWED_HOSTS" = mkAllowedHosts srv.allowedHosts;
            })
            // srv.extraEnv;
        in
          lib.filterAttrs (_: v: v != null) env;

        # Build the systemd service for one server.
        mkService = name: srv: let
          defaults = serverDefaults srv.serverType;
          env = mkServerEnv srv;
          hasTokenFile = (defaults.hasToken or true) && srv.tokenFile != null;
          tokenCredentialName = "${name}-token";
        in {
          description = "${name} — MCP server";
          wantedBy = ["multi-user.target"];
          after = ["network-online.target"];
          wants = ["network-online.target"];

          environment = env;

          serviceConfig = {
            Type = "simple";
            Restart = "on-failure";
            RestartSec = 5;

            DynamicUser = true;
            LoadCredential = lib.optional hasTokenFile "${tokenCredentialName}:${srv.tokenFile}";

            # Security hardening (from hamcp-rs)
            NoNewPrivileges = true;
            ProtectSystem = "strict";
            ProtectHome = true;
            PrivateTmp = true;
            PrivateDevices = true;
            ProtectKernelTunables = true;
            ProtectKernelModules = true;
            ProtectControlGroups = true;
            RestrictSUIDSGID = true;
            RestrictNamespaces = true;
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            RestrictRealtime = true;
          };

          script = let
            tokenVar = defaults.tokenVar or "${defaults.prefix}_TOKEN";
            tokenExport =
              lib.optionalString hasTokenFile
              ''export ${tokenVar}="$(< "$CREDENTIALS_DIRECTORY/${tokenCredentialName}")"''
              + "\n";
          in
            tokenExport
            + ''exec ${srv.package}/bin/${srv.serverType}'';
        };

        # Build firewall ports for enabled servers that request it.
        firewallPorts = lib.mapAttrsToList (
          name: srv:
            if srv.openFirewall
            then let
              portStr = lib.last (lib.splitString ":" srv.bind);
            in
              lib.toInt portStr
            else null
        ) (lib.filterAttrs (_: srv: srv.enable) cfg.servers);
      in {
        options.services.homelab-mcp = {
          servers = lib.mkOption {
            type = lib.types.attrsOf (lib.types.submodule ({name, ...}: {
              options = {
                enable = lib.mkEnableOption "this MCP server";

                serverType = lib.mkOption {
                  type = lib.types.str;
                  default = name;
                  description = ''
                    Known-server entry this instance is based on (e.g.
                    `"pgmcp-server"`), used to pick the env-var prefix, default
                    port, and token env var. Defaults to the instance name, so
                    existing configs that already name a known server need not
                    set this.

                    Set it when running several instances of the same binary
                    under different names — e.g. a second Postgres MCP named
                    `pg-warehouse` would set `serverType = "pgmcp-server"` so it
                    reads `PG_*` env vars instead of the (wrong) `PG_WAREHOUSE`
                    prefix derived from its name.
                  '';
                };

                package = lib.mkOption {
                  type = lib.types.package;
                  defaultText = lib.literalExpression "self.packages.\${pkgs.stdenv.hostPlatform.system}.<server>";
                  description = "The package to use for this server.";
                };

                host = lib.mkOption {
                  type = lib.types.nullOr lib.types.str;
                  default = null;
                  example = "https://pbs.lan:8007";
                  description = "Base URL or host of the target service.";
                };

                tokenFile = lib.mkOption {
                  type = lib.types.nullOr lib.types.path;
                  default = null;
                  example = "/run/secrets/pbs-token";
                  description = ''
                    Path to a file containing the authentication token.
                    Loaded at runtime via systemd LoadCredential so the secret
                    never enters the Nix store.
                  '';
                };

                insecure = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = "Accept invalid / self-signed TLS certificates.";
                };

                bind = lib.mkOption {
                  type = lib.types.str;
                  default = "127.0.0.1:8080";
                  example = "0.0.0.0:8080";
                  description = "Address to bind the streamable-HTTP MCP server.";
                };

                allowedHosts = lib.mkOption {
                  type = lib.types.listOf lib.types.str;
                  default = [];
                  example = ["mcp.example.ts.net" "localhost"];
                  description = ''
                    Comma-separated allowed Host header values.
                    Defaults to loopback only (DNS-rebinding protection).
                    Set when serving on a hostname.
                  '';
                };

                openFirewall = lib.mkOption {
                  type = lib.types.bool;
                  default = false;
                  description = "Open the bind port in the NixOS firewall.";
                };

                extraEnv = lib.mkOption {
                  type = lib.types.attrsOf lib.types.str;
                  default = {};
                  example = {
                    PBS_NODE = "localhost";
                    LOKI_ORG_ID = "tenant-1";
                  };
                  description = ''
                    Extra environment variables specific to this server.
                    These are merged with the standard HOST/TOKEN/BIND/INSECURE
                    variables. Use for server-specific options like PBS_NODE,
                    LOKI_ORG_ID, PG_MAX_CONNECTIONS, etc.
                  '';
                };
              };
            }));
            default = {};
            description = "MCP server instances to run.";
          };
        };

        config = lib.mkIf (cfg.servers != {}) {
          systemd.services =
            lib.mapAttrs (
              name: srv:
                lib.mkIf srv.enable (mkService name srv)
            )
            cfg.servers;

          networking.firewall.allowedTCPPorts = lib.filter (x: x != null) firewallPorts;
        };
      };
    };
}
