use std::fs;
use std::os::unix::fs::PermissionsExt;

use codex_zectrix_dashboard::{AppServerClient, CorrelationKey};

const SUPPORTED_USER_AGENT: &str = "codex-zectrix-dashboard/0.146.1 (Mac OS 26.5; arm64) dumb";

#[test]
fn standalone_client_initializes_then_reads_quota_without_other_requests() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}}}'
read -r initialized
printf '%s\n' "$initialized" >> '{}'
read -r quota
printf '%s\n' "$quota" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"rateLimits":{{"primary":{{"usedPercent":37,"windowDurationMins":300,"resetsAt":1786337200}},"secondary":null}},"rateLimitResetCredits":{{"availableCount":0}},"account":{{"email":"SECRET_ACCOUNT_MARKER"}},"accessToken":"SECRET_TOKEN_MARKER"}}}}'
"#,
        requests.display(),
        requests.display(),
        requests.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let quota = AppServerClient::new(&server).read_quota().unwrap();

    assert_eq!(quota.windows.len(), 1);
    assert_eq!(quota.windows[0].used_percent, 37);
    let messages: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["method"], "initialize");
    assert_eq!(
        messages[0]["params"]["clientInfo"]["name"],
        "codex-zectrix-dashboard"
    );
    assert_eq!(messages[1]["method"], "initialized");
    assert_eq!(messages[2]["method"], "account/rateLimits/read");
    assert!(messages[2]["params"].is_null());
}

#[test]
fn unavailable_app_server_returns_an_error_without_synthesizing_quota() {
    let error = AppServerClient::new("/path/that/does/not/exist")
        .read_quota()
        .unwrap_err();

    assert!(error.to_string().contains("启动 Codex app-server"));
}

#[test]
fn unsupported_app_server_version_fails_closed_before_a_data_request() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    fs::write(
        &server,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/9.0.0 (future)"}}'
read -r initialized
read -r request
[ -z "$request" ]
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let error = AppServerClient::new(&server).read_quota().unwrap_err();

    assert!(error.to_string().contains("版本不受支持"));
}

#[test]
fn rpc_error_details_are_not_exposed_by_the_client_error() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    fs::write(
        &server,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"error":{"message":"SECRET_TOKEN_MARKER","accessToken":"SECRET_TOKEN_MARKER"}}'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let error = AppServerClient::new(&server).read_quota().unwrap_err();

    assert!(!error.to_string().contains("SECRET_TOKEN_MARKER"));
}

#[test]
fn standalone_client_reads_official_titles_without_rollout_scan_or_repair() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}"}}}}'
read -r initialized
printf '%s\n' "$initialized" >> '{}'
read -r threads
printf '%s\n' "$threads" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"data":[{{"id":"different-thread-id","sessionId":"task-1","name":"官方任务标题","parentThreadId":null,"source":"appServer","preview":"SECRET_PROMPT","cwd":"SECRET_PATH"}}],"nextCursor":null}}}}'
"#,
        requests.display(),
        requests.display(),
        requests.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let tasks = AppServerClient::new(&server)
        .read_task_metadata("test-installation")
        .unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "官方任务标题");
    assert_eq!(
        tasks[0].correlation,
        CorrelationKey::derive("different-thread-id", "test-installation")
    );
    let messages: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["method"], "thread/list");
    assert_eq!(messages[2]["params"]["useStateDbOnly"], true);
    assert_eq!(messages[2]["params"]["sortKey"], "updated_at");
    assert_eq!(messages[2]["params"]["sortDirection"], "desc");
    assert_eq!(messages[2]["params"]["archived"], false);
    assert!(
        messages[2]["params"]["sourceKinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "subAgent")
    );
    assert!(!format!("{tasks:?}").contains("SECRET_"));
}

#[test]
fn standalone_client_follows_every_read_only_task_metadata_page() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let counter = temp.path().join("counter");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}"}}}}'
read -r initialized
read -r threads
printf '%s\n' "$threads" >> '{}'
count=0
[ -f '{}' ] && count=$(cat '{}')
count=$((count + 1))
printf '%s' "$count" > '{}'
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{{"id":2,"result":{{"data":[{{"id":"thread-1","sessionId":"session-1","name":"第一页","parentThreadId":null,"source":"appServer"}}],"nextCursor":"cursor-2"}}}}'
else
  printf '%s\n' '{{"id":2,"result":{{"data":[{{"id":"thread-2","sessionId":"session-2","name":"第二页","parentThreadId":null,"source":"appServer"}}],"nextCursor":null}}}}'
fi
"#,
        requests.display(),
        counter.display(),
        counter.display(),
        counter.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let tasks = AppServerClient::new(&server)
        .read_task_metadata("test-installation")
        .unwrap();

    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "第一页");
    assert_eq!(tasks[1].title, "第二页");
    let requests: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["params"]["cursor"].is_null());
    assert_eq!(requests[1]["params"]["cursor"], "cursor-2");
    assert!(
        requests
            .iter()
            .all(|request| { request["params"]["useStateDbOnly"] == true })
    );
}
