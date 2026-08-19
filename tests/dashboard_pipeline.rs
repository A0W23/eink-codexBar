use codex_zectrix_dashboard::{
    ActivityState, DashboardConfig, DisplayLocale, ObservedDashboardState, ObservedQuota,
    ObservedQuotaWindow, ObservedTask, PublishDecision, QuotaCache, TaskActivityAvailability,
    normalize_dashboard, parse_app_server_quota, render_dashboard, render_normalized_dashboard,
    render_normalized_dashboard_with_sync,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct TaskActivityFixture {
    now: i64,
    selection: SelectionFixture,
}

#[derive(Deserialize)]
struct SelectionFixture {
    tasks: Vec<ObservedTask>,
    expected_titles: Vec<String>,
    expected_states: Vec<ActivityState>,
    hidden_task_count: usize,
}

fn sample_state() -> ObservedDashboardState {
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
        task_activity_availability: TaskActivityAvailability::Available,
        task_activity_stale: false,
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
fn unavailable_task_activity_replaces_old_rows_without_changing_current_quota() {
    let mut state = sample_state();
    state.task_activity_availability = TaskActivityAvailability::Unavailable;

    let output = render_dashboard(state, 1_786_330_000, DashboardConfig::default()).unwrap();

    assert_eq!((output.frame.width, output.frame.height), (400, 300));
    assert!(
        output
            .frame
            .pixels
            .iter()
            .all(|pixel| matches!(pixel, 0 | 255))
    );
    assert_eq!(output.normalized.quota.windows[0].used_percent, 37);
    assert!(!output.normalized.quota.stale);
    assert_eq!(
        output.normalized.task_activity_availability,
        TaskActivityAvailability::Unavailable
    );
    assert!(output.normalized.tasks.is_empty());
    assert_eq!(output.normalized.hidden_task_count, 0);
    for expected in [
        "63%",
        "已用 37%",
        "重置 2小时 0分",
        "状态暂不可用",
        "请检查插件兼容性",
    ] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }
    for old_claim in ["生成本地看板", "执行中", "修复配额布局", "本轮完成"] {
        assert!(!output.visible_text.iter().any(|text| text == old_claim));
    }
}

#[test]
fn stale_task_activity_is_visible_without_changing_turn_semantics() {
    let mut state = sample_state();
    state.task_activity_stale = true;

    let output = render_dashboard(state, 1_786_330_000, DashboardConfig::default()).unwrap();

    assert!(output.normalized.task_activity_stale);
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "任务数据可能已过期")
    );
    assert!(output.visible_text.iter().any(|text| text == "本轮完成"));
}

fn state_with_quota(quota: ObservedQuota) -> ObservedDashboardState {
    ObservedDashboardState {
        quota,
        task_activity_availability: TaskActivityAvailability::Available,
        task_activity_stale: false,
        tasks: sample_state().tasks,
        prompt: None,
        response: None,
        reasoning: None,
        project_path: None,
        tool: None,
        error_text: None,
        plan: None,
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
    assert_eq!(output.frame.pixels[0], 0);
    assert_eq!(output.frame.pixels[133 * 400], 255);
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "重置 2小时 0分")
    );
    assert_eq!(output.publish_decision, PublishDecision::Publish);
}

#[test]
fn quota_reset_time_keeps_days_hours_and_minutes() {
    let now = 1_786_330_000;
    let mut state = sample_state();
    state.quota.windows[0].resets_at_epoch_seconds = now + 443_880;

    let output = render_dashboard(state, now, DashboardConfig::default()).unwrap();

    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "重置 5天 3小时 18分")
    );
}

#[test]
fn rendered_page_shows_the_successful_sync_time_without_a_ticking_clock() {
    let normalized =
        normalize_dashboard(sample_state(), 1_786_330_000, &DashboardConfig::default());

    let first = render_normalized_dashboard_with_sync(
        normalized.clone(),
        1_786_330_000,
        DashboardConfig::default(),
        Some(1_786_330_000),
    )
    .unwrap();
    assert!(
        first
            .visible_text
            .iter()
            .any(|text| text.starts_with("上次同步 "))
    );
    assert_eq!(first.normalized, normalized);
}

