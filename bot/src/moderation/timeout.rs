use std::{sync::Arc, time::Duration};

use serenity::model::id::{ChannelId, GuildId, UserId};
use tracing::{error, info, warn};

use super::{ModerationBackend, TimeoutBlocker, check_timeout_allowed};
use crate::detection::Trigger;

/// A threshold crossing that should be acted on.
#[derive(Debug, Clone)]
pub struct TimeoutRequest {
    pub guild_id: GuildId,
    pub user_id: UserId,
    pub user_name: String,
    /// Where to post the notice; `None` means log only.
    pub notify_channel: Option<ChannelId>,
    pub trigger: Trigger,
    pub required_detections: usize,
    pub window: Duration,
    pub duration: Duration,
}

/// What happened in response to a trigger.
#[derive(Debug, Clone, PartialEq)]
pub enum ModerationOutcome {
    /// Monitor-only mode: nothing was applied. `precheck` says whether a real timeout would have
    /// been allowed (`Err` holds the reason it would have been blocked or could not be checked).
    Simulated { precheck: Result<(), String> },
    /// The timeout was applied.
    TimedOut,
    /// A pre-check failed, so no timeout was attempted.
    Blocked(TimeoutBlocker),
    /// Discord rejected the request or could not be reached.
    Failed(String),
}

impl ModerationOutcome {
    pub fn summary(&self) -> String {
        match self {
            Self::Simulated { precheck: Ok(()) } => {
                "monitor-only: timeout would have been applied".into()
            }
            Self::Simulated { precheck: Err(e) } => {
                format!("monitor-only: timeout would have been blocked ({e})")
            }
            Self::TimedOut => "timed out".into(),
            Self::Blocked(reason) => format!("blocked: {reason}"),
            Self::Failed(e) => format!("failed: {e}"),
        }
    }
}

/// Applies (or, in monitor-only mode, simulates) timeouts.
#[derive(Clone)]
pub struct Moderator {
    backend: Arc<dyn ModerationBackend>,
    monitor_only: bool,
}

impl Moderator {
    pub fn new(backend: Arc<dyn ModerationBackend>, monitor_only: bool) -> Self {
        Self {
            backend,
            monitor_only,
        }
    }

    pub fn monitor_only(&self) -> bool {
        self.monitor_only
    }

    pub async fn handle_trigger(&self, request: &TimeoutRequest) -> ModerationOutcome {
        // The checks are read-only, so they also run in monitor-only mode. That lets admins
        // verify permissions and role order before enabling real timeouts.
        let precheck = match self
            .backend
            .timeout_facts(request.guild_id, request.user_id)
            .await
        {
            Ok(facts) => check_timeout_allowed(&facts).map_err(PrecheckError::Blocked),
            Err(e) => Err(PrecheckError::Unavailable(e.0)),
        };

        if self.monitor_only {
            return self.simulate(request, precheck).await;
        }

        match precheck {
            Err(PrecheckError::Blocked(blocker)) => {
                warn!(
                    user_id = %request.user_id,
                    reason = %blocker,
                    "timeout_blocked"
                );
                self.post(request, &blocked_message(request, &blocker.to_string()))
                    .await;
                ModerationOutcome::Blocked(blocker)
            }
            Err(PrecheckError::Unavailable(e)) => {
                error!(user_id = %request.user_id, error = %e, "timeout_precheck_failed");
                self.post(request, &failed_message(request, &e)).await;
                ModerationOutcome::Failed(e)
            }
            Ok(()) => self.apply(request).await,
        }
    }

    async fn simulate(
        &self,
        request: &TimeoutRequest,
        precheck: Result<(), PrecheckError>,
    ) -> ModerationOutcome {
        let precheck = precheck.map_err(|e| e.to_string());
        warn!(
            user_id = %request.user_id,
            emotion = "Angry",
            confidence = %request.trigger.peak_confidence,
            detections = request.trigger.count(),
            required = request.required_detections,
            precheck = %precheck.as_ref().err().map_or("passed", String::as_str),
            "[MONITOR ONLY] Timeout would have been applied to {}. Emotion: Anger, Confidence: {}, Detections: {}/{}",
            request.user_name,
            percent(request.trigger.peak_confidence),
            request.trigger.count(),
            request.required_detections,
        );
        self.post(request, &monitor_only_message(request, &precheck))
            .await;
        ModerationOutcome::Simulated { precheck }
    }

