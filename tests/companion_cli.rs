use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    ActivityEvent, ActivityEventKind, ActivityState, CorrelationKey, DashboardConfig,
    ObservedDashboardState, ObservedQuota, ObservedQuotaWindow, ObservedTask,
    TaskActivityAvailability, normalize_dashboard, render_normalized_dashboard_with_sync,
};

mod common;

#[test]
fn companion_publishes_current_quota_with_a_compatibility_frame_instead_of_cached_activity() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    fs::write(
        data_dir.join("activity.json"),
        r#"[{"title":"SECRET_TASK_TITLE","state":"running","activity_at_epoch_seconds":4102444800}]"#,
    )
    .unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let codex_log = temp.path().join("codex.log");
    let codex = fake_codex_command(temp.path());
    let security = fake_security_command(temp.path());
    let started_at = current_epoch_seconds();
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env("CODEX_ZECTRIX_CODEX_BIN", &codex)
        .env("CODEX_ZECTRIX_SECURITY_BIN", &security)
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();
    let finished_at = current_epoch_seconds();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = request.recv().unwrap();
    assert!(
        request
            .0
            .contains("content-type: multipart/form-data; boundary=")
    );
    let png = extract_png(&request.1);
    assert!(!contains(&request.1, secret.as_bytes()));
    assert!(!request.0.lines().next().unwrap().contains(secret));
    let image = image::load_from_memory(png).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    let expected_frame = (started_at..=finished_at).any(|render_time| {
        let observed = ObservedDashboardState {
            quota: ObservedQuota {
                windows: vec![ObservedQuotaWindow {
                    name: "5 小时".into(),
                    used_percent: 37,
                    resets_at_epoch_seconds: 4_102_444_800,
                }],
                reset_credits: 0,
                stale: false,
            },
            task_activity_availability: TaskActivityAvailability::Unavailable,
            task_activity_stale: false,
            tasks: vec![ObservedTask::new(
                "SECRET_TASK_TITLE",
                ActivityState::Running,
                4_102_444_800,
            )],
            prompt: None,
            response: None,
            reasoning: None,
            project_path: None,
            tool: None,
            error_text: None,
            plan: None,
        };
        let normalized = normalize_dashboard(observed, render_time, &DashboardConfig::default());
        render_normalized_dashboard_with_sync(
            normalized,
            render_time,
            DashboardConfig::default(),
            Some(render_time),
        )
        .is_ok_and(|dashboard| dashboard.frame.png_bytes().is_ok_and(|bytes| bytes == png))
    });
    assert!(
        expected_frame,
        "published PNG was not the compatibility frame"
    );
    assert!(data_dir.join("publisher-state.json").is_file());
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "unavailable");

    let codex_requests = fs::read_to_string(codex_log).unwrap();
    assert!(codex_requests.contains("account/rateLimits/read"));
    assert!(codex_requests.contains("thread/list"));
    for forbidden in [
        "turn/start",
        "turn/interrupt",
        "thread/archive",
        "review/start",
    ] {
        assert!(!codex_requests.contains(forbidden));
    }
    let diagnostics = [output.stdout, output.stderr].concat();
    for secret in [
        "SECRET_API_KEY",
        "SECRET_DEVICE_ID",
        "SECRET_TASK_TITLE",
        "SECRET_PROMPT",
        "SECRET_PATH",
    ] {
        assert!(!contains(&diagnostics, secret.as_bytes()));
    }
}

