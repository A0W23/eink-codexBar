use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadMetadata {
    id: String,
    name: Option<String>,
    parent_thread_id: Option<String>,
    source: Value,
}

pub fn parse_app_server_tasks(
    response: &str,
    installation_salt: &str,
) -> Result<Vec<OfficialTaskMetadata>, ActivitySourceError> {
    let response: ThreadListResponse =
        serde_json::from_str(response).map_err(|_| ActivitySourceError::UnsupportedTaskMetadata)?;
    response
        .data
        .iter()
        .try_for_each(|task| validate_thread_source(&task.source))?;
    Ok(response
        .data
        .into_iter()
        .filter_map(|task| {
            let title = task.name.filter(|title| !title.trim().is_empty())?;
            Some(OfficialTaskMetadata {
                correlation: CorrelationKey::derive(&task.id, installation_salt),
                title,
                parent_correlation: task
                    .parent_thread_id
                    .as_deref()
                    .map(|id| CorrelationKey::derive(id, installation_salt)),
            })
        })
        .collect())
}

fn validate_thread_source(source: &Value) -> Result<(), ActivitySourceError> {
    if source
        .as_str()
        .is_some_and(|source| matches!(source, "cli" | "vscode" | "exec" | "appServer" | "unknown"))
    {
        return Ok(());
    }
    let Some(source) = source.as_object() else {
        return Err(ActivitySourceError::UnsupportedTaskMetadata);
    };
    if source.len() != 1 {
        return Err(ActivitySourceError::UnsupportedTaskMetadata);
    }
    if source.get("custom").is_some_and(Value::is_string) {
        return Ok(());
    }
    let Some(subagent) = source.get("subAgent") else {
        return Err(ActivitySourceError::UnsupportedTaskMetadata);
    };
    if subagent
        .as_str()
        .is_some_and(|kind| matches!(kind, "review" | "compact" | "memory_consolidation"))
    {
        return Ok(());
    }
    if subagent.as_object().is_some_and(|kind| {
        kind.len() == 1 && (kind.contains_key("thread_spawn") || kind.contains_key("other"))
    }) {
        return Ok(());
    }
    Err(ActivitySourceError::UnsupportedTaskMetadata)
}

pub fn parse_hook_event(
    input: &str,
    installation_salt: &str,
    coarse_epoch_seconds: i64,
) -> Result<Option<ActivityEvent>, ActivitySourceError> {
    let payload: Value =
        serde_json::from_str(input).map_err(|_| ActivitySourceError::UnsupportedHook)?;
    let payload = payload
        .as_object()
        .ok_or(ActivitySourceError::UnsupportedHook)?;
    let event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or(ActivitySourceError::UnsupportedHook)?;
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(ActivitySourceError::UnsupportedHook)?;
    let kind = match event {
        "UserPromptSubmit" => ActivityEventKind::UserSubmission,
        "PreToolUse" | "PostToolUse" => ActivityEventKind::ToolActivity,
        "Stop" => match payload.get("status").and_then(Value::as_str) {
            None | Some("success" | "completed") => ActivityEventKind::TurnStopped,
            Some("failure" | "failed" | "error") => ActivityEventKind::TurnFailed,
            Some("interrupted" | "cancelled") => ActivityEventKind::TurnInterrupted,
            Some(_) => return Err(ActivitySourceError::UnsupportedHook),
        },
        "SessionStart" | "PermissionRequest" | "SessionEnd" => return Ok(None),
        _ => return Err(ActivitySourceError::UnsupportedHook),
    };
    Ok(Some(ActivityEvent {
        correlation: CorrelationKey::derive(session_id, installation_salt),
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
    let mut correlation = None;
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|_| ActivitySourceError::ReadOnlyObservation)?;
        let envelope: Value =
            serde_json::from_str(&line).map_err(|_| ActivitySourceError::UnsupportedRollout)?;
        let envelope_type = envelope.get("type").and_then(Value::as_str);
        let payload = envelope
            .get("payload")
            .and_then(Value::as_object)
            .ok_or(ActivitySourceError::UnsupportedRollout)?;
        if envelope_type == Some("session_meta") {
            let cli_version = payload
                .get("cli_version")
                .and_then(Value::as_str)
                .ok_or(ActivitySourceError::UnsupportedRollout)?;
            if cli_version != supported_cli_version {
                return Err(ActivitySourceError::UnsupportedVersion);
            }
            let id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(ActivitySourceError::UnsupportedRollout)?;
            correlation = Some(CorrelationKey::derive(id, installation_salt));
            continue;
        }
        if envelope_type != Some("event_msg") {
            continue;
        }
        let Some(payload_type) = payload.get("type").and_then(Value::as_str) else {
            return Err(ActivitySourceError::UnsupportedRollout);
        };
        let kind = match payload_type {
            "task_started" => Some(ActivityEventKind::RolloutStarted),
            "task_complete" if payload.get("error").is_some_and(|value| !value.is_null()) => {
                Some(ActivityEventKind::TurnFailed)
            }
            "task_complete" => Some(ActivityEventKind::TurnStopped),
            "turn_aborted" => Some(ActivityEventKind::TurnInterrupted),
            value if value.starts_with("task_") || value.starts_with("turn_") => {
                return Err(ActivitySourceError::UnsupportedRollout);
            }
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        let correlation = correlation
            .clone()
            .ok_or(ActivitySourceError::UnsupportedRollout)?;
        let timestamp = match kind {
            ActivityEventKind::RolloutStarted => payload.get("started_at"),
            _ => payload
                .get("completed_at")
                .or_else(|| payload.get("started_at")),
        }
        .and_then(Value::as_i64)
        .ok_or(ActivitySourceError::UnsupportedRollout)?;
        events.push(ActivityEvent {
            correlation,
            kind,
            observed_at_epoch_seconds: timestamp,
        });
    }
    if correlation.is_none() {
        return Err(ActivitySourceError::UnsupportedRollout);
    }
    Ok(events)
}
