---
name: setup-zectrix-dashboard
description: Set up the installed Codex Dashboard for a ZECTRIX NOTE4.
disable-model-invocation: true
---

Resolve the plugin root as the directory two levels above this file. Run its bundled `bin/codex-zectrix-dashboard` companion; do not install Python, Node.js, or Rust.

For a fresh install, run:

1. `lifecycle install --plugin-root <plugin-root> --plugin-id codex-zectrix-dashboard@codex-zectrix-dashboard`
2. `setup`
3. `diagnostics`

Do not enter or request the ZECTRIX API key in conversation. Let the companion collect it through the local non-echoing terminal prompt. Stop at the preview confirmation so the user decides whether the first physical push proceeds.

For an update:

1. Read the future version from the remote `plugin/.codex-plugin/plugin.json` without refreshing the configured marketplace, then derive its future installed root as `$CODEX_HOME/plugins/cache/codex-zectrix-dashboard/codex-zectrix-dashboard/<version>` (use `~/.codex` only when `CODEX_HOME` is unset).
2. Using the currently installed binary, run `lifecycle update --plugin-root <future-installed-root> --plugin-id codex-zectrix-dashboard@codex-zectrix-dashboard`.
3. Ask the user to reload or restart Codex Desktop. Do not perform that action for them.
4. Using the retained old binary, run `lifecycle resume`. It refreshes the marketplace, installs the new plugin, verifies exact hooks, starts the new companion, and only then removes the old executable.

Do not run `codex plugin marketplace upgrade` before `lifecycle update`: current Codex versions can replace the installed cache path during the refresh.

For uninstall, run `lifecycle uninstall`, follow its reload or restart instruction, then run `lifecycle resume`. Never remove the cached hook executable early.
