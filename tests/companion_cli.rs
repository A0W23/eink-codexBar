use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::mpsc;
use std::thread;

#[test]
fn companion_combines_cached_activity_with_live_quota_and_publishes_content_free() {
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
    let output = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
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
    let image = image::load_from_memory(png).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(data_dir.join("publisher-state.json").is_file());

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

fn fake_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"fake","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
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
