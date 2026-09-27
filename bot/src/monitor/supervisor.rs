//! Keeps each server's voice connection where the monitored members are.
//!
//! A bot can only be in one voice channel per server, so there is one supervisor per server,
//! shared by all monitored members there. It joins the channel with the most monitored members
//! (ties go to the member who has been monitored longest).
//!
//! Rather than reacting to each event individually, every event triggers a *reconcile*: compare
//! where the members are (gateway cache) with where the bot is (songbird) and fix any difference.
//! This handles moves, leaves, lost connections and missed events uniformly.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serenity::{
    cache::Cache,
    client::Context,
    model::{
        Timestamp,
        id::{ChannelId, GuildId, UserId},
    },
};
use songbird::error::JoinError;
use tokio::{
    sync::{Mutex, mpsc},
    time::{Instant, MissedTickBehavior, interval, sleep_until},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{StartError, target_voice_channel, voice_access};
use crate::{
    app::AppContext,
    server_log::{LogEvent, ServerLog},
    state::{MonitorSession, SessionSignal, SessionStatus},
    voice::{BufferSettings, Target, Targets, VoiceLink},
};

/// Safety net in case a gateway event was missed.
const PERIODIC_RECONCILE: Duration = Duration::from_secs(30);
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(120);
/// After leaving on purpose, give Discord time to process the leave before joining again;
/// otherwise its late "left" update cancels the new join.
const REJOIN_PAUSE: Duration = Duration::from_millis(1500);
/// Interrupted joins are retried quickly and quietly this many times before being reported.
const QUIET_RETRIES: u32 = 3;
const QUIET_RETRY_DELAY: Duration = Duration::from_secs(1);

/// The voice channel to be in: the one with the most monitored members; ties go to the channel
/// of the member monitored longest. `members` are (monitoring start, current voice channel).
pub fn choose_channel(members: &[(Timestamp, Option<ChannelId>)]) -> Option<ChannelId> {
    let mut counts: HashMap<ChannelId, (usize, Timestamp)> = HashMap::new();
    for &(started, channel) in members {
        if let Some(channel) = channel {
            let entry = counts.entry(channel).or_insert((0, started));
            entry.0 += 1;
            entry.1 = entry.1.min(started);
        }
    }
    counts
        .into_iter()
        .max_by(|(_, (count_a, first_a)), (_, (count_b, first_b))| {
            count_a.cmp(count_b).then(first_b.cmp(first_a))
        })
        .map(|(channel, _)| channel)
}

struct GuildVoice {
    targets: Targets,
    signals: mpsc::UnboundedSender<SessionSignal>,
    cancel: CancellationToken,
    generation: u64,
}

/// Starts, feeds and stops the per-server voice supervisors.
#[derive(Default)]
pub struct GuildVoiceManager {
    guilds: Mutex<HashMap<GuildId, GuildVoice>>,
    next_generation: AtomicU64,
}

impl GuildVoiceManager {
    /// Adds a monitored member, starting the server's supervisor if it isn't running yet.
    pub async fn add(
        &self,
        ctx: &Context,
        app: &Arc<AppContext>,
        target: Target,
        buffer_settings: BufferSettings,
        server_log: ServerLog,
    ) -> Result<(), StartError> {
        let guild_id = target.session.info.guild_id;
        let mut guilds = self.guilds.lock().await;
        if let Some(voice) = guilds.get(&guild_id) {
            voice.targets.insert(target);
            let _ = voice
                .signals
                .send(SessionSignal::Reconcile("monitored member added"));
            return Ok(());
        }

        let manager = songbird::get(ctx).await.ok_or(StartError::NoVoiceManager)?;
        let targets = Targets::default();
        targets.insert(target);
        let (signals, receiver) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let link = VoiceLink::new(
            manager,
            guild_id,
            targets.clone(),
            signals.clone(),
            buffer_settings,
            app.voice_locks.for_guild(guild_id),
        );
        tokio::spawn(run(Supervisor {
            guild_id,
            link,
            targets: targets.clone(),
            signals: receiver,
            cache: ctx.cache.clone(),
            app: app.clone(),
            server_log,
            cancel: cancel.clone(),
            generation,
        }));
        let _ = signals.send(SessionSignal::Reconcile("monitoring started"));
        guilds.insert(
            guild_id,
            GuildVoice {
                targets,
                signals,
                cancel,
                generation,
            },
        );
        Ok(())
    }

    /// Removes a monitored member. The last one out stops the supervisor, which leaves voice.
    pub async fn remove(&self, guild_id: GuildId, user_id: UserId) {
        let mut guilds = self.guilds.lock().await;
        let Some(voice) = guilds.get(&guild_id) else {
            return;
        };
        voice.targets.remove(user_id);
        if voice.targets.is_empty() {
            voice.cancel.cancel();
            guilds.remove(&guild_id);
        } else {
            let _ = voice
                .signals
                .send(SessionSignal::Reconcile("monitored member removed"));
        }
    }

    /// Forwards an event (e.g. a voice-state change) to the server's supervisor, if any.
    pub async fn signal(&self, guild_id: GuildId, signal: SessionSignal) {
        if let Some(voice) = self.guilds.lock().await.get(&guild_id) {
            let _ = voice.signals.send(signal);
        }
    }

    /// Whether `user_id` is monitored in the server (used to filter voice-state events).
    pub async fn is_target(&self, guild_id: GuildId, user_id: UserId) -> bool {
        self.guilds
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|v| v.targets.contains(user_id))
    }

    /// Stops every supervisor (shutdown).
    pub async fn shutdown(&self) {
        for (_, voice) in self.guilds.lock().await.drain() {
            voice.cancel.cancel();
        }
    }

    /// Leaves voice unless a newer supervisor has taken over the server. Holding the map lock
    /// while leaving keeps a new supervisor from joining in the middle of it.
    async fn leave_if_unclaimed(&self, guild_id: GuildId, generation: u64, link: &VoiceLink) {
        let guilds = self.guilds.lock().await;
        match guilds.get(&guild_id) {
            Some(voice) if voice.generation != generation => {
                debug!(guild_id = %guild_id, "a new supervisor owns the call; not leaving");
            }
            _ => link.disconnect().await,
        }
    }
}

