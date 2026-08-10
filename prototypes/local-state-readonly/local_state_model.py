"""Pure state reduction for the disposable local Codex-state prototype."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


TERMINAL_TYPES = {"task_complete", "turn_aborted"}
VALID_PLAN_STATUSES = {"pending", "in_progress", "completed"}


@dataclass(frozen=True)
class TaskSnapshot:
    internal_id: str | None
    state: str
    state_source: str
    event_timestamp: float | None
    plan_completed: int | None
    plan_total: int | None
    plan_current_ordinal: int | None
    plan_source: str
    needs_user_hint: bool
    rollout_format: str


def _decode_arguments(value):
    if isinstance(value, dict):
        return value
    if not isinstance(value, str):
        return None
    try:
        decoded = json.loads(value)
    except json.JSONDecodeError:
        return None
    return decoded if isinstance(decoded, dict) else None


def _event_time(record, payload):
    for key in ("completed_at", "started_at", "timestamp"):
        value = payload.get(key)
        if isinstance(value, (int, float)):
            return float(value) / 1000 if value > 10_000_000_000 else float(value)
        if isinstance(value, str):
            try:
                return __import__("datetime").datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
            except ValueError:
                pass
    value = record.get("timestamp")
    if isinstance(value, str):
        try:
            return __import__("datetime").datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
        except ValueError:
            pass
    return None


def reduce_rollout(records: list[dict]) -> TaskSnapshot:
    internal_id = None
    latest_lifecycle = None
    latest_lifecycle_time = None
    latest_plan = None
    needs_user_calls: set[str] = set()
    completed_calls: set[str] = set()
    saw_function_call = False
    saw_wrapped_exec = False

    for index, record in enumerate(records):
        if not isinstance(record, dict):
            continue
        payload = record.get("payload")
        if not isinstance(payload, dict):
            continue

        if record.get("type") == "session_meta" and internal_id is None:
            value = payload.get("id")
            internal_id = value if isinstance(value, str) else None

        payload_type = payload.get("type")
        if payload_type in {"task_started", "task_complete", "turn_aborted"}:
            latest_lifecycle = payload
            latest_lifecycle_time = _event_time(record, payload)

        if record.get("type") != "response_item" or payload_type not in {"function_call", "custom_tool_call", "function_call_output", "custom_tool_call_output"}:
            continue

        call_id = payload.get("call_id")
        if payload_type in {"function_call_output", "custom_tool_call_output"} and isinstance(call_id, str):
            completed_calls.add(call_id)
            continue

        name = payload.get("name")
        if name == "exec":
            saw_wrapped_exec = True
        if name == "request_user_input" and isinstance(call_id, str):
            needs_user_calls.add(call_id)
        if name != "update_plan":
            continue

        saw_function_call = True
        arguments = _decode_arguments(payload.get("arguments", payload.get("input")))
        plan = arguments.get("plan") if arguments else None
        if not isinstance(plan, list):
            continue
        statuses = [step.get("status") for step in plan if isinstance(step, dict)]
        if len(statuses) != len(plan) or any(status not in VALID_PLAN_STATUSES for status in statuses):
            continue
        latest_plan = statuses

    if latest_lifecycle is None:
        state, state_source = "unavailable", "unavailable"
    elif latest_lifecycle.get("type") == "task_started":
        state, state_source = "running", "inferred"
    elif latest_lifecycle.get("type") == "turn_aborted":
        state, state_source = "interrupted", "locally_authoritative"
    elif latest_lifecycle.get("error"):
        state, state_source = "failed", "locally_authoritative"
    else:
        state, state_source = "ready_for_review", "inferred"

    needs_user_hint = bool(needs_user_calls - completed_calls)
    if state == "running" and needs_user_hint:
        state, state_source = "needs_user", "inferred"

    if latest_plan is None:
        completed = total = current = None
        plan_source = "unavailable_current_format" if saw_wrapped_exec and not saw_function_call else "unavailable"
    else:
        total = len(latest_plan)
        completed = sum(status == "completed" for status in latest_plan)
        current = next((i + 1 for i, status in enumerate(latest_plan) if status == "in_progress"), None)
        plan_source = "inferred_structured_tool_call"

    rollout_format = "structured_tools" if saw_function_call else "wrapped_tools" if saw_wrapped_exec else "events_only"
    return TaskSnapshot(
        internal_id=internal_id,
        state=state,
        state_source=state_source,
        event_timestamp=latest_lifecycle_time,
        plan_completed=completed,
        plan_total=total,
        plan_current_ordinal=current,
        plan_source=plan_source,
        needs_user_hint=needs_user_hint,
        rollout_format=rollout_format,
    )


def parse_jsonl_tail(path: Path, max_bytes: int) -> list[dict]:
    size = path.stat().st_size
    with path.open("rb") as handle:
        if size > max_bytes:
            handle.seek(size - max_bytes)
            handle.readline()
        data = handle.read()
    records = []
    for line in data.splitlines():
        try:
            value = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        if isinstance(value, dict):
            records.append(value)
    return records
