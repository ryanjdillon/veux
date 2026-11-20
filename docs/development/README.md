# Development

This page is for contributors. For the design, see [spec.md](../spec.md) and [architecture.md](../architecture.md). For the
parameter explanations, see [explanations.md](explanations.md).

## Toolchain

`rust-toolchain.toml` pins the Rust version. Nix supplies the toolchains:

```bash
nix develop
```

Without Nix, install `rustup`, `just`, `cargo-deny`, and `cargo-zigbuild`.

## Build

```bash
just release   # cross-builds the static helpers (aarch64 and x86_64 musl) and embeds them
```

`just release` builds the helpers for remote hosts with `cargo-zigbuild`. Then it builds
`target/release/veux` with the helpers embedded.

## Checks

```bash
just check     # format, lint, test, and audit (CI runs this)
just lint      # cargo fmt --check and clippy
prek install   # run `just lint` before each commit
```

## Contribute

[CONTRIBUTING.md](../../CONTRIBUTING.md) describes the workflow: fork, branch, `just check`, pull
request, and the maintainer approval of CI for a fork.

## Conventions

[AGENTS.md](../../AGENTS.md) holds the conventions and the invariants. Its drift table lists
the docs that you must update when you change code.
