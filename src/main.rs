use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    AppServerClient, DashboardConfig, DisplayLocale, ObservedDashboardState, ObservedQuota,
    ObservedTask, PluginLifecycle, PublishAttempt, PublishCoordinator, PublisherState, QuotaCache,
    ReadonlyObservationConfig, ReadonlyRolloutObserver, TaskActivityAvailability,
    TaskActivityCache, TaskActivitySnapshot, ZectrixPublisher, find_codex_owner_pid,
    hook_is_tombstoned, parse_hook_event, persist_hook_event, read_hook_events, record_hook_owner,
    reduce_task_activity, render_dashboard,
};

mod setup;

const CORRELATION_SALT: &str = "codex-zectrix-dashboard-v1";

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum QuotaSourceStatus {
    Current,
    Stale,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskActivitySourceStatus {
    Inferred,
    Stale,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceStatus {
    quota: QuotaSourceStatus,
    task_activity: TaskActivitySourceStatus,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let command = args
        .next()
        .ok_or("用法：codex-zectrix-dashboard <preview|live-preview|setup|companion> ...")?;
    if command == "build-fingerprint" {
        if args.next().is_some() {
            return Err("build-fingerprint 不接受命令行参数".into());
        }
        println!(
            "{}",
            option_env!("CODEX_ZECTRIX_SOURCE_FINGERPRINT").unwrap_or("development")
        );
        return Ok(());
    }
    if command == "version" {
        if args.next().is_some() {
            return Err("version 不接受命令行参数".into());
        }
        println!(env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if command == "diagnostics" {
        if args.next().is_some() {
            return Err("diagnostics 不接受命令行参数".into());
        }
        return run_diagnostics();
    }
    if command == "setup" {
        if args.next().is_some() {
            return Err("setup 不接受命令行参数".into());
        }
        return setup::run_setup();
    }
    if command == "lifecycle" {
        return run_lifecycle(args.collect());
    }
    if command == "hook-record" {
        if args.next().is_some() {
            return Err("hook-record 不接受命令行参数".into());
        }
        return record_hook();
    }
    if command == "companion" {
        if args.next().is_some() {
            return Err("companion 不接受命令行参数".into());
        }
        return run_companion();
    }
    if !matches!(command.as_str(), "preview" | "live-preview") {
        return Err("用法：codex-zectrix-dashboard <preview|live-preview|setup> ...".into());
    }

    let mut input = None;
    let mut output = None;
    let mut privacy_mode = false;
    let mut locale = DisplayLocale::Chinese;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--privacy" => privacy_mode = true,
            "--language" => {
                let code = args.next().ok_or("--language 需要 zh 或 en")?;
                locale = DisplayLocale::from_code(&code).ok_or("--language 只支持 zh 或 en")?;
            }
            _ => return Err(format!("未知参数：{argument}").into()),
        }
    }

    let output = output.ok_or("缺少 --output")?;
    let now_epoch_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?;
    let (observed, render_time) = if command == "preview" {
        let input = input.ok_or("缺少 --input")?;
        (
            serde_json::from_slice::<ObservedDashboardState>(&fs::read(input)?)?,
            1_786_330_000,
        )
    } else {
        if input.is_some() {
            return Err("只有 preview 支持 --input".into());
        }
        let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
            .map(AppServerClient::new)
            .unwrap_or_default();
        let data_dir = data_dir()?;
        fs::create_dir_all(&data_dir)?;
        let quota_cache_path = data_dir.join("quota.json");
        let last_known = fs::read(&quota_cache_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ObservedQuota>(&bytes).ok());
        let mut quota_cache = QuotaCache::new(last_known);
        let quota = match client.read_quota() {
            Ok(quota) => {
                let quota = quota_cache.update::<std::convert::Infallible>(Ok(quota))?;
                fs::write(&quota_cache_path, serde_json::to_vec(&quota)?)?;
                quota
            }
            Err(error) => quota_cache.update(Err(error))?,
        };
        let activity_cache_path = data_dir.join("activity.json");
        let last_known_tasks = fs::read(&activity_cache_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<ObservedTask>>(&bytes).ok());
        let mut activity_cache = TaskActivityCache::new(last_known_tasks);
        let (activity, task_activity_availability) =
            match observe_activity(&client, &data_dir, now_epoch_seconds) {
                Ok(snapshot) => {
                    let snapshot = activity_cache.update::<std::convert::Infallible>(Ok(snapshot));
                    if !snapshot.stale {
                        fs::write(&activity_cache_path, serde_json::to_vec(&snapshot.tasks)?)?;
                    }
                    (snapshot, TaskActivityAvailability::Available)
                }
                Err(_) => unavailable_task_activity(),
            };
        (
            ObservedDashboardState {
                quota,
                task_activity_availability,
                task_activity_stale: activity.stale,
                tasks: activity.tasks,
                prompt: None,
                response: None,
                reasoning: None,
                project_path: None,
                tool: None,
                error_text: None,
                plan: None,
            },
            now_epoch_seconds,
        )
    };
    let dashboard = render_dashboard(
        observed,
        render_time,
        DashboardConfig {
            privacy_mode,
            locale,
            previous_frame_hash: None,
        },
    )?;
    dashboard.frame.write_png(&output)?;
    println!("{}  {}", dashboard.frame.sha256, output.display());
    Ok(())
}

fn run_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    let path = data_dir()?.join("source-status.json");
    let status = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SourceStatus>(&bytes).ok())
        .unwrap_or_default();
    println!(
        "quota_source={}\ntask_activity_source={}",
        match status.quota {
            QuotaSourceStatus::Current => "current",
            QuotaSourceStatus::Stale => "stale",
            QuotaSourceStatus::Unavailable => "unavailable",
        },
        match status.task_activity {
            TaskActivitySourceStatus::Inferred => "inferred",
            TaskActivitySourceStatus::Stale => "stale",
            TaskActivitySourceStatus::Unavailable => "unavailable",
        }
    );
    Ok(())
}

