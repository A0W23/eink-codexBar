use std::fs;
use std::process::Command;

mod common;

#[test]
fn diagnostics_distinguish_source_freshness_without_exposing_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("source-status.json"),
        r#"{"quota":"current","taskActivity":"inferred","arbitrary":"SECRET_PAYLOAD"}"#,
    )
    .unwrap();

    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "quota_source=current\ntask_activity_source=inferred\n"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SECRET_PAYLOAD"));
}

#[test]
fn diagnostics_report_unavailable_before_any_observation() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path().join("missing"))
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "quota_source=unavailable\ntask_activity_source=unavailable\n"
    );
}
