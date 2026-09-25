# Build performance

Why the build tuning in this repo looks the way it does, and the measurements it
came from. Read this before changing `[profile.*]` in the root `Cargo.toml`, the
env vars in `flake.nix`'s dev shell, [`rust-analyzer.toml`](../rust-analyzer.toml),
or the flags on `just check` / `just clippy` / `just test`.

Everything below was measured on the workstation this repo is developed on:
**6 cores, 8 GB RAM**, rustc 1.98.0 from the pinned fenix toolchain, 312 crates
in `Cargo.lock`, 11 workspace members, ~11k lines of Rust.

## The three things worth knowing

### 1. Artifact sets coexist per invocation shape — they do not clobber

Cargo keys artifacts on the resolved feature set and the selected targets, so
`cargo check --workspace` and `cargo check --workspace --all-targets` are
different builds. The first run of a new shape pays to populate it; after that,
alternating between shapes is free.

| step | time |
|---|---|
| `check --workspace` (settle) | 150 s |
| `check --workspace` (repeat) | 1 s |
| `check -p pbsmcp` (new shape) | 16 s |
| `check -p pbsmcp` (repeat) | 0 s |
| `check --workspace` (switch back) | **0 s** |

That last row is the important one. An earlier reading of a noisier benchmark
concluded the shapes were evicting each other; they are not. The 40-55 s figures
in that first run were one-time population of shapes the target dir had never
seen.

The practical consequence is still that **fewer shapes is better**, because each
one costs 70-150 s once and some disk forever. That is why `just check`,
`just clippy` and the `clippy` check in `flake.nix` all select the same units.

### 2. Type-checking is already fast. Linking is not.

After a one-line edit to `crates/pbs-mcp/pbsmcp/src/server.rs`, measured in a
dedicated target dir on the profile that was in place before this branch:

| command | time |
|---|---|
| `cargo check --workspace --all-targets --all-features` | **3 s** |
| `cargo clippy --all-targets --all-features` | **1 s** |
| `cargo build -p pbsmcp-server` | 38 s |
| `cargo test --workspace --no-run` | **61 s** |

The edit is the same in all four rows. The difference is entirely codegen and
linking — `check` emits no object code and links nothing. Anything that claims
to speed up "builds" but does not touch the linker is not addressing this.