fn observe_activity(
    client: &AppServerClient,
    data_dir: &std::path::Path,
    now_epoch_seconds: i64,
) -> Result<TaskActivitySnapshot, Box<dyn std::error::Error>> {
    let metadata = client.read_task_metadata(CORRELATION_SALT)?;
    let codex_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or("无法确定 Codex 数据目录")?;
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home,
        installation_salt: CORRELATION_SALT.into(),
    });
    let hook_path = data_dir.join("hook-events.jsonl");
    let rollout_events = observer.observe();
    let hook_events = read_hook_events(&hook_path);
    let events = match (rollout_events, hook_events) {
        (Ok(mut rollout), Ok(hooks)) => {
            rollout.extend(hooks);
            rollout
        }
        (Err(error), Ok(hooks)) => {
            let snapshot = reduce_task_activity(metadata, hooks, now_epoch_seconds);
            if snapshot.tasks.is_empty() {
                return Err(Box::new(error));
            }
            return Ok(snapshot);
        }
        (Ok(rollout), Err(_)) => rollout,
        (Err(error), _) => return Err(Box::new(error)),
    };
    Ok(reduce_task_activity(metadata, events, now_epoch_seconds))
}

fn record_hook() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(executable) = env::current_exe() else {
        return Ok(());
    };
    if hook_is_tombstoned(&executable) {
        return Ok(());
    }
    let Ok(data_dir) = data_dir() else {
        return Ok(());
    };
    let owner_pid = env::var("CODEX_ZECTRIX_HOOK_OWNER_PID")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            let ps = env::var_os("CODEX_ZECTRIX_PS_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/bin/ps"));
            find_codex_owner_pid(&ps, unsafe { libc::getppid() as u32 })
        })
        .unwrap_or(0);
    record_hook_owner(&data_dir, owner_pid);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Ok(());
    }
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return Ok(());
    };
    let Ok(now_epoch_seconds) = elapsed.as_secs().try_into() else {
        return Ok(());
    };
    if let Ok(Some(event)) = parse_hook_event(&input, CORRELATION_SALT, now_epoch_seconds) {
        let _ = persist_hook_event(&data_dir.join("hook-events.jsonl"), &event);
    }
    Ok(())
}

