#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)
artifact=${1:-"$repo_root/target/codex-zectrix-dashboard-$version-macos.tar.gz"}
stage=$(mktemp -d "${TMPDIR:-/tmp}/codex-zectrix-release.XXXXXX")
trap 'rm -rf "$stage"' EXIT

cd "$repo_root"
cargo build --locked --release --target aarch64-apple-darwin
cargo build --locked --release --target x86_64-apple-darwin

/usr/bin/lipo -create \
  target/aarch64-apple-darwin/release/codex-zectrix-dashboard \
  target/x86_64-apple-darwin/release/codex-zectrix-dashboard \
  -output plugin/bin/codex-zectrix-dashboard
chmod 755 plugin/bin/codex-zectrix-dashboard

mkdir -p "$stage/.agents/plugins" "$stage/plugin/bin"
cp .agents/plugins/marketplace.json "$stage/.agents/plugins/marketplace.json"
cp -R plugin/. "$stage/plugin/"
cp LICENSE "$stage/plugin/LICENSE"
mkdir -p "$(dirname -- "$artifact")"
tar -czf "$artifact" -C "$stage" .
printf '%s\n' "$artifact"
