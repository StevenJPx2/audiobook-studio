# Contributing

## Commit messages — Conventional Commits

This repo uses [Conventional Commits](https://www.conventionalcommits.org) so
release notes and version bumps are meaningful.

```
<type>[optional scope]: <description>
```

Common types:

| Type | When | Version effect |
|------|------|----------------|
| `feat:` | new user-facing capability | minor |
| `fix:` | bug fix | patch |
| `perf:` | performance improvement | patch |
| `docs:` | docs only | none |
| `refactor:` | internal change, no behavior change | none/patch |
| `test:` | tests only | none |
| `build:` / `ci:` | build system / CI | none |
| `chore:` | misc maintenance | none |

A `!` after the type/scope (or a `BREAKING CHANGE:` footer) marks a breaking
change (major bump). Examples:

```
feat(cli): add `abs detect --json`
fix(g2p): prefer the frozen sidecar over uv at runtime
docs: document the Homebrew release flow
```

## Tests / checks

- Rust: `cargo fmt --check`, `cargo check --locked`, `cargo test --locked`
  (the binary-crate tests live in the library: `cargo test`).
- Sidecar: `cd sidecar && uv lock --check && uv sync --frozen`.

## Releasing

Tag-driven; see **BUNDLING.md → Release process**. In short: bump
`Cargo.toml` version, tag `vX.Y.Z`, push the tag — CI builds the macOS CLI
tarball, publishes the GitHub Release, and bumps the Homebrew tap.
