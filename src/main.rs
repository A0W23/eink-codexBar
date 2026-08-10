use std::env;
use std::fs;
use std::path::PathBuf;

use codex_zectrix_dashboard::{DashboardConfig, ObservedDashboardState, render_dashboard};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("preview") {
        return Err("usage: codex-zectrix-dashboard preview --input <fixture.json> --output <preview.png> [--privacy]".into());
    }

    let mut input = None;
    let mut output = None;
    let mut privacy_mode = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--privacy" => privacy_mode = true,
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    let input = input.ok_or("missing --input")?;
    let output = output.ok_or("missing --output")?;
    let observed: ObservedDashboardState = serde_json::from_slice(&fs::read(input)?)?;
    let dashboard = render_dashboard(
        observed,
        1_786_330_000,
        DashboardConfig {
            privacy_mode,
            previous_frame_hash: None,
        },
    )?;
    dashboard.frame.write_png(&output)?;
    println!("{}  {}", dashboard.frame.sha256, output.display());
    Ok(())
}
