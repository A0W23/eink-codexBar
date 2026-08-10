use std::fs;
use std::os::unix::fs::PermissionsExt;

use codex_zectrix_dashboard::AppServerClient;

#[test]
fn standalone_client_initializes_then_reads_quota_without_other_requests() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"fake","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}}}'
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
