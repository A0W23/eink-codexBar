use std::process::Command;
use std::{fs, os::unix::fs::PermissionsExt};

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
fn live_preview_reads_the_official_app_server_quota_and_writes_a_frame() {
    let output_dir = tempfile::tempdir().unwrap();
    let server = output_dir.path().join("fake-codex");
    let output_path = output_dir.path().join("live.png");
    fs::write(
        &server,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"fake","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
read -r quota
printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":37,"windowDurationMins":300,"resetsAt":1786337200},"secondary":null},"rateLimitResetCredits":{"availableCount":0}}}'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args(["live-preview", "--output", output_path.to_str().unwrap()])
        .env("CODEX_ZECTRIX_CODEX_BIN", &server)
        .status()
        .unwrap();

    assert!(status.success());
    let fresh_bytes = fs::read(&output_path).unwrap();
    let image = image::open(&output_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));

    fs::write(&server, "#!/bin/sh\nexit 1\n").unwrap();
    let stale_status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args(["live-preview", "--output", output_path.to_str().unwrap()])
        .env("CODEX_ZECTRIX_CODEX_BIN", &server)
        .status()
        .unwrap();

    assert!(stale_status.success());
    assert_ne!(fs::read(output_path).unwrap(), fresh_bytes);
    assert!(output_dir.path().join("live.quota.json").is_file());
}