fn run_lifecycle(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let action = arguments.first().map(String::as_str).ok_or(
        "用法：codex-zectrix-dashboard lifecycle <install|update|uninstall|resume|diagnostics>",
    )?;
    let mut plugin_root = None;
    let mut plugin_id = None;
    let mut index = 1;
    while index < arguments.len() {
        let value = arguments.get(index + 1).ok_or("生命周期参数缺少值")?;
        match arguments[index].as_str() {
            "--plugin-root" => plugin_root = Some(PathBuf::from(value)),
            "--plugin-id" => plugin_id = Some(value.clone()),
            _ => return Err("未知生命周期参数".into()),
        }
        index += 2;
    }
    let codex = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let launchctl = env::var_os("CODEX_ZECTRIX_LAUNCHCTL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/launchctl"));
    let launch_agents_dir = env::var_os("CODEX_ZECTRIX_LAUNCH_AGENTS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
        })
        .ok_or("无法确定 LaunchAgents 目录")?;
    let ps = env::var_os("CODEX_ZECTRIX_PS_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/ps"));
    let lifecycle = PluginLifecycle::new(data_dir()?, codex, launchctl, launch_agents_dir, ps);
    match action {
        "install" => lifecycle.install(
            plugin_root.as_deref().ok_or("install 缺少 --plugin-root")?,
            plugin_id.as_deref().ok_or("install 缺少 --plugin-id")?,
        )?,
        "update" => {
            lifecycle.begin_update(
                plugin_root.as_deref().ok_or("update 缺少 --plugin-root")?,
                plugin_id.as_deref().ok_or("update 缺少 --plugin-id")?,
            )?;
            println!("hooks_disabled=true\ncompanion_stopped=true\ndesktop_reload_required=true");
        }
        "uninstall" => {
            lifecycle.begin_uninstall()?;
            println!("hooks_disabled=true\ncompanion_stopped=true\ndesktop_reload_required=true");
        }
        "resume" => lifecycle.resume()?,
        "diagnostics" => print!("{}", lifecycle.diagnostics()?),
        _ => return Err("未知生命周期操作".into()),
    }
    Ok(())
}

