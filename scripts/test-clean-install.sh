#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)
archive=${1:-"$repo_root/target/codex-zectrix-dashboard-$version-macos.tar.gz"}
stage=$(mktemp -d "${TMPDIR:-/tmp}/codex-zectrix-clean-install.XXXXXX")
trap 'rm -rf "$stage"' EXIT

cd "$repo_root"
tar -xzf "$archive" -C "$stage"
CODEX_ZECTRIX_TEST_BINARY="$stage/plugin/bin/codex-zectrix-dashboard" \
  cargo test --test release_package --test plugin_lifecycle --test setup_cli --test companion_cli --test publisher
"$repo_root/scripts/validate-release.sh" "$archive"

printf '%s\n' \
  'plugin_install=passed' \
  'exact_hook_trust=passed' \
  'setup_preview_fake_first_push=passed' \
  'companion_start=passed' \
  'meaningful_change_publish=passed' \
  'safe_uninstall=passed' \
  'real_device=not_run'
