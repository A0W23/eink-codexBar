use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde::de::IgnoredAny;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ActivityEvent, ActivityEventKind, CorrelationKey, OfficialTaskMetadata};

#[derive(Debug, Error)]
pub enum ActivitySourceError {
    #[error("任务元数据格式不受支持")]
    UnsupportedTaskMetadata,
    #[error("Hook 生命周期枚举不受支持")]
    UnsupportedHook,
    #[error("Rollout 生命周期格式不受支持")]
    UnsupportedRollout,
    #[error("Codex 版本不受支持")]
    UnsupportedVersion,
    #[error("Codex 本地 schema 不受支持")]
    UnsupportedSchema,
    #[error("无法只读观察 Codex 本地状态")]
    ReadOnlyObservation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResponse {
    data: Vec<ThreadMetadata>,
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
    let tasks = response
        .data
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
    serde_json::to_writer(&mut file, event)
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    file.write_all(b"\n")
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)
}

pub fn read_hook_events(path: &Path) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ActivitySourceError::ReadOnlyObservation),
    };
    BufReader::new(file)
        .lines()
        .map(|line| {
            let line = line.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
            serde_json::from_str(&line).map_err(|_| ActivitySourceError::UnsupportedHook)
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct ReadonlyObservationConfig {
    pub codex_home: PathBuf,
    pub installation_salt: String,
    pub supported_cli_version: String,
    pub supported_schema_sha256: String,
}

pub struct ReadonlyRolloutObserver {
    config: ReadonlyObservationConfig,
}

impl ReadonlyRolloutObserver {
    pub fn new(config: ReadonlyObservationConfig) -> Self {
        Self { config }
    }

    pub fn observe(&self) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
        let fingerprint = compute_state_schema_fingerprint(&self.config.codex_home)?;
        if fingerprint != self.config.supported_schema_sha256 {
            return Err(ActivitySourceError::UnsupportedSchema);
        }
        let mut paths = Vec::new();
        collect_rollouts(&self.config.codex_home.join("sessions"), &mut paths)?;
        let mut events = Vec::new();
        for path in paths {
            events.extend(parse_rollout(
                &path,
                &self.config.installation_salt,
                &self.config.supported_cli_version,
            )?);
        }
        Ok(events)
    }
}

pub fn compute_state_schema_fingerprint(codex_home: &Path) -> Result<String, ActivitySourceError> {
    let database = codex_home.join("state_5.sqlite");
    let uri = format!("file:{}?immutable=1", database.display());
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    let mut statement = connection
        .prepare("select name from sqlite_schema where type='table' order by name")
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    let table_names: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)?
        .collect::<Result<_, _>>()
        .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    drop(statement);
    let mut tables = Vec::new();
    for table in table_names {
        let quoted = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("pragma table_info(\"{quoted}\")"))
            .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        let columns: Vec<String> = statement
            .query_map([], |row| row.get(1))
            .map_err(|_| ActivitySourceError::ReadOnlyObservation)?
            .collect::<Result<_, _>>()
            .map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        tables.push((table, columns));
    }
    let encoded =
        serde_json::to_vec(&tables).map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn collect_rollouts(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), ActivitySourceError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
    supported_cli_version: &str,
) -> Result<Vec<ActivityEvent>, ActivitySourceError> {
    let reader =
        BufReader::new(File::open(path).map_err(|_| ActivitySourceError::ReadOnlyObservation)?);
    let mut correlations = Vec::new();
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        let envelope: RolloutEnvelope =
            serde_json::from_str(&line).map_err(|_| ActivitySourceError::UnsupportedRollout)?;
        if envelope.envelope_type == "session_meta" {
            let cli_version = envelope
                .payload
                .cli_version
                .as_deref()
                .ok_or(ActivitySourceError::UnsupportedRollout)?;
            if cli_version != supported_cli_version {
                return Err(ActivitySourceError::UnsupportedVersion);
            }
            let thread_id = envelope
                .payload
                .id
                .as_deref()
                .filter(|id| !id.is_empty())
                .ok_or(ActivitySourceError::UnsupportedRollout)?;
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
            if matches!(
                envelope.envelope_type.as_str(),
                "response_item"
                    | "world_state"
                    | "turn_context"
                    | "inter_agent_communication_metadata"
                    | "compacted"
            ) {
                continue;
            }
            return Err(ActivitySourceError::UnsupportedRollout);
        }
        let Some(payload_type) = envelope.payload.payload_type.as_deref() else {
            return Err(ActivitySourceError::UnsupportedRollout);
        };
        let kind = match payload_type {
            "task_started" => Some(ActivityEventKind::RolloutStarted),
            "task_complete" if envelope.payload.error.is_some() => {
                Some(ActivityEventKind::TurnFailed)
            }
            "task_complete" => Some(ActivityEventKind::TurnStopped),
            "turn_aborted" => Some(ActivityEventKind::TurnInterrupted),
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
            _ => return Err(ActivitySourceError::UnsupportedRollout),
        };
        let Some(kind) = kind else {
            continue;
        };
        let timestamp = match kind {
            ActivityEventKind::RolloutStarted => envelope.payload.started_at,
            _ => envelope
                .payload
                .completed_at
                .or(envelope.payload.started_at),
        }
        .ok_or(ActivitySourceError::UnsupportedRollout)?;
        if correlations.is_empty() {
            return Err(ActivitySourceError::UnsupportedRollout);
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
    if correlations.is_empty() {
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
    cli_version: Option<String>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    error: Option<IgnoredAny>,
}