/// Why joining failed.
struct JoinFailure {
    channel: ChannelId,
    message: String,
    /// The join was interrupted (songbird's `Dropped`), not refused; retrying usually works.
    transient: bool,
}

struct Supervisor {
    guild_id: GuildId,
    link: VoiceLink,
    targets: Targets,
    signals: mpsc::UnboundedReceiver<SessionSignal>,
    cache: Arc<Cache>,
    app: Arc<AppContext>,
    server_log: ServerLog,
    cancel: CancellationToken,
    generation: u64,
}

async fn run(mut sup: Supervisor) {
    let mut periodic = interval(PERIODIC_RECONCILE);
    periodic.set_missed_tick_behavior(MissedTickBehavior::Delay);
    periodic.reset();

    let mut backoff = INITIAL_BACKOFF;
    let mut retry_at: Option<Instant> = None;
    let mut force_reconnect = false;
    // A join failure was already reported to the server log for the current outage.
    let mut failure_reported = false;
    let mut quiet_retries = 0;

    loop {
        let retry = async {
            match retry_at {
                Some(at) => sleep_until(at).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;
            _ = sup.cancel.cancelled() => break,
            signal = sup.signals.recv() => match signal {
                Some(signal) => force_reconnect |= note_signal(&signal),
                None => break,
            },
            _ = retry => {}
            _ = periodic.tick() => {}
        }
        // Coalesce bursts (e.g. several voice-state updates) into one reconcile.
        while let Ok(signal) = sup.signals.try_recv() {
            force_reconnect |= note_signal(&signal);
        }
        if sup.cancel.is_cancelled() {
            break;
        }

        let sessions = sup.targets.sessions();
        match reconcile(&sup, &sessions, force_reconnect).await {
            Ok(()) => {
                backoff = INITIAL_BACKOFF;
                retry_at = None;
                force_reconnect = false;
                failure_reported = false;
                quiet_retries = 0;
            }
            Err(failure) if failure.transient && quiet_retries < QUIET_RETRIES => {
                quiet_retries += 1;
                debug!(guild_id = %sup.guild_id, error = %failure.message, attempt = quiet_retries, "voice join interrupted; retrying");
                force_reconnect = false;
                retry_at = Some(Instant::now() + QUIET_RETRY_DELAY);
            }
            Err(failure) => {
                let JoinFailure {
                    channel, message, ..
                } = failure;
                warn!(guild_id = %sup.guild_id, error = %message, retry_in_s = backoff.as_secs(), "voice_join_failed");
                for session in &sessions {
                    session.with_state(|s| {
                        s.status = SessionStatus::Reconnecting;
                        s.last_error = Some(format!("voice: {message}"));
                    });
                }
                if !failure_reported && let Some(first) = sessions.first() {
                    failure_reported = true;
                    sup.server_log
                        .post(LogEvent::VoiceProblem {
                            target: first.info.user_id,
                            channel,
                            error: message,
                        })
                        .await;
                }
                force_reconnect = false;
                quiet_retries = 0;
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    sup.app
        .voice
        .leave_if_unclaimed(sup.guild_id, sup.generation, &sup.link)
        .await;
    debug!(guild_id = %sup.guild_id, "voice supervisor stopped");
}

/// Logs a signal; returns true if it requires tearing down the connection.
fn note_signal(signal: &SessionSignal) -> bool {
    match signal {
        SessionSignal::Reconcile(reason) => {
            debug!(reason, "voice reconcile requested");
            false
        }
        SessionSignal::ConnectionLost(reason) => {
            warn!(reason = %reason, "voice connection lost; reconnecting");
            true
        }
    }
}

/// Sets each member's status relative to the channel the bot is in (or joining).
fn update_statuses(
    members: &[(Arc<MonitorSession>, Option<ChannelId>)],
    bot_channel: Option<ChannelId>,
    joining: Option<SessionStatus>,
) {
    for (session, channel) in members {
        session.with_state(|s| {
            s.voice_channel = *channel;
            s.status = match (*channel, bot_channel) {
                (None, _) => SessionStatus::WaitingForTarget,
                (Some(c), Some(b)) if c == b => joining.unwrap_or(SessionStatus::Listening),
                (Some(_), _) => SessionStatus::InOtherChannel,
            };
        });
    }
}

async fn reconcile(
    sup: &Supervisor,
    sessions: &[Arc<MonitorSession>],
    force: bool,
) -> Result<(), JoinFailure> {
    let members: Vec<(Arc<MonitorSession>, Option<ChannelId>)> = sessions
        .iter()
        .map(|s| {
            let channel = target_voice_channel(&sup.cache, sup.guild_id, s.info.user_id);
            (s.clone(), channel)
        })
        .collect();
    let desired = choose_channel(
        &members
            .iter()
            .map(|(s, c)| (s.info.started_at, *c))
            .collect::<Vec<_>>(),
    );
    let actual = sup.link.current_channel().await;

    let Some(channel) = desired else {
        if actual.is_some() {
            info!(guild_id = %sup.guild_id, "no monitored member in voice: leaving until one returns");
            sup.link.disconnect().await;
        }
        update_statuses(&members, None, None);
        return Ok(());
    };

    if actual == Some(channel) && !force {
        update_statuses(&members, Some(channel), None);
        return Ok(());
    }

    if force {
        sup.link.disconnect().await;
        tokio::time::sleep(REJOIN_PAUSE).await;
    }
    let moving = actual.is_some() && actual != Some(channel);
    update_statuses(
        &members,
        Some(channel),
        Some(if force || moving {
            SessionStatus::Reconnecting
        } else {
            SessionStatus::Connecting
        }),
    );
    info!(guild_id = %sup.guild_id, channel_id = %channel, moving, "joining voice channel");

    // Discord ignores joins it refuses, so check first and report a clear reason.
    voice_access(&sup.cache, sup.guild_id, channel).map_err(|problem| JoinFailure {
        channel,
        message: format!("{problem}. {}", problem.hint()),
        transient: false,
    })?;
    sup.link.connect(channel).await.map_err(|e| match e {
        JoinError::Dropped => JoinFailure {
            channel,
            message: "the join was interrupted".to_owned(),
            transient: true,
        },
        other => JoinFailure {
            channel,
            message: format!(
                "{other}. Check RageGuard's View Channel and Connect permissions there."
            ),
            transient: false,
        },
    })?;
    update_statuses(&members, Some(channel), None);
    Ok(())
}