A warning about measuring this yourself: an early run of the same command in the
shared `target/` reported 364 s, and that number was wrong — it was populating a
shape the directory had never held (finding #1), not doing incremental work.
**Benchmark in a fresh `CARGO_TARGET_DIR`, and run the command twice.** The
first run of anything measures population; only the second measures the loop.

### 3. `aws-lc-sys` is the single most expensive crate in the tree

A cold `cargo build --workspace --release`: **325 s wall, 1406 s CPU, 421 units**.

| unit | CPU |
|---|---|
| `aws-lc-sys` | **125 s** |
| `prommcp-server` | 67 s |
| `wpmcp-server` | 65 s |
| `pbsmcp-server` | 64 s |
| `hamcp-server` | 63 s |
| `pgmcp` | 60 s |
| `sqlx-postgres` | 49 s |
| `ring` (2 units) | 61 s |

`aws-lc-sys` was twice the next-largest unit, and being a C build, none of the
Rust-level caching touched it. CI felt it most: the (since decommissioned) Woodpecker
pipeline pinned Nix to `cores = 1`, so anything the Attic cache cannot serve is rebuilt
single-threaded.

It was in the tree because reqwest 0.13's `rustls` feature selects the aws-lc-rs
crypto provider:

```toml
rustls             = ["__rustls-aws-lc-rs", "dep:rustls-platform-verifier", "__rustls"]
rustls-no-provider = [                      "dep:rustls-platform-verifier", "__rustls"]
```

That was never a deliberate choice — commit `e9c202f` switched to rustls to drop
OpenSSL and get static musl binaries, and the provider arrived with the feature
name. **The workspace now asks for `rustls-no-provider` and installs ring
itself** (`mcp_common::install_crypto_provider`), which removes `aws-lc-sys` and
`aws-lc-rs` from the tree entirely and let the container image drop `cmake`.

The two features differ *only* in the provider. Certificate trust is
`rustls-platform-verifier`'s job and it is enabled by both, so the system trust
store — and the homelab CA on the deployment host — is unaffected; this was
checked before making the change, because "the production VM has certs the MCPs
need" is exactly the kind of thing a crypto swap could plausibly break.

What ring genuinely cannot do, from `rustls-0.23.43/src/crypto/{ring,aws_lc_rs}/mod.rs`:

| | ring | aws-lc-rs |
|---|---|---|
| ECDSA P-256 / P-384, Ed25519, RSA PKCS1 + PSS | yes | yes |
| **ECDSA P-521** | **no** | yes |
| ML-KEM post-quantum hybrids | no | yes |

If a target endpoint ever presents a P-521 certificate, this is the first thing
to check. Nothing in the homelab does today (step-ca and Tailscale both issue
P-256), and reverting is a one-line feature change plus deleting the
`install_crypto_provider` calls.

## What is configured, and why

| setting | where | why |
|---|---|---|
| `profile.dev.debug = "line-tables-only"` | `Cargo.toml` | Backtraces keep file/line; the linker stops moving variable-level DWARF. |
| `profile.dev.package."*".debug = false` | `Cargo.toml` | Drops debuginfo for the ~300 dependencies. `"*"` never matches workspace members. |
| `RUSTC_WRAPPER = sccache` | `flake.nix` dev shell | Halves a `cargo clean` rebuild in the same directory. Does **not** help a different target dir — see below. |
| `CARGO_BUILD_RUSTFLAGS = -C link-arg=-fuse-ld=mold` | `flake.nix` dev shell | Attacks finding #2 directly: 66 s → 11 s on the edit-to-test-binaries loop. |
| `cargo.targetDir = true` | `rust-analyzer.toml` | Stops the editor and the terminal fighting over the `target/` lock. Costs one extra dependency build and ~3 GB. |
| identical flags on check/clippy/test | `justfile`, `flake.nix` | One populated shape instead of three (finding #1). |
| `reqwest` on `rustls-no-provider` + ring | `Cargo.toml`, `mcp-common` | Removes the most expensive crate in the tree (finding #3). |

### What sccache does and does not do

It caches rustc invocations for registry dependencies. It **cannot** cache
proc-macro crates or anything that invokes the linker (excluded by design), and
it refuses any unit built with `-C incremental` — which cargo passes for
workspace members in the dev profile. So it does nothing for the warm
edit-compile loop, and a large "non-cacheable" count in `just sccache-stats` is
expected rather than a misconfiguration.

Its payoff was measured in both directions, because the obvious assumption about
it is wrong:

| scenario | cold build | Rust hit rate |
|---|---|---|
| same target dir, after `cargo clean` | 92 s → **52 s** | 50 % |
| **different** target dir, cache warm | 100 s → **119 s** | **0 %** |

sccache's cache keys are sensitive to the absolute paths cargo passes, so a
brand-new target directory gets nothing from it and pays the wrapper overhead.
This branch originally justified enabling sccache *by* cross-target-dir reuse —
specifically to make rust-analyzer's own directory cheap — and that justification
did not survive being measured. It is kept because halving a `cargo clean`
rebuild is worth the overhead on its own.

**Do not reinstate the cross-directory claim without re-measuring it.**

### Why mold is an env var and not `.cargo/config.toml`

crane's source filter globs **every** `*.toml` file, so a committed
`.cargo/config.toml` would silently become a build input of `nix build` and of
the cargo-zigbuild container — neither of which has mold installed. Scoping the
flag to the dev shell keeps the flake's derivations and the musl cross-build on
their stock linker and reproducible.

## Results

Final numbers for the shipped configuration, each lever isolated in a fresh
target directory. "incr" is after a one-line edit to a leaf source file.

| | before this branch | shipped |
|---|---|---|
| `cargo test --workspace --no-run` **incr** | 61 s | **11 s** |
| `cargo test --workspace --no-run` cold | 351 s | **93 s** |
| `cargo build --workspace --release` cold | 325 s | **203 s** |
| `cargo build -p pbsmcp-server` incr | 38 s | 25 s |
| dev target dir | 7.0 G | 3.0 G |
| crates in `Cargo.lock` | 312 | 298 |

Attribution, so the next person knows which lever to pull:

- **debuginfo trim** — the dev loop. Cold test build 351 s → 177 s, incremental
  61 s → 5 s, disk 7.0 G → 3.0 G.
- **mold** — the dev loop, on top of that. Incremental test build 66 s → 11 s,
  cold 121 s → 93 s. **Nothing for release builds** (206 s vs 203 s): release
  carries no debuginfo, so linking is not the bottleneck there.
- **ring instead of aws-lc-rs** — the release and CI path. Cold release build
  325 s → 203 s, and 14 crates leave the lockfile.
- **sccache** — only a `cargo clean` rebuild, 92 s → 52 s.

## Considered and rejected

- **`.cargo/config.toml` for the linker** — see above.
- **mold in the nix builds and the container** — it does nothing for release
  builds (measured above), which is all those two produce. Adding it would mean
  a mold dependency in `commonArgs` and an `apt-get` in the Containerfile, for
  3 s of noise.
- **sccache in `devShells.ci`** — CI already gets the compiled dependency tree
  from Attic via crane's `cargoArtifacts`. A second cache layer would add
  closure to a shell whose whole purpose is being small.
- **cargo-nextest** — it speeds up *running* tests. Here the 364 s is compiling
  and linking them; nextest links the same binaries.
- **A `workspace-hack` crate (cargo-hakari)** — the standard fix for feature
  unification thrash between `--workspace` and `-p` builds. Finding #1 shows the
  thrash is a one-time 16 s here, not a recurring cost, so the generated crate
  and its upkeep are not worth it at this size. Revisit if the workspace grows.
- **`panic = "abort"` in release** — would cut codegen, but a panic would take
  down the whole daemon instead of one MCP request, which is the opposite of
  what the workspace lints in `Cargo.toml` are protecting.

## Re-measuring

```sh
just timings                      # per-crate profile of a workspace build
just sccache-stats                # cache hit rate
cargo clean && time just test     # cold, honest, ~5-10 min
```

Adopting the dev-shell env vars changes every fingerprint in `target/`, so the
first build after pulling this branch recompiles the world exactly once. If
`target/` has accumulated stale shapes (it was 23 GB when this was written), a
`cargo clean` first is the faster path — sccache keeps the dependency half.