#[test]
fn task_list_uses_the_section_without_a_heading_or_empty_footer_rule() {
    let output =
        render_dashboard(sample_state(), 1_786_330_000, DashboardConfig::default()).unwrap();

    assert!(!output.visible_text.iter().any(|text| text == "任务动态"));
    assert!((14..386).all(|x| output.frame.pixels[272 * 400 + x] == 255));
}

#[test]
fn sample_state_has_a_stable_hash_and_suppresses_an_identical_frame() {
    let first =
        render_dashboard(sample_state(), 1_786_330_000, DashboardConfig::default()).unwrap();
    assert_eq!(
        first.frame.sha256,
        "049655963fe3b8bd7d842ac418aa801b6769c6d8033b71c0ec41a12498ad5ff5"
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
            ..DashboardConfig::default()
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
fn english_locale_translates_dashboard_chrome_without_changing_task_titles() {
    let output = render_dashboard(
        sample_state(),
        1_786_330_000,
        DashboardConfig {
            locale: DisplayLocale::English,
            ..DashboardConfig::default()
        },
    )
    .unwrap();

    for expected in [
        "5 hours",
        "Used 37%",
        "Resets 2h 0m",
        "You're in good shape.",
        "Running",
        "Failed",
        "Task completed",
        "生成本地看板",
    ] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }
    for chinese_chrome in ["已用 37%", "重置 2小时 0分", "执行中", "失败", "本轮完成"]
    {
        assert!(
            !output
                .visible_text
                .iter()
                .any(|text| text == chinese_chrome),
            "unexpected Chinese dashboard label {chinese_chrome}"
        );
    }
}

#[test]
fn english_locale_covers_two_windows_privacy_and_compatibility_states() {
    let config = DashboardConfig {
        locale: DisplayLocale::English,
        privacy_mode: true,
        ..DashboardConfig::default()
    };
    let quota = parse_app_server_quota(include_str!("../fixtures/quota-two-windows.json")).unwrap();
    let output = render_dashboard(
        state_with_quota(quota),
        1_786_330_000,
        config.clone(),
    )
    .unwrap();

    for expected in ["5 hours", "7 days", "Reset credits 2", "Private task"] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }

    let mut unavailable = sample_state();
    unavailable.task_activity_availability = TaskActivityAvailability::Unavailable;
    let output = render_dashboard(unavailable, 1_786_330_000, config).unwrap();
    for expected in ["Status unavailable", "Check plugin compatibility"] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn long_task_titles_stop_before_the_right_display_edge() {
    let mut state = sample_state();
    let title = "研究如何让墨水屏上的超长任务标题保持清晰且不会越过右侧边界";
    state.tasks[0].title = title.into();

    let output = render_dashboard(state, 1_786_330_000, DashboardConfig::default()).unwrap();

    assert!(output.visible_text.iter().any(|text| text == title));
    for y in 170..192 {
        assert!(
            (387..400).all(|x| output.frame.pixels[y * 400 + x] == 255),
            "title crossed the right safe edge at row {y}"
        );
    }
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

#[test]
fn task_selection_uses_state_priority_recency_and_preserves_recency_ties() {
    let fixture: TaskActivityFixture =
        serde_json::from_str(include_str!("../fixtures/task-activity-cases.json")).unwrap();
    let mut observed = sample_state();
    observed.tasks = fixture.selection.tasks;

    let visible = normalize_dashboard(observed.clone(), fixture.now, &DashboardConfig::default());
    let hidden = normalize_dashboard(
        observed,
        fixture.now,
        &DashboardConfig {
            privacy_mode: true,
            previous_frame_hash: None,
            ..DashboardConfig::default()
        },
    );

    assert_eq!(
        visible
            .tasks
            .iter()
            .map(|task| (task.title.as_deref(), task.state))
            .collect::<Vec<_>>(),
        fixture
            .selection
            .expected_titles
            .iter()
            .zip(fixture.selection.expected_states.iter().copied())
            .map(|(title, state)| (Some(title.as_str()), state))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        visible.hidden_task_count,
        fixture.selection.hidden_task_count
    );
    assert_eq!(
        hidden
            .tasks
            .iter()
            .map(|task| task.state)
            .collect::<Vec<_>>(),
        visible
            .tasks
            .iter()
            .map(|task| task.state)
            .collect::<Vec<_>>()
    );
    assert_eq!(hidden.hidden_task_count, visible.hidden_task_count);
    assert!(hidden.tasks.iter().all(|task| task.title.is_none()));
}

#[test]
fn one_window_fixture_uses_the_full_quota_area_and_omits_zero_reset_credits() {
    let quota = parse_app_server_quota(include_str!("../fixtures/quota-one-window.json")).unwrap();
    let sanitized = serde_json::to_string(&quota).unwrap();
    assert!(!sanitized.contains("SECRET_ACCOUNT_MARKER"));
    assert!(!sanitized.contains("SECRET_TOKEN_MARKER"));
    let output = render_dashboard(
        state_with_quota(quota),
        1_786_330_000,
        DashboardConfig::default(),
    )
    .unwrap();

    assert_eq!(output.normalized.quota.windows.len(), 1);
    assert!(output.visible_text.iter().any(|text| text == "63%"));
    assert!(output.visible_text.iter().any(|text| text == "已用 37%"));
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "重置 2小时 0分")
    );
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "还能蹬，别急着坐下。")
    );
    assert!(
        !output
            .visible_text
            .iter()
            .any(|text| text.contains("重置额度"))
    );
}

