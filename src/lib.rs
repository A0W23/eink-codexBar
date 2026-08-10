mod activity_sources;
mod app_server;
mod publisher;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::Cursor;
use std::path::Path;

use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{DrawTarget, Pixel, Point};
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, StyledDrawable};
use image::GrayImage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts::{u8g2_font_logisoso24_tn, u8g2_font_wqy13_t_gb2312};
use u8g2_fonts::types::{FontColor, VerticalPosition};

pub use activity_sources::{
    ActivitySourceError, ReadonlyObservationConfig, ReadonlyRolloutObserver,
    compute_state_schema_fingerprint, parse_app_server_tasks, parse_hook_event, persist_hook_event,
    read_hook_events,
};
pub use app_server::{AppServerClient, AppServerError};
pub use publisher::{
    FramePublisher, MIN_PUSH_INTERVAL_SECONDS, PublishAttempt, PublishCoordinator, PublisherState,
    ZectrixPublishError, ZectrixPublisher,
};

pub const DISPLAY_WIDTH: u32 = 400;
pub const DISPLAY_HEIGHT: u32 = 300;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservedQuotaWindow {
    pub name: String,
    pub used_percent: u8,
    pub resets_at_epoch_seconds: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservedQuota {
    pub windows: Vec<ObservedQuotaWindow>,
    #[serde(default)]
    pub reset_credits: u64,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QuotaCache {
    last_known: Option<ObservedQuota>,
}

impl QuotaCache {
    pub fn new(last_known: Option<ObservedQuota>) -> Self {
        Self { last_known }
    }

    pub fn update<E>(
        &mut self,
        observation: Result<ObservedQuota, E>,
    ) -> Result<ObservedQuota, DashboardError> {
        match observation {
            Ok(mut quota) => {
                quota.stale = false;
                self.last_known = Some(quota.clone());
                Ok(quota)
            }
            Err(_) => stale_quota(self.last_known.as_ref()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Running,
    TurnCompleted,
    Failed,
    Interrupted,
}

impl ActivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "执行中",
            Self::TurnCompleted => "本轮完成",
            Self::Failed => "失败",
            Self::Interrupted => "已中断",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Failed => 1,
            Self::Interrupted => 2,
            Self::TurnCompleted => 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct CorrelationKey(String);

impl CorrelationKey {
    pub fn derive(external_id: &str, installation_salt: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"codex-zectrix-correlation-v1\0");
        digest.update(installation_salt.as_bytes());
        digest.update(b"\0");
        digest.update(external_id.as_bytes());
        Self(format!("c1:{:x}", digest.finalize()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfficialTaskMetadata {
    pub correlation: CorrelationKey,
    pub correlation_aliases: Vec<CorrelationKey>,
    pub title: String,
    pub parent_correlation: Option<CorrelationKey>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEventKind {
    UserSubmission,
    ToolActivity,
    RolloutStarted,
    TurnStopped,
    TurnFailed,
    TurnInterrupted,
}

impl ActivityEventKind {
    fn state(self) -> ActivityState {
        match self {
            Self::UserSubmission | Self::ToolActivity | Self::RolloutStarted => {
                ActivityState::Running
            }
            Self::TurnStopped => ActivityState::TurnCompleted,
            Self::TurnFailed => ActivityState::Failed,
            Self::TurnInterrupted => ActivityState::Interrupted,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivityEvent {
    pub correlation: CorrelationKey,
    pub kind: ActivityEventKind,
    pub observed_at_epoch_seconds: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskActivitySnapshot {
    pub tasks: Vec<ObservedTask>,
    pub stale: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TaskActivityCache {
    last_known: Option<Vec<ObservedTask>>,
}

impl TaskActivityCache {
    pub fn new(last_known: Option<Vec<ObservedTask>>) -> Self {
        Self { last_known }
    }

    pub fn update<E>(
        &mut self,
        observation: Result<TaskActivitySnapshot, E>,
    ) -> TaskActivitySnapshot {
        match observation {
            Ok(snapshot) => {
                self.last_known = Some(snapshot.tasks.clone());
                snapshot
            }
            Err(_) => TaskActivitySnapshot {
                tasks: self.last_known.clone().unwrap_or_default(),
                stale: true,
            },
        }
    }
}

pub fn reduce_task_activity(
    metadata: impl IntoIterator<Item = OfficialTaskMetadata>,
    events: impl IntoIterator<Item = ActivityEvent>,
    now_epoch_seconds: i64,
) -> TaskActivitySnapshot {
    let metadata: Vec<_> = metadata.into_iter().collect();
    let parent_by_child: HashMap<_, _> = metadata
        .iter()
        .filter_map(|task| {
            task.parent_correlation
                .as_ref()
                .map(|parent| (task.correlation.clone(), parent.clone()))
        })
        .collect();
    let alias_to_task: HashMap<_, _> = metadata
        .iter()
        .flat_map(|task| {
            task.correlation_aliases
                .iter()
                .cloned()
                .map(|alias| (alias, task.correlation.clone()))
        })
        .collect();
    let mut latest: HashMap<CorrelationKey, ActivityEvent> = HashMap::new();
    for mut event in events {
        if let Some(task) = alias_to_task.get(&event.correlation) {
            event.correlation = task.clone();
        }
        if let Some(parent) = parent_by_child.get(&event.correlation) {
            event.correlation = parent.clone();
        }
        let replace = latest.get(&event.correlation).is_none_or(|current| {
            event.observed_at_epoch_seconds >= current.observed_at_epoch_seconds
        });
        if replace {
            latest.insert(event.correlation.clone(), event);
        }
    }

    let mut stale = false;
    let mut tasks = Vec::new();
    for task in metadata
        .into_iter()
        .filter(|task| task.parent_correlation.is_none())
    {
        let Some(event) = latest.remove(&task.correlation) else {
            continue;
        };
        let age = now_epoch_seconds.saturating_sub(event.observed_at_epoch_seconds);
        let state = event.kind.state();
        if state == ActivityState::Running && age > 120 {
            stale = true;
            continue;
        }
        if state != ActivityState::Running && age > 24 * 60 * 60 {
            continue;
        }
        tasks.push(ObservedTask::new(
            task.title,
            state,
            event.observed_at_epoch_seconds,
        ));
    }
    TaskActivitySnapshot { tasks, stale }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservedTask {
    pub title: String,
    pub state: ActivityState,
    pub activity_at_epoch_seconds: i64,
}

impl ObservedTask {
    pub fn new(
        title: impl Into<String>,
        state: ActivityState,
        activity_at_epoch_seconds: i64,
    ) -> Self {
        Self {
            title: title.into(),
            state,
            activity_at_epoch_seconds,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservedDashboardState {
    pub quota: ObservedQuota,
    #[serde(default)]
    pub task_activity_stale: bool,
    pub tasks: Vec<ObservedTask>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub project_path: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub error_text: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedQuotaWindow {
    pub name: String,
    pub used_percent: u8,
    pub remaining_percent: u8,
    pub resets_at_epoch_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedQuota {
    pub windows: Vec<NormalizedQuotaWindow>,
    pub reset_credits: u64,
    pub stale: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedTask {
    pub title: Option<String>,
    pub state: ActivityState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedDashboardState {
    pub quota: NormalizedQuota,
    pub task_activity_stale: bool,
    pub tasks: Vec<NormalizedTask>,
    pub hidden_task_count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayConfig {
    pub privacy_mode: bool,
    pub previous_frame_hash: Option<String>,
}

pub type DashboardConfig = DisplayConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishDecision {
    Publish,
    Unchanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonochromeFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub sha256: String,
}

impl MonochromeFrame {
    pub fn png_bytes(&self) -> Result<Vec<u8>, DashboardError> {
        let image = GrayImage::from_raw(self.width, self.height, self.pixels.clone())
            .ok_or(DashboardError::InvalidFrame)?;
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(DashboardError::Image)?;
        Ok(output.into_inner())
    }

    pub fn write_png(&self, path: impl AsRef<Path>) -> Result<(), DashboardError> {
        std::fs::write(path, self.png_bytes()?)
            .map_err(image::ImageError::IoError)
            .map_err(DashboardError::Image)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardOutput {
    pub normalized: NormalizedDashboardState,
    pub frame: MonochromeFrame,
    pub visible_text: Vec<String>,
    pub publish_decision: PublishDecision,
}

#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("failed to render dashboard text: {0}")]
    Font(String),
    #[error("invalid frame dimensions")]
    InvalidFrame,
    #[error("failed to write preview image: {0}")]
    Image(#[source] image::ImageError),
    #[error("app-server 额度响应无效：{0}")]
    InvalidQuota(String),
    #[error("没有可用的上次额度数据")]
    MissingQuota,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerQuotaResponse {
    rate_limits: AppServerRateLimits,
    rate_limit_reset_credits: Option<AppServerResetCredits>,
}

#[derive(Deserialize)]
struct AppServerRateLimits {
    primary: AppServerQuotaWindow,
    secondary: Option<AppServerQuotaWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerQuotaWindow {
    used_percent: i64,
    window_duration_mins: i64,
    resets_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppServerResetCredits {
    available_count: i64,
}

pub fn parse_app_server_quota(response: &str) -> Result<ObservedQuota, DashboardError> {
    let response: AppServerQuotaResponse = serde_json::from_str(response)
        .map_err(|error| DashboardError::InvalidQuota(error.to_string()))?;
    let mut windows = vec![normalize_observed_window(response.rate_limits.primary)?];
    if let Some(secondary) = response.rate_limits.secondary {
        windows.push(normalize_observed_window(secondary)?);
    }
    let reset_credits = response
        .rate_limit_reset_credits
        .map(|credits| {
            u64::try_from(credits.available_count)
                .map_err(|_| DashboardError::InvalidQuota("重置额度数量不能为负数".into()))
        })
        .transpose()?
        .unwrap_or(0);

    Ok(ObservedQuota {
        windows,
        reset_credits,
        stale: false,
    })
}

pub fn stale_quota(last_known: Option<&ObservedQuota>) -> Result<ObservedQuota, DashboardError> {
    let mut quota = last_known.cloned().ok_or(DashboardError::MissingQuota)?;
    quota.stale = true;
    Ok(quota)
}

fn normalize_observed_window(
    window: AppServerQuotaWindow,
) -> Result<ObservedQuotaWindow, DashboardError> {
    if !(0..=100).contains(&window.used_percent) {
        return Err(DashboardError::InvalidQuota(
            "额度使用比例不在 0% 到 100% 之间".into(),
        ));
    }
    let used_percent = u8::try_from(window.used_percent).unwrap();
    if window.window_duration_mins <= 0 {
        return Err(DashboardError::InvalidQuota(
            "额度窗口时长必须大于零".into(),
        ));
    }
    let name = if window.window_duration_mins % (24 * 60) == 0 {
        format!("{} 天", window.window_duration_mins / (24 * 60))
    } else if window.window_duration_mins % 60 == 0 {
        format!("{} 小时", window.window_duration_mins / 60)
    } else {
        format!("{} 分钟", window.window_duration_mins)
    };
    Ok(ObservedQuotaWindow {
        name,
        used_percent,
        resets_at_epoch_seconds: window.resets_at,
    })
}

pub fn render_dashboard(
    observed: ObservedDashboardState,
    now_epoch_seconds: i64,
    config: DisplayConfig,
) -> Result<DashboardOutput, DashboardError> {
    let normalized = normalize_dashboard(observed, now_epoch_seconds, &config);
    render_normalized_dashboard(normalized, now_epoch_seconds, config)
}

pub fn normalize_dashboard(
    observed: ObservedDashboardState,
    now_epoch_seconds: i64,
    config: &DisplayConfig,
) -> NormalizedDashboardState {
    let mut tasks = observed.tasks;
    tasks.retain(|task| {
        task.state == ActivityState::Running
            || now_epoch_seconds.saturating_sub(task.activity_at_epoch_seconds) <= 24 * 60 * 60
    });
    tasks.sort_by_key(|task| {
        (
            task.state.priority(),
            Reverse(task.activity_at_epoch_seconds),
        )
    });
    let hidden_task_count = tasks.len().saturating_sub(3);
    tasks.truncate(3);

    NormalizedDashboardState {
        quota: NormalizedQuota {
            windows: observed
                .quota
                .windows
                .into_iter()
                .map(|window| NormalizedQuotaWindow {
                    name: window.name,
                    used_percent: window.used_percent.min(100),
                    remaining_percent: 100_u8.saturating_sub(window.used_percent),
                    resets_at_epoch_seconds: window.resets_at_epoch_seconds,
                })
                .collect(),
            reset_credits: observed.quota.reset_credits,
            stale: observed.quota.stale,
        },
        task_activity_stale: observed.task_activity_stale,
        tasks: tasks
            .into_iter()
            .map(|task| NormalizedTask {
                title: (!config.privacy_mode).then_some(task.title),
                state: task.state,
            })
            .collect(),
        hidden_task_count,
    }
}

pub fn render_normalized_dashboard(
    normalized: NormalizedDashboardState,
    now_epoch_seconds: i64,
    config: DisplayConfig,
) -> Result<DashboardOutput, DashboardError> {
    render_normalized_dashboard_with_sync(normalized, now_epoch_seconds, config, None)
}

pub fn render_normalized_dashboard_with_sync(
    normalized: NormalizedDashboardState,
    now_epoch_seconds: i64,
    config: DisplayConfig,
    last_successful_sync_epoch_seconds: Option<i64>,
) -> Result<DashboardOutput, DashboardError> {
    let (pixels, visible_text) = draw_dashboard(
        &normalized,
        now_epoch_seconds,
        last_successful_sync_epoch_seconds,
    )?;
    let sha256 = format!("{:x}", Sha256::digest(&pixels));
    let publish_decision = if config.previous_frame_hash.as_deref() == Some(&sha256) {
        PublishDecision::Unchanged
    } else {
        PublishDecision::Publish
    };

    Ok(DashboardOutput {
        normalized,
        frame: MonochromeFrame {
            width: DISPLAY_WIDTH,
            height: DISPLAY_HEIGHT,
            pixels,
            sha256,
        },
        visible_text,
        publish_decision,
    })
}

fn draw_dashboard(
    state: &NormalizedDashboardState,
    now_epoch_seconds: i64,
    last_successful_sync_epoch_seconds: Option<i64>,
) -> Result<(Vec<u8>, Vec<String>), DashboardError> {
    let mut display = FrameDisplay::new();
    let text_font = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let number_font = FontRenderer::new::<u8g2_font_logisoso24_tn>();
    let mut visible = Vec::new();

    match state.quota.windows.as_slice() {
        [window] => draw_quota_window(
            window,
            14,
            366,
            now_epoch_seconds,
            &text_font,
            &number_font,
            &mut display,
            &mut visible,
        )?,
        [first, second] => {
            draw_quota_window(
                first,
                14,
                172,
                now_epoch_seconds,
                &text_font,
                &number_font,
                &mut display,
                &mut visible,
            )?;
            draw_quota_window(
                second,
                214,
                172,
                now_epoch_seconds,
                &text_font,
                &number_font,
                &mut display,
                &mut visible,
            )?;
        }
        _ => {
            return Err(DashboardError::InvalidQuota(
                "额度窗口数量必须为一或二".into(),
            ));
        }
    }

    if state.quota.reset_credits > 0 {
        let credits = format!("重置额度 {}", state.quota.reset_credits);
        draw_text(&text_font, credits.as_str(), 286, 22, &mut display)?;
        visible.push(credits);
    }
    if state.quota.stale {
        draw_text(&text_font, "数据可能已过期", 286, 98, &mut display)?;
        visible.push("数据可能已过期".into());
    }

    Rectangle::new(Point::new(0, 103), Size::new(DISPLAY_WIDTH, 2))
        .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::On), &mut display)
        .unwrap();
    draw_text(&text_font, "任务动态", 14, 128, &mut display)?;
    visible.push("任务动态".into());
    if state.task_activity_stale {
        draw_text(&text_font, "任务数据可能已过期", 246, 128, &mut display)?;
        visible.push("任务数据可能已过期".into());
    }

    for (index, task) in state.tasks.iter().enumerate() {
        let y = 162 + index as i32 * 48;
        let label = task.state.label();
        draw_text(&text_font, label, 14, y, &mut display)?;
        visible.push(label.into());
        let title = task.title.as_deref().unwrap_or("隐私任务");
        draw_text(&text_font, title, 104, y, &mut display)?;
        visible.push(title.into());
        if index < state.tasks.len() - 1 {
            Rectangle::new(Point::new(14, y + 16), Size::new(366, 1))
                .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::On), &mut display)
                .unwrap();
        }
    }

    if state.hidden_task_count > 0 {
        let overflow = format!("另有 {} 项", state.hidden_task_count);
        draw_text(&text_font, overflow.as_str(), 286, 286, &mut display)?;
        visible.push(overflow);
    }

    if let Some(timestamp) = last_successful_sync_epoch_seconds {
        let seconds = timestamp.rem_euclid(24 * 60 * 60);
        let sync = format!(
            "上次同步 {:02}:{:02}Z",
            seconds / 3_600,
            seconds % 3_600 / 60
        );
        draw_text(&text_font, sync.as_str(), 304, 298, &mut display)?;
        visible.push(sync);
    }

    Ok((display.pixels, visible))
}

#[allow(clippy::too_many_arguments)]
fn draw_quota_window(
    window: &NormalizedQuotaWindow,
    x: i32,
    width: u32,
    now_epoch_seconds: i64,
    text_font: &FontRenderer,
    number_font: &FontRenderer,
    display: &mut FrameDisplay,
    visible: &mut Vec<String>,
) -> Result<(), DashboardError> {
    draw_text(text_font, window.name.as_str(), x, 22, display)?;
    visible.push(window.name.clone());

    let remaining = window.remaining_percent.to_string();
    draw_text(number_font, remaining.as_str(), x, 58, display)?;
    draw_text(text_font, "%", x + 50, 58, display)?;
    visible.push(format!("{remaining}%"));

    let used = format!("已用 {}%", window.used_percent);
    draw_text(text_font, used.as_str(), x + 78, 54, display)?;
    visible.push(used);

    Rectangle::new(Point::new(x, 66), Size::new(width, 12))
        .draw_styled(&PrimitiveStyle::with_stroke(BinaryColor::On, 1), display)
        .unwrap();
    let used_width = width.saturating_sub(4) * u32::from(window.used_percent) / 100;
    if used_width > 0 {
        Rectangle::new(Point::new(x + 2, 68), Size::new(used_width, 8))
            .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::On), display)
            .unwrap();
    }

    let reset = quota_reset_label(window.resets_at_epoch_seconds, now_epoch_seconds);
    draw_text(text_font, reset.as_str(), x, 96, display)?;
    visible.push(reset);
    Ok(())
}

pub(crate) fn quota_reset_label(resets_at_epoch_seconds: i64, now_epoch_seconds: i64) -> String {
    let seconds = resets_at_epoch_seconds
        .saturating_sub(now_epoch_seconds)
        .max(0);
    if seconds >= 86_400 {
        format!("重置 {} 天", (seconds + 86_399) / 86_400)
    } else if seconds >= 3_600 {
        format!("重置 {} 小时", (seconds + 3_599) / 3_600)
    } else {
        format!("重置 {} 分钟", (seconds + 59) / 60)
    }
}

fn draw_text<F: u8g2_fonts::Content>(
    font: &FontRenderer,
    content: F,
    x: i32,
    baseline_y: i32,
    display: &mut FrameDisplay,
) -> Result<(), DashboardError> {
    font.render(
        content,
        Point::new(x, baseline_y),
        VerticalPosition::Baseline,
        FontColor::Transparent(BinaryColor::On),
        display,
    )
    .map(|_| ())
    .map_err(|error| DashboardError::Font(format!("{error:?}")))
}

struct FrameDisplay {
    pixels: Vec<u8>,
}

impl FrameDisplay {
    fn new() -> Self {
        Self {
            pixels: vec![255; (DISPLAY_WIDTH * DISPLAY_HEIGHT) as usize],
        }
    }
}

impl OriginDimensions for FrameDisplay {
    fn size(&self) -> Size {
        Size::new(DISPLAY_WIDTH, DISPLAY_HEIGHT)
    }
}

impl DrawTarget for FrameDisplay {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x >= 0
                && point.y >= 0
                && point.x < DISPLAY_WIDTH as i32
                && point.y < DISPLAY_HEIGHT as i32
            {
                let index = point.y as usize * DISPLAY_WIDTH as usize + point.x as usize;
                self.pixels[index] = if color == BinaryColor::On { 0 } else { 255 };
            }
        }
        Ok(())
    }
}
