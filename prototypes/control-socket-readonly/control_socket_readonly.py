#!/usr/bin/env python3
"""PROTOTYPE — disposable read-only Codex app-server observer.

Question: can a second official-protocol client observe Codex Desktop's
pre-existing tasks, quota, live status, plan progress, and turn completion
without loading, resuming, or mutating a task?
"""

import argparse
import collections
import json
import os
import selectors
import socket
import stat
import subprocess
import sys
import time
from pathlib import Path


READ_ONLY_METHODS = {
    "initialize",
    "account/rateLimits/read",
    "thread/loaded/list",
    "thread/list",
}


def parse_args():
    codex_home = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    parser = argparse.ArgumentParser()
    parser.add_argument("--transport", choices=("proxy", "standalone"), default="proxy")
    parser.add_argument("--socket", type=Path, default=codex_home / "app-server-control/app-server-control.sock")
    parser.add_argument("--observe-seconds", type=float, default=10)
    return parser.parse_args()


def codex_binary():
    overridden = os.environ.get("CODEX_BIN")
    if overridden:
        return overridden
    desktop = Path("/Applications/ChatGPT.app/Contents/Resources/codex")
    return str(desktop) if desktop.is_file() else "codex"


def is_socket(path):
    try:
        return stat.S_ISSOCK(path.stat().st_mode)
    except FileNotFoundError:
        return False


def redact_home(value):
    return str(value).replace(str(Path.home()), "$HOME", 1)


def send(process, message):
    method = message.get("method")
    if method != "initialized" and method not in READ_ONLY_METHODS:
        raise RuntimeError(f"blocked non-read-only method: {method}")
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def read_message(process, selector, timeout):
    events = selector.select(timeout)
    if not events:
        return None
    line = process.stdout.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return None


