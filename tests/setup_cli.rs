use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

mod common;

#[derive(Debug)]
struct Request {
    head: String,
    body: Vec<u8>,
}

#[test]
fn setup_discovers_a_current_note4_previews_then_uploads_only_after_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let secret = format!(
        "zt_runtime_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (base_url, requests) = fake_zectrix_service(&secret, 2, "200 OK");
    let security = fake_security_command(temp.path());
    let codex = fake_codex_command(temp.path());
    let keychain_state = temp.path().join("keychain-state");

    let mut child = Command::new(common::dashboard_binary())
        .arg("setup")
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path().join("data"))
        .env("CODEX_ZECTRIX_SECURITY_BIN", &security)
        .env("CODEX_ZECTRIX_CODEX_BIN", &codex)
        .env("TEST_KEYCHAIN_SECRET", &secret)
        .env("TEST_KEYCHAIN_STATE", &keychain_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{secret}\n1\n3\n\n2\ny\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains(&secret));
    assert!(!stderr.contains(&secret));
    assert!(stdout.contains("标题将作为图像像素上传到 ZECTRIX Cloud"));
    assert!(stdout.contains("已生成待上传预览"));

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .head
            .starts_with("get /open/v1/devices http/1.1")
    );
    assert!(
        requests[1]
            .head
            .starts_with("post /open/v1/devices/aa:bb:cc:dd:ee:ff/display/image http/1.1")
    );
    for request in &requests {
        assert!(request.head.contains(&format!("x-api-key: {secret}")));
    }
    let upload = &requests[1];
    assert!(
        upload
            .head
            .contains("content-type: multipart/form-data; boundary=")
    );
    assert!(contains(&upload.body, b"name=\"images\""));
    assert!(contains(&upload.body, b"name=\"dither\""));
    assert!(contains(&upload.body, b"\r\n\r\nfalse\r\n"));
    assert!(contains(&upload.body, b"name=\"pageId\""));
    assert!(contains(&upload.body, b"\r\n\r\n3\r\n"));
    assert!(!contains(&upload.body, secret.as_bytes()));
    assert!(!upload.head.lines().next().unwrap().contains(&secret));
    let png = extract_png(&upload.body);
    assert!(png.len() <= 2 * 1024 * 1024);
    let image = image::load_from_memory(png).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));

    let settings_path = temp.path().join("data/settings.json");
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(settings["deviceId"], "AA:BB:CC:DD:EE:FF");
    assert_eq!(settings["pageId"], 3);
    assert_eq!(settings["privacyMode"], false);
    assert_eq!(settings["locale"], "en");
    assert!(settings.get("apiKey").is_none());
    assert!(keychain_state.is_file());
    assert_eq!(
        fs::read(temp.path().join("data/pending-preview.png")).unwrap(),
        png
    );

    for entry in walkdir::WalkDir::new(temp.path()) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            assert!(!contains(
                &fs::read(entry.path()).unwrap(),
                secret.as_bytes()
            ));
        }
    }
}

#[test]
fn cancelling_setup_keeps_the_device_unchanged_and_does_not_store_the_key() {
    let temp = tempfile::tempdir().unwrap();
    let secret = format!(
        "zt_runtime_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (base_url, requests) = fake_zectrix_service(&secret, 1, "200 OK");
    let keychain_state = temp.path().join("keychain-state");
    fs::create_dir_all(temp.path().join("data")).unwrap();
    let previous_settings = br#"{"deviceId":"AA:BB:CC:DD:EE:FF","pageId":4,"privacyMode":true}"#;
    fs::write(temp.path().join("data/settings.json"), previous_settings).unwrap();
    let output = run_setup(
        temp.path(),
        &base_url,
        &secret,
        &keychain_state,
        format!("{secret}\n1\n1\nn\n\nn\n"),
    );

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("已取消，未上传图像")
    );
    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .head
            .starts_with("get /open/v1/devices http/1.1")
    );
    assert_eq!(
        fs::read(temp.path().join("data/settings.json")).unwrap(),
        previous_settings
    );
    assert!(!keychain_state.exists());
}