    async fn apply(&self, request: &TimeoutRequest) -> ModerationOutcome {
        let reason = audit_log_reason(request);
        match self
            .backend
            .apply_timeout(request.guild_id, request.user_id, request.duration, &reason)
            .await
        {
            Ok(()) => {
                warn!(
                    user_id = %request.user_id,
                    duration = request.duration.as_secs(),
                    confidence = %request.trigger.peak_confidence,
                    detections = request.trigger.count(),
                    "timeout_triggered"
                );
                self.post(request, &timeout_message(request)).await;
                ModerationOutcome::TimedOut
            }
            Err(e) => {
                error!(user_id = %request.user_id, error = %e, "timeout_failed");
                self.post(request, &failed_message(request, &e.0)).await;
                ModerationOutcome::Failed(e.0)
            }
        }
    }

    async fn post(&self, request: &TimeoutRequest, content: &str) {
        let Some(channel) = request.notify_channel else {
            return;
        };
        match self.backend.notify(channel, content).await {
            Ok(()) => info!(channel_id = %channel, "moderation_notice_sent"),
            Err(e) => warn!(channel_id = %channel, error = %e, "moderation_notice_failed"),
        }
    }
}

#[derive(Debug)]
enum PrecheckError {
    Blocked(TimeoutBlocker),
    Unavailable(String),
}

impl std::fmt::Display for PrecheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(b) => write!(f, "{b}"),
            Self::Unavailable(e) => write!(f, "could not verify permissions: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Message formatting
// ---------------------------------------------------------------------------------------------

/// `0.913` → `"91%"`.
pub fn percent(value: f32) -> String {
    format!("{:.0}%", (value * 100.0).clamp(0.0, 100.0))
}

/// Human-readable duration: "5 minutes", "1 hour 30 minutes", "45 seconds".
pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (days, hours, minutes, seconds) = (
        total / 86_400,
        total % 86_400 / 3600,
        total % 3600 / 60,
        total % 60,
    );
    let parts: Vec<String> = [
        (days, "day"),
        (hours, "hour"),
        (minutes, "minute"),
        (seconds, "second"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, unit)| format!("{n} {unit}{}", if n == 1 { "" } else { "s" }))
    .collect();
    if parts.is_empty() {
        "0 seconds".into()
    } else {
        parts.join(" ")
    }
}

fn detections_line(request: &TimeoutRequest) -> String {
    format!(
        "{} within {}",
        request.trigger.count(),
        format_duration(request.window)
    )
}

/// Reason given for every RageGuard timeout (audit log, notice, `/anger-config`).
pub const TIMEOUT_REASON: &str = "Repeated angry voice detection";

/// Audit-log reason: [`TIMEOUT_REASON`] plus the detection details.
pub fn audit_log_reason(request: &TimeoutRequest) -> String {
    format!(
        "RageGuard: {TIMEOUT_REASON} ({}, peak confidence {})",
        detections_line(request),
        percent(request.trigger.peak_confidence)
    )
}

pub fn timeout_message(request: &TimeoutRequest) -> String {
    format!(
        "⚠️ **RageGuard**\n\n<@{user}> has been timed out for {duration}.\n\n**Reason:**\n{TIMEOUT_REASON}\n\n**Confidence:**\n{confidence}\n\n**Detections:**\n{detections}",
        user = request.user_id,
        duration = format_duration(request.duration),
        confidence = percent(request.trigger.peak_confidence),
        detections = detections_line(request),
    )
}

pub fn monitor_only_message(request: &TimeoutRequest, precheck: &Result<(), String>) -> String {
    let precheck = match precheck {
        Ok(()) => "✅ A real timeout would have been allowed.".to_owned(),
        Err(reason) => format!("⛔ A real timeout would have been blocked: {reason}."),
    };
    format!(
        "🧪 **RageGuard - MONITOR ONLY**\n\nA timeout would have been applied to <@{user}> ({duration}). No action was taken.\n\n**Emotion:** Anger\n**Confidence:** {confidence}\n**Detections:** {count}/{required} within {window}\n\n{precheck}",
        user = request.user_id,
        duration = format_duration(request.duration),
        confidence = percent(request.trigger.peak_confidence),
        count = request.trigger.count(),
        required = request.required_detections,
        window = format_duration(request.window),
    )
}

fn blocked_message(request: &TimeoutRequest, reason: &str) -> String {
    format!(
        "⚠️ **RageGuard**\n\nRepeated angry speech was detected from <@{user}> ({detections}, peak confidence {confidence}), but the timeout was **not** applied: {reason}.",
        user = request.user_id,
        detections = detections_line(request),
        confidence = percent(request.trigger.peak_confidence),
    )
}

fn failed_message(request: &TimeoutRequest, error: &str) -> String {
    format!(
        "⚠️ **RageGuard**\n\nRepeated angry speech was detected from <@{user}>, but the timeout failed: {error}",
        user = request.user_id,
    )
}
