use codex_zectrix_dashboard::{
    ActivityEvent, ActivityEventKind, ActivityState, CorrelationKey, OfficialTaskMetadata,
    TaskActivityCache, TaskActivitySnapshot, reduce_task_activity,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct TaskActivityFixture {
    now: i64,
    reductions: Vec<ReductionFixture>,
}

#[derive(Deserialize)]
struct ReductionFixture {
    name: String,
    events: Vec<EventFixture>,
    expected_state: Option<ActivityState>,
}

#[derive(Deserialize)]
struct EventFixture {
    kind: ActivityEventKind,
    age_seconds: i64,
}

const NOW: i64 = 1_786_330_000;

fn task(id: &str, title: &str) -> OfficialTaskMetadata {
    OfficialTaskMetadata {
        correlation: CorrelationKey::derive(id, "test-installation"),
        correlation_aliases: Vec::new(),
        title: title.into(),
        parent_correlation: None,
    }
}

fn event(id: &str, kind: ActivityEventKind, observed_at: i64) -> ActivityEvent {
    ActivityEvent {
        correlation: CorrelationKey::derive(id, "test-installation"),
        kind,
        observed_at_epoch_seconds: observed_at,
    }
}

#[test]
fn fresh_execution_edges_show_the_official_task_title_as_running() {
    let task = task("task-1", "实现任务动态");

    for kind in [
        ActivityEventKind::UserSubmission,
        ActivityEventKind::ToolActivity,
        ActivityEventKind::RolloutStarted,
    ] {
        let snapshot = reduce_task_activity([task.clone()], [event("task-1", kind, NOW - 1)], NOW);

        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].title, "实现任务动态");
        assert_eq!(snapshot.tasks[0].state, ActivityState::Running);
        assert!(!snapshot.stale);
    }
}

#[test]
fn normal_stop_replaces_running_without_claiming_any_task_completion_semantic() {
    let snapshot = reduce_task_activity(
        [task("task-1", "实现任务动态")],
        [
            event("task-1", ActivityEventKind::RolloutStarted, NOW - 20),
            event("task-1", ActivityEventKind::TurnStopped, NOW - 10),
        ],
        NOW,
    );

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].state, ActivityState::TurnCompleted);
    assert_eq!(snapshot.tasks[0].state.label(), "本轮完成");
    let serialized = serde_json::to_string(&snapshot.tasks[0]).unwrap();
    assert!(serialized.contains("turn_completed"));
    for unsupported_semantic in ["unread", "ready_for_review", "completed_task", "archived"] {
        assert!(!serialized.contains(unsupported_semantic));
    }
}

#[test]
fn task_activity_fixture_covers_transitions_and_expiry_boundaries() {
    let fixture: TaskActivityFixture =
        serde_json::from_str(include_str!("../fixtures/task-activity-cases.json")).unwrap();
    let correlation = CorrelationKey::derive("fixture-task", "test-installation");
    let metadata = OfficialTaskMetadata {
        correlation: correlation.clone(),
        correlation_aliases: Vec::new(),
        title: "fixture task".into(),
        parent_correlation: None,
    };

    for case in fixture.reductions {
        let events = case.events.into_iter().map(|event| ActivityEvent {
            correlation: correlation.clone(),
            kind: event.kind,
            observed_at_epoch_seconds: fixture.now - event.age_seconds,
        });
        let snapshot = reduce_task_activity([metadata.clone()], events, fixture.now);

        assert_eq!(
            snapshot.tasks.first().map(|task| task.state),
            case.expected_state,
            "{}",
            case.name
        );
        assert!(snapshot.tasks.len() <= 1, "{}", case.name);
    }
}

#[test]
fn session_tree_alias_correlates_hook_edges_with_official_thread_metadata() {
    let task = OfficialTaskMetadata {
        correlation: CorrelationKey::derive("official-thread-id", "test-installation"),
        correlation_aliases: vec![CorrelationKey::derive(
            "shared-session-id",
            "test-installation",
        )],
        title: "官方任务标题".into(),
        parent_correlation: None,
    };
    let hook_event = ActivityEvent {
        correlation: CorrelationKey::derive("shared-session-id", "test-installation"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: NOW,
    };

    let snapshot = reduce_task_activity([task], [hook_event], NOW);

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].title, "官方任务标题");
    assert_eq!(snapshot.tasks[0].state, ActivityState::Running);
}

#[test]
fn unmatched_running_edges_expire_instead_of_running_forever() {
    let snapshot = reduce_task_activity(
        [task("task-1", "实现任务动态")],
        [event(
            "task-1",
            ActivityEventKind::UserSubmission,
            NOW - 121,
        )],
        NOW,
    );

    assert!(snapshot.tasks.is_empty());
    assert!(snapshot.stale);
}

#[test]
fn subagents_are_folded_away_while_top_level_automation_titles_remain() {
    let parent = task("parent", "主任务");
    let subagent = OfficialTaskMetadata {
        correlation: CorrelationKey::derive("child", "test-installation"),
        correlation_aliases: Vec::new(),
        title: "子代理".into(),
        parent_correlation: Some(parent.correlation.clone()),
    };
    let automation = task("automation", "每日自动化");

    let snapshot = reduce_task_activity(
        [parent, subagent, automation],
        [
            event("child", ActivityEventKind::ToolActivity, NOW),
            event("automation", ActivityEventKind::RolloutStarted, NOW),
        ],
        NOW,
    );

    assert_eq!(snapshot.tasks.len(), 2);
    assert!(
        snapshot
            .tasks
            .iter()
            .any(|task| task.title == "主任务" && task.state == ActivityState::Running)
    );
    assert!(
        snapshot
            .tasks
            .iter()
            .any(|task| task.title == "每日自动化" && task.state == ActivityState::Running)
    );
}

#[test]
fn unsupported_sources_preserve_last_known_activity_as_stale() {
    let fresh = TaskActivitySnapshot {
        tasks: vec![codex_zectrix_dashboard::ObservedTask::new(
            "实现任务动态",
            ActivityState::TurnCompleted,
            NOW - 10,
        )],
        stale: false,
    };
    let mut cache = TaskActivityCache::default();
    cache.update::<()>(Ok(fresh.clone()));

    let stale = cache.update::<()>(Err(()));

    assert_eq!(stale.tasks, fresh.tasks);
    assert!(stale.stale);
}
