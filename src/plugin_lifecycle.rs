use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{
    AppServerClient, HookMetadata, KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE, LAUNCH_AGENT_LABEL,
    PLUGIN_BINARY,
};

const REVIEWED_HOOKS: [(&str, &str); 4] = [
    (
        "postToolUse",
        "sha256:17b77d2f37d63dd85cc2e38772206476e89d3f0103a9dca736f811058927368e",
    ),
    (
        "preToolUse",
        "sha256:d063f36a5ca5702387c3ad9113a6f269fb237390b3dd3ce711aafce9068d9d9a",
    ),
    (
        "stop",
        "sha256:34792817128542de402eba581bddd8029a9831085edb19585233fb1c54018039",
    ),
    (
        "userPromptSubmit",
        "sha256:0c9f3f0266c19378ac76046c24257c3159981424760443eed39e2ff3931da7f5",
    ),
];

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
    plugin_root: &Path,
) -> Result<Vec<HookMetadata>, LifecycleError> {
    let mut selected = discovered
        .into_iter()
        .filter(|hook| hook.plugin_id.as_deref() == Some(plugin_id))
        .collect::<Vec<_>>();
    let events = selected
        .iter()
        .map(|hook| hook.event_name.as_str())
        .collect::<BTreeSet<_>>();
    let reviewed_events = REVIEWED_HOOKS
        .iter()
        .map(|(event, _)| *event)
        .collect::<BTreeSet<_>>();
    if events != reviewed_events || selected.len() != REVIEWED_HOOKS.len() {
        return Err(LifecycleError::HookSet);
    }
    if selected.iter().any(|hook| {
        REVIEWED_HOOKS
            .iter()
            .find(|(event, _)| *event == hook.event_name)
            .is_none_or(|(_, hash)| *hash != hook.current_hash)
    }) {
        return Err(LifecycleError::HookHash);
    }
    let expected_command = format!(
        "\"{}\" hook-record",
        plugin_root.join("bin").join(PLUGIN_BINARY).display()
    );
    if selected.iter().any(|hook| {
        hook.is_managed
            || hook.handler_type != "command"
            || hook
                .execution_mode
                .as_deref()
                .is_some_and(|mode| mode != "sync")
            || hook.matcher.is_some()
            || hook.command.as_deref() != Some(expected_command.as_str())
            || hook.timeout_sec != 5
            || hook.status_message.is_some()
            || hook.additional_context_limit.is_some()
            || hook.source_path != plugin_root.join("hooks/hooks.json")
    }) {
        return Err(LifecycleError::HookDefinition);
    }
    selected.sort_by(|left, right| left.event_name.cmp(&right.event_name));
    Ok(selected)
}

fn recorded_hooks_inactive(
    current: Vec<HookMetadata>,
    recorded: &[HookMetadata],
    plugin_id: &str,
    allow_missing: bool,
) -> Result<bool, LifecycleError> {
    let current = current
        .into_iter()
        .filter(|hook| hook.plugin_id.as_deref() == Some(plugin_id))
        .collect::<Vec<_>>();
    if current.is_empty() {
        return Ok(allow_missing);
    }
    if current.len() != recorded.len() {
        return Err(LifecycleError::HookDefinition);
    }
    for recorded_hook in recorded {
        let current_hook = current
            .iter()
            .find(|hook| hook.key == recorded_hook.key)
            .ok_or(LifecycleError::HookDefinition)?;
        if current_hook.current_hash != recorded_hook.current_hash {
            return Err(LifecycleError::HookHash);
        }
        if current_hook.enabled {
            return Ok(false);
        }
    }
    Ok(true)
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
    FinalizingUpdate {
        plugin_id: String,
        plugin_root: PathBuf,
        next_plugin_id: String,
        next_plugin_root: PathBuf,
    },
    FinalizingUninstall {
        plugin_id: String,
        plugin_root: PathBuf,
    },
}

pub struct PluginLifecycle {
    data_dir: PathBuf,
    codex: AppServerClient,
    codex_program: PathBuf,
    launchctl: PathBuf,
    launch_agents_dir: PathBuf,
    ps_program: PathBuf,
}

