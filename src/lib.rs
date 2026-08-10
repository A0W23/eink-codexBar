use std::cmp::Reverse;
use std::convert::Infallible;
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

pub const DISPLAY_WIDTH: u32 = 400;
pub const DISPLAY_HEIGHT: u32 = 300;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservedQuotaWindow {
    pub name: String,
    pub used_percent: u8,
    pub resets_at_epoch_seconds: i64,
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
    fn label(self) -> &'static str {
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
    pub quota: ObservedQuotaWindow,
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
pub struct NormalizedTask {
    pub title: Option<String>,
    pub state: ActivityState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedDashboardState {
    pub quota: NormalizedQuotaWindow,
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
    pub fn write_png(&self, path: impl AsRef<Path>) -> Result<(), DashboardError> {
        let image = GrayImage::from_raw(self.width, self.height, self.pixels.clone())
            .ok_or(DashboardError::InvalidFrame)?;
        image.save(path).map_err(DashboardError::Image)
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
        quota: NormalizedQuotaWindow {
            name: observed.quota.name,
            used_percent: observed.quota.used_percent.min(100),
            remaining_percent: 100_u8.saturating_sub(observed.quota.used_percent),
            resets_at_epoch_seconds: observed.quota.resets_at_epoch_seconds,
        },
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
    let (pixels, visible_text) = draw_dashboard(&normalized, now_epoch_seconds)?;
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
) -> Result<(Vec<u8>, Vec<String>), DashboardError> {
    let mut display = FrameDisplay::new();
    let text_font = FontRenderer::new::<u8g2_font_wqy13_t_gb2312>();
    let number_font = FontRenderer::new::<u8g2_font_logisoso24_tn>();
    let mut visible = Vec::new();

    let quota_heading = format!("配额 | {}", state.quota.name);
    draw_text(&text_font, quota_heading.as_str(), 14, 22, &mut display)?;
    visible.push(quota_heading);

    let remaining = state.quota.remaining_percent.to_string();
    draw_text(&number_font, remaining.as_str(), 14, 58, &mut display)?;
    draw_text(&text_font, "%", 62, 58, &mut display)?;
    visible.push(format!("{remaining}%"));

    let used = format!("剩余  已用 {}%", state.quota.used_percent);
    draw_text(&text_font, used.as_str(), 92, 54, &mut display)?;
    visible.push(used);

    Rectangle::new(Point::new(92, 66), Size::new(288, 14))
        .draw_styled(
            &PrimitiveStyle::with_stroke(BinaryColor::On, 1),
            &mut display,
        )
        .unwrap();
    let used_width = 284 * u32::from(state.quota.used_percent) / 100;
    if used_width > 0 {
        Rectangle::new(Point::new(94, 68), Size::new(used_width, 10))
            .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::On), &mut display)
            .unwrap();
    }

    let seconds_until_reset = state
        .quota
        .resets_at_epoch_seconds
        .saturating_sub(now_epoch_seconds)
        .max(0);
    let reset = if seconds_until_reset >= 3_600 {
        format!("重置 {} 小时", (seconds_until_reset + 3_599) / 3_600)
    } else {
        format!("重置 {} 分钟", (seconds_until_reset + 59) / 60)
    };
    draw_text(&text_font, reset.as_str(), 92, 96, &mut display)?;
    visible.push(reset);

    Rectangle::new(Point::new(0, 103), Size::new(DISPLAY_WIDTH, 2))
        .draw_styled(&PrimitiveStyle::with_fill(BinaryColor::On), &mut display)
        .unwrap();
    draw_text(&text_font, "任务动态", 14, 128, &mut display)?;
    visible.push("任务动态".into());

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

    Ok((display.pixels, visible))
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
