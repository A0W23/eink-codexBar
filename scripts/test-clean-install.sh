#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
archive=${1:-"$repo_root/target/codex-zectrix-dashboard-0.1.0-macos.tar.gz"}

cd "$repo_root"
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