impl PluginLifecycle {
    pub fn new(
        data_dir: impl AsRef<Path>,
        codex_program: impl AsRef<Path>,
        launchctl: impl AsRef<Path>,
        launch_agents_dir: impl AsRef<Path>,
        ps_program: impl AsRef<Path>,
    ) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_owned(),
            codex: AppServerClient::new(&codex_program),
            codex_program: codex_program.as_ref().to_owned(),
            launchctl: launchctl.as_ref().to_owned(),
            launch_agents_dir: launch_agents_dir.as_ref().to_owned(),
            ps_program: ps_program.as_ref().to_owned(),
        }
    }

    pub fn install(&self, plugin_root: &Path, plugin_id: &str) -> Result<(), LifecycleError> {
        if self.state_path().exists() {
            return Err(LifecycleError::State);
        }
        self.add_plugin(plugin_id)?;
        fs::create_dir_all(&self.data_dir).map_err(|_| LifecycleError::State)?;
        fs::write(self.data_dir.join("hook-owner-pids"), []).map_err(|_| LifecycleError::State)?;
        let hooks = self.activate(plugin_root, plugin_id)?;
        self.write_state(&LifecycleState::Active {
            plugin_id: plugin_id.into(),
            plugin_root: plugin_root.to_owned(),
            hooks,
        })?;
        Ok(())
    }

    fn activate(
        &self,
        plugin_root: &Path,
        plugin_id: &str,
    ) -> Result<Vec<HookMetadata>, LifecycleError> {
        if !plugin_root.join("bin").join(PLUGIN_BINARY).is_file() {
            return Err(LifecycleError::State);
        }
        let hooks = self
            .codex
            .list_hooks(plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let hooks = reviewed_plugin_hooks(hooks, plugin_id, plugin_root)?;
        self.codex
            .configure_hooks(&hooks, true)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let configured = self
            .codex
            .list_hooks(plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        let configured = reviewed_plugin_hooks(configured, plugin_id, plugin_root)?;
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
        Ok(hooks)
    }

    pub fn begin_update(
        &self,
        next_plugin_root: &Path,
        next_plugin_id: &str,
    ) -> Result<(), LifecycleError> {
        if let LifecycleState::Active { plugin_root, .. } = self.read_state()?
            && plugin_root == next_plugin_root
        {
            return Err(LifecycleError::State);
        }
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
        if !recorded_hooks_inactive(current, &hooks, &plugin_id, false)? {
            return Err(LifecycleError::HookConfiguration);
        }
        self.stop_companion()?;
        fs::write(plugin_root.join("bin/.codex-zectrix-tombstone"), [])
            .map_err(|_| LifecycleError::State)?;
        let mut owner_pids = read_owner_pids(&self.data_dir);
        owner_pids.extend(running_desktop_codex_pids(&self.ps_program));
        owner_pids.sort_unstable();
        owner_pids.dedup();
        self.write_state(&LifecycleState::AwaitingReload {
            action,
            plugin_id,
            plugin_root,
            hooks,
            next_plugin_id,
            next_plugin_root,
            owner_pids,
        })
    }

    pub fn resume(&self) -> Result<(), LifecycleError> {
        let state = self.read_state()?;
        let state = match state {
            LifecycleState::AwaitingReload {
                action,
                plugin_id,
                plugin_root,
                hooks,
                next_plugin_id,
                next_plugin_root,
                owner_pids,
            } => {
                if owner_pids.into_iter().any(process_is_alive)
                    || !self.old_hooks_inactive(&plugin_root, &plugin_id, &hooks)?
                {
                    return Err(LifecycleError::ReloadRequired);
                }
                let state = match action {
                    LifecycleAction::Update => LifecycleState::FinalizingUpdate {
                        plugin_id,
                        plugin_root,
                        next_plugin_id: next_plugin_id.ok_or(LifecycleError::State)?,
                        next_plugin_root: next_plugin_root.ok_or(LifecycleError::State)?,
                    },
                    LifecycleAction::Uninstall => LifecycleState::FinalizingUninstall {
                        plugin_id,
                        plugin_root,
                    },
                };
                self.write_state(&state)?;
                state
            }
            state @ (LifecycleState::FinalizingUpdate { .. }
            | LifecycleState::FinalizingUninstall { .. }) => state,
            LifecycleState::Active { .. } => return Err(LifecycleError::State),
        };
        match state {
            LifecycleState::FinalizingUpdate {
                plugin_id: _,
                plugin_root,
                next_plugin_id,
                next_plugin_root,
            } => {
                self.refresh_plugin(&next_plugin_id)?;
                fs::write(self.data_dir.join("hook-owner-pids"), [])
                    .map_err(|_| LifecycleError::State)?;
                let hooks = self.activate(&next_plugin_root, &next_plugin_id)?;
                remove_if_present(&plugin_root.join("bin").join(PLUGIN_BINARY))?;
                remove_if_present(&plugin_root.join("bin/.codex-zectrix-tombstone"))?;
                self.write_state(&LifecycleState::Active {
                    plugin_id: next_plugin_id,
                    plugin_root: next_plugin_root,
                    hooks,
                })?;
            }
            LifecycleState::FinalizingUninstall {
                plugin_id,
                plugin_root,
            } => self.finish_uninstall(&plugin_id, &plugin_root)?,
            _ => return Err(LifecycleError::State),
        }
        Ok(())
    }

    fn old_hooks_inactive(
        &self,
        plugin_root: &Path,
        plugin_id: &str,
        recorded_hooks: &[HookMetadata],
    ) -> Result<bool, LifecycleError> {
        let hooks = self
            .codex
            .list_hooks(plugin_root)
            .map_err(|_| LifecycleError::HookConfiguration)?;
        recorded_hooks_inactive(hooks, recorded_hooks, plugin_id, true)
    }

    fn finish_uninstall(&self, plugin_id: &str, plugin_root: &Path) -> Result<(), LifecycleError> {
        let security = std::env::var_os("CODEX_ZECTRIX_SECURITY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/security"));
        let credential_status = Command::new(security)
            .args([
                "delete-generic-password",
                "-a",
                KEYCHAIN_ACCOUNT,
                "-s",
                KEYCHAIN_SERVICE,
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
            .args(["plugin", "remove", plugin_id, "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| LifecycleError::State)?;
        if !status.success() {
            let hooks = self
                .codex
                .list_hooks(plugin_root)
                .map_err(|_| LifecycleError::State)?;
            if hooks
                .iter()
                .any(|hook| hook.plugin_id.as_deref() == Some(plugin_id))
            {
                return Err(LifecycleError::State);
            }
        }
        remove_if_present(&self.plist_path())?;
        fs::remove_dir_all(&self.data_dir).map_err(|_| LifecycleError::State)
    }

    fn refresh_plugin(&self, plugin_id: &str) -> Result<(), LifecycleError> {
        let (_, marketplace) = plugin_id.rsplit_once('@').ok_or(LifecycleError::State)?;
        if marketplace.is_empty() {
            return Err(LifecycleError::State);
        }
        let status = Command::new(&self.codex_program)
            .args(["plugin", "marketplace", "upgrade", marketplace, "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| LifecycleError::State)?;
        if !status.success() {
            return Err(LifecycleError::State);
        }
        self.add_plugin(plugin_id)
    }

    fn add_plugin(&self, plugin_id: &str) -> Result<(), LifecycleError> {
        let status = Command::new(&self.codex_program)
            .args(["plugin", "add", plugin_id, "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| LifecycleError::State)?;
        status.success().then_some(()).ok_or(LifecycleError::State)
    }

    pub fn diagnostics(&self) -> Result<String, LifecycleError> {
        let state = self.read_state()?;
        let (phase, hooks, owners) = match state {
            LifecycleState::Active { hooks, .. } => ("active", hooks.len(), 0),
            LifecycleState::AwaitingReload {
                hooks, owner_pids, ..
            } => ("awaiting_reload", hooks.len(), owner_pids.len()),
            LifecycleState::FinalizingUpdate { .. } => ("finalizing_update", 4, 0),
            LifecycleState::FinalizingUninstall { .. } => ("finalizing_uninstall", 4, 0),
        };
        Ok(format!(
            "lifecycle_phase={phase}\nreviewed_hooks={hooks}\ncached_owner_processes={owners}\n"
        ))
    }

    fn write_launch_agent(&self, plugin_root: &Path) -> Result<(), LifecycleError> {
        let executable = plugin_root.join("bin").join(PLUGIN_BINARY);
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{LAUNCH_AGENT_LABEL}</string><key>ProgramArguments</key><array><string>{}</string><string>companion</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>",
            xml_escape(&executable.to_string_lossy())
        );
        fs::create_dir_all(&self.launch_agents_dir).map_err(|_| LifecycleError::State)?;
        fs::write(self.plist_path(), plist).map_err(|_| LifecycleError::State)
    }

    fn bootstrap_companion(&self) -> Result<(), LifecycleError> {
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        let service = format!("{domain}/{LAUNCH_AGENT_LABEL}");
        let _ = Command::new(&self.launchctl)
            .args(["bootout", &service])
            .status();
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
        let service = format!("gui/{}/{LAUNCH_AGENT_LABEL}", unsafe { libc::geteuid() });
        let status = Command::new(&self.launchctl)
            .args(["bootout", &service])
            .status()
            .map_err(|_| LifecycleError::Companion)?;
        if status.success() {
            return Ok(());
        }
        let print = Command::new(&self.launchctl)
            .args(["print", &service])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| LifecycleError::Companion)?;
        (!print.success())
            .then_some(())
            .ok_or(LifecycleError::Companion)
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("lifecycle.json")
    }

    fn plist_path(&self) -> PathBuf {
        self.launch_agents_dir
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
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

pub fn find_codex_owner_pid(ps_program: &Path, mut pid: u32) -> Option<u32> {
    for _ in 0..8 {
        let output = Command::new(ps_program)
            .args(["-o", "ppid=", "-o", "comm=", "-p"])
            .arg(pid.to_string())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let line = String::from_utf8(output.stdout).ok()?;
        let mut fields = line.trim().splitn(2, char::is_whitespace);
        let parent_pid = fields.next()?.parse().ok()?;
        let command = fields.next()?.trim();
        if Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "codex")
        {
            return Some(pid);
        }
        if parent_pid <= 1 || parent_pid == pid {
            return None;
        }
        pid = parent_pid;
    }
    None
}

fn running_desktop_codex_pids(ps_program: &Path) -> Vec<u32> {
    let output = match Command::new(ps_program)
        .args(["-axo", "pid=,comm="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return vec![0],
    };
    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().splitn(2, char::is_whitespace);
            let pid = fields.next()?.parse().ok()?;
            let command = fields.next()?.trim();
            (command.contains("/ChatGPT.app/")
                && Path::new(command)
                    .file_name()
                    .is_some_and(|name| name == "codex"))
            .then_some(pid)
        })
        .collect()
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
    if pid == 0 {
        return true;
    }
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn remove_if_present(path: &Path) -> Result<(), LifecycleError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(LifecycleError::State),
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
