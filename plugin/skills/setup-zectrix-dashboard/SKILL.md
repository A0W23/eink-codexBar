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

For an update or uninstall, use the lifecycle command and follow its reload or restart instruction before running `lifecycle resume`. Never remove the cached hook executable early.
