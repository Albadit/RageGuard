use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serenity::model::{
    Timestamp,
    id::{ChannelId, GuildId, UserId},
};
use tokio::sync::RwLock;
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use crate::{ai::Emotion, detection::AngerDetector};

/// Immutable facts about a monitoring session, recorded when it starts.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub guild_id: GuildId,
    pub user_id: UserId,
    pub user_name: String,
    pub started_by: UserId,
    pub started_by_name: String,
    pub started_at: Timestamp,
    /// Voice channel the user was in when monitoring started (`None`: they weren't in voice yet).
    pub initial_channel: Option<ChannelId>,
    /// Text channel the command was used in; fallback for moderation notices.
    pub command_channel: ChannelId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Connecting,
    Listening,
    WaitingForTarget,
    /// The member is in voice, but RageGuard is listening in another channel where more monitored
    /// members are (a bot can only be in one voice channel per server).
    InOtherChannel,
    Reconnecting,
}

impl SessionStatus {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting to voice",
            Self::Listening => "Listening",
            Self::WaitingForTarget => "Waiting (user is not in a voice channel)",
            Self::InOtherChannel => {
                "Waiting (RageGuard is listening to other monitored members in another channel)"
            }
            Self::Reconnecting => "Reconnecting to voice",
        }
    }
}

/// Most recent model output for the monitored user.
#[derive(Debug, Clone, PartialEq)]
pub struct LastAnalysis {
    pub emotion: Emotion,
    pub confidence: f32,
    pub angry_score: f32,
    pub at: Instant,
}

/// Mutable per-session state. Guarded by a synchronous mutex because every critical section is
/// short and never spans an `.await`.
#[derive(Debug)]
pub struct SessionState {
    pub status: SessionStatus,
    pub voice_channel: Option<ChannelId>,
    pub detector: AngerDetector,
    pub last_analysis: Option<LastAnalysis>,
    pub segments_analyzed: u64,
    pub segments_dropped: u64,
    pub ai_failures: u64,
    pub last_error: Option<String>,
    pub triggers: u32,
    pub last_action: Option<String>,
}

/// Messages to a server's voice supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSignal {
    /// Something changed; compare where the monitored members are with where the bot is.
    Reconcile(&'static str),
    /// The voice driver gave up on the connection; force a fresh one.
    ConnectionLost(String),
}

/// One monitored user in one guild.
#[derive(Debug)]
pub struct MonitorSession {
    pub info: MonitorInfo,
    state: Mutex<SessionState>,
    cancel: CancellationToken,
}

impl MonitorSession {
    pub fn new(info: MonitorInfo) -> Arc<Self> {
        Arc::new(Self {
            info,
            state: Mutex::new(SessionState {
                status: SessionStatus::Connecting,
                voice_channel: None,
                detector: AngerDetector::new(),
                last_analysis: None,
                segments_analyzed: 0,
                segments_dropped: 0,
                ai_failures: 0,
                last_error: None,
                triggers: 0,
                last_action: None,
            }),
            cancel: CancellationToken::new(),
        })
    }

    pub fn with_state<R>(&self, f: impl FnOnce(&mut SessionState) -> R) -> R {
        f(&mut self.state.lock())
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.cancel.cancelled()
    }

    pub fn snapshot(&self, now: Instant, window: Duration) -> SessionSnapshot {
        let state = self.state.lock();
        SessionSnapshot {
            info: self.info.clone(),
            status: state.status,
            voice_channel: state.voice_channel,
            last_analysis: state.last_analysis.clone(),
            anger_count: state.detector.count(now, window),
            segments_analyzed: state.segments_analyzed,
            segments_dropped: state.segments_dropped,
            ai_failures: state.ai_failures,
            last_error: state.last_error.clone(),
            triggers: state.triggers,
            last_action: state.last_action.clone(),
        }
    }
}

/// Point-in-time copy of a session for display.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub info: MonitorInfo,
    pub status: SessionStatus,
    pub voice_channel: Option<ChannelId>,
    pub last_analysis: Option<LastAnalysis>,
    pub anger_count: usize,
    pub segments_analyzed: u64,
    pub segments_dropped: u64,
    pub ai_failures: u64,
    pub last_error: Option<String>,
    pub triggers: u32,
    pub last_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("<@{user}> is already being monitored (started by <@{started_by}>).")]
    AlreadyMonitoring { user: UserId, started_by: UserId },
    #[error("<@{0}> is not being monitored.")]
    NotMonitoring(UserId),
}

/// All active sessions, any number per guild but one per member.
///
/// Check-and-insert happens under a single write lock, so two moderators starting monitoring of
/// the same member at the same moment cannot both succeed.
#[derive(Debug, Default)]
pub struct MonitorRegistry {
    sessions: RwLock<HashMap<GuildId, HashMap<UserId, Arc<MonitorSession>>>>,
}

impl MonitorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn try_insert(&self, session: Arc<MonitorSession>) -> Result<(), RegistryError> {
        let mut guilds = self.sessions.write().await;
        let guild = guilds.entry(session.info.guild_id).or_default();
        if let Some(existing) = guild.get(&session.info.user_id) {
            return Err(RegistryError::AlreadyMonitoring {
                user: existing.info.user_id,
                started_by: existing.info.started_by,
            });
        }
        guild.insert(session.info.user_id, session);
        Ok(())
    }

    /// Every session in the guild, oldest first.
    pub async fn sessions(&self, guild_id: GuildId) -> Vec<Arc<MonitorSession>> {
        let mut sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .get(&guild_id)
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default();
        sessions.sort_by_key(|s| s.info.started_at);
        sessions
    }

    pub async fn get(&self, guild_id: GuildId, user_id: UserId) -> Option<Arc<MonitorSession>> {
        self.sessions
            .read()
            .await
            .get(&guild_id)?
            .get(&user_id)
            .cloned()
    }

    /// Removes and cancels the session monitoring `user_id`.
    pub async fn remove_user(
        &self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Arc<MonitorSession>, RegistryError> {
        let mut guilds = self.sessions.write().await;
        let guild = guilds
            .get_mut(&guild_id)
            .ok_or(RegistryError::NotMonitoring(user_id))?;
        let session = guild
            .remove(&user_id)
            .ok_or(RegistryError::NotMonitoring(user_id))?;
        if guild.is_empty() {
            guilds.remove(&guild_id);
        }
        session.cancel();
        Ok(session)
    }

    /// Removes and cancels exactly this session, if it is still registered.
    pub async fn remove_session(&self, session: &Arc<MonitorSession>) -> bool {
        let mut guilds = self.sessions.write().await;
        let (guild_id, user_id) = (session.info.guild_id, session.info.user_id);
        let registered = guilds
            .get(&guild_id)
            .and_then(|g| g.get(&user_id))
            .is_some_and(|s| Arc::ptr_eq(s, session));
        if registered && let Some(guild) = guilds.get_mut(&guild_id) {
            guild.remove(&user_id);
            if guild.is_empty() {
                guilds.remove(&guild_id);
            }
        }
        session.cancel();
        registered
    }

    /// Removes and cancels every session (shutdown).
    pub async fn drain(&self) -> Vec<Arc<MonitorSession>> {
        let drained: Vec<_> = self
            .sessions
            .write()
            .await
            .drain()
            .flat_map(|(_, g)| g.into_values())
            .collect();
        drained.iter().for_each(|s| s.cancel());
        drained
    }

    /// Total number of sessions across all guilds.
    pub async fn len(&self) -> usize {
        self.sessions.read().await.values().map(HashMap::len).sum()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}
