use codex_zectrix_dashboard::{
    ActivityEvent, ActivityEventKind, ActivityState, CorrelationKey, OfficialTaskMetadata,
    TaskActivityCache, TaskActivitySnapshot, reduce_task_activity,
};

const NOW: i64 = 1_786_330_000;

fn task(id: &str, title: &str) -> OfficialTaskMetadata {
    OfficialTaskMetadata {
        correlation: CorrelationKey::derive(id, "test-installation"),
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
fn a_new_execution_replaces_the_same_tasks_ended_turn() {
    let snapshot = reduce_task_activity(
        [task("task-1", "实现任务动态")],
        [
            event("task-1", ActivityEventKind::TurnStopped, NOW - 20),
            event("task-1", ActivityEventKind::UserSubmission, NOW - 10),
        ],
        NOW,
    );

    assert_eq!(snapshot.tasks.len(), 1);
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
fn ended_turns_remain_for_exactly_twenty_four_hours() {
    let visible = reduce_task_activity(
        [task("task-1", "实现任务动态")],
        [event(
            "task-1",
            ActivityEventKind::TurnStopped,
            NOW - 24 * 60 * 60,
        )],
        NOW,
    );
    let expired = reduce_task_activity(
        [task("task-1", "实现任务动态")],
        [event(
            "task-1",
            ActivityEventKind::TurnStopped,
            NOW - 24 * 60 * 60 - 1,
        )],
        NOW,
    );

    assert_eq!(visible.tasks.len(), 1);
    assert!(expired.tasks.is_empty());
}

#[test]
fn subagents_are_folded_away_while_top_level_automation_titles_remain() {
    let parent = task("parent", "主任务");
    let subagent = OfficialTaskMetadata {
        correlation: CorrelationKey::derive("child", "test-installation"),
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
