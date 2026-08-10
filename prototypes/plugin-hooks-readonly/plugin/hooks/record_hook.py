#!/usr/bin/env python3
"""PROTOTYPE — persist only allowlisted hook metadata, never hook content."""

import datetime
import hashlib
import json
import os
import sys
from pathlib import Path


EVENTS = {
    "SessionStart": "session_start",
    "UserPromptSubmit": "user_prompt_submit",
    "PermissionRequest": "permission_request",
    "PreToolUse": "pre_tool_use",
    "PostToolUse": "post_tool_use",
    "Stop": "stop",
    "SessionEnd": "session_end",
}
RESULTS = {
    "completed": "success",
    "success": "success",
    "failed": "failure",
    "error": "failure",
    "interrupted": "interrupted",
    "cancelled": "interrupted",
}
OUTPUT_DIR = Path("/tmp/codex-zectrix-plugin-hooks-readonly")
OUTPUT_FILE = OUTPUT_DIR / "events.jsonl"


def safe_result(payload):
    success = payload.get("success")
    if isinstance(success, bool):
        return "success" if success else "failure"
    for key in ("status", "reason", "outcome"):
        value = payload.get(key)
        if isinstance(value, str) and value.lower() in RESULTS:
            return RESULTS[value.lower()]
    exit_code = payload.get("exit_code")
    if isinstance(exit_code, int) and not isinstance(exit_code, bool):
        return "success" if exit_code == 0 else "failure"
    return None


def main():
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return 0
    if not isinstance(payload, dict):
        return 0

    raw_event = payload.get("hook_event_name")
    event = EVENTS.get(raw_event)
    if event is None:
        return 0

    minute = datetime.datetime.now(datetime.timezone.utc).replace(second=0, microsecond=0)
    slot = hashlib.sha256(f"zectrix-hook-slot:{os.getppid()}".encode()).hexdigest()[:10]
    record = {
        "event": event,
        "minute_utc": minute.isoformat().replace("+00:00", "Z"),
        "slot": slot,
    }
    result = safe_result(payload)
    if result is not None:
        record["result"] = result

    OUTPUT_DIR.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(OUTPUT_FILE, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
    try:
        os.write(descriptor, (json.dumps(record, sort_keys=True) + "\n").encode())
    finally:
        os.close(descriptor)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