fn run_companion() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let settings: setup::Settings =
        serde_json::from_slice(&fs::read(data_dir.join("settings.json"))?)?;
    let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(AppServerClient::new)
        .unwrap_or_default();

    let quota_cache_path = data_dir.join("quota.json");
    let last_known_quota = fs::read(&quota_cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ObservedQuota>(&bytes).ok());
    let mut quota_cache = QuotaCache::new(last_known_quota);
    let activity_cache_path = data_dir.join("activity.json");
    let last_known_tasks = fs::read(&activity_cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<ObservedTask>>(&bytes).ok());
    let mut activity_cache = TaskActivityCache::new(last_known_tasks);
    let publisher_state_path = data_dir.join("publisher-state.json");
    let publisher_state = fs::read(&publisher_state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PublisherState>(&bytes).ok())
        .unwrap_or_default();
    let source_status_path = data_dir.join("source-status.json");
    let mut source_status = fs::read(&source_status_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SourceStatus>(&bytes).ok())
        .unwrap_or_default();
    let mut coordinator = PublishCoordinator::new(
        DashboardConfig {
            privacy_mode: settings.privacy_mode,
            locale: settings.locale,
            previous_frame_hash: None,
        },
        publisher_state,
    );
    let max_cycles = env::var("CODEX_ZECTRIX_MAX_CYCLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let poll_interval = env::var("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(2));
    let base_url =
        env::var("CODEX_ZECTRIX_API_BASE").unwrap_or_else(|_| "https://cloud.zectrix.com".into());
    let api_key = setup::Keychain::from_environment().find().ok().flatten();

    let mut cycles = 0;
    loop {
        let now_epoch_seconds = current_epoch_seconds()?;
        let quota = match client.read_quota() {
            Ok(quota) => {
                source_status.quota = QuotaSourceStatus::Current;
                let mut quota = quota_cache.update::<std::convert::Infallible>(Ok(quota))?;
                if write_json_atomically(&quota_cache_path, &quota).is_err() {
                    quota.stale = true;
                    source_status.quota = QuotaSourceStatus::Stale;
                    eprintln!("state_persist_unavailable");
                }
                quota
            }
            Err(error) => match quota_cache.update(Err(error)) {
                Ok(quota) => {
                    source_status.quota = QuotaSourceStatus::Stale;
                    quota
                }
                Err(_) => {
                    source_status.quota = QuotaSourceStatus::Unavailable;
                    let _ = write_json_atomically(&source_status_path, &source_status);
                    eprintln!("observation_unavailable");
                    if finish_cycle(&mut cycles, max_cycles, poll_interval) {
                        break;
                    }
                    continue;
                }
            },
        };
        let (activity, task_activity_availability) =
            match observe_activity(&client, &data_dir, now_epoch_seconds) {
                Ok(snapshot) => {
                    let mut snapshot =
                        activity_cache.update::<std::convert::Infallible>(Ok(snapshot));
                    source_status.task_activity = if snapshot.stale {
                        TaskActivitySourceStatus::Stale
                    } else {
                        TaskActivitySourceStatus::Inferred
                    };
                    if write_json_atomically(&activity_cache_path, &snapshot.tasks).is_err() {
                        snapshot.stale = true;
                        source_status.task_activity = TaskActivitySourceStatus::Stale;
                        eprintln!("state_persist_unavailable");
                    }
                    (snapshot, TaskActivityAvailability::Available)
                }
                Err(_) => {
                    source_status.task_activity = TaskActivitySourceStatus::Unavailable;
                    eprintln!("observation_unavailable");
                    unavailable_task_activity()
                }
            };
        if write_json_atomically(&source_status_path, &source_status).is_err() {
            eprintln!("state_persist_unavailable");
        }
        coordinator.observe(
            ObservedDashboardState {
                quota,
                task_activity_availability,
                task_activity_stale: activity.stale,
                tasks: activity.tasks,
                prompt: None,
                response: None,
                reasoning: None,
                project_path: None,
                tool: None,
                error_text: None,
                plan: None,
            },
            now_epoch_seconds,
        );

        if coordinator.has_pending() {
            let attempt = api_key.as_ref().and_then(|api_key| {
                ZectrixPublisher::new(api_key, &base_url, &settings.device_id, settings.page_id)
                    .ok()
                    .and_then(|mut publisher| {
                        coordinator
                            .try_publish_with_reservation(
                                now_epoch_seconds,
                                &mut publisher,
                                |state| write_json_atomically(&publisher_state_path, state).is_ok(),
                            )
                            .ok()
                    })
            });
            match attempt {
                Some(PublishAttempt::Published | PublishAttempt::Unchanged) => {
                    if write_json_atomically(&publisher_state_path, coordinator.state()).is_err() {
                        eprintln!("state_persist_unavailable");
                    }
                }
                Some(PublishAttempt::Failed) => {
                    if write_json_atomically(&publisher_state_path, coordinator.state()).is_err() {
                        eprintln!("state_persist_unavailable");
                    }
                    eprintln!("publish_unavailable");
                }
                Some(PublishAttempt::ReservationFailed) => {
                    eprintln!("state_persist_unavailable");
                }
                None => eprintln!("publish_unavailable"),
                Some(PublishAttempt::Idle | PublishAttempt::Deferred { .. }) => {}
            }
        }

        if finish_cycle(&mut cycles, max_cycles, poll_interval) {
            break;
        }
    }
    Ok(())
}

fn unavailable_task_activity() -> (TaskActivitySnapshot, TaskActivityAvailability) {
    (
        TaskActivitySnapshot {
            tasks: Vec::new(),
            stale: false,
        },
        TaskActivityAvailability::Unavailable,
    )
}

fn current_epoch_seconds() -> Result<i64, Box<dyn std::error::Error>> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}

fn finish_cycle(cycles: &mut usize, max_cycles: Option<usize>, poll_interval: Duration) -> bool {
    *cycles += 1;
    if max_cycles.is_some_and(|maximum| *cycles >= maximum) {
        return true;
    }
    std::thread::sleep(poll_interval);
    false
}

fn write_json_atomically(
    path: &std::path::Path,
    value: &impl serde::Serialize,
) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn data_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("CODEX_ZECTRIX_DATA_DIR") {
        return Ok(path.into());
    }
    let home = env::var_os("HOME").ok_or("无法确定用户目录")?;
    Ok(PathBuf::from(home).join("Library/Application Support/codex-zectrix-dashboard"))
}
