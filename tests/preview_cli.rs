use std::io::Write;
use std::process::{Command, Stdio};
use std::{fs, os::unix::fs::PermissionsExt};

use codex_zectrix_dashboard::{
    DashboardConfig, ObservedDashboardState, ObservedQuota, ObservedQuotaWindow,
    TaskActivityAvailability, render_dashboard,
};

#[test]
fn preview_command_writes_a_400_by_300_monochrome_png() {
    let output_dir = tempfile::tempdir().unwrap();
    let first_path = output_dir.path().join("first.png");
    let second_path = output_dir.path().join("second.png");
    for output_path in [&first_path, &second_path] {
        let status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
            .args([
                "preview",
                "--input",
                "fixtures/sample-dashboard.json",
                "--output",
                output_path.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    assert_eq!(
        std::fs::read(&first_path).unwrap(),
        std::fs::read(&second_path).unwrap()
    );
    let image = image::open(first_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
}

#[test]
fn preview_command_accepts_english_locale() {
    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().join("english.png");
    let status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args([
            "preview",
            "--input",
            "fixtures/sample-dashboard.json",
            "--output",
            output_path.to_str().unwrap(),
            "--language",
            "en",
        ])
        .status()
        .unwrap();

    assert!(status.success());
    let image = image::open(output_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
}

#[test]
fn live_preview_reads_the_official_app_server_quota_and_writes_a_frame() {
    let output_dir = tempfile::tempdir().unwrap();
    let server = output_dir.path().join("fake-codex");
    let output_path = output_dir.path().join("live.png");
    let data_dir = output_dir.path().join("data");
    fs::write(
        &server,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/0.146.1 (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
read -r quota
printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":37,"windowDurationMins":300,"resetsAt":1786337200},"secondary":null},"rateLimitResetCredits":{"availableCount":0}}}'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let started_at = current_epoch_seconds();
    let status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args(["live-preview", "--output", output_path.to_str().unwrap()])
        .env("CODEX_ZECTRIX_CODEX_BIN", &server)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .status()
        .unwrap();
    let finished_at = current_epoch_seconds();

    assert!(status.success());
    let fresh_bytes = fs::read(&output_path).unwrap();
    let image = image::open(&output_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
    assert!((started_at..=finished_at).any(|render_time| {
        render_dashboard(
            ObservedDashboardState {
                quota: ObservedQuota {
                    windows: vec![ObservedQuotaWindow {
                        name: "5 小时".into(),
                        used_percent: 37,
                        resets_at_epoch_seconds: 1_786_337_200,
                    }],
                    reset_credits: 0,
                    stale: false,
                },
                task_activity_availability: TaskActivityAvailability::Unavailable,
                task_activity_stale: false,
                tasks: Vec::new(),
                prompt: None,
                response: None,
                reasoning: None,
                project_path: None,
                tool: None,
                error_text: None,
                plan: None,
            },
            render_time,
            DashboardConfig::default(),
        )
        .is_ok_and(|dashboard| {
            dashboard
                .frame
                .png_bytes()
                .is_ok_and(|bytes| bytes == fresh_bytes)
        })
    }));

    fs::write(&server, "#!/bin/sh\nexit 1\n").unwrap();
    let stale_status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args(["live-preview", "--output", output_path.to_str().unwrap()])
        .env("CODEX_ZECTRIX_CODEX_BIN", &server)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .status()
        .unwrap();

    assert!(stale_status.success());
    assert_ne!(fs::read(output_path).unwrap(), fresh_bytes);
    assert!(data_dir.join("quota.json").is_file());
}

fn current_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

#[test]
fn hook_record_command_persists_no_raw_hook_content() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .arg("hook-record")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"hook_event_name":"Stop","session_id":"task-1","status":"failed","prompt":"SECRET_PROMPT","response":"SECRET_RESPONSE","reasoning":"SECRET_REASONING","path":"SECRET_PATH","tool":"SECRET_TOOL","result":"SECRET_RESULT","plan":"SECRET_PLAN","error":"SECRET_ERROR"}"#,
        )
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let persisted = fs::read_to_string(temp.path().join("hook-events.jsonl")).unwrap();
    for secret in [
        "task-1",
        "SECRET_PROMPT",
        "SECRET_RESPONSE",
        "SECRET_REASONING",
        "SECRET_PATH",
        "SECRET_TOOL",
        "SECRET_RESULT",
        "SECRET_PLAN",
        "SECRET_ERROR",
    ] {
        assert!(!persisted.contains(secret));
    }
}
