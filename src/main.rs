use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    AppServerClient, DashboardConfig, ObservedDashboardState, ObservedQuota, QuotaCache,
    render_dashboard,
};

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
        .ok_or("用法：codex-zectrix-dashboard <preview|live-preview> ...")?;
    if !matches!(command.as_str(), "preview" | "live-preview") {
        return Err("用法：codex-zectrix-dashboard <preview|live-preview> ...".into());
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
        let quota_cache_path = output.with_extension("quota.json");
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
        (
            ObservedDashboardState {
                quota,
                tasks: Vec::new(),
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
