use std::fs;
use std::os::unix::fs::MetadataExt;

use codex_zectrix_dashboard::{
    ActivityEventKind, CorrelationKey, ReadonlyObservationConfig, ReadonlyRolloutObserver,
    compute_state_schema_fingerprint, parse_app_server_tasks, parse_hook_event, persist_hook_event,
    read_hook_events,
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
fn official_thread_metadata_fails_closed_for_an_unknown_source_enum() {
    let response = r#"{"data":[{"id":"task-1","sessionId":"session-1","name":"任务","parentThreadId":null,"source":"futureSource"}]}"#;

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
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"completed_at\":1786329990,\"last_agent_message\":\"SECRET_RESULT\"}}\n"
        ),
    )
    .unwrap();
    let before = inventory(temp.path());
    let fingerprint = compute_state_schema_fingerprint(temp.path()).unwrap();
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
        supported_cli_version: "0.147.0-alpha.6.5".into(),
        supported_schema_sha256: fingerprint,
    });

    let events = observer.observe().unwrap();

    assert_eq!(events.len(), 4);
    assert_eq!(events[0].kind, ActivityEventKind::RolloutStarted);
    assert_eq!(events[1].kind, ActivityEventKind::RolloutStarted);
    assert_eq!(events[2].kind, ActivityEventKind::TurnStopped);
    assert_eq!(events[3].kind, ActivityEventKind::TurnStopped);
    assert_eq!(before, inventory(temp.path()));
    let diagnostic = format!("{events:?}");
    for secret in ["task-1", "SECRET_PATH", "SECRET_TURN", "SECRET_RESPONSE"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn rollout_observation_fails_closed_for_version_schema_or_lifecycle_drift() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let connection = Connection::open(temp.path().join("state_5.sqlite")).unwrap();
    connection
        .execute("create table threads (id text)", [])
        .unwrap();
    drop(connection);
    let fingerprint = compute_state_schema_fingerprint(temp.path()).unwrap();
    let rollout = sessions.join("rollout-sanitized.jsonl");
    fs::write(
        &rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different-thread-id\",\"session_id\":\"task-1\",\"cli_version\":\"future\"}}\n",
    )
    .unwrap();
    let config = ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
        supported_cli_version: "0.147.0-alpha.6.5".into(),
        supported_schema_sha256: fingerprint.clone(),
    };
    assert!(ReadonlyRolloutObserver::new(config).observe().is_err());

    fs::write(
        &rollout,
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different-thread-id\",\"session_id\":\"task-1\",\"cli_version\":\"0.147.0-alpha.6.5\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_finished_in_a_new_way\",\"completed_at\":1786329990}}\n"
        ),
    )
    .unwrap();
    let unknown_lifecycle = ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
        supported_cli_version: "0.147.0-alpha.6.5".into(),
        supported_schema_sha256: fingerprint,
    };
    assert!(
        ReadonlyRolloutObserver::new(unknown_lifecycle)
            .observe()
            .is_err()
    );

    fs::write(
        &rollout,
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different-thread-id\",\"session_id\":\"task-1\",\"cli_version\":\"0.147.0-alpha.6.5\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"execution_started\",\"started_at\":1786329990}}\n"
        ),
    )
    .unwrap();
    let unknown_enum = ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
        supported_cli_version: "0.147.0-alpha.6.5".into(),
        supported_schema_sha256: compute_state_schema_fingerprint(temp.path()).unwrap(),
    };
    assert!(
        ReadonlyRolloutObserver::new(unknown_enum)
            .observe()
            .is_err()
    );

    let wrong_schema = ReadonlyObservationConfig {
        codex_home: temp.path().to_owned(),
        installation_salt: SALT.into(),
        supported_cli_version: "0.147.0-alpha.6.5".into(),
        supported_schema_sha256: "wrong".into(),
    };
    assert!(
        ReadonlyRolloutObserver::new(wrong_schema)
            .observe()
            .is_err()
    );
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
