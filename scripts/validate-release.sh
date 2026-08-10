#!/bin/sh
set -eu

[ "$#" -eq 1 ] || { printf 'usage: %s <release.tar.gz>\n' "$0" >&2; exit 2; }
archive=$1
stage=$(mktemp -d "${TMPDIR:-/tmp}/codex-zectrix-validate.XXXXXX")
codex_data=$(mktemp -d "${TMPDIR:-/tmp}/codex-zectrix-home.XXXXXX")
trap 'rm -rf "$stage" "$codex_data"' EXIT
codex_bin=${CODEX_BIN:-codex}

if ! command -v "$codex_bin" >/dev/null 2>&1; then
  printf 'error: Codex CLI is required for release package validation: %s\n' "$codex_bin" >&2
  exit 127
fi

tar -xzf "$archive" -C "$stage"
test -f "$stage/.agents/plugins/marketplace.json"
test -f "$stage/plugin/.codex-plugin/plugin.json"
test -f "$stage/plugin/hooks/hooks.json"
test -f "$stage/plugin/LICENSE"
test -f "$stage/plugin/RELEASE_NOTES.md"
test -x "$stage/plugin/bin/codex-zectrix-dashboard"

architectures=$(/usr/bin/lipo -archs "$stage/plugin/bin/codex-zectrix-dashboard")
case "$architectures" in *arm64*) ;; *) exit 1 ;; esac
case "$architectures" in *x86_64*) ;; *) exit 1 ;; esac

CODEX_HOME="$codex_data" "$codex_bin" plugin marketplace add "$stage" --json >/dev/null
CODEX_HOME="$codex_data" "$codex_bin" plugin add codex-zectrix-dashboard@codex-zectrix-dashboard --json >/dev/null
CODEX_HOME="$codex_data" "$codex_bin" plugin list --json | grep -q 'codex-zectrix-dashboard'
"$stage/plugin/bin/codex-zectrix-dashboard" version >/dev/null

printf 'release_package=valid\narchitectures=%s\nreal_device=not_run\n' "$architectures"
