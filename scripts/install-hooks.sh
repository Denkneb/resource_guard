#!/usr/bin/env sh
set -eu

if ! command -v pre-commit >/dev/null 2>&1; then
    echo "pre-commit is required; install it before enabling repository hooks" >&2
    exit 1
fi

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

pre-commit validate-config
pre-commit install --hook-type pre-commit --hook-type pre-push

echo "Installed pre-commit and pre-push hooks for $repository_root"
