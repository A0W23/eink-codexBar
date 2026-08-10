#!/usr/bin/env python3
"""PROTOTYPE — install, inspect, observe, and remove the local hook plugin."""

import argparse
import collections
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from plugin_state_model import initial_state, reduce_event


ROOT = Path(__file__).resolve().parent
OUTPUT_FILE = Path("/tmp/codex-zectrix-plugin-hooks-readonly/events.jsonl")
MARKETPLACE = "zectrix-plugin-hooks-prototype"
PLUGIN = f"zectrix-hooks-readonly@{MARKETPLACE}"
CODEX = Path("/Applications/ChatGPT.app/Contents/Resources/codex")


def run(*args, capture=False):
    command = [str(CODEX if CODEX.is_file() else "codex"), *args]
    return subprocess.run(command, check=False, text=True, capture_output=capture)


def install():
    added_marketplace = run("plugin", "marketplace", "add", str(ROOT), "--json")
    if added_marketplace.returncode != 0:
        return added_marketplace.returncode
    return run("plugin", "add", PLUGIN, "--json").returncode


def cleanup():
    removed_plugin = run("plugin", "remove", PLUGIN, "--json")
    removed_marketplace = run("plugin", "marketplace", "remove", MARKETPLACE, "--json")
    return removed_plugin.returncode or removed_marketplace.returncode


def read_records():
    if not OUTPUT_FILE.is_file():
        return []
    records = []
    for line in OUTPUT_FILE.read_text().splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(record, dict) and set(record) <= {"event", "minute_utc", "slot", "result"}:
            records.append(record)
    return records


def render(records):
    states = {}
    for record in records:
        slot = record.get("slot")
        if not isinstance(slot, str):
            continue
        states[slot] = reduce_event(states.get(slot, initial_state()), record)

    print("PROTOTYPE: ZECTRIX Codex plugin hook observer")
    print(f"event_file: {OUTPUT_FILE}")
    counts = collections.Counter(record.get("event") for record in records)
    print(f"events: {len(records)}")
    print("event_counts: " + (", ".join(f"{key}={counts[key]}" for key in sorted(counts)) or "none"))
    print(f"anonymous_slots: {len(states)}")
    for slot, state in sorted(states.items()):
        print(f"slot {slot}: {json.dumps(state, sort_keys=True)}")
    print("persisted_fields: event, minute_utc, slot, optional result")


def observe(seconds):
    deadline = time.monotonic() + seconds
    prior = None
    while True:
        records = read_records()
        signature = json.dumps(records, sort_keys=True)
        if signature != prior:
            if sys.stdout.isatty():
                print("\033[2J\033[H", end="")
            render(records)
            prior = signature
        if time.monotonic() >= deadline:
            return 0
        time.sleep(0.25)


def reset_events():
    if OUTPUT_FILE.exists():
        OUTPUT_FILE.unlink()
    return 0


def hooks_list():
    process = subprocess.Popen(
        [str(CODEX if CODEX.is_file() else "codex"), "app-server", "--listen", "stdio://"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    messages = [
        {
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "zectrix-plugin-hooks-readonly-prototype",
                    "title": "ZECTRIX hook metadata probe",
                    "version": "0.0.0",
                },
                "capabilities": {"experimentalApi": True},
            },
        },
        {"method": "initialized"},
        {"id": 2, "method": "hooks/list", "params": {"cwds": [str(ROOT)]}},
    ]
    for message in messages:
        process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        process.stdin.flush()

    response = None
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            break
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("id") == 2:
            response = message
            break
    process.terminate()

    if response is None or "error" in response:
        print("hooks_list: unavailable")
        return 1
    entries = response.get("result", {}).get("data", [])
    for entry in entries:
        errors = entry.get("errors", []) if isinstance(entry, dict) else []
        warnings = entry.get("warnings", []) if isinstance(entry, dict) else []
        print(f"validation_errors: {len(errors)}")
        print(f"validation_warnings: {len(warnings)}")
        for warning in warnings:
            print(f"warning: {warning}")
        hooks = entry.get("hooks", []) if isinstance(entry, dict) else []
        selected = [hook for hook in hooks if hook.get("pluginId") == PLUGIN]
        for hook in sorted(selected, key=lambda item: item.get("eventName", "")):
            print(
                "hook: "
                f"event={hook.get('eventName', 'absent')} "
                f"enabled={hook.get('enabled', False)} "
                f"trust={hook.get('trustStatus', 'absent')} "
                f"key={hook.get('key', 'absent')} "
                f"hash={hook.get('currentHash', 'absent')}"
            )
        print(f"prototype_hooks: {len(selected)}")
    return 0


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("setup")
    observe_parser = subparsers.add_parser("observe")
    observe_parser.add_argument("--seconds", type=float, default=10)
    subparsers.add_parser("status")
    subparsers.add_parser("hooks-list")
    subparsers.add_parser("reset-events")
    subparsers.add_parser("cleanup")
    args = parser.parse_args()

    if args.command == "setup":
        return install()
    if args.command == "observe":
        return observe(args.seconds)
    if args.command == "status":
        render(read_records())
        return 0
    if args.command == "hooks-list":
        return hooks_list()
    if args.command == "reset-events":
        return reset_events()
    return cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
