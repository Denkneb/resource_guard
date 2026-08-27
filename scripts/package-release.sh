#!/usr/bin/env bash

set -euo pipefail

release_script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
release_repo_root="$(cd -- "$release_script_dir/.." && pwd)"
release_target="x86_64-unknown-linux-gnu"
release_output_dir="${2:-$release_repo_root/dist}"

cd "$release_repo_root"

package_id="$(cargo pkgid --locked)"
package_version="${package_id##*@}"
release_version="${1:-$package_version}"

if [[ "$release_version" != "$package_version" ]]; then
    echo "release version $release_version does not match Cargo package version $package_version" >&2
    exit 1
fi

archive_root="resource-guard-$release_version-$release_target"
archive_path="$release_output_dir/$archive_root.tar.gz"
checksum_path="$archive_path.sha256"

if [[ -e "$archive_path" || -e "$checksum_path" ]]; then
    echo "release artifact already exists in $release_output_dir" >&2
    exit 1
fi

if [[ -n "${RESOURCE_GUARD_RELEASE_BINARY:-}" ]]; then
    release_binary="$RESOURCE_GUARD_RELEASE_BINARY"
else
    cargo build --release --locked --target "$release_target"
    release_binary="$release_repo_root/target/$release_target/release/resource-guard"
fi

if [[ ! -x "$release_binary" ]]; then
    echo "release binary is missing or not executable: $release_binary" >&2
    exit 1
fi

release_staging_dir="$(mktemp -d)"
trap 'rm -rf -- "$release_staging_dir"' EXIT

install -Dm755 "$release_binary" "$release_staging_dir/$archive_root/bin/resource-guard"
install -Dm644 config.example.toml "$release_staging_dir/$archive_root/config/config.example.toml"
install -Dm644 packaging/resource-guard.service "$release_staging_dir/$archive_root/systemd/resource-guard.service"
install -Dm644 packaging/io.github.denkneb.ResourceGuard.desktop \
    "$release_staging_dir/$archive_root/applications/io.github.denkneb.ResourceGuard.desktop"
install -Dm644 README.md "$release_staging_dir/$archive_root/README.md"
install -Dm644 CHANGELOG.md "$release_staging_dir/$archive_root/CHANGELOG.md"
install -Dm644 LICENSE "$release_staging_dir/$archive_root/LICENSE"

source_date_epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
mkdir -p "$release_output_dir"

tar \
    --sort=name \
    --mtime="@$source_date_epoch" \
    --clamp-mtime \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    -C "$release_staging_dir" \
    -cf - \
    "$archive_root" | gzip -n >"$archive_path"

(
    cd "$release_output_dir"
    sha256sum "$(basename -- "$archive_path")" >"$(basename -- "$checksum_path")"
)

echo "created $archive_path"
echo "created $checksum_path"
