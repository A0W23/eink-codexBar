use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use codex_zectrix_dashboard::{
    ActivityState, DashboardConfig, DashboardOutput, FramePublisher, ObservedDashboardState,
    ObservedQuota, ObservedQuotaWindow, ObservedTask, PublishAttempt, PublishCoordinator,
    PublisherState, ZectrixPublisher, normalize_dashboard, render_normalized_dashboard_with_sync,
};

fn observed(title: &str, state: ActivityState) -> ObservedDashboardState {
    ObservedDashboardState {
        quota: ObservedQuota {
            windows: vec![ObservedQuotaWindow {
                name: "5 小时".into(),
                used_percent: 37,
                resets_at_epoch_seconds: 10_000,
            }],
            reset_credits: 0,
            stale: false,
        },
        task_activity_stale: false,
        tasks: vec![ObservedTask::new(title, state, 1_000)],
        prompt: None,
        response: None,
        reasoning: None,
        project_path: None,
        tool: None,
        error_text: None,
        plan: None,
    }
}

#[derive(Debug)]
struct Request {
    head: String,
    body: Vec<u8>,
}

#[derive(Default)]
struct RecordingPublisher {
    fail_next: bool,
    visible_text: Vec<Vec<String>>,
}

impl FramePublisher for RecordingPublisher {
    type Error = ();

    fn publish(&mut self, dashboard: &DashboardOutput) -> Result<(), ()> {
        self.visible_text.push(dashboard.visible_text.clone());
        if self.fail_next {
            self.fail_next = false;
            Err(())
        } else {
            Ok(())
        }
    }
}

#[test]
fn visible_changes_coalesce_and_retries_keep_the_newest_state_behind_the_interval() {
    let mut coordinator =
        PublishCoordinator::new(DashboardConfig::default(), PublisherState::default());
    let mut publisher = RecordingPublisher::default();

    assert!(coordinator.observe(observed("初始", ActivityState::Running), 0));
    assert_eq!(
        coordinator.try_publish(0, &mut publisher).unwrap(),
        PublishAttempt::Published
    );

    let mut internal = observed("初始", ActivityState::Running);
    internal.prompt = Some("SECRET_PROMPT".into());
    assert!(!coordinator.observe(internal, 1));
    assert_eq!(
        coordinator.try_publish(1, &mut publisher).unwrap(),
        PublishAttempt::Idle
    );

    assert!(coordinator.observe(observed("较旧", ActivityState::Failed), 10));
    assert!(coordinator.observe(observed("最新", ActivityState::Interrupted), 20));
    assert_eq!(
        coordinator.try_publish(59, &mut publisher).unwrap(),
        PublishAttempt::Deferred {
            until_epoch_seconds: 60
        }
    );

    publisher.fail_next = true;
    assert_eq!(
        coordinator.try_publish(60, &mut publisher).unwrap(),
        PublishAttempt::Failed
    );
    assert!(coordinator.observe(observed("恢复", ActivityState::TurnCompleted), 61));
    assert_eq!(
        coordinator.try_publish(61, &mut publisher).unwrap(),
        PublishAttempt::Deferred {
            until_epoch_seconds: 120
        }
    );
    assert_eq!(
        coordinator.try_publish(120, &mut publisher).unwrap(),
        PublishAttempt::Published
    );

    assert_eq!(publisher.visible_text.len(), 3);
    assert!(publisher.visible_text[1].iter().any(|text| text == "最新"));
    assert!(publisher.visible_text[2].iter().any(|text| text == "恢复"));
    assert_eq!(
        coordinator.state().last_successful_sync_epoch_seconds,
        Some(120)
    );
}