def wait_for_id(process, selector, expected_id, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        message = read_message(process, selector, deadline - time.monotonic())
        if message and message.get("id") == expected_id:
            return message
    return None


def shape(value, depth=0):
    if depth >= 2:
        return type_name(value)
    if isinstance(value, dict):
        fields = ",".join(f"{key}:{shape(item, depth + 1)}" for key, item in sorted(value.items()))
        return "{" + fields + "}"
    if isinstance(value, list):
        return "[]" if not value else f"[{shape(value[0], depth + 1)}]"
    return type_name(value)


def type_name(value):
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return "unknown"


def data_array(value):
    if isinstance(value, list):
        return value
    data = value.get("data", []) if isinstance(value, dict) else []
    return data if isinstance(data, list) else []


def status_name(value):
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and isinstance(value.get("type"), str):
        return value["type"]
    if isinstance(value, dict) and value:
        return next(iter(value))
    return "absent"


def safe_number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else "absent"


def compact(counter):
    return ",".join(f"{key}={counter[key]}" for key in sorted(counter)) or "none"


def print_rate_limits(result):
    limits = result.get("rateLimits", result) if isinstance(result, dict) else {}
    for name in ("primary", "secondary"):
        window = limits.get(name)
        if not isinstance(window, dict):
            print(f"quota_{name}: absent")
            continue
        print(
            f"quota_{name}: usedPercent={safe_number(window.get('usedPercent'))} "
            f"windowDurationMins={safe_number(window.get('windowDurationMins'))} "
            f"resetsAt={safe_number(window.get('resetsAt'))}"
        )


def print_threads(result):
    threads = data_array(result)
    statuses = collections.Counter(status_name(thread.get("status")) for thread in threads)
    sources = collections.Counter()
    for thread in threads:
        source = thread.get("source")
        if isinstance(source, str):
            sources[source] += 1
        elif isinstance(source, dict) and source:
            sources[next(iter(source))] += 1
    print(f"listed_threads: {len(threads)}")
    print(f"thread_status_counts: {compact(statuses)}")
    print(f"thread_source_counts: {compact(sources)}")
    if threads:
        newest = threads[0]
        print(
            f"newest_thread: status={status_name(newest.get('status'))} "
            f"updatedAt_present={'updatedAt' in newest} turns_field_present={'turns' in newest}"
        )


def print_response(method, response):
    if response is None:
        print(f"response {method}: missing")
        return
    if "error" in response:
        error = response.get("error") or {}
        print(f"response {method}: error code={error.get('code', 'absent')}")
        return
    result = response.get("result")
    print(f"response {method}: ok shape={shape(result)}")
    if method == "account/rateLimits/read":
        print_rate_limits(result)
    elif method == "thread/loaded/list":
        print(f"loaded_threads: {len(data_array(result))}")
    elif method == "thread/list":
        print_threads(result)


def print_notification(method, params):
    params = params if isinstance(params, dict) else {}
    if method == "thread/status/changed":
        print(f"event thread/status/changed: status={status_name(params.get('status'))}")
    elif method == "turn/started":
        print("event turn/started: received (content redacted)")
    elif method == "turn/completed":
        turn = params.get("turn") if isinstance(params.get("turn"), dict) else {}
        print(f"event turn/completed: status={turn.get('status', 'absent')}")
    elif method == "turn/plan/updated":
        plan = params.get("plan") if isinstance(params.get("plan"), list) else []
        statuses = collections.Counter(step.get("status", "absent") for step in plan)
        print(f"event turn/plan/updated: steps={len(plan)} statuses={compact(statuses)} (step text redacted)")
    elif method == "account/rateLimits/updated":
        print("event account/rateLimits/updated: received (account metadata redacted)")


def main():
    args = parse_args()
    print("PROTOTYPE: read-only Codex app-server observer")
    print(f"allowlisted requests: {', '.join(sorted(READ_ONLY_METHODS))}")
    print(f"observe_seconds: {args.observe_seconds:g}")

    if args.transport == "proxy" and not is_socket(args.socket):
        print("transport: proxy")
        print(f"socket: absent ({redact_home(args.socket)})")
        print("verdict_hint: Desktop control socket is not exposed at the requested path")
        return 2

    codex = codex_binary()
    version = subprocess.run([codex, "--version"], capture_output=True, text=True, check=False).stdout.strip()
    print(f"codex_binary: {redact_home(codex)}")
    print(f"codex_version: {version or 'unavailable'}")

    if args.transport == "proxy":
        command = [codex, "app-server", "proxy", "--sock", str(args.socket)]
        print("transport: proxy")
        print(f"socket: present ({redact_home(args.socket)})")
    else:
        command = [codex, "app-server", "--listen", "stdio://"]
        print("transport: standalone stdio (comparison only)")

    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)

    send(
        process,
        {
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "zectrix-control-socket-readonly-prototype",
                    "title": "ZECTRIX read-only protocol probe",
                    "version": "0.0.0",
                },
                "capabilities": {"experimentalApi": False},
            },
        },
    )
    initialized = wait_for_id(process, selector, 1, 8)
    print_response("initialize", initialized)
    if initialized is None:
        process.terminate()
        return 3

    send(process, {"method": "initialized"})
    send(process, {"id": 2, "method": "account/rateLimits/read", "params": {}})
    send(process, {"id": 3, "method": "thread/loaded/list", "params": {}})
    send(
        process,
        {
            "id": 4,
            "method": "thread/list",
            "params": {
                "limit": 50,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "useStateDbOnly": True,
            },
        },
    )

    deadline = time.monotonic() + args.observe_seconds
    responses = {}
    notifications = collections.Counter()
    while time.monotonic() < deadline:
        message = read_message(process, selector, min(0.25, deadline - time.monotonic()))
        if not message:
            continue
        if isinstance(message.get("id"), int):
            responses[message["id"]] = message
            continue
        method = message.get("method")
        if isinstance(method, str):
            notifications[method] += 1
            print_notification(method, message.get("params"))

    print_response("account/rateLimits/read", responses.get(2))
    print_response("thread/loaded/list", responses.get(3))
    print_response("thread/list", responses.get(4))
    print(f"live_notification_counts: {compact(notifications)}")
    target_methods = (
        "thread/status/changed",
        "turn/started",
        "turn/completed",
        "turn/plan/updated",
    )
    print(
        "target_live_event_counts: "
        + ",".join(f"{method}={notifications[method]}" for method in target_methods)
    )

    send(process, {"id": 5, "method": "thread/loaded/list", "params": {}})
    loaded_after = wait_for_id(process, selector, 5, 3)
    after_result = loaded_after.get("result") if loaded_after else None
    print(f"loaded_threads_after_observation: {len(data_array(after_result))}")
    print("mutation_requests_sent: 0")
    print("task_load_or_resume_requests_sent: 0")

    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())
