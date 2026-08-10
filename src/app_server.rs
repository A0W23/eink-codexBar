use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;

use crate::activity_sources::parse_app_server_task_page;
use crate::{
    ActivitySourceError, DashboardError, ObservedQuota, OfficialTaskMetadata,
    parse_app_server_quota,
};

#[derive(Clone, Debug)]
pub struct AppServerClient {
    program: PathBuf,
}

impl AppServerClient {
    pub fn new(program: impl AsRef<Path>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
        }
    }

    pub fn read_quota(&self) -> Result<ObservedQuota, AppServerError> {
        let result = self.read_only_request(json!({
            "id": 2,
            "method": "account/rateLimits/read",
            "params": null
        }))?;
        parse_app_server_quota(&serde_json::to_string(&result)?).map_err(AppServerError::Quota)
    }

    pub fn read_task_metadata(
        &self,
        installation_salt: &str,
    ) -> Result<Vec<OfficialTaskMetadata>, AppServerError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut tasks = Vec::new();
        loop {
            let result = self.read_only_request(json!({
                "id": 2,
                "method": "thread/list",
                "params": {
                    "cursor": cursor,
                    "limit": 100,
                    "sortKey": "updated_at",
                    "sortDirection": "desc",
                    "archived": false,
                    "sourceKinds": [
                        "cli", "vscode", "exec", "appServer", "subAgent", "subAgentReview",
                        "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"
                    ],
                    "useStateDbOnly": true
                }
            }))?;
            let page =
                parse_app_server_task_page(&serde_json::to_string(&result)?, installation_salt)?;
            tasks.extend(page.tasks);
            let Some(next_cursor) = page.next_cursor else {
                return Ok(tasks);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(AppServerError::Activity(
                    ActivitySourceError::UnsupportedTaskMetadata,
                ));
            }
            cursor = Some(next_cursor);
        }
    }

    fn read_only_request(&self, request: Value) -> Result<Value, AppServerError> {
        let mut child = Command::new(&self.program)
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(AppServerError::Start)?;
        let mut stdin = child.stdin.take().ok_or(AppServerError::MissingPipe)?;
        let stdout = child.stdout.take().ok_or(AppServerError::MissingPipe)?;
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        let result = (|| {
            write_message(
                &mut stdin,
                &json!({
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {
                            "name": "codex-zectrix-dashboard",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                }),
            )?;
            read_result(&receiver, 1)?;

            write_message(&mut stdin, &json!({ "method": "initialized" }))?;
            write_message(&mut stdin, &request)?;
            read_result(&receiver, 2)
        })();

        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        result
    }
}

impl Default for AppServerClient {
    fn default() -> Self {
        Self::new("codex")
    }
}

fn write_message(writer: &mut impl Write, message: &Value) -> Result<(), AppServerError> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_result(
    receiver: &Receiver<std::io::Result<String>>,
    id: u64,
) -> Result<Value, AppServerError> {
    loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => AppServerError::Timeout,
                RecvTimeoutError::Disconnected => AppServerError::Closed,
            })?
            .map_err(AppServerError::Io)?;
        let message: Value = serde_json::from_str(&line)?;
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if message.get("error").is_some() {
            return Err(AppServerError::Rpc);
        }
        return message
            .get("result")
            .cloned()
            .ok_or(AppServerError::MissingResult);
    }
}

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("无法启动 Codex app-server：{0}")]
    Start(#[source] std::io::Error),
    #[error("Codex app-server 未提供标准输入输出管道")]
    MissingPipe,
    #[error("Codex app-server 未在限定时间内响应")]
    Timeout,
    #[error("Codex app-server 在返回响应前结束")]
    Closed,
    #[error("Codex app-server 输入输出失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("Codex app-server 返回了无效 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex app-server 拒绝了只读请求")]
    Rpc,
    #[error("Codex app-server 响应缺少结果")]
    MissingResult,
    #[error(transparent)]
    Quota(#[from] DashboardError),
    #[error(transparent)]
    Activity(#[from] ActivitySourceError),
}
