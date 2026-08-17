use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::de::IgnoredAny;
use thiserror::Error;

use crate::{ActivityEvent, ActivityEventKind, CorrelationKey, OfficialTaskMetadata};

const ROLLOUT_LOOKBACK: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Error)]
pub enum ActivitySourceError {
    #[error("任务元数据格式不受支持")]
    UnsupportedTaskMetadata,
    #[error("Hook 生命周期枚举不受支持")]
    UnsupportedHook,
    #[error("Rollout 生命周期格式不受支持")]
    UnsupportedRollout,
    #[error("无法只读观察 Codex 本地状态")]
    ReadOnlyObservation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResponse {
    data: Vec<serde_json::Value>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadMetadata {
    id: String,
    session_id: String,
    name: Option<String>,
    parent_thread_id: Option<String>,
    #[serde(rename = "source")]
    _source: ThreadSource,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[allow(dead_code)]
enum ThreadSource {
    Named(NamedThreadSource),
    Custom {
        custom: String,
    },
    Subagent {
        #[serde(rename = "subAgent")]
        subagent: SubagentSource,
    },
}

#[derive(Deserialize)]
enum NamedThreadSource {
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "exec")]
    Exec,
    #[serde(rename = "appServer")]
    AppServer,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[allow(dead_code)]
enum SubagentSource {
    Named(NamedSubagentSource),
    ThreadSpawn { thread_spawn: IgnoredAny },
    Other { other: IgnoredAny },
}

#[derive(Deserialize)]
enum NamedSubagentSource {
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "compact")]
    Compact,
    #[serde(rename = "memory_consolidation")]
    MemoryConsolidation,
}

pub fn parse_app_server_tasks(
    response: &str,
    installation_salt: &str,
) -> Result<Vec<OfficialTaskMetadata>, ActivitySourceError> {
    Ok(parse_app_server_task_page(response, installation_salt)?.tasks)
}

pub(crate) struct TaskMetadataPage {
    pub tasks: Vec<OfficialTaskMetadata>,
    pub next_cursor: Option<String>,
}

pub(crate) fn parse_app_server_task_page(
    response: &str,
    installation_salt: &str,
) -> Result<TaskMetadataPage, ActivitySourceError> {
    let response: ThreadListResponse =
        serde_json::from_str(response).map_err(|_| ActivitySourceError::UnsupportedTaskMetadata)?;
    let page_had_records = !response.data.is_empty();
    let recognized: Vec<ThreadMetadata> = response
        .data
        .into_iter()
        .filter_map(|task| serde_json::from_value::<ThreadMetadata>(task).ok())
        .collect();
    if page_had_records && recognized.is_empty() {
        return Err(ActivitySourceError::UnsupportedTaskMetadata);
    }
    let tasks = recognized
        .into_iter()
        .filter_map(|task| {
            let title = task.name.filter(|title| !title.trim().is_empty())?;
            let correlation = CorrelationKey::derive(&task.id, installation_salt);
            Some(OfficialTaskMetadata {
                correlation,
                correlation_aliases: vec![CorrelationKey::derive(
                    &task.session_id,
                    installation_salt,
                )],
                title,
                parent_correlation: task
                    .parent_thread_id
                    .map(|id| CorrelationKey::derive(&id, installation_salt)),
            })
        })
        .collect();
    Ok(TaskMetadataPage {
        tasks,
        next_cursor: response.next_cursor,
    })
}

#[derive(Deserialize)]
struct HookPayload {
    hook_event_name: String,
    session_id: String,
    status: Option<String>,
}

pub fn parse_hook_event(
    input: &str,
    installation_salt: &str,
    coarse_epoch_seconds: i64,
) -> Result<Option<ActivityEvent>, ActivitySourceError> {
    let payload: HookPayload =
        serde_json::from_str(input).map_err(|_| ActivitySourceError::UnsupportedHook)?;
    if payload.session_id.is_empty() {
        return Err(ActivitySourceError::UnsupportedHook);
    }
    let kind = match payload.hook_event_name.as_str() {
        "UserPromptSubmit" => ActivityEventKind::UserSubmission,
        "PreToolUse" | "PostToolUse" => ActivityEventKind::ToolActivity,
        "Stop" => match payload.status.as_deref() {
            None | Some("success" | "completed") => ActivityEventKind::TurnStopped,
            Some("failure" | "failed" | "error") => ActivityEventKind::TurnFailed,
            Some("interrupted" | "cancelled") => ActivityEventKind::TurnInterrupted,
            Some(_) => return Err(ActivitySourceError::UnsupportedHook),
        },
        _ => return Err(ActivitySourceError::UnsupportedHook),
    };
    Ok(Some(ActivityEvent {
        correlation: CorrelationKey::derive(&payload.session_id, installation_salt),
        kind,
        observed_at_epoch_seconds: coarse_epoch_seconds - coarse_epoch_seconds.rem_euclid(60),
    }))
}

pub fn persist_hook_event(path: &Path, event: &ActivityEvent) -> Result<(), ActivitySourceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    let mut record =
        serde_json::to_vec(event).map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    record.push(b'\n');
    file.write_all(&record)
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)
}

