//! Joining and leaving voice channels for one server.

use std::{collections::HashMap, sync::Arc};

use serenity::model::id::{ChannelId, GuildId};
use songbird::{
    Songbird,
    driver::{Channels, DecodeConfig, DecodeMode, SampleRate},
    error::JoinError,
    events::CoreEvent,
};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, warn};

use super::{
    buffer::{BufferSettings, PcmFormat},
    receiver::{Targets, VoiceReceiver},
};
use crate::state::SessionSignal;

/// Capture format; must match [`songbird_config`].
pub const CAPTURE_FORMAT: PcmFormat = PcmFormat::DISCORD;

/// Songbird configuration: decode received Opus into 48 kHz stereo PCM (Discord's native format).
/// Conversion to 16 kHz mono happens later, off the voice thread.
pub fn songbird_config() -> songbird::Config {
    songbird::Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::new(
        Channels::Stereo,
        SampleRate::Hz48000,
    )))
}

/// One async lock per guild so joins and leaves for the same guild never interleave (e.g. an old
/// supervisor leaving while a new one is joining).
#[derive(Debug, Default)]
pub struct VoiceLocks {
    locks: parking_lot::Mutex<HashMap<GuildId, Arc<Mutex<()>>>>,
}

impl VoiceLocks {
    pub fn for_guild(&self, guild_id: GuildId) -> Arc<Mutex<()>> {
        self.locks.lock().entry(guild_id).or_default().clone()
    }
}

/// The voice connection of one server, shared by all monitored members there.
#[derive(Clone)]
pub struct VoiceLink {
    manager: Arc<Songbird>,
    guild_id: GuildId,
    targets: Targets,
    signals: mpsc::UnboundedSender<SessionSignal>,
    buffer_settings: BufferSettings,
    guild_lock: Arc<Mutex<()>>,
}

impl VoiceLink {
    pub fn new(
        manager: Arc<Songbird>,
        guild_id: GuildId,
        targets: Targets,
        signals: mpsc::UnboundedSender<SessionSignal>,
        buffer_settings: BufferSettings,
        guild_lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            manager,
            guild_id,
            targets,
            signals,
            buffer_settings,
            guild_lock,
        }
    }

    /// Joins (or moves to) `channel` and attaches a fresh receiver for all targets.
    pub async fn connect(&self, channel: ChannelId) -> Result<(), JoinError> {
        let _guard = self.guild_lock.lock().await;
        let call = self.manager.get_or_insert(self.guild_id);

        let join = {
            let mut call = call.lock().await;
            // Attach handlers before joining so no early Speaking events are missed. Replacing
            // them also discards any SSRC mapping from a previous channel.
            call.remove_all_global_events();
            let receiver = VoiceReceiver::new(
                self.targets.clone(),
                self.signals.clone(),
                CAPTURE_FORMAT,
                self.buffer_settings,
                channel.get(),
            );
            for event in [
                CoreEvent::SpeakingStateUpdate,
                CoreEvent::VoiceTick,
                CoreEvent::ClientDisconnect,
                CoreEvent::DriverConnect,
                CoreEvent::DriverReconnect,
                CoreEvent::DriverDisconnect,
            ] {
                call.add_global_event(event.into(), receiver.clone());
            }
            // RageGuard never speaks. It must not self-deafen, or Discord stops sending audio.
            if let Err(e) = call.mute(true).await {
                debug!(error = %e, "could not self-mute before joining");
            }
            call.join(channel).await?
        };
        join.await
    }

    /// Leaves voice for this guild.
    pub async fn disconnect(&self) {
        let _guard = self.guild_lock.lock().await;
        match self.manager.remove(self.guild_id).await {
            Ok(()) | Err(JoinError::NoCall) => {}
            Err(e) => warn!(guild_id = %self.guild_id, error = %e, "failed to leave voice"),
        }
    }

    /// The voice channel songbird believes the bot is in.
    pub async fn current_channel(&self) -> Option<ChannelId> {
        let call = self.manager.get(self.guild_id)?;
        let channel = call.lock().await.current_channel()?;
        Some(ChannelId::new(channel.0.get()))
    }
}
