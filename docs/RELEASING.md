# Releasing dactor

This document describes the branch and tag workflow for publishing new versions of dactor to crates.io.

## Prerequisites

- You are on the `main` branch with all changes merged.
- All CI checks pass.
- Version numbers in all `Cargo.toml` files are updated and consistent.

## Release Workflow

### 1. Create a release branch

```bash
git checkout main
git pull origin main
git checkout -b release/vX.Y.Z
git push origin release/vX.Y.Z
```

### 2. Tag the release

```bash
git tag -a vX.Y.Z -m "Release vX.Y.Z - <brief description>"
git push origin vX.Y.Z
```

### 3. Publish to crates.io

Publish in dependency order (core first, then adapters):

```bash
cargo publish -p dactor
cargo publish -p dactor-ractor
cargo publish -p dactor-kameo
cargo publish -p dactor-coerce
```

Wait for each crate to be indexed before publishing the next one (crates.io may take a few seconds).

### 4. Create a GitHub release (optional)

```bash
gh release create vX.Y.Z --title "vX.Y.Z" --notes "Release notes here"
```

## Version Bumping

When starting work on the next version:

1. Bump versions in all `Cargo.toml` files (workspace root + each crate).
2. Update inter-crate dependency versions to match.
3. Commit: `chore: bump version to X.Y.Z`.

## Existing Releases

| Version | Branch | Tag | Date |
|---------|--------|-----|------|
| 0.2.0 | `release/v0.2.0` | `v0.2.0` | 2025 |
