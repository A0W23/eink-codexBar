#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)
archive=${1:-"$repo_root/target/codex-zectrix-dashboard-$version-macos.tar.gz"}
cd "$repo_root"
"$repo_root/scripts/validate-release.sh" "$archive"

printf '%s\n' \
  'plugin_install=passed' \
  'packaged_binary=passed' \
  'developer_runtime=not_required' \
  'real_device=not_run'
