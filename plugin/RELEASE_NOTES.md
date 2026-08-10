# Codex Dashboard for ZECTRIX 0.1.1

Adds support for the current NOTE4 board identifier `zectrix-s3-epaper-4.2`, while retaining compatibility with the legacy API identifier. Also accepts the current Codex hook schema when `executionMode` is omitted.

MVP limitations:

- no exact Desktop unread blue dot synchronization;
- no 待你 state;
- no authoritative 检查 state;
- no plan progress;
- no task mutation or control;
- no support for other operating systems;
- no support for other display models.

Automated fixture and package validation does not constitute physical NOTE4 validation. Device selection, persisted `pageId`, first push, subsequent meaningful updates, and physical readability require a separately authorized real-device check.