#[test]
fn rerunning_setup_reuses_keychain_and_changes_page_and_privacy_mode() {
    let temp = tempfile::tempdir().unwrap();
    let secret = format!(
        "zt_runtime_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let keychain_state = temp.path().join("keychain-state");
    fs::write(&keychain_state, []).unwrap();
    let (base_url, requests) = fake_zectrix_service(&secret, 2, "200 OK");
    let output = run_setup(
        temp.path(),
        &base_url,
        &secret,
        &keychain_state,
        "\n1\n5\ny\n2\ny\n".into(),
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("使用 macOS 钥匙串中的现有 API Key"));
    assert!(!stdout.contains("请输入 ZECTRIX API Key"));
    let requests = requests.recv().unwrap();
    assert!(contains(&requests[1].body, b"\r\n\r\n5\r\n"));
    assert!(
        requests[1]
            .head
            .starts_with("post /open/v1/devices/aa:bb:cc:dd:ee:ff/display/image http/1.1")
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("data/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["pageId"], 5);
    assert_eq!(settings["privacyMode"], true);
    assert_eq!(settings["locale"], "en");
}

#[test]
fn failed_upload_restores_the_previous_settings() {
    let temp = tempfile::tempdir().unwrap();
    let secret = format!(
        "zt_runtime_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let keychain_state = temp.path().join("keychain-state");
    fs::write(&keychain_state, []).unwrap();
    fs::create_dir_all(temp.path().join("data")).unwrap();
    let previous_settings = br#"{"deviceId":"AA:BB:CC:DD:EE:FF","pageId":2,"privacyMode":false}"#;
    fs::write(temp.path().join("data/settings.json"), previous_settings).unwrap();
    let (base_url, requests) = fake_zectrix_service(&secret, 2, "400 Bad Request");
    let output = run_setup(
        temp.path(),
        &base_url,
        &secret,
        &keychain_state,
        "\n1\n5\n\n\ny\n".into(),
    );

    assert!(!output.status.success());
    assert_eq!(requests.recv().unwrap().len(), 2);
    assert_eq!(
        fs::read(temp.path().join("data/settings.json")).unwrap(),
        previous_settings
    );
}

#[test]
fn server_error_reports_an_unknown_result_and_keeps_the_selected_settings() {
    let temp = tempfile::tempdir().unwrap();
    let secret = format!(
        "zt_runtime_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let keychain_state = temp.path().join("keychain-state");
    fs::write(&keychain_state, []).unwrap();
    let (base_url, requests) = fake_zectrix_service(&secret, 2, "500 Internal Server Error");
    let output = run_setup(
        temp.path(),
        &base_url,
        &secret,
        &keychain_state,
        "\n1\n5\n\n\ny\n".into(),
    );

    assert!(!output.status.success());
    assert_eq!(requests.recv().unwrap().len(), 2);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("未确认上传结果")
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("data/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["pageId"], 5);
}

fn run_setup(
    temp: &std::path::Path,
    base_url: &str,
    secret: &str,
    keychain_state: &std::path::Path,
    input: String,
) -> std::process::Output {
    let security = fake_security_command(temp);
    let codex = fake_codex_command(temp);
    let mut child = Command::new(common::dashboard_binary())
        .arg("setup")
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", temp.join("data"))
        .env("CODEX_ZECTRIX_SECURITY_BIN", security)
        .env("CODEX_ZECTRIX_CODEX_BIN", codex)
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_KEYCHAIN_STATE", keychain_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn fake_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/0.146.1 (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
read -r quota
printf '%s\n' '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":37,"windowDurationMins":300,"resetsAt":1786337200},"secondary":null},"rateLimitResetCredits":{"availableCount":0}}}'
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
        "#!/bin/sh\nif [ \"$1\" = find-generic-password ]; then [ -f \"$TEST_KEYCHAIN_STATE\" ] || exit 44; printf '%s' \"$TEST_KEYCHAIN_SECRET\"; exit 0; fi\nif [ \"$1\" = add-generic-password ]; then IFS= read -r supplied; [ \"$supplied\" = \"$TEST_KEYCHAIN_SECRET\" ] || exit 3; : > \"$TEST_KEYCHAIN_STATE\"; exit 0; fi\nexit 2\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_zectrix_service(
    secret: &str,
    request_count: usize,
    upload_status: &'static str,
) -> (String, mpsc::Receiver<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected_secret = secret.to_owned();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(
                request
                    .head
                    .contains(&format!("x-api-key: {expected_secret}"))
            );
            requests.push(request);
            let (status, body) = if index == 0 {
                (
                    "200 OK",
                    r#"{"code":0,"data":[{"deviceId":"11:22:33:44:55:66","alias":"Other display","board":"other-board"},{"deviceId":"AA:BB:CC:DD:EE:FF","alias":"Desk NOTE4","board":"zectrix-s3-epaper-4.2"}]}"#,
                )
            } else if upload_status == "200 OK" {
                (
                    upload_status,
                    r#"{"code":0,"data":{"totalPages":1,"pushedPages":1,"pageId":"3"}}"#,
                )
            } else {
                (upload_status, r#"{"code":1}"#)
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut impl Read) -> Request {
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
        .map(|value| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
    Request {
        head,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
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