#[test]
fn companion_keeps_verified_hook_activity_when_rollout_storage_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    let now = current_epoch_seconds();
    let hook = ActivityEvent {
        correlation: CorrelationKey::derive("session-1", "codex-zectrix-dashboard-v1"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: now - now.rem_euclid(60),
    };
    fs::write(
        data_dir.join("hook-events.jsonl"),
        format!("{}\n", serde_json::to_string(&hook).unwrap()),
    )
    .unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = request.recv().unwrap();
    let png = extract_png(&request.1);
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "inferred");

    let expected = ObservedDashboardState {
        quota: ObservedQuota {
            windows: vec![ObservedQuotaWindow {
                name: "5 小时".into(),
                used_percent: 37,
                resets_at_epoch_seconds: 4_102_444_800,
            }],
            reset_credits: 0,
            stale: false,
        },
        task_activity_availability: TaskActivityAvailability::Available,
        task_activity_stale: false,
        tasks: vec![ObservedTask::new(
            "HOOK_TASK",
            ActivityState::Running,
            now - now.rem_euclid(60),
        )],
        prompt: None,
        response: None,
        reasoning: None,
        project_path: None,
        tool: None,
        error_text: None,
        plan: None,
    };
    let normalized = normalize_dashboard(expected, now, &DashboardConfig::default());
    assert!(
        render_normalized_dashboard_with_sync(
            normalized,
            now,
            DashboardConfig::default(),
            Some(now),
        )
        .unwrap()
        .frame
        .png_bytes()
        .is_ok_and(|bytes| bytes == png)
    );
}

#[test]
fn companion_does_not_treat_an_empty_hook_file_as_verified_activity() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    fs::write(data_dir.join("hook-events.jsonl"), []).unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(output.status.success());
    request.recv().unwrap();
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "unavailable");
}

fn current_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

#[test]
fn companion_reads_keychain_only_once_across_repeated_publish_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    fs::write(
        data_dir.join("activity.json"),
        r#"[{"title":"SECRET_TASK_TITLE","state":"running","activity_at_epoch_seconds":4102444800}]"#,
    )
    .unwrap();
    let keychain_log = temp.path().join("keychain.log");
    let security = fake_missing_security_command(temp.path());
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_API_BASE", "http://127.0.0.1:1")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env("CODEX_ZECTRIX_CODEX_BIN", fake_codex_command(temp.path()))
        .env("CODEX_ZECTRIX_SECURITY_BIN", security)
        .env("TEST_KEYCHAIN_LOG", &keychain_log)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "3")
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "0")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(fs::read_to_string(keychain_log).unwrap().lines().count(), 1);
}

fn fake_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/0.146.1 (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
printf '%s\n' "$initialized" >> "$TEST_CODEX_LOG"
read -r request
printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
case "$request" in
  *account/rateLimits/read*) printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":37,"windowDurationMins":300,"resetsAt":4102444800},"secondary":null},"rateLimitResetCredits":{"availableCount":0}}}' ;;
  *) printf '%s\n' '{"id":2,"error":{"code":-1,"message":"SECRET_PROMPT SECRET_PATH"}}' ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_codex_with_task_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex-with-task");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
printf '%s\n' "$initialized" >> "$TEST_CODEX_LOG"
read -r request
printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
case "$request" in
  *account/rateLimits/read*) printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":37,"windowDurationMins":300,"resetsAt":4102444800},"secondary":null},"rateLimitResetCredits":{"availableCount":0}}}' ;;
  *thread/list*) printf '%s\n' '{"id":2,"result":{"data":[{"id":"task-1","sessionId":"session-1","name":"HOOK_TASK","parentThreadId":null,"source":"appServer"}],"nextCursor":null}}' ;;
  *) printf '%s\n' '{"id":2,"error":{"code":-1,"message":"unsupported"}}' ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_security_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("security");
    fs::write(
        &path,
        "#!/bin/sh\n[ \"$1\" = find-generic-password ] || exit 2\nprintf '%s' \"$TEST_KEYCHAIN_SECRET\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_missing_security_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("missing-security");
    fs::write(
        &path,
        "#!/bin/sh\nprintf 'read\\n' >> \"$TEST_KEYCHAIN_LOG\"\nexit 44\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_zectrix_service(secret: &str) -> (String, mpsc::Receiver<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected_secret = secret.to_ascii_lowercase();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.0.contains(&format!("x-api-key: {expected_secret}")));
        let body = r#"{"code":0}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        sender.send(request).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut impl Read) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec())
        .unwrap()
        .to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap();
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
    (
        head,
        bytes[header_end..header_end + content_length].to_vec(),
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn extract_png(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .windows(8)
        .position(|window| window == b"\x89PNG\r\n\x1a\n")
        .unwrap();
    let iend = bytes[start..]
        .windows(4)
        .position(|window| window == b"IEND")
        .unwrap()
        + start;
    &bytes[start..iend + 8]
}
