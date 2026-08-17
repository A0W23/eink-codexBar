use std::fs;
use std::fs::FileTimes;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    ActivityEventKind, CorrelationKey, ReadonlyObservationConfig, ReadonlyRolloutObserver,
    parse_app_server_tasks, parse_hook_event, persist_hook_event, read_hook_events,
};
use rusqlite::Connection;

const SALT: &str = "test-installation";

#[test]
fn official_thread_metadata_keeps_only_named_top_level_task_identity() {
    let response = r#"{
      "data": [
        {"id":"top-id","sessionId":"session-tree-1","name":"主任务","parentThreadId":null,"source":"appServer","preview":"SECRET_PROMPT","cwd":"SECRET_PATH"},
        {"id":"automation-id","sessionId":"automation-session","name":"每日自动化","parentThreadId":null,"source":{"custom":"automation"},"preview":"SECRET_AUTOMATION_PROMPT"},
        {"id":"child-id","sessionId":"session-tree-1","name":"子代理","parentThreadId":"top-id","source":{"subAgent":"review"},"preview":"SECRET_CHILD_PROMPT"},
        {"id":"unnamed-id","sessionId":"unnamed-session","name":null,"parentThreadId":null,"source":"appServer","preview":"SECRET_UNNAMED_PROMPT"}
      ],
      "nextCursor": null
    }"#;

    let tasks = parse_app_server_tasks(response, SALT).unwrap();

    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0].title, "主任务");
    assert_eq!(tasks[1].title, "每日自动化");
    assert_eq!(tasks[2].title, "子代理");
    assert_eq!(
        tasks[2].parent_correlation,
        Some(tasks[0].correlation.clone())
    );
    assert_eq!(tasks[0].correlation, CorrelationKey::derive("top-id", SALT));
    assert_eq!(
        tasks[0].correlation_aliases,
        vec![CorrelationKey::derive("session-tree-1", SALT)]
    );
    let diagnostic = format!("{tasks:?}");
    for secret in [
        "top-id",
        "automation-id",
        "child-id",
        "SECRET_PROMPT",
        "SECRET_PATH",
    ] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn official_thread_metadata_skips_unknown_records_without_poisoning_known_tasks() {
    let response = r#"{
      "data": [
        {"id":"known","sessionId":"session-1","name":"已识别任务","parentThreadId":null,"source":"appServer"},
        {"id":"unknown","sessionId":"session-2","name":"未知任务","parentThreadId":null,"source":"futureSource"}
      ],
      "nextCursor": null
    }"#;

    let tasks = parse_app_server_tasks(response, SALT).unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "已识别任务");
}

#[test]
fn official_thread_metadata_reports_an_entirely_unrecognized_page_as_unavailable() {
    let response = r#"{
      "data": [
        {"id":"unknown","sessionId":"session-1","name":"未知任务","parentThreadId":null,"source":"futureSource"}
      ],
      "nextCursor": null
    }"#;

    assert!(parse_app_server_tasks(response, SALT).is_err());
}

#[test]
fn hook_ingestion_allowlists_lifecycle_enum_and_discards_every_other_field() {
    let raw = r#"{
      "hook_event_name":"PreToolUse",
      "session_id":"task-1",
      "prompt":"SECRET_PROMPT",
      "tool_name":"SECRET_TOOL",
      "tool_input":{"path":"SECRET_PATH"},
      "arbitrary":"SECRET_ARBITRARY"
    }"#;

    let event = parse_hook_event(raw, SALT, 1_786_329_960).unwrap().unwrap();

    assert_eq!(event.kind, ActivityEventKind::ToolActivity);
    assert_eq!(event.observed_at_epoch_seconds, 1_786_329_960);
    assert_eq!(event.correlation, CorrelationKey::derive("task-1", SALT));
    let diagnostic = format!("{event:?}");
    for secret in ["task-1", "SECRET_PROMPT", "SECRET_TOOL", "SECRET_PATH"] {
        assert!(!diagnostic.contains(secret));
    }
    assert!(
        parse_hook_event(
            r#"{"hook_event_name":"FutureHook","session_id":"task-1"}"#,
            SALT,
            1_786_329_960
        )
        .is_err()
    );
}

