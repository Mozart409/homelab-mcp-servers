{
  description = "homelab-mcp-servers — Cargo workspace of MCP servers for a personal homelab";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [rust-overlay.overlays.default];
      };
      rust = pkgs.rust-bin.stable."1.96.0".default.override {
        extensions = ["rustfmt" "clippy" "rust-src"];
      };

      craneLib = (crane.mkLib pkgs).overrideToolchain rust;

      src = craneLib.cleanCargoSource ./.;

      commonArgs = {
        inherit src;
        pname = "homelab-mcp-servers";
        version = "0.2.5";
        strictDeps = true;
        buildInputs =
          [pkgs.openssl]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
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
      };
    in {
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
        buildInputs = with pkgs; [
          # keep-sorted start
          act
          cargo-audit
          cargo-deny
          cargo-edit
          cargo-watch
          cargo-workspaces
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

        # Known server defaults: name -> { prefix, port, hasToken }
        knownServers = {
          pbsmcp-server = {
            prefix = "PBS";
            port = 8080;
            hasToken = true;
          };
          pgmcp-server = {
            prefix = "PG";
            port = 8081;
            hasToken = false;
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
        };

        # Turn a list of strings into a comma-separated string, or null if empty.
        mkAllowedHosts = hosts:
          if hosts == [] || hosts == null
          then null
          else lib.concatStringsSep "," hosts;

        # Build the environment attrset for one server instance.
        mkServerEnv = name: srv: let
          defaults =
            knownServers.${
              name
            } or {
              prefix = lib.toUpper name;
              port = 8080;
              hasToken = true;
            };
          p = defaults.prefix;
          bind = srv.bind;
          env =
            {
              "${p}_BIND" = bind;
              "${p}_INSECURE" = lib.boolToString srv.insecure;
            }
            // (lib.optionalAttrs (srv.host != null) {"${p}_HOST" = srv.host;})
            // (lib.optionalAttrs (defaults.hasToken && srv.tokenFile != null) {
              "${p}_TOKEN_FILE" = "${srv.tokenFile}";
            })
            // (lib.optionalAttrs (srv.allowedHosts != []) {
              "${p}_ALLOWED_HOSTS" = mkAllowedHosts srv.allowedHosts;
            })
            // srv.extraEnv;
        in
          lib.filterAttrs (_: v: v != null) env;

        # Build the systemd service for one server.
        mkService = name: srv: let
          env = mkServerEnv name srv;
          hasTokenFile = (knownServers.${name}.hasToken or true) && srv.tokenFile != null;
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
            tokenExport =
              lib.optionalString hasTokenFile
              ''export ${knownServers.${name}.prefix}_TOKEN="$(< "$CREDENTIALS_DIRECTORY/${tokenCredentialName}")"''
              + "\n";
          in
            tokenExport
            + ''exec ${srv.package}/bin/${name}'';
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
            type = lib.types.attrsOf (lib.types.submodule {
              options = {
                enable = lib.mkEnableOption "this MCP server";

                package = lib.mkOption {
                  type = lib.types.package;
                  defaultText = lib.literalExpression "self.packages.\${pkgs.system}.<server>";
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
            });
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
