# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## 0.5.0 - 2026-07-31
#### Features
- (**pbsmcp**) add start/tail paging with total to task_log - (25c3fd2) - Amadeus Mader
#### Bug Fixes
- (**hamcp**) percent-encode user-supplied URL path segments - (057cabf) - Amadeus Mader
- (**lokimcp**) percent-encode label name in label_values path - (8c2a3f0) - Amadeus Mader
- (**pbsmcp**) percent-encode user-supplied URL path segments - (20e059f) - Amadeus Mader
- (**prommcp**) percent-encode label name in label_values path - (1b2dadb) - Amadeus Mader
#### Documentation
- (**agents**) document hamcp exception to the read-only rule - (4326c50) - Amadeus Mader
- (**readme**) add hamcp 8084 to quick-start and client config - (00267b7) - Amadeus Mader
- (**todos**) create todos to fix - (db43173) - Amadeus Mader
#### Continuous Integration
- (**action**) remove actions - (d12bcb2) - Amadeus Mader
#### Miscellaneous Chores
- (**deps**) add percent-encoding to workspace dependencies - (6e7c9e8) - Amadeus Mader
- (**deps**) upgrade flake - (73893ed) - Amadeus Mader
- (**deps**) upgrade to rust 1.96.1 - (de97c5b) - Amadeus Mader

- - -

## 0.4.0 - 2026-07-12
#### Features
- (**harbor**) verify registry login before pushing images - (acc5e4f) - Amadeus Mader

- - -

## 0.3.1 - 2026-07-12
#### Bug Fixes
- (**version**) flake uses version from cargo.toml - (618d6e0) - Amadeus Mader

- - -

## 0.3.0 - 2026-07-12
#### Features
- (**compose**) enable pgmcp and template multi-instance postgres - (9a9f073) - Amadeus Mader
- (**nixos**) add serverType option for multi-instance servers - (1704019) - Amadeus Mader
- add homeassistant-mcp server - (95ee518) - Amadeus Mader
- crane builds and nixos module - (2071e5c) - Amadeus Mader
- add hamcp homeassistant mcp server - (c5ccc7d) - Amadeus Mader
- add mcp-common shared crate - (41b9b6a) - Amadeus Mader
#### Bug Fixes
- (**nix**) correct secret env vars in NixOS module - (4fe4af5) - Amadeus Mader
#### Documentation
- (**agents**) update agents - (1cdfa8e) - Amadeus Mader
- (**hamcp**) mark migration plan as executed - (7fca73f) - Amadeus Mader
- document multi-instance pgmcp deployment - (725af4c) - Amadeus Mader
#### Continuous Integration
- add github actions workflows - (1d0d59e) - Amadeus Mader
#### Refactoring
- healthchecks via mcp-common in all servers - (9d7f460) - Amadeus Mader
#### Miscellaneous Chores
- (**container**) upgrade rust v - (e0b4dc1) - Amadeus Mader
- (**deps**) upgrade flake and cargo - (81fbb5c) - Amadeus Mader

- - -

## 0.2.5 - 2026-06-30
#### Features
- (**lokimcp**) add read-only loki mcp server - (3f6e5a8) - Amadeus Mader
- (**prommcp**) add read-only prometheus mcp server - (a2116fd) - Amadeus Mader
#### Documentation
- (**agents**) add AGENTS.md contributor guide - (99dfb70) - Amadeus Mader
- (**readme**) document prometheus and loki servers - (41a2edd) - Amadeus Mader
#### Continuous Integration
- (**harbor**) also push prommcp image - (22ac582) - Amadeus Mader
#### Miscellaneous Chores
- (**just**) add prommcp and lokimcp to image-all - (3703aa4) - Amadeus Mader

- - -

## 0.2.4 - 2026-06-30
#### Documentation
- (**pgmcp**) added pgmcp readme and link to main readme - (ba5ec8c) - Amadeus Mader
- (**readme**) :memo: add README - (f66f7a8) - Amadeus Mader

- - -

## 0.2.3 - 2026-06-30
#### Features
- (**compose**) add postgres 18 service - (48a9ee8) - Amadeus Mader
- (**pgmcp**) implement read-only postgres mcp server - (d7958cd) - Amadeus Mader
#### Tests
- (**pgmcp**) add integration tests - (3c8e47e) - Amadeus Mader
#### Refactoring
- (**error**) drop anyhow for color-eyre - (0b59e12) - Amadeus Mader
#### Miscellaneous Chores
- (**just**) tidy recipes, detach compose up - (e56b010) - Amadeus Mader
- (**toolchain**) bump rust to 1.96 - (7e2392e) - Amadeus Mader
- ignore local dev files - (49cafbb) - Amadeus Mader

- - -

## 0.2.2 - 2026-06-23
#### Features
- (**tools**) add cargo-edit for cog to automatically bump the version - (14176b1) - Amadeus Mader
#### Miscellaneous Chores
- (**deps**) upgrade cargo deps - (f2da52f) - Amadeus Mader

- - -

## 0.2.1 - 2026-06-23
#### Miscellaneous Chores
- (**version**) 0.2.1 - (168f7ad) - Amadeus Mader

- - -

## 0.2.0 - 2026-06-23
#### Features
- v0.1.1 - (cf95176) - Amadeus Mader

- - -

## 0.1.0 - 2026-06-23
#### Features
- (**flake**) update flake and project setup - (e3fca32) - Amadeus Mader
- (**oci**) add podman container build for MCP servers - (14722ce) - Amadeus Mader
- (**pbs-mcp**) add Proxmox Backup Server MCP server - (62159d9) - Amadeus Mader
- (**postgres-mcp**) add postgres MCP server crate and binary - (b43a51e) - Amadeus Mader
- (**template**) add server-mcp template for new MCP servers - (b7e1049) - Amadeus Mader
- (**tools**) add trivy scan cmd - (4f59549) - Amadeus Mader
- (**workspace**) add Cargo workspace root with shared dependencies - (22f8592) - Amadeus Mader
- :tada: init - (802d8c2) - Amadeus Mader
#### Documentation
- (**architecture**) add monorepo overview and decision documentation - (a0dc41a) - Amadeus Mader
- (**pbs-mcp**) add permission explaination - (808beb2) - Amadeus Mader
#### Miscellaneous Chores
- (**cargo**) add cargo-deny configuration for license and security auditing - (e2c9758) - Amadeus Mader
- (**deps**) switch reqwest and sqlx to rustls - (e9c202f) - Amadeus Mader
- (**harbor**) add harbor push script - (96f5b9c) - Amadeus Mader
- (**workspace**) add generated Cargo.lock - (f0420ab) - Amadeus Mader

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).