#[test]
fn hook_stop_accepts_only_reviewed_results() {
    let normal = parse_hook_event(
        r#"{"hook_event_name":"Stop","session_id":"task-1"}"#,
        SALT,
        1_786_329_960,
    )
    .unwrap()
    .unwrap();
    let failed = parse_hook_event(
        r#"{"hook_event_name":"Stop","session_id":"task-1","status":"failed","error":"SECRET_ERROR"}"#,
        SALT,
        1_786_329_960,
    )
    .unwrap()
    .unwrap();

    assert_eq!(normal.kind, ActivityEventKind::TurnStopped);
    assert_eq!(failed.kind, ActivityEventKind::TurnFailed);
    assert!(
        parse_hook_event(
            r#"{"hook_event_name":"Stop","session_id":"task-1","status":"future-result"}"#,
            SALT,
            1_786_329_960
        )
        .is_err()
    );
}

#[test]
fn persisted_hook_records_contain_only_normalized_internal_data() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hooks/events.jsonl");
    let event = parse_hook_event(
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"task-1","prompt":"SECRET_PROMPT"}"#,
        SALT,
        1_786_329_999,
    )
    .unwrap()
    .unwrap();

    persist_hook_event(&path, &event).unwrap();

    let bytes = fs::read(&path).unwrap();
    let persisted = String::from_utf8(bytes).unwrap();
    assert!(!persisted.contains("task-1"));
    assert!(!persisted.contains("SECRET_PROMPT"));
    assert_eq!(read_hook_events(&path).unwrap(), vec![event]);
    assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn corrupted_persisted_hook_record_does_not_hide_valid_task_activity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hook-events.jsonl");
    let first = parse_hook_event(
        r#"{"hook_event_name":"PreToolUse","session_id":"task-1"}"#,
        SALT,
        1_786_329_960,
    )
    .unwrap()
    .unwrap();
    let second = parse_hook_event(
        r#"{"hook_event_name":"Stop","session_id":"task-1"}"#,
        SALT,
        1_786_330_020,
    )
    .unwrap()
    .unwrap();
    persist_hook_event(&path, &first).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{interleaved-record}\n")
        .unwrap();
    persist_hook_event(&path, &second).unwrap();

    assert_eq!(read_hook_events(&path).unwrap(), vec![first, second]);
}

#[test]
fn concurrent_hook_writers_preserve_every_normalized_event() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hook-events.jsonl");
    let writer_count = 64;
    let barrier = Arc::new(Barrier::new(writer_count));
    let writers = (0..writer_count)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let event = parse_hook_event(
                    &format!(r#"{{"hook_event_name":"PreToolUse","session_id":"task-{index}"}}"#),
                    SALT,
                    1_786_329_960,
                )
                .unwrap()
                .unwrap();
                barrier.wait();
                persist_hook_event(&path, &event).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }

    assert_eq!(read_hook_events(&path).unwrap().len(), writer_count);
}

