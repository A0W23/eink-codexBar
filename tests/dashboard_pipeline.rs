use codex_zectrix_dashboard::{
    ActivityState, DashboardConfig, ObservedDashboardState, ObservedQuotaWindow, ObservedTask,
    PublishDecision, normalize_dashboard, render_dashboard, render_normalized_dashboard,
};

fn sample_state() -> ObservedDashboardState {
    ObservedDashboardState {
        quota: ObservedQuotaWindow {
            name: "5 小时".into(),
            used_percent: 37,
            resets_at_epoch_seconds: 1_786_337_200,
        },
        tasks: vec![
            ObservedTask::new("生成本地看板", ActivityState::Running, 1_786_330_000),
            ObservedTask::new("修复配额布局", ActivityState::TurnCompleted, 1_786_329_000),
            ObservedTask::new("检查隐私模式", ActivityState::Failed, 1_786_328_000),
        ],
        prompt: Some("SECRET_PROMPT_MARKER".into()),
        response: Some("SECRET_RESPONSE_MARKER".into()),
        reasoning: Some("SECRET_REASONING_MARKER".into()),
        project_path: Some("SECRET_PROJECT_PATH_MARKER".into()),
        tool: Some("SECRET_TOOL_MARKER".into()),
        error_text: Some("SECRET_ERROR_MARKER".into()),
        plan: Some("SECRET_PLAN_MARKER".into()),
    }
}

#[test]
fn renders_a_400_by_300_monochrome_frame_and_requests_first_publish() {
    let config = DashboardConfig::default();
    let normalized = normalize_dashboard(sample_state(), 1_786_330_000, &config);
    let output = render_normalized_dashboard(normalized, 1_786_330_000, config).unwrap();

    assert_eq!((output.frame.width, output.frame.height), (400, 300));
    assert_eq!(output.frame.pixels.len(), 400 * 300);
    assert!(
        output
            .frame
            .pixels
            .iter()
            .all(|pixel| matches!(pixel, 0 | 255))
    );
    assert!(output.visible_text.iter().any(|text| text == "重置 2 小时"));
    assert_eq!(output.publish_decision, PublishDecision::Publish);
}

#[test]
fn sample_state_has_a_stable_hash_and_suppresses_an_identical_frame() {
    let first =
        render_dashboard(sample_state(), 1_786_330_000, DashboardConfig::default()).unwrap();
    assert_eq!(
        first.frame.sha256,
        "8a41309581b627c8c2a0f1fb4c9ecc1cf203b1cc2c9b66da54629ad6586772fd"
    );

    let second = render_dashboard(
        sample_state(),
        1_786_330_000,
        DashboardConfig {
            previous_frame_hash: Some(first.frame.sha256.clone()),
            ..DashboardConfig::default()
        },
    )
    .unwrap();

    assert_eq!(second.frame.pixels, first.frame.pixels);
    assert_eq!(second.publish_decision, PublishDecision::Unchanged);
}

#[test]
fn titles_are_visible_by_default_and_hidden_in_privacy_mode() {
    let visible =
        render_dashboard(sample_state(), 1_786_330_000, DashboardConfig::default()).unwrap();
    assert!(
        visible
            .visible_text
            .iter()
            .any(|text| text == "生成本地看板")
    );
    assert_eq!(
        visible.normalized.tasks[0].title.as_deref(),
        Some("生成本地看板")
    );

    let hidden = render_dashboard(
        sample_state(),
        1_786_330_000,
        DashboardConfig {
            privacy_mode: true,
            previous_frame_hash: None,
        },
    )
    .unwrap();
    assert!(
        hidden
            .normalized
            .tasks
            .iter()
            .all(|task| task.title.is_none())
    );
    assert!(hidden.visible_text.iter().any(|text| text == "隐私任务"));
    assert!(
        !hidden
            .visible_text
            .iter()
            .any(|text| text == "生成本地看板")
    );
    let title_region = 150 * 400 + 100..180 * 400 + 380;
    assert_ne!(
        &visible.frame.pixels[title_region.clone()],
        &hidden.frame.pixels[title_region]
    );
}

#[test]
fn content_bearing_fields_never_enter_normalized_or_rendered_content() {
    let output =
        render_dashboard(sample_state(), 1_786_330_000, DashboardConfig::default()).unwrap();
    let observable = format!(
        "{}\n{}",
        serde_json::to_string(&output.normalized).unwrap(),
        output.visible_text.join("\n")
    );

    for marker in [
        "SECRET_PROMPT_MARKER",
        "SECRET_RESPONSE_MARKER",
        "SECRET_REASONING_MARKER",
        "SECRET_PROJECT_PATH_MARKER",
        "SECRET_TOOL_MARKER",
        "SECRET_ERROR_MARKER",
        "SECRET_PLAN_MARKER",
    ] {
        assert!(!observable.contains(marker), "leaked {marker}");
    }

    let mut other_secrets = sample_state();
    other_secrets.prompt = Some("DIFFERENT_PROMPT".into());
    other_secrets.response = Some("DIFFERENT_RESPONSE".into());
    other_secrets.reasoning = Some("DIFFERENT_REASONING".into());
    other_secrets.project_path = Some("DIFFERENT_PATH".into());
    other_secrets.tool = Some("DIFFERENT_TOOL".into());
    other_secrets.error_text = Some("DIFFERENT_ERROR".into());
    other_secrets.plan = Some("DIFFERENT_PLAN".into());
    let rerendered =
        render_dashboard(other_secrets, 1_786_330_000, DashboardConfig::default()).unwrap();
    assert_eq!(rerendered.normalized, output.normalized);
    assert_eq!(rerendered.frame.pixels, output.frame.pixels);
}

#[test]
fn normalization_expires_old_ended_activity_and_reports_hidden_eligible_tasks() {
    let mut observed = sample_state();
    observed.tasks.push(ObservedTask::new(
        "额外任务",
        ActivityState::Interrupted,
        1_786_327_000,
    ));
    observed.tasks.push(ObservedTask::new(
        "过期任务",
        ActivityState::TurnCompleted,
        1_786_330_000 - 24 * 60 * 60 - 1,
    ));

    let normalized = normalize_dashboard(observed, 1_786_330_000, &DashboardConfig::default());

    assert_eq!(normalized.tasks.len(), 3);
    assert_eq!(normalized.hidden_task_count, 1);
    assert!(
        normalized
            .tasks
            .iter()
            .all(|task| task.title.as_deref() != Some("过期任务"))
    );
    let rendered =
        render_normalized_dashboard(normalized, 1_786_330_000, DashboardConfig::default()).unwrap();
    assert!(rendered.visible_text.iter().any(|text| text == "另有 1 项"));
}
