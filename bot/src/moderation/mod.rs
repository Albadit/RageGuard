//! Turning a detection trigger into a (possibly simulated) Discord timeout.

mod permissions;
mod timeout;

use std::time::Duration;

use async_trait::async_trait;
use serenity::model::id::{ChannelId, GuildId, UserId};

pub use permissions::{
    RoleInfo, TimeoutBlocker, TimeoutFacts, check_timeout_allowed, guild_permissions,
    has_moderation_permission, highest_role_position,
};
pub use timeout::{
    ModerationOutcome, Moderator, TIMEOUT_REASON, TimeoutRequest, audit_log_reason,
    format_duration, monitor_only_message, percent, timeout_message,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModerationError(pub String);

impl From<serenity::Error> for ModerationError {
    fn from(err: serenity::Error) -> Self {
        Self(format!("Discord API error: {err}"))
    }
}

/// The Discord operations moderation needs. Implemented with serenity in production and mocked
/// in tests.
#[async_trait]
pub trait ModerationBackend: Send + Sync + 'static {
    /// Gathers everything needed to decide whether a timeout is allowed. Read-only.
    async fn timeout_facts(
        &self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<TimeoutFacts, ModerationError>;

    /// Applies a communication timeout.
    async fn apply_timeout(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        duration: Duration,
        reason: &str,
    ) -> Result<(), ModerationError>;

    /// Posts a notice to a text channel without pinging anyone.
    async fn notify(&self, channel_id: ChannelId, content: &str) -> Result<(), ModerationError>;
}
