//! Starting, running and stopping monitoring sessions.
//!
//! Any number of members can be monitored per server:
//! * each monitored member has a **worker** that turns their audio segments into emotion
//!   results, detections and moderation;
//! * each server has one voice **supervisor** (a bot can only be in one voice channel per
//!   server) that keeps the bot in the channel with the most monitored members.

mod supervisor;
mod worker;

use std::sync::Arc;

use serenity::{
    cache::Cache,
    client::Context,
    model::{
        Timestamp,
        id::{ChannelId, GuildId, UserId},
    },
};
use tokio::sync::mpsc;
use tracing::{info, warn};

pub use supervisor::{GuildVoiceManager, choose_channel};
pub use worker::{AnalysisPipeline, SegmentOutcome};

use crate::{
    app::AppContext,
    discord::SerenityModeration,
    moderation::{ModerationBackend, Moderator},
    server_log::{LogEvent, ServerLog},
    state::{MonitorInfo, MonitorSession, RegistryError, SessionStatus},
    voice::{BufferSettings, Target, VoiceAccessProblem, check_voice_access},
};

/// Parameters of `/anger-monitor`.
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub guild_id: GuildId,
    pub target_id: UserId,
    pub target_name: String,
    pub target_is_bot: bool,
    pub moderator_id: UserId,
    pub moderator_name: String,
    pub command_channel: ChannelId,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("Bots can't be monitored.")]
    TargetIsBot,
    #[error("{0}")]
    Registry(#[from] RegistryError),
    #[error("I can't join <#{channel}>: {problem}. {}", problem.hint())]
    NoVoiceAccess {
        channel: ChannelId,
        problem: VoiceAccessProblem,
    },
    #[error("Voice support is not initialised.")]
    NoVoiceManager,
}

/// Voice channel the user is currently in, according to the gateway cache.
pub fn target_voice_channel(
    cache: &Cache,
    guild_id: GuildId,
    user_id: UserId,
) -> Option<ChannelId> {
    cache
        .guild(guild_id)?
        .voice_states
        .get(&user_id)?
        .channel_id
}

/// Checks with the cached permissions whether RageGuard can join `channel`. When the cache lacks
/// the data, the answer is "yes" and Discord decides.
pub fn voice_access(
    cache: &Cache,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<(), VoiceAccessProblem> {
    let bot_id = cache.current_user().id;
    let Some(guild) = cache.guild(guild_id) else {
        return Ok(());
    };
    let (Some(channel), Some(me)) = (guild.channels.get(&channel_id), guild.members.get(&bot_id))
    else {
        return Ok(());
    };
    let occupants = guild
        .voice_states
        .values()
        .filter(|vs| vs.channel_id == Some(channel_id) && vs.user_id != bot_id)
        .count();
    check_voice_access(
        guild.user_permissions_in(channel, me),
        channel.user_limit,
        occupants,
    )
}

/// Starts monitoring a member. RageGuard joins their voice channel now, or as soon as they are in
/// one. With several monitored members it stays in the channel where most of them are.
pub async fn start(
    ctx: &Context,
    app: &Arc<AppContext>,
    request: StartRequest,
) -> Result<Arc<MonitorSession>, StartError> {
    if request.target_is_bot {
        return Err(StartError::TargetIsBot);
    }
    let channel = target_voice_channel(&ctx.cache, request.guild_id, request.target_id);
    let settings = app.settings.get(request.guild_id).await;

    let session = MonitorSession::new(MonitorInfo {
        guild_id: request.guild_id,
        user_id: request.target_id,
        user_name: request.target_name,
        started_by: request.moderator_id,
        started_by_name: request.moderator_name,
        started_at: Timestamp::now(),
        initial_channel: channel,
        command_channel: request.command_channel,
    });
    // Atomic check-and-insert: the same member can't be monitored twice.
    app.registry.try_insert(session.clone()).await?;

    // For the first monitored member, check up front that RageGuard may join their channel, so
    // the moderator gets immediate feedback about permission problems.
    let first_in_server = app.registry.sessions(request.guild_id).await.len() == 1;
    if let Some(channel) = channel
        && first_in_server
        && let Err(problem) = voice_access(&ctx.cache, request.guild_id, channel)
    {
        warn!(guild_id = %request.guild_id, channel_id = %channel, problem = %problem, "voice_access_denied");
        app.registry.remove_session(&session).await;
        return Err(StartError::NoVoiceAccess { channel, problem });
    }
    session.with_state(|s| {
        s.voice_channel = channel;
        s.status = if channel.is_some() {
            SessionStatus::Connecting
        } else {
            SessionStatus::WaitingForTarget
        };
    });

    let backend = discord_backend(ctx);
    let server_log = ServerLog::new(backend.clone(), app.settings.clone(), request.guild_id);
    let audio = &app.config.file.audio;
    let (segment_tx, segment_rx) = mpsc::channel(audio.max_pending_segments);
    let pipeline = AnalysisPipeline::new(
        app.analyzer.clone(),
        Moderator::new(backend, app.monitor_only()),
        app.settings.clone(),
        server_log.clone(),
    );
    tokio::spawn(worker::run(session.clone(), segment_rx, pipeline));

    let target = Target {
        session: session.clone(),
        segments: segment_tx,
    };
    let buffer_settings = BufferSettings::from_config(&settings.detection, audio);
    if let Err(e) = app
        .voice
        .add(ctx, app, target, buffer_settings, server_log.clone())
        .await
    {
        app.registry.remove_session(&session).await;
        return Err(e);
    }

    info!(
        guild_id = %request.guild_id,
        user_id = %request.target_id,
        channel_id = ?channel.map(|c| c.get()),
        waiting_for_voice = channel.is_none(),
        started_by = %request.moderator_id,
        monitor_only = app.monitor_only(),
        "monitoring_started"
    );
    server_log
        .post(LogEvent::MonitoringStarted {
            by: request.moderator_id,
            target: request.target_id,
            channel,
        })
        .await;
    Ok(session)
}

/// Stops monitoring `user_id`. When the last monitored member is stopped, RageGuard leaves voice.
pub async fn stop(
    ctx: &Context,
    app: &AppContext,
    guild_id: GuildId,
    user_id: UserId,
    stopped_by: UserId,
) -> Result<Arc<MonitorSession>, RegistryError> {
    let session = app.registry.remove_user(guild_id, user_id).await?;
    app.voice.remove(guild_id, user_id).await;
    info!(guild_id = %guild_id, user_id = %user_id, "monitoring_stopped");
    let (segments_analyzed, triggers) = session.with_state(|s| (s.segments_analyzed, s.triggers));
    ServerLog::new(discord_backend(ctx), app.settings.clone(), guild_id)
        .post(LogEvent::MonitoringStopped {
            by: stopped_by,
            target: user_id,
            segments_analyzed,
            triggers,
        })
        .await;
    Ok(session)
}

fn discord_backend(ctx: &Context) -> Arc<dyn ModerationBackend> {
    Arc::new(SerenityModeration::new(ctx.http.clone(), ctx.cache.clone()))
}