#[test]
fn rollout_observation_reads_minimal_lifecycle_envelopes_without_modifying_codex_state() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions/2026/08/10");
    fs::create_dir_all(&sessions).unwrap();
    let database = temp.path().join("state_5.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("create table threads (id text primary key, title text)", [])
        .unwrap();
    drop(connection);
    let rollout = sessions.join("rollout-sanitized.jsonl");
    fs::write(
        &rollout,
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different-thread-id\",\"session_id\":\"task-1\",\"cli_version\":\"0.147.0-alpha.6.5\",\"cwd\":\"SECRET_PATH\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329980,\"turn_id\":\"SECRET_TURN\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"SECRET_RESPONSE\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"completed_at\":1786329990,\"last_agent_message\":\"SECRET_RESULT\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329991}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"completed_at\":1786329992,\"error\":\"SECRET_ERROR\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329993}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\",\"reason\":\"interrupted\",\"completed_at\":1786329994}}\n"
        ),
    )
    .unwrap();
    let expired = sessions.join("rollout-expired.jsonl");
    fs::write(
        &expired,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old-thread\",\"session_id\":\"old-session\",\"cli_version\":\"unsupported-old-version\"}}\n",
    )
    .unwrap();
    fs::File::options()
        .write(true)
        .open(&expired)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000)))
        .unwrap();
    let before = inventory(temp.path());
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    let events = observer.observe().unwrap();

    assert_eq!(events.len(), 12);
    assert_eq!(
        events
            .chunks_exact(2)
            .map(|pair| pair[0].kind)
            .collect::<Vec<_>>(),
        vec![
            ActivityEventKind::RolloutStarted,
            ActivityEventKind::TurnStopped,
            ActivityEventKind::RolloutStarted,
            ActivityEventKind::TurnFailed,
            ActivityEventKind::RolloutStarted,
            ActivityEventKind::TurnInterrupted,
        ]
    );
    assert!(
        events
            .chunks_exact(2)
            .all(|pair| pair[0].kind == pair[1].kind)
    );
    assert_eq!(before, inventory(temp.path()));
    let diagnostic = format!("{events:?}");
    for secret in [
        "task-1",
        "SECRET_PATH",
        "SECRET_TURN",
        "SECRET_RESPONSE",
        "SECRET_RESULT",
        "SECRET_ERROR",
    ] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn additive_schema_and_cli_changes_preserve_recognized_activity_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let database = temp.path().join("state_5.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("create table threads (id text primary key)", [])
        .unwrap();
    drop(connection);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("alter table threads add column additive text", [])
        .unwrap();
    connection
        .execute("create table additive_table (value text)", [])
        .unwrap();
    drop(connection);
    fs::write(
        sessions.join("rollout-current.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"task-1\",\"cli_version\":\"0.148.0-alpha.9\",\"additive\":true}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329990,\"additive\":true}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"completed_at\":1786329991,\"additive\":true}}\n"
        ),
    )
    .unwrap();
    let before = inventory(temp.path());
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    let events = observer.observe().unwrap();

    assert_eq!(
        events.iter().map(|event| event.kind).collect::<Vec<_>>(),
        vec![
            ActivityEventKind::RolloutStarted,
            ActivityEventKind::TurnStopped,
        ]
    );
    assert_eq!(before, inventory(temp.path()));
}

#[test]
fn unknown_and_malformed_rollout_records_do_not_poison_verified_activity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let connection = Connection::open(temp.path().join("state_5.sqlite")).unwrap();
    connection
        .execute("create table threads (id text)", [])
        .unwrap();
    drop(connection);
    fs::write(
        sessions.join("rollout-additive.jsonl"),
        include_bytes!("../fixtures/rollout-0.148-additive.jsonl"),
    )
    .unwrap();
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    let events = observer.observe().unwrap();

    assert_eq!(
        events
            .iter()
            .map(|event| (event.kind, event.observed_at_epoch_seconds))
            .collect::<Vec<_>>(),
        vec![
            (ActivityEventKind::RolloutStarted, 1_786_329_990),
            (ActivityEventKind::TurnStopped, 1_786_329_992),
        ]
    );
    let diagnostic = format!("{events:?}");
    assert!(!diagnostic.contains("SECRET_"));
}

#[test]
fn missing_rollout_directory_is_an_unavailable_source_not_an_empty_activity_list() {
    let temp = tempfile::tempdir().unwrap();
    let connection = Connection::open(temp.path().join("state_5.sqlite")).unwrap();
    connection
        .execute("create table threads (id text)", [])
        .unwrap();
    drop(connection);
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    assert!(observer.observe().is_err());
}

#[test]
fn supported_recent_rollouts_remain_available_beside_an_old_cli_rollout() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let connection = Connection::open(temp.path().join("state_5.sqlite")).unwrap();
    connection
        .execute("create table threads (id text)", [])
        .unwrap();
    drop(connection);
    fs::write(
        sessions.join("rollout-old.jsonl"),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old-thread\",\"cli_version\":\"0.146.0\"}}\n",
    )
    .unwrap();
    fs::write(
        sessions.join("rollout-current.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"current-thread\",\"cli_version\":\"0.147.0-alpha.6.5\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329990}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"completed_at\":1786329991}}\n"
        ),
    )
    .unwrap();
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    assert_eq!(observer.observe().unwrap().len(), 2);
}

#[test]
fn rollout_observation_fails_closed_when_state_database_is_not_readable() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-sanitized.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"task-1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1786329990}}\n"
        ),
    )
    .unwrap();
    fs::write(temp.path().join("state_5.sqlite"), b"not a database").unwrap();
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
    });

    assert!(observer.observe().is_err());
}

fn inventory(root: &std::path::Path) -> Vec<(String, u64, i64, i64)> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let metadata = entry.metadata().unwrap();
            entries.push((
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .display()
                    .to_string(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ));
        }
    }
    entries.sort();
    entries
}
