use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    AppServerClient, DashboardConfig, ObservedDashboardState, ObservedQuota, ObservedTask,
    QuotaCache, ReadonlyObservationConfig, ReadonlyRolloutObserver, TaskActivityCache,
    TaskActivitySnapshot, parse_hook_event, persist_hook_event, read_hook_events,
    reduce_task_activity, render_dashboard,
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
        .ok_or("用法：codex-zectrix-dashboard <preview|live-preview|setup> ...")?;
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

fn data_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("CODEX_ZECTRIX_DATA_DIR") {
        return Ok(path.into());
    }
    let home = env::var_os("HOME").ok_or("无法确定用户目录")?;
    Ok(PathBuf::from(home).join("Library/Application Support/codex-zectrix-dashboard"))
}
