//! Songbird event handler that extracts the monitored members' audio from the voice stream.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use serenity::model::id::UserId;
use songbird::{
    events::{
        Event, EventContext, EventHandler,
        context_data::{DisconnectData, DisconnectReason, VoiceTick},
    },
    model::{
        CloseCode,
        payload::{ClientDisconnect, Speaking},
    },
};
use tokio::sync::mpsc::{self, error::TrySendError};
use tracing::{debug, info, warn};

use super::buffer::{AudioSegment, BufferEvent, BufferSettings, PcmFormat, SegmentBuffer};
use crate::state::{MonitorSession, SessionSignal};

/// Songbird clocks out received audio every 20 ms.
const TICK: Duration = Duration::from_millis(20);

fn user_id(raw: u64) -> Option<UserId> {
    (raw != 0).then(|| UserId::new(raw))
}

/// Where one monitored member's audio goes.
#[derive(Clone)]
pub struct Target {
    pub session: Arc<MonitorSession>,
    pub segments: mpsc::Sender<AudioSegment>,
}

/// The monitored members of one server, shared by the voice supervisor and the receiver so that
/// members can be added or removed while the bot stays connected.
#[derive(Clone, Default)]
pub struct Targets(Arc<RwLock<HashMap<UserId, Target>>>);

impl Targets {
    pub fn insert(&self, target: Target) {
        self.0.write().insert(target.session.info.user_id, target);
    }

    /// Returns true if the member was a target.
    pub fn remove(&self, user_id: UserId) -> bool {
        self.0.write().remove(&user_id).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.0.read().is_empty()
    }

    pub fn contains(&self, user_id: UserId) -> bool {
        self.0.read().contains_key(&user_id)
    }

    /// Current targets, oldest session first.
    pub fn sessions(&self) -> Vec<Arc<MonitorSession>> {
        let mut sessions: Vec<_> = self.0.read().values().map(|t| t.session.clone()).collect();
        sessions.sort_by_key(|s| s.info.started_at);
        sessions
    }

    fn get(&self, user_id: UserId) -> Option<Target> {
        self.0.read().get(&user_id).cloned()
    }

    fn user_ids(&self) -> Vec<UserId> {
        self.0.read().keys().copied().collect()
    }
}

/// Receives every voice event for the call, keeps only the monitored members' audio (each in its
/// own buffer), and hands finished segments to each member's analysis worker.
///
/// Everyone else's audio is never buffered: it is dropped as soon as the tick is inspected.
#[derive(Clone)]
pub struct VoiceReceiver {
    inner: Arc<Inner>,
}

struct Inner {
    targets: Targets,
    signals: mpsc::UnboundedSender<SessionSignal>,
    /// Voice channel this receiver was attached for.
    channel: u64,
    format: PcmFormat,
    buffer_settings: BufferSettings,
    state: Mutex<ReceiverState>,
}

#[derive(Default)]
struct ReceiverState {
    /// RTP source IDs of everyone who has spoken, learned from Speaking events. Kept for all
    /// users so a member who becomes a target later is recognised immediately.
    ssrc_by_user: HashMap<UserId, u32>,
    buffers: HashMap<UserId, SegmentBuffer>,
}

