use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{AppServerClient, HookMetadata};

const REVIEWED_EVENTS: [&str; 4] = ["PostToolUse", "PreToolUse", "Stop", "UserPromptSubmit"];
const REVIEWED_COMMAND: &str = "\"${CLAUDE_PLUGIN_ROOT}/bin/codex-zectrix-dashboard\" hook-record";

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("插件 hooks 与已审核清单不一致")]
    HookSet,
    #[error("插件 hook 缺少精确的 SHA-256 内容哈希")]
    HookHash,
    #[error("插件 hook 定义与已审核内容不一致")]
    HookDefinition,
    #[error("生命周期状态不可用")]
    State,
    #[error("无法启动或停止 companion")]
    Companion,
    #[error("旧 Codex Desktop 进程仍可能缓存 hook，请 reload 或 restart 后重试")]
    ReloadRequired,
    #[error("Codex hook 配置未达到预期状态")]
    HookConfiguration,
}

pub fn reviewed_plugin_hooks(
    discovered: Vec<HookMetadata>,
    plugin_id: &str,
) -> Result<Vec<HookMetadata>, LifecycleError> {
    let mut selected = discovered
        .into_iter()
        .filter(|hook| hook.plugin_id.as_deref() == Some(plugin_id))
        .collect::<Vec<_>>();
    let events = selected
        .iter()
        .map(|hook| hook.event_name.as_str())
        .collect::<BTreeSet<_>>();
    if events != REVIEWED_EVENTS.into_iter().collect() || selected.len() != REVIEWED_EVENTS.len() {
        return Err(LifecycleError::HookSet);
    }
    if selected.iter().any(|hook| {
        hook.is_managed
            || hook
                .current_hash
                .strip_prefix("sha256:")
                .is_none_or(|hash| {
                    hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
    }) {
        return Err(LifecycleError::HookHash);
    }
    if selected.iter().any(|hook| {
        hook.handler_type != "command"
            || hook.execution_mode != "sync"
            || hook.matcher.is_some()
            || hook.command.as_deref() != Some(REVIEWED_COMMAND)
            || hook.timeout_sec != 5
            || hook.status_message.is_some()
            || hook.additional_context_limit.is_some()
    }) {
        return Err(LifecycleError::HookDefinition);
    }
    selected.sort_by(|left, right| left.event_name.cmp(&right.event_name));
    Ok(selected)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    Update,
    Uninstall,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LifecycleState {
    Active {
        plugin_id: String,
        plugin_root: PathBuf,
        hooks: Vec<HookMetadata>,
    },
    AwaitingReload {
        action: LifecycleAction,
        plugin_id: String,
        plugin_root: PathBuf,
        hooks: Vec<HookMetadata>,
        next_plugin_id: Option<String>,
        next_plugin_root: Option<PathBuf>,
        owner_pids: Vec<u32>,
    },
}

pub struct PluginLifecycle {
    data_dir: PathBuf,
    codex: AppServerClient,
    codex_program: PathBuf,
    launchctl: PathBuf,
}

impl PluginLifecycle {
    pub fn new(
        data_dir: impl AsRef<Path>,
        codex_program: impl AsRef<Path>,
        launchctl: impl AsRef<Path>,
    ) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_owned(),
            codex: AppServerClient::new(&codex_program),
            codex_program: codex_program.as_ref().to_owned(),
            launchctl: launchctl.as_ref().to_owned(),
        }
    }

    pub fn install(&self, plugin_root: &Path, plugin_id: &str) -> Result<(), LifecycleError> {
        fs::create_dir_all(&self.data_dir).map_err(|_| LifecycleError::State)?;
        let hooks = self
            .codex
            .list_hooks(plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let hooks = reviewed_plugin_hooks(hooks, plugin_id)?;
        self.codex
            .configure_hooks(&hooks, true)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let configured = self
            .codex
            .list_hooks(plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let configured = reviewed_plugin_hooks(configured, plugin_id)?;
        if configured.iter().any(|hook| {
            !hook.enabled
                || hook.trust_status != "trusted"
                || hooks
                    .iter()
                    .find(|reviewed| reviewed.key == hook.key)
                    .map(|reviewed| &reviewed.current_hash)
                    != Some(&hook.current_hash)
        }) {
            return Err(LifecycleError::HookConfiguration);
        }
        self.write_launch_agent(plugin_root)?;
        self.bootstrap_companion()?;
        self.write_state(&LifecycleState::Active {
            plugin_id: plugin_id.into(),
            plugin_root: plugin_root.to_owned(),
            hooks,
        })
    }

    pub fn begin_update(
        &self,
        next_plugin_root: &Path,
        next_plugin_id: &str,
    ) -> Result<(), LifecycleError> {
        self.begin(
            LifecycleAction::Update,
            Some(next_plugin_root.to_owned()),
            Some(next_plugin_id.to_owned()),
        )
    }

    pub fn begin_uninstall(&self) -> Result<(), LifecycleError> {
        self.begin(LifecycleAction::Uninstall, None, None)
    }

    fn begin(
        &self,
        action: LifecycleAction,
        next_plugin_root: Option<PathBuf>,
        next_plugin_id: Option<String>,
    ) -> Result<(), LifecycleError> {
        let LifecycleState::Active {
            plugin_id,
            plugin_root,
            hooks,
        } = self.read_state()?
        else {
            return Err(LifecycleError::State);
        };
        self.codex
            .configure_hooks(&hooks, false)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let current = self
            .codex
            .list_hooks(&plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let current = reviewed_plugin_hooks(current, &plugin_id)?;
        if current.iter().any(|hook| hook.enabled) {
            return Err(LifecycleError::HookConfiguration);
        }
        self.stop_companion()?;
        fs::write(plugin_root.join("bin/.codex-zectrix-tombstone"), [])
            .map_err(|_| LifecycleError::State)?;
        self.write_state(&LifecycleState::AwaitingReload {
            action,
            plugin_id,
            plugin_root,
            hooks,
            next_plugin_id,
            next_plugin_root,
            owner_pids: read_owner_pids(&self.data_dir),
        })
    }

    pub fn resume(&self) -> Result<(), LifecycleError> {
        let LifecycleState::AwaitingReload {
            action,
            plugin_id,
            plugin_root,
            hooks: _,
            next_plugin_id,
            next_plugin_root,
            owner_pids,
        } = self.read_state()?
        else {
            return Err(LifecycleError::State);
        };
        if owner_pids.into_iter().any(process_is_alive) {
            return Err(LifecycleError::ReloadRequired);
        }
        match action {
            LifecycleAction::Update => {
                let root = next_plugin_root.ok_or(LifecycleError::State)?;
                let id = next_plugin_id.ok_or(LifecycleError::State)?;
                self.install(&root, &id)?;
                match fs::remove_file(plugin_root.join("bin/codex-zectrix-dashboard")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(LifecycleError::State),
                }
            }
            LifecycleAction::Uninstall => {
                let security = std::env::var_os("CODEX_ZECTRIX_SECURITY_BIN")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/usr/bin/security"));
                let credential_status = Command::new(security)
                    .args([
                        "delete-generic-password",
                        "-a",
                        "zectrix-api-key",
                        "-s",
                        "com.barrybarrywu.codex-zectrix-dashboard",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|_| LifecycleError::State)?;
                if !credential_status.success() && credential_status.code() != Some(44) {
                    return Err(LifecycleError::State);
                }
                let status = Command::new(&self.codex_program)
                    .args(["plugin", "remove", &plugin_id, "--json"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|_| LifecycleError::State)?;
                if !status.success() {
                    return Err(LifecycleError::State);
                }
                let _ = fs::remove_dir_all(&self.data_dir);
            }
        }
        let _ = fs::remove_file(plugin_root.join("bin/.codex-zectrix-tombstone"));
        Ok(())
    }

    pub fn diagnostics(&self) -> Result<String, LifecycleError> {
        let state = self.read_state()?;
        let (phase, hooks, owners) = match state {
            LifecycleState::Active { hooks, .. } => ("active", hooks.len(), 0),
            LifecycleState::AwaitingReload {
                hooks, owner_pids, ..
            } => ("awaiting_reload", hooks.len(), owner_pids.len()),
        };
        Ok(format!(
            "lifecycle_phase={phase}\nreviewed_hooks={hooks}\ncached_owner_processes={owners}\n"
        ))
    }

    fn write_launch_agent(&self, plugin_root: &Path) -> Result<(), LifecycleError> {
        let executable = plugin_root.join("bin/codex-zectrix-dashboard");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>com.barrybarrywu.codex-zectrix-dashboard</string><key>ProgramArguments</key><array><string>{}</string><string>companion</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>",
            xml_escape(&executable.to_string_lossy())
        );
        fs::write(self.plist_path(), plist).map_err(|_| LifecycleError::State)
    }

    fn bootstrap_companion(&self) -> Result<(), LifecycleError> {
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        let status = Command::new(&self.launchctl)
            .args(["bootstrap", &domain])
            .arg(self.plist_path())
            .status()
            .map_err(|_| LifecycleError::Companion)?;
        status
            .success()
            .then_some(())
            .ok_or(LifecycleError::Companion)
    }

    fn stop_companion(&self) -> Result<(), LifecycleError> {
        let service = format!("gui/{}/com.barrybarrywu.codex-zectrix-dashboard", unsafe {
            libc::geteuid()
        });
        let status = Command::new(&self.launchctl)
            .args(["bootout", &service])
            .status()
            .map_err(|_| LifecycleError::Companion)?;
        status
            .success()
            .then_some(())
            .ok_or(LifecycleError::Companion)
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("lifecycle.json")
    }

    fn plist_path(&self) -> PathBuf {
        self.data_dir.join("companion.plist")
    }

    fn read_state(&self) -> Result<LifecycleState, LifecycleError> {
        serde_json::from_slice(&fs::read(self.state_path()).map_err(|_| LifecycleError::State)?)
            .map_err(|_| LifecycleError::State)
    }

    fn write_state(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        let temporary = self.state_path().with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec(state).map_err(|_| LifecycleError::State)?,
        )
        .map_err(|_| LifecycleError::State)?;
        fs::rename(temporary, self.state_path()).map_err(|_| LifecycleError::State)
    }
}

pub fn record_hook_owner(data_dir: &Path, owner_pid: u32) {
    let path = data_dir.join("hook-owner-pids");
    let _ = fs::create_dir_all(data_dir);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{owner_pid}");
    }
}

pub fn hook_is_tombstoned(executable: &Path) -> bool {
    executable
        .parent()
        .is_some_and(|parent| parent.join(".codex-zectrix-tombstone").is_file())
}

fn read_owner_pids(data_dir: &Path) -> Vec<u32> {
    fs::read_to_string(data_dir.join("hook-owner-pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.parse().ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn process_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