pub fn read_hook_events(path: &Path) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ActivitySourceError::ReadOnlyObservation),
    };
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        if let Ok(event) = serde_json::from_str(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

#[derive(Clone, Debug)]
pub struct ReadonlyObservationConfig {
    pub codex_home: PathBuf,
    pub installation_salt: String,
}

pub struct ReadonlyRolloutObserver {
    config: ReadonlyObservationConfig,
}

impl ReadonlyRolloutObserver {
    pub fn new(config: ReadonlyObservationConfig) -> Self {
        Self { config }
    }

    pub fn observe(&self) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
        let mut paths = Vec::new();
        collect_rollouts(&self.config.codex_home.join("sessions"), &mut paths)?;
        let mut events = Vec::new();
        let mut incompatible_rollouts = 0;
        for path in paths {
            match parse_rollout(&path, &self.config.installation_salt) {
                Ok(rollout_events) => {
                    events.extend(rollout_events);
                }
                Err(ActivitySourceError::UnsupportedRollout) => incompatible_rollouts += 1,
                Err(error) => return Err(error),
            }
        }
        if events.is_empty() && incompatible_rollouts > 0 {
            return Err(ActivitySourceError::UnsupportedRollout);
        }
        Ok(events)
    }
}

fn collect_rollouts(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), ActivitySourceError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ActivitySourceError::ReadOnlyObservation);
        }
        Err(_) => return Err(ActivitySourceError::ReadOnlyObservation),
    };
    for entry in entries {
        let entry = entry.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        let file_type = entry
            .file_type()
            .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        if file_type.is_dir() {
            collect_rollouts(&entry.path(), paths)?;
        } else if file_type.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(|_| ActivitySourceError::ReadOnlyObservation)?
                .elapsed()
                .map_or(true, |age| age <= ROLLOUT_LOOKBACK)
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(())
}

fn parse_rollout(
    path: &Path,
    installation_salt: &str,
) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
    let reader =
        BufReader::new(File::open(path).map_err(|_| ActivitySourceError::ReadOnlyObservation)?);
    let mut correlations = Vec::new();
    let mut events = Vec::new();
    let mut incompatible_lifecycle = false;
    for line in reader.lines() {
        let line = line.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        let Ok(envelope) = serde_json::from_str::<RolloutEnvelope>(&line) else {
            continue;
        };
        if envelope.envelope_type == "session_meta" {
            let Some(thread_id) = envelope.payload.id.as_deref().filter(|id| !id.is_empty()) else {
                continue;
            };
            correlations.push(CorrelationKey::derive(thread_id, installation_salt));
            if let Some(session_id) = envelope
                .payload
                .session_id
                .as_deref()
                .filter(|id| !id.is_empty())
            {
                correlations.push(CorrelationKey::derive(session_id, installation_salt));
            }
            continue;
        }
        if envelope.envelope_type != "event_msg" {
            continue;
        }
        let Some(payload_type) = envelope.payload.payload_type.as_deref() else {
            continue;
        };
        let kind = match payload_type {
            "task_started" => Some(ActivityEventKind::RolloutStarted),
            "task_complete" if envelope.payload.error.is_some() => {
                Some(ActivityEventKind::TurnFailed)
            }
            "task_complete" => Some(ActivityEventKind::TurnStopped),
            "turn_aborted" if envelope.payload.abort_reason.as_deref() == Some("interrupted") => {
                Some(ActivityEventKind::TurnInterrupted)
            }
            "turn_aborted" => None,
            "agent_message"
            | "agent_reasoning"
            | "context_compacted"
            | "image_generation_end"
            | "mcp_tool_call_end"
            | "patch_apply_end"
            | "sub_agent_activity"
            | "thread_rolled_back"
            | "thread_settings_applied"
            | "token_count"
            | "user_message"
            | "web_search_end" => None,
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        let timestamp = match kind {
            ActivityEventKind::RolloutStarted => envelope.payload.started_at,
            _ => envelope.payload.completed_at,
        };
        let Some(timestamp) = timestamp else {
            incompatible_lifecycle = true;
            continue;
        };
        if correlations.is_empty() {
            incompatible_lifecycle = true;
            continue;
        }
        events.extend(
            correlations
                .iter()
                .cloned()
                .map(|correlation| ActivityEvent {
                    correlation,
                    kind,
                    observed_at_epoch_seconds: timestamp,
                }),
        );
    }
    if events.is_empty() && incompatible_lifecycle {
        return Err(ActivitySourceError::UnsupportedRollout);
    }
    Ok(events)
}

#[derive(Deserialize)]
struct RolloutEnvelope {
    #[serde(rename = "type")]
    envelope_type: String,
    payload: RolloutPayload,
}

#[derive(Deserialize)]
struct RolloutPayload {
    #[serde(rename = "type")]
    payload_type: Option<String>,
    id: Option<String>,
    session_id: Option<String>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    error: Option<IgnoredAny>,
    #[serde(rename = "reason")]
    abort_reason: Option<String>,
}