#[test]
fn quota_message_tracks_the_lowest_remaining_window() {
    for (used_percent, expected) in [
        (10, "站起来蹬！"),
        (30, "还能蹬，别急着坐下。"),
        (60, "悠着点蹬，链条开始响了。"),
        (80, "省着点，车快散架了。"),
        (95, "就等Tibo重置了。"),
    ] {
        let mut state = sample_state();
        state.quota.windows[0].used_percent = used_percent;
        let output = render_dashboard(state, 1_786_330_000, DashboardConfig::default()).unwrap();
        assert!(output.visible_text.iter().any(|text| text == expected));
    }
}

#[test]
fn two_window_fixture_renders_both_windows_and_positive_reset_credits() {
    let quota = parse_app_server_quota(include_str!("../fixtures/quota-two-windows.json")).unwrap();
    let output = render_dashboard(
        state_with_quota(quota),
        1_786_330_000,
        DashboardConfig::default(),
    )
    .unwrap();

    assert_eq!(output.normalized.quota.windows.len(), 2);
    for expected in ["5 小时", "7 天", "63%", "79%", "重置额度 2"] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }

    let one_window = render_dashboard(
        state_with_quota(
            parse_app_server_quota(include_str!("../fixtures/quota-one-window.json")).unwrap(),
        ),
        1_786_330_000,
        DashboardConfig::default(),
    )
    .unwrap();
    let center_of_single_bar = 98 * 400 + 200;
    assert_eq!(one_window.frame.pixels[center_of_single_bar], 255);
    assert_eq!(output.frame.pixels[center_of_single_bar], 0);
}

#[test]
fn malformed_or_unknown_quota_preserves_the_last_known_values_and_marks_them_stale() {
    let mut cache = QuotaCache::default();
    let current = cache
        .update(parse_app_server_quota(include_str!(
            "../fixtures/quota-one-window.json"
        )))
        .unwrap();
    assert!(!current.stale);
    let stale = cache
        .update(parse_app_server_quota(include_str!(
            "../fixtures/quota-unknown-schema.json"
        )))
        .unwrap();
    let output = render_dashboard(
        state_with_quota(stale),
        1_786_330_000,
        DashboardConfig::default(),
    )
    .unwrap();

    assert_eq!(output.normalized.quota.windows[0].used_percent, 37);
    assert!(output.normalized.quota.stale);
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "数据可能已过期")
    );
    assert!(
        QuotaCache::default()
            .update(parse_app_server_quota(include_str!(
                "../fixtures/quota-unknown-schema.json"
            )))
            .is_err()
    );
    assert!(
        parse_app_server_quota(
            r#"{"rateLimits":{"primary":{"usedPercent":-1,"windowDurationMins":300,"resetsAt":1786337200}}}"#
        )
        .is_err()
    );
}
