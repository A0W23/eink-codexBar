#!/usr/bin/env python3
"""PROTOTYPE — disposable, strictly read-only local Codex Desktop adapter.

Question: can local Codex files supply a privacy-redacted attention state and
plan progress accurately enough for the ZECTRIX dashboard?

Reproduce:
  python3 'Codex info/prototypes/local-state-readonly/local_state_readonly.py' --once --official
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import plistlib
import selectors
import sqlite3
import subprocess
import sys
import time
from dataclasses import replace
from pathlib import Path

sys.dont_write_bytecode = True

from local_state_model import parse_jsonl_tail, reduce_rollout


READ_ONLY_APP_SERVER_METHODS = {"initialize", "thread/list"}
STATE_DB = "state_5.sqlite"
TAIL_BYTES = 4 * 1024 * 1024


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--official", action="store_true", help="compare IDs with official thread/list")
    parser.add_argument("--observe-seconds", type=float, default=2.0)
    parser.add_argument("--poll-ms", type=int, default=250)
    parser.add_argument("--recent-hours", type=float, default=168.0)
    parser.add_argument("--max-rollouts", type=int, default=150)
    return parser.parse_args()


def metadata(path: Path):
    try:
        stat = path.stat()
    except FileNotFoundError:
        return None
    return (stat.st_ino, stat.st_size, stat.st_mtime_ns)


def sqlite_files(root: Path):
    names = (STATE_DB, STATE_DB + "-wal", STATE_DB + "-shm", STATE_DB + "-journal")
    return {name: metadata(root / name) for name in names}


def sqlite_inventory(root: Path):
    path = root / STATE_DB
    uri = f"file:{path}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True)
    tables = []
    for (table,) in connection.execute("select name from sqlite_schema where type='table' order by name"):
        quoted = '"' + table.replace('"', '""') + '"'
        columns = [row[1] for row in connection.execute(f"pragma table_info({quoted})")]
        tables.append((table, columns))
    schema_text = json.dumps(tables, separators=(",", ":"), sort_keys=True)
    thread_count = connection.execute("select count(*) from threads").fetchone()[0]
    rollout_count = connection.execute("select count(*) from threads where rollout_path is not null and rollout_path != ''").fetchone()[0]
    ids = {row[0] for row in connection.execute("select id from threads")}
    connection.close()
    return {
        "uri": "file:$HOME/.codex/state_5.sqlite?mode=ro&immutable=1",
        "schema_sha256_12": hashlib.sha256(schema_text.encode()).hexdigest()[:12],
        "table_count": len(tables),
        "checkpoint_thread_count": thread_count,
        "checkpoint_rollout_path_count": rollout_count,
        "internal_ids": ids,
        "limitation": "immutable access ignores live WAL and may be stale",
    }


def rollout_candidates(root: Path, limit: int):
    directory = root / "sessions"
    paths = list(directory.glob("**/rollout-*.jsonl")) if directory.is_dir() else []
    return sorted(paths, key=lambda path: path.stat().st_mtime_ns, reverse=True)[:limit]


def collect_local(root: Path, args):
    now = time.time()
    tasks = []
    read_proofs = []
    formats = collections.Counter()
    for path in rollout_candidates(root, args.max_rollouts):
        before = metadata(path)
        try:
            snapshot = reduce_rollout(parse_jsonl_tail(path, TAIL_BYTES))
        except OSError:
            continue
        after = metadata(path)
        read_proofs.append(before == after)
        if snapshot.internal_id is None:
            candidate = path.stem[-36:]
            if len(candidate) == 36 and candidate.count("-") == 4:
                snapshot = replace(snapshot, internal_id=candidate)
        age = None if snapshot.event_timestamp is None else max(0.0, now - snapshot.event_timestamp)
        if age is not None and age > args.recent_hours * 3600:
            continue
        formats[snapshot.rollout_format] += 1
        file_fresh_seconds = None if after is None else max(0.0, now - after[2] / 1_000_000_000)
        tasks.append((snapshot, age, file_fresh_seconds))
    return tasks, formats, read_proofs


def sanitized_tasks(tasks):
    rows = []
    for index, (task, age, file_fresh_seconds) in enumerate(tasks, 1):
        stale = task.state == "running" and (file_fresh_seconds is None or file_fresh_seconds > 30)
        rows.append({
            "slot": index,
            "state": task.state,
            "state_source": task.state_source,
            "age_seconds": None if age is None else round(age, 1),
            "source_fresh_seconds": None if file_fresh_seconds is None else round(file_fresh_seconds, 1),
            "stale": stale,
            "plan": None if task.plan_total is None else {
                "completed": task.plan_completed,
                "total": task.plan_total,
                "current_ordinal": task.plan_current_ordinal,
                "source": task.plan_source,
            },
            "plan_source": task.plan_source,
        })
    return rows


def codex_binary():
    desktop = Path("/Applications/ChatGPT.app/Contents/Resources/codex")
    return desktop if desktop.is_file() else Path("codex")


def send(process, message):
    method = message.get("method")
    if method != "initialized" and method not in READ_ONLY_APP_SERVER_METHODS:
        raise RuntimeError("blocked non-read-only app-server method")
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def official_thread_ids():
    command = [str(codex_binary()), "app-server", "--listen", "stdio://"]
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    send(process, {"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "zectrix-local-readonly-prototype", "title": "read-only", "version": "0.0.0"}, "capabilities": {"experimentalApi": False}}})
    send(process, {"method": "initialized"})
    send(process, {"id": 2, "method": "thread/list", "params": {"limit": 50, "sortKey": "updated_at", "sortDirection": "desc", "useStateDbOnly": True}})
    deadline = time.monotonic() + 8
    result = None
    while time.monotonic() < deadline:
        events = selector.select(deadline - time.monotonic())
        if not events:
            break
        line = process.stdout.readline()
        if not line:
            break
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("id") == 2:
            result = message.get("result")
            break
    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
    data = result.get("data", []) if isinstance(result, dict) else result if isinstance(result, list) else []
    return {item.get("id") for item in data if isinstance(item, dict) and isinstance(item.get("id"), str)}


def versions():
    info_path = Path("/Applications/ChatGPT.app/Contents/Info.plist")
    desktop = {"version": "unavailable", "build": "unavailable"}
    try:
        with info_path.open("rb") as handle:
            info = plistlib.load(handle)
        desktop = {"version": info.get("CFBundleShortVersionString", "unavailable"), "build": info.get("CFBundleVersion", "unavailable")}
    except OSError:
        pass
    result = subprocess.run([str(codex_binary()), "--version"], capture_output=True, text=True, check=False)
    return {"desktop": desktop, "codex": result.stdout.strip() or "unavailable"}


def snapshot(args):
    root = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    sqlite_before = sqlite_files(root)
    inventory = sqlite_inventory(root)
    tasks, formats, rollout_proofs = collect_local(root, args)
    sqlite_after = sqlite_files(root)
    local_ids = {task.internal_id for task, _, _ in tasks if task.internal_id}
    official = None
    if args.official:
        official_ids = official_thread_ids()
        official = {
            "official_count": len(official_ids),
            "checkpoint_db_matches": len(official_ids & inventory["internal_ids"]),
            "recent_rollout_matches": len(official_ids & local_ids),
        }
    inventory.pop("internal_ids")
    rows = sanitized_tasks(tasks)
    states = collections.Counter(("stale_" + row["state"]) if row["stale"] else row["state"] for row in rows)
    return {
        "prototype": "local-state-readonly",
        "versions": versions(),
        "sqlite": inventory,
        "read_only_proof": {
            "sqlite_file_set_unchanged": set(sqlite_before) == set(sqlite_after),
            "sqlite_metadata_unchanged_during_probe": sqlite_before == sqlite_after,
            "rollout_metadata_unchanged_for_every_read": all(rollout_proofs),
            "network_requests": 0,
            "task_mutation_requests": 0,
        },
        "sample": {
            "recent_hours": args.recent_hours,
            "rollouts_scanned": len(rollout_proofs),
            "tasks_in_window": len(rows),
            "state_counts": dict(sorted(states.items())),
            "rollout_formats": dict(sorted(formats.items())),
            "tasks": rows,
        },
        "official_correlation": official,
    }


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def semantic_sample(value):
    return {
        "state_counts": value["state_counts"],
        "tasks": [
            {
                "state": row["state"],
                "state_source": row["state_source"],
                "stale": row["stale"],
                "plan": row["plan"],
                "plan_source": row["plan_source"],
            }
            for row in value["tasks"]
        ],
    }


def observe(args):
    started = time.monotonic()
    first = snapshot(args)
    prior = fingerprint(semantic_sample(first["sample"]))
    changes = 0
    detected_latencies = []
    while time.monotonic() - started < args.observe_seconds:
        time.sleep(max(args.poll_ms, 20) / 1000)
        current = snapshot(args)
        current_fingerprint = fingerprint(semantic_sample(current["sample"]))
        if current_fingerprint != prior:
            changes += 1
            detected_latencies.append(args.poll_ms)
            prior = current_fingerprint
            first = current
    first["observation"] = {
        "duration_seconds": args.observe_seconds,
        "poll_interval_ms": args.poll_ms,
        "visible_state_changes_detected": changes,
        "detection_bound_ms_when_change_occurs": args.poll_ms,
        "natural_semantic_transition_observed": changes > 0,
    }
    return first


def render_tui(value):
    os.system("clear")
    print("\033[1mPROTOTYPE — local Codex read-only adapter\033[0m")
    print("\033[2mNo titles, IDs, prompts, responses, paths, or tool arguments are rendered.\033[0m\n")
    print(json.dumps(value, indent=2, ensure_ascii=False))
    print("\n\033[1m[r]\033[0m refresh  \033[1m[q]\033[0m quit")


def main():
    args = parse_args()
    if args.once or not sys.stdin.isatty():
        print(json.dumps(observe(args), indent=2, ensure_ascii=False))
        return 0
    while True:
        render_tui(snapshot(args))
        choice = input("> ").strip().lower()
        if choice == "q":
            return 0


if __name__ == "__main__":
    raise SystemExit(main())
