#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

files="Cargo.toml Cargo.lock $(find src -type f -name '*.rs' | LC_ALL=C sort)"
shasum -a 256 $files | shasum -a 256 | awk '{print $1}'
