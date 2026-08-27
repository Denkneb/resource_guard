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

`cargo-audit` and `cargo-deny` are not part of the local hooks. They require separate installation and policy configuration and can be added to CI independently.
