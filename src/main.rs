use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    AppServerClient, DashboardConfig, ObservedDashboardState, ObservedQuota, ObservedTask,
    PublishAttempt, PublishCoordinator, PublisherState, QuotaCache, ReadonlyObservationConfig,
    ReadonlyRolloutObserver, TaskActivityCache, TaskActivitySnapshot, ZectrixPublisher,
    parse_hook_event, persist_hook_event, read_hook_events, reduce_task_activity, render_dashboard,
};

mod setup;

const CORRELATION_SALT: &str = "codex-zectrix-dashboard-v1";
const SUPPORTED_CLI_VERSION: &str = "0.147.0-alpha.6.5";
const SUPPORTED_SCHEMA_SHA256: &str =
    "cb29555a6be238d57dc4a1a8171f1107aa7b5bb0e9fb97a33c0ca112f3d37452";

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
    if command == "setup" {
        if args.next().is_some() {
            return Err("setup 不接受命令行参数".into());
        }
        return setup::run_setup();
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
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--privacy" => privacy_mode = true,
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
        let activity =
            activity_cache.update(observe_activity(&client, &data_dir, now_epoch_seconds));
        if !activity.stale {
            fs::write(&activity_cache_path, serde_json::to_vec(&activity.tasks)?)?;
        }
        (
            ObservedDashboardState {
                quota,
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
            previous_frame_hash: None,
        },
    )?;
    dashboard.frame.write_png(&output)?;
    println!("{}  {}", dashboard.frame.sha256, output.display());
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
        supported_cli_version: SUPPORTED_CLI_VERSION.into(),
        supported_schema_sha256: SUPPORTED_SCHEMA_SHA256.into(),
    });
    let mut events = observer.observe()?;
    events.extend(read_hook_events(&data_dir.join("hook-events.jsonl"))?);
    Ok(reduce_task_activity(metadata, events, now_epoch_seconds))
}

fn record_hook() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let now_epoch_seconds: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?;
    if let Ok(Some(event)) = parse_hook_event(&input, CORRELATION_SALT, now_epoch_seconds) {
        persist_hook_event(&data_dir()?.join("hook-events.jsonl"), &event)?;
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
    let mut has_activity = last_known_tasks.is_some();
    let mut activity_cache = TaskActivityCache::new(last_known_tasks);
    let publisher_state_path = data_dir.join("publisher-state.json");
    let publisher_state = fs::read(&publisher_state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PublisherState>(&bytes).ok())
        .unwrap_or_default();
    let mut coordinator = PublishCoordinator::new(
        DashboardConfig {
            privacy_mode: settings.privacy_mode,
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

    let mut cycles = 0;
    loop {
        let now_epoch_seconds = current_epoch_seconds()?;
        let quota = match client.read_quota() {
            Ok(quota) => {
                let mut quota = quota_cache.update::<std::convert::Infallible>(Ok(quota))?;
                if write_json_atomically(&quota_cache_path, &quota).is_err() {
                    quota.stale = true;
                    eprintln!("state_persist_unavailable");
                }
                quota
            }
            Err(error) => match quota_cache.update(Err(error)) {
                Ok(quota) => quota,
                Err(_) => {
                    eprintln!("observation_unavailable");
                    if finish_cycle(&mut cycles, max_cycles, poll_interval) {
                        break;
                    }
                    continue;
                }
            },
        };
        let activity = match observe_activity(&client, &data_dir, now_epoch_seconds) {
            Ok(snapshot) => {
                has_activity = true;
                let mut snapshot = activity_cache.update::<std::convert::Infallible>(Ok(snapshot));
                if write_json_atomically(&activity_cache_path, &snapshot.tasks).is_err() {
                    snapshot.stale = true;
                    eprintln!("state_persist_unavailable");
                }
                snapshot
            }
            Err(error) if has_activity => activity_cache.update(Err(error)),
            Err(_) => {
                eprintln!("observation_unavailable");
                if finish_cycle(&mut cycles, max_cycles, poll_interval) {
                    break;
                }
                continue;
            }
        };
        coordinator.observe(
            ObservedDashboardState {
                quota,
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
            let keychain = setup::Keychain::from_environment();
            let attempt = keychain.find().ok().flatten().and_then(|api_key| {
                ZectrixPublisher::new(&api_key, &base_url, &settings.device_id, settings.page_id)
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