#[test]
fn fake_zectrix_service_verifies_multipart_interval_retry_and_recovery() {
    let secret = "SECRET_API_KEY";
    let (base_url, requests) = fake_zectrix_service(secret);
    let mut publisher = ZectrixPublisher::new(secret, &base_url, "SECRET_DEVICE_ID", 3).unwrap();
    let mut coordinator = PublishCoordinator::new(
        DashboardConfig::default(),
        PublisherState {
            last_successful_sync_epoch_seconds: Some(0),
            ..PublisherState::default()
        },
    );

    coordinator.observe(observed("较旧", ActivityState::Failed), 10);
    coordinator.observe(observed("最新", ActivityState::Interrupted), 20);
    assert_eq!(
        coordinator.try_publish(59, &mut publisher).unwrap(),
        PublishAttempt::Deferred {
            until_epoch_seconds: 60
        }
    );
    assert_eq!(
        coordinator.try_publish(60, &mut publisher).unwrap(),
        PublishAttempt::Failed
    );
    coordinator.observe(observed("恢复", ActivityState::TurnCompleted), 61);
    assert_eq!(
        coordinator.try_publish(61, &mut publisher).unwrap(),
        PublishAttempt::Deferred {
            until_epoch_seconds: 120
        }
    );
    assert_eq!(
        coordinator.try_publish(120, &mut publisher).unwrap(),
        PublishAttempt::Published
    );
    assert!(!coordinator.observe(observed("恢复", ActivityState::TurnCompleted), 180));
    assert_eq!(
        coordinator.try_publish(180, &mut publisher).unwrap(),
        PublishAttempt::Idle
    );

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            request
                .head
                .starts_with("post /open/v1/devices/secret_device_id/display/image http/1.1")
        );
        assert!(
            request
                .head
                .contains("content-type: multipart/form-data; boundary=")
        );
        assert!(request.head.contains("x-api-key: secret_api_key"));
        assert!(contains(&request.body, b"name=\"images\""));
        assert!(contains(&request.body, b"name=\"dither\""));
        assert!(contains(&request.body, b"\r\n\r\nfalse\r\n"));
        assert!(contains(&request.body, b"name=\"pageId\""));
        assert!(contains(&request.body, b"\r\n\r\n3\r\n"));
        assert_eq!(
            image::load_from_memory(extract_png(&request.body))
                .unwrap()
                .width(),
            400
        );
    }
    assert_ne!(
        extract_png(&requests[0].body),
        extract_png(&requests[1].body)
    );
}

#[test]
fn reset_timestamp_jitter_is_invisible_until_the_displayed_countdown_changes() {
    let mut coordinator =
        PublishCoordinator::new(DashboardConfig::default(), PublisherState::default());
    let mut publisher = RecordingPublisher::default();
    coordinator.observe(observed("任务", ActivityState::Running), 0);
    assert_eq!(
        coordinator.try_publish(0, &mut publisher).unwrap(),
        PublishAttempt::Published
    );

    let mut jitter = observed("任务", ActivityState::Running);
    jitter.quota.windows[0].resets_at_epoch_seconds += 1;
    assert!(!coordinator.observe(jitter, 1));

    let mut changed = observed("任务", ActivityState::Running);
    changed.quota.windows[0].resets_at_epoch_seconds += 3_600;
    assert!(coordinator.observe(changed, 1));
}

#[test]
fn fake_zectrix_receives_no_request_for_a_byte_identical_render() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let state = observed("任务", ActivityState::Running);
    let normalized = normalize_dashboard(state.clone(), 100, &DashboardConfig::default());
    let rendered = render_normalized_dashboard_with_sync(
        normalized,
        100,
        DashboardConfig::default(),
        Some(100),
    )
    .unwrap();
    let mut coordinator = PublishCoordinator::new(
        DashboardConfig::default(),
        PublisherState {
            last_frame_hash: Some(rendered.frame.sha256),
            last_visible_state_hash: Some("different-visible-state".into()),
            ..PublisherState::default()
        },
    );
    coordinator.observe(state, 100);
    let mut publisher = ZectrixPublisher::new("key", base_url, "device", 3).unwrap();

    assert_eq!(
        coordinator.try_publish(100, &mut publisher).unwrap(),
        PublishAttempt::Unchanged
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn failed_cooldown_reservation_prevents_any_upload_attempt() {
    let mut coordinator =
        PublishCoordinator::new(DashboardConfig::default(), PublisherState::default());
    coordinator.observe(observed("任务", ActivityState::Running), 100);
    let mut publisher = RecordingPublisher::default();

    assert_eq!(
        coordinator
            .try_publish_with_reservation(100, &mut publisher, |_| false)
            .unwrap(),
        PublishAttempt::ReservationFailed
    );
    assert!(publisher.visible_text.is_empty());
}

fn fake_zectrix_service(secret: &str) -> (String, mpsc::Receiver<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected_secret = secret.to_ascii_lowercase();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut requests = Vec::new();
        for status in ["500 Internal Server Error", "200 OK"] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(
                request
                    .head
                    .contains(&format!("x-api-key: {expected_secret}"))
            );
            requests.push(request);
            let body = if status == "200 OK" {
                r#"{"code":0,"data":{"totalPages":1,"pushedPages":1,"pageId":"3"}}"#
            } else {
                r#"{"code":1}"#
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
