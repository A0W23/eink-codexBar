use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    DashboardConfig, DashboardError, DashboardOutput, NormalizedDashboardState,
    ObservedDashboardState, normalize_dashboard, render_normalized_dashboard_with_sync,
};

pub const MIN_PUSH_INTERVAL_SECONDS: i64 = 60;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherState {
    pub last_successful_sync_epoch_seconds: Option<i64>,
    pub next_allowed_push_epoch_seconds: Option<i64>,
    pub last_frame_hash: Option<String>,
    pub last_visible_state_hash: Option<String>,
    pub last_reset_at_epoch_seconds: Vec<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishAttempt {
    Idle,
    Deferred { until_epoch_seconds: i64 },
    Unchanged,
    Published,
    Failed,
    ReservationFailed,
}

pub trait FramePublisher {
    type Error;

    fn publish(&mut self, dashboard: &DashboardOutput) -> Result<(), Self::Error>;
}

pub struct ZectrixPublisher {
    client: Client,
    base_url: String,
    device_id: String,
    page_id: u8,
}

#[derive(Debug, Error)]
pub enum ZectrixPublishError {
    #[error("ZECTRIX 发布配置无效")]
    InvalidConfiguration,
    #[error("无法编码看板图像")]
    Image,
    #[error("ZECTRIX 发布暂时不可用")]
    Unavailable,
}

impl ZectrixPublisher {
    pub fn new(
        api_key: &str,
        base_url: impl Into<String>,
        device_id: impl Into<String>,
        page_id: u8,
    ) -> Result<Self, ZectrixPublishError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-API-Key",
            HeaderValue::from_str(api_key)
                .map_err(|_| ZectrixPublishError::InvalidConfiguration)?,
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| ZectrixPublishError::InvalidConfiguration)?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            device_id: device_id.into(),
            page_id,
        })
    }
}

impl FramePublisher for ZectrixPublisher {
    type Error = ZectrixPublishError;

