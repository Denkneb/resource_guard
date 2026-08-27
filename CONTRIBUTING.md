# Contributing

## Prerequisites

Development requires the Rust toolchain components used by the repository checks:

```text
rustfmt
clippy
```

It also requires `pre-commit` 3.6 or newer. The hooks use the system Rust toolchain and do not install or update it.

## Install Git hooks

Run the repository installer once per clone:

```sh
./scripts/install-hooks.sh
```

This installs both `pre-commit` and `pre-push` hooks in the current repository. It does not change global Git configuration.

## Checks

Every commit runs:

- standard whitespace, merge-marker, YAML, TOML, and large-file checks;
- `cargo fmt --all -- --check`;
- `cargo clippy --locked --all-targets --all-features -- -D warnings`.

Every push runs:

```sh
cargo test --locked --all-targets --all-features
```

Run either stage manually with:

```sh
pre-commit run --all-files --hook-stage pre-commit
pre-commit run --all-files --hook-stage pre-push
```

GitHub Actions runs formatting, Clippy, the complete test suite, and a locked release build for every pull request and every push to `main`. All CI checks must pass before merging.

Dependency security and policy checks run when Cargo dependency files change and on a weekly schedule. To run them locally, install the tools and use the repository policy:

```sh
cargo install --locked cargo-audit --version 0.22.2
cargo install --locked cargo-deny --version 0.20.2
cargo audit
cargo deny --all-features check advisories bans licenses sources
```

These network-dependent checks are deliberately kept out of the local Git hooks. Do not add advisory or license exceptions without documenting a concrete reason and reviewing the impact.

## Dependency updates

Dependabot checks Cargo dependencies and GitHub Actions every Monday. Minor and patch updates are grouped per ecosystem, while major updates remain separate pull requests. Dependabot does not merge changes automatically.

Review grouped updates as one change set, inspect upstream release notes, and confirm both CI and dependency-policy checks before merging. Major updates require an explicit compatibility review even when all automated checks pass.