impl VoiceReceiver {
    pub fn new(
        targets: Targets,
        signals: mpsc::UnboundedSender<SessionSignal>,
        format: PcmFormat,
        buffer_settings: BufferSettings,
        channel: u64,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                targets,
                signals,
                channel,
                format,
                buffer_settings,
                state: Mutex::new(ReceiverState::default()),
            }),
        }
    }

    fn signal(&self, signal: SessionSignal) {
        let _ = self.inner.signals.send(signal);
    }

    fn on_speaking(&self, speaking: &Speaking) {
        let Some(user) = speaking.user_id.and_then(|u| user_id(u.0)) else {
            return;
        };
        let mut state = self.inner.state.lock();
        // An SSRC belongs to one user at a time.
        state
            .ssrc_by_user
            .retain(|other, ssrc| *other == user || *ssrc != speaking.ssrc);
        if state.ssrc_by_user.insert(user, speaking.ssrc) != Some(speaking.ssrc) {
            debug!(user_id = %user, ssrc = speaking.ssrc, "ssrc_mapped");
            state.buffers.remove(&user);
        }
    }

    fn on_tick(&self, tick: &VoiceTick) {
        let now = Instant::now();
        let targets = self.inner.targets.user_ids();
        let mut events = Vec::new();
        {
            let mut state = self.inner.state.lock();
            let state = &mut *state;
            state.buffers.retain(|user, _| targets.contains(user));
            for user in targets {
                let Some(&ssrc) = state.ssrc_by_user.get(&user) else {
                    continue;
                };
                let buffer = state.buffers.entry(user).or_insert_with(|| {
                    SegmentBuffer::new(self.inner.format, self.inner.buffer_settings)
                });
                let pcm = tick
                    .speaking
                    .get(&ssrc)
                    .and_then(|data| data.decoded_voice.as_deref())
                    .filter(|pcm| !pcm.is_empty());
                let event = match pcm {
                    Some(pcm) => buffer.push_voice(pcm, now),
                    None => buffer.push_silence(TICK, now),
                };
                if let Some(event) = event {
                    events.push((user, event));
                }
            }
        }

        for (user, event) in events {
            match event {
                BufferEvent::Segment(segment) => self.dispatch(user, segment),
                BufferEvent::DiscardedShort(duration) => debug!(
                    user_id = %user,
                    duration_ms = duration.as_millis() as u64,
                    "short_utterance_discarded"
                ),
            }
        }
    }

    /// Queues a segment without blocking the voice event loop. If analysis is falling behind,
    /// the segment is dropped rather than queued without bound.
    fn dispatch(&self, user: UserId, segment: AudioSegment) {
        let Some(target) = self.inner.targets.get(user) else {
            return;
        };
        match target.segments.try_send(segment) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                warn!(user_id = %user, "analysis_backlog_full: dropping segment");
                target.session.with_state(|s| s.segments_dropped += 1);
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    fn on_client_disconnect(&self, disconnect: &ClientDisconnect) {
        let Some(user) = user_id(disconnect.user_id.0) else {
            return;
        };
        {
            let mut state = self.inner.state.lock();
            state.ssrc_by_user.remove(&user);
            state.buffers.remove(&user);
        }
        if self.inner.targets.contains(user) {
            info!(user_id = %user, "target_left_voice_call");
            self.signal(SessionSignal::Reconcile("monitored user left the call"));
        }
    }

    fn on_driver_disconnect(&self, data: &DisconnectData<'_>) {
        {
            let mut state = self.inner.state.lock();
            state.ssrc_by_user.clear();
            state.buffers.clear();
        }
        match data.reason {
            None | Some(DisconnectReason::Requested) => {
                debug!(channel_id = %data.channel_id, "voice_driver_disconnected (requested)");
            }
            // The old connection closing after a channel move, or Discord's "disconnected"
            // (moved/kicked) close: gateway voice-state updates drive reconnection in those
            // cases, and forcing a reconnect here would cancel the join that is in progress.
            _ if data.channel_id.0.get() != self.inner.channel => {
                debug!(channel_id = %data.channel_id, "old voice connection closed");
            }
            Some(DisconnectReason::WsClosed(Some(CloseCode::Disconnected))) => {
                debug!(channel_id = %data.channel_id, "voice connection closed by Discord (moved or disconnected)");
                self.signal(SessionSignal::Reconcile(
                    "voice connection closed by Discord",
                ));
            }
            Some(reason) => {
                warn!(channel_id = %data.channel_id, reason = ?reason, "voice_connection_lost");
                self.signal(SessionSignal::ConnectionLost(format!("{reason:?}")));
            }
        }
    }

    fn reset(&self) {
        self.inner.state.lock().buffers.clear();
    }
}

#[async_trait]
impl EventHandler for VoiceReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => self.on_speaking(speaking),
            EventContext::VoiceTick(tick) => self.on_tick(tick),
            EventContext::ClientDisconnect(disconnect) => self.on_client_disconnect(disconnect),
            EventContext::DriverConnect(data) => {
                info!(channel_id = ?data.channel_id, "voice_connected");
                self.reset();
            }
            EventContext::DriverReconnect(data) => {
                info!(channel_id = ?data.channel_id, "voice_reconnected");
                self.reset();
            }
            EventContext::DriverDisconnect(data) => self.on_driver_disconnect(data),
            _ => {}
        }
        None
    }
}