    fn publish(&mut self, dashboard: &DashboardOutput) -> Result<(), Self::Error> {
        let image = dashboard
            .frame
            .png_bytes()
            .map_err(|_| ZectrixPublishError::Image)?;
        if image.len() > 2 * 1024 * 1024 {
            return Err(ZectrixPublishError::Image);
        }
        let image = Part::bytes(image)
            .file_name("codex-dashboard.png")
            .mime_str("image/png")
            .map_err(|_| ZectrixPublishError::Image)?;
        let form = Form::new()
            .text("dither", "false")
            .text("pageId", self.page_id.to_string())
            .part("images", image);
        let response = self
            .client
            .post(format!(
                "{}/open/v1/devices/{}/display/image",
                self.base_url, self.device_id
            ))
            .multipart(form)
            .send()
            .map_err(|_| ZectrixPublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(ZectrixPublishError::Unavailable);
        }
        let response: ApiCodeResponse = response
            .json()
            .map_err(|_| ZectrixPublishError::Unavailable)?;
        if response.code != 0 {
            return Err(ZectrixPublishError::Unavailable);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct ApiCodeResponse {
    code: i64,
}

struct PendingDashboard {
    normalized: NormalizedDashboardState,
}

pub struct PublishCoordinator {
    config: DashboardConfig,
    state: PublisherState,
    pending: Option<PendingDashboard>,
    latest_observed: Option<NormalizedDashboardState>,
    latest_visible_hash: Option<String>,
}

impl PublishCoordinator {
    pub fn new(config: DashboardConfig, state: PublisherState) -> Self {
        Self {
            config,
            state,
            pending: None,
            latest_observed: None,
            latest_visible_hash: None,
        }
    }

    pub fn observe(&mut self, observed: ObservedDashboardState, now_epoch_seconds: i64) -> bool {
        let normalized = normalize_dashboard(observed, now_epoch_seconds, &self.config);
        let current_visible_hash =
            visible_state_hash(&normalized, now_epoch_seconds, self.config.locale);
        let changed = if let Some(previous) = self.latest_observed.as_ref() {
            self.latest_visible_hash.as_deref() != Some(&current_visible_hash)
                || reset_labels_changed(
                    previous,
                    &normalized,
                    now_epoch_seconds,
                    self.config.locale,
                )
        } else {
            self.state.last_visible_state_hash.as_deref() != Some(&current_visible_hash)
                || persisted_reset_labels_changed(
                    &self.state.last_reset_at_epoch_seconds,
                    &normalized,
                    now_epoch_seconds,
                    self.config.locale,
                )
        };
        self.latest_observed = Some(normalized.clone());
        self.latest_visible_hash = Some(current_visible_hash.clone());
        if changed {
            self.pending = Some(PendingDashboard { normalized });
        } else if let Some(pending) = self.pending.as_mut() {
            pending.normalized = normalized;
        }
        changed
    }

    pub fn try_publish<P: FramePublisher>(
        &mut self,
        now_epoch_seconds: i64,
        publisher: &mut P,
    ) -> Result<PublishAttempt, DashboardError> {
        self.try_publish_with_reservation(now_epoch_seconds, publisher, |_| true)
    }

    pub fn try_publish_with_reservation<P, R>(
        &mut self,
        now_epoch_seconds: i64,
        publisher: &mut P,
        mut reserve: R,
    ) -> Result<PublishAttempt, DashboardError>
    where
        P: FramePublisher,
        R: FnMut(&PublisherState) -> bool,
    {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(PublishAttempt::Idle);
        };
        let successful_interval = self
            .state
            .last_successful_sync_epoch_seconds
            .map(|last_success| last_success.saturating_add(MIN_PUSH_INTERVAL_SECONDS));
        let until_epoch_seconds = successful_interval
            .into_iter()
            .chain(self.state.next_allowed_push_epoch_seconds)
            .max();
        if let Some(until_epoch_seconds) = until_epoch_seconds
            && now_epoch_seconds < until_epoch_seconds
        {
            return Ok(PublishAttempt::Deferred {
                until_epoch_seconds,
            });
        }

        let dashboard = render_normalized_dashboard_with_sync(
            pending.normalized.clone(),
            now_epoch_seconds,
            DashboardConfig {
                previous_frame_hash: None,
                ..self.config.clone()
            },
            Some(now_epoch_seconds),
        )?;
        let published_visible_hash =
            visible_state_hash(&pending.normalized, now_epoch_seconds, self.config.locale);
        if self.state.last_frame_hash.as_deref() == Some(&dashboard.frame.sha256) {
            self.state.last_visible_state_hash = Some(published_visible_hash.clone());
            self.state.last_reset_at_epoch_seconds = reset_timestamps(&pending.normalized);
            self.latest_visible_hash = Some(published_visible_hash);
            self.pending = None;
            return Ok(PublishAttempt::Unchanged);
        }
        let previous_reservation = self.state.next_allowed_push_epoch_seconds;
        self.state.next_allowed_push_epoch_seconds =
            Some(now_epoch_seconds.saturating_add(MIN_PUSH_INTERVAL_SECONDS));
        if !reserve(&self.state) {
            self.state.next_allowed_push_epoch_seconds = previous_reservation;
            return Ok(PublishAttempt::ReservationFailed);
        }
        if publisher.publish(&dashboard).is_err() {
            return Ok(PublishAttempt::Failed);
        }

        self.state.last_successful_sync_epoch_seconds = Some(now_epoch_seconds);
        self.state.next_allowed_push_epoch_seconds = None;
        self.state.last_frame_hash = Some(dashboard.frame.sha256);
        self.state.last_visible_state_hash = Some(published_visible_hash.clone());
        self.state.last_reset_at_epoch_seconds = reset_timestamps(&pending.normalized);
        self.latest_visible_hash = Some(published_visible_hash);
        self.pending = None;
        Ok(PublishAttempt::Published)
    }

    pub fn state(&self) -> &PublisherState {
        &self.state
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

fn visible_state_hash(
    state: &NormalizedDashboardState,
    now_epoch_seconds: i64,
    locale: crate::DisplayLocale,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"codex-zectrix-visible-state-v5\0");
    digest.update(locale.code().as_bytes());
    digest.update(crate::current_date_label_for(now_epoch_seconds, locale).as_bytes());
    for window in &state.quota.windows {
        digest.update(window.name.as_bytes());
        digest.update([0, window.used_percent, window.remaining_percent]);
    }
    digest.update(state.quota.reset_credits.to_be_bytes());
    digest.update([
        state.quota.stale.into(),
        state.task_activity_availability as u8,
        state.task_activity_stale.into(),
    ]);
    for task in &state.tasks {
        if let Some(title) = &task.title {
            digest.update(title.as_bytes());
        }
        digest.update([0, task.state as u8]);
    }
    digest.update(state.hidden_task_count.to_be_bytes());
    format!("{:x}", digest.finalize())
}

fn reset_labels_changed(
    previous: &NormalizedDashboardState,
    current: &NormalizedDashboardState,
    now_epoch_seconds: i64,
    locale: crate::DisplayLocale,
) -> bool {
    let previous = reset_timestamps(previous);
    let current = reset_timestamps(current);
    previous != current
        && previous
            .iter()
            .map(|timestamp| crate::quota_reset_label_for(*timestamp, now_epoch_seconds, locale))
            .ne(current.iter().map(|timestamp| {
                crate::quota_reset_label_for(*timestamp, now_epoch_seconds, locale)
            }))
}

fn persisted_reset_labels_changed(
    previous: &[i64],
    current: &NormalizedDashboardState,
    now_epoch_seconds: i64,
    locale: crate::DisplayLocale,
) -> bool {
    let current = reset_timestamps(current);
    previous != current
        && previous
            .iter()
            .map(|timestamp| crate::quota_reset_label_for(*timestamp, now_epoch_seconds, locale))
            .ne(current.iter().map(|timestamp| {
                crate::quota_reset_label_for(*timestamp, now_epoch_seconds, locale)
            }))
}

fn reset_timestamps(state: &NormalizedDashboardState) -> Vec<i64> {
    state
        .quota
        .windows
        .iter()
        .map(|window| window.resets_at_epoch_seconds)
        .collect()
}
