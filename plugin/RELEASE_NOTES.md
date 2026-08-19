# Codex Dashboard for ZECTRIX 0.1.7+codex.20260819205756

Adds a persisted Chinese/English display locale. Existing settings remain Chinese for backward
compatibility; rerun setup to select English. Quota labels, reset timing, dates, task states,
privacy and compatibility messages, overflow counts, and sync status are localized while task
titles retain their original language.

The English completed-state label is `Task completed`.

The preview CLI accepts `--language zh|en`, and a locale change is treated as a visible state
change so the companion publishes a newly localized frame.

Keeps current Codex quota available when task activity observation is temporarily incompatible, and shows an explicit compatibility notice instead of stale task claims.

Recognized execution-turn lifecycle evidence now survives additive Codex app-server, SQLite, and rollout changes. Unknown or ambiguous evidence remains ignored, diagnostics stay content-safe, and quota publishing continues independently.

MVP limitations:

- no exact Desktop unread blue dot synchronization;
- no 待你 state;
- no authoritative 检查 state;
- no plan progress;
- no task mutation or control;
- no support for other operating systems;
- no support for other display models.

Automated fixture and package validation does not constitute physical NOTE4 validation. Device selection, persisted `pageId`, first push, subsequent meaningful updates, and physical readability require a separately authorized real-device check.
