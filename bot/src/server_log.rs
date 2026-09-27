//! The server log: warnings for moderators, posted to the log channel chosen with `/anger-setup`.
//!
//! Only warnings reach Discord (voice problems, the AI service going down); everyday activity
//! such as "listening" or "started monitoring" stays in the bot's own log (stdout /
//! `docker compose logs bot`). Trigger notices are posted separately by the moderator. Nothing is
//! posted until a log channel has been chosen.

use std::{sync::Arc, time::Duration};

use serenity::model::id::{ChannelId, GuildId, UserId};
use tracing::warn;

use crate::{
    moderation::{ModerationBackend, format_duration, percent},
    state::SettingsStore,
};

/// Something moderators should see in the server log.
#[derive(Debug, Clone, PartialEq)]
pub enum LogEvent {
    MonitoringStarted {
        by: UserId,
        target: UserId,
        /// `None` when the member was not in voice yet.
        channel: Option<ChannelId>,
    },
    MonitoringStopped {
        by: UserId,
        target: UserId,
        segments_analyzed: u64,
        triggers: u32,
    },
    /// The bot is now listening in a (new) voice channel.
    Listening {
        target: UserId,
        channel: ChannelId,
    },
    /// The member left voice; the bot left too and waits for them.
    WaitingForVoice {
        target: UserId,
    },
    /// Joining the member's voice channel failed (reported once per outage).
    VoiceProblem {
        target: UserId,
        channel: ChannelId,
        error: String,
    },
    /// One angry segment that counts towards a trigger.
    AngerDetected {
        target: UserId,
        confidence: f32,
        count: usize,
        required: usize,
        window: Duration,
    },
    /// The AI service stopped answering (reported once per outage).
    AiUnavailable {
        error: String,
    },
    AiRecovered,
}

impl LogEvent {
    /// Whether the event is posted to Discord. Everything else is informational.
    pub fn is_warning(&self) -> bool {
        matches!(self, Self::VoiceProblem { .. } | Self::AiUnavailable { .. })
    }

    pub fn message(&self) -> String {
        match self {
            Self::MonitoringStarted {
                by,
                target,
                channel: Some(channel),
            } => format!("🎙️ <@{by}> started monitoring <@{target}> in <#{channel}>."),
            Self::MonitoringStarted {
                by,
                target,
                channel: None,
            } => format!(
                "🎙️ <@{by}> started monitoring <@{target}>. Waiting for them to join a voice channel."
            ),
            Self::MonitoringStopped {
                by,
                target,
                segments_analyzed,
                triggers,
            } => format!(
                "⏹️ <@{by}> stopped monitoring <@{target}> ({segments_analyzed} segment(s) analysed, {triggers} trigger(s))."
            ),
            Self::Listening { target, channel } => {
                format!("🔊 Listening to <@{target}> in <#{channel}>.")
            }
            Self::WaitingForVoice { target } => format!(
                "⏳ <@{target}> left voice. I left too and will rejoin when they join a voice channel."
            ),
            Self::VoiceProblem {
                target,
                channel,
                error,
            } => format!(
                "⚠️ Couldn't join <#{channel}> to follow <@{target}>: {error} I'll keep retrying."
            ),
            Self::AngerDetected {
                target,
                confidence,
                count,
                required,
                window,
            } => format!(
                "😠 Angry speech from <@{target}> ({}). {count}/{required} within {}.",
                percent(*confidence),
                format_duration(*window)
            ),
            Self::AiUnavailable { error } => {
                format!("⚠️ The AI service isn't responding, so voice analysis is paused: {error}")
            }
            Self::AiRecovered => "✅ The AI service is back; voice analysis resumed.".to_owned(),
        }
    }
}

/// Posts [`LogEvent`]s to one server's chosen log channel.
#[derive(Clone)]
pub struct ServerLog {
    backend: Arc<dyn ModerationBackend>,
    settings: Arc<SettingsStore>,
    guild_id: GuildId,
}

impl ServerLog {
    pub fn new(
        backend: Arc<dyn ModerationBackend>,
        settings: Arc<SettingsStore>,
        guild_id: GuildId,
    ) -> Self {
        Self {
            backend,
            settings,
            guild_id,
        }
    }

    /// Posts the event if a log channel is chosen. Failures are logged, never propagated.
    /// Posts the event if it is a warning and a log channel is chosen. Failures are logged,
    /// never propagated.
    pub async fn post(&self, event: LogEvent) {
        if !event.is_warning() {
            return;
        }
        let Some(channel) = self.settings.get(self.guild_id).await.log_channel else {
            return;
        };
        if let Err(e) = self.backend.notify(channel, &event.message()).await {
            warn!(guild_id = %self.guild_id, channel_id = %channel, error = %e, "server_log_post_failed");
        }
    }
}
