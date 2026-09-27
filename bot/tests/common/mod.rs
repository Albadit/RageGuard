//! Shared test doubles: a scripted AI analyzer and a recording Discord moderation backend.
#![allow(dead_code)]

use std::{
    collections::VecDeque,
    f32::consts::PI,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use rageguard::{
    ai::{AiError, EmotionAnalyzer, EmotionResult, HealthStatus},
    moderation::{ModerationBackend, ModerationError, TimeoutFacts},
    state::{MonitorInfo, MonitorSession},
    voice::{AudioSegment, PcmFormat},
};
use serenity::model::{
    Permissions, Timestamp,
    id::{ChannelId, GuildId, UserId},
};

pub const GUILD: GuildId = GuildId::new(100);
pub const TARGET: UserId = UserId::new(200);
pub const MODERATOR: UserId = UserId::new(300);
pub const VOICE_CHANNEL: ChannelId = ChannelId::new(400);
pub const COMMAND_CHANNEL: ChannelId = ChannelId::new(500);

/// The mocked AI responses from the specification.
pub const ANGRY_91: &str = r#"{"emotion": "Angry", "confidence": 0.91}"#;
pub const NEUTRAL_92: &str = r#"{"emotion": "Neutral", "confidence": 0.92}"#;

pub fn emotion(json: &str) -> EmotionResult {
    EmotionResult::from_json(json.as_bytes()).expect("valid mock response")
}

pub fn angry(confidence: f32) -> EmotionResult {
    emotion(&format!(
        r#"{{"emotion": "Angry", "confidence": {confidence}}}"#
    ))
}

pub fn neutral(confidence: f32) -> EmotionResult {
    emotion(&format!(
        r#"{{"emotion": "Neutral", "confidence": {confidence}}}"#
    ))
}

/// Analyzer that replays scripted results and records every WAV it receives.
#[derive(Default)]
pub struct MockAnalyzer {
    responses: Mutex<VecDeque<Result<EmotionResult, AiError>>>,
    pub received: Mutex<Vec<Vec<u8>>>,
}

impl MockAnalyzer {
    pub fn with(responses: impl IntoIterator<Item = Result<EmotionResult, AiError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            received: Mutex::new(Vec::new()),
        })
    }

    pub fn calls(&self) -> usize {
        self.received.lock().len()
    }
}

#[async_trait]
impl EmotionAnalyzer for MockAnalyzer {
    async fn analyze(&self, wav: Vec<u8>) -> Result<EmotionResult, AiError> {
        self.received.lock().push(wav);
        self.responses
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err(AiError::InvalidResponse("no scripted response left".into())))
    }

    async fn health(&self) -> Result<HealthStatus, AiError> {
        Ok(HealthStatus {
            status: "ok".into(),
            model_loaded: true,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppliedTimeout {
    pub guild_id: GuildId,
    pub user_id: UserId,
    pub duration: Duration,
    pub reason: String,
}

/// Moderation backend that records calls instead of talking to Discord.
pub struct MockBackend {
    pub facts: Mutex<Result<TimeoutFacts, ModerationError>>,
    pub apply_result: Mutex<Result<(), ModerationError>>,
    pub timeouts: Mutex<Vec<AppliedTimeout>>,
    pub notices: Mutex<Vec<(ChannelId, String)>>,
}

impl MockBackend {
    pub fn new(facts: TimeoutFacts) -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(Ok(facts)),
            apply_result: Mutex::new(Ok(())),
            timeouts: Mutex::new(Vec::new()),
            notices: Mutex::new(Vec::new()),
        })
    }

    pub fn allowed() -> Arc<Self> {
        Self::new(allowed_facts())
    }

    pub fn timeout_count(&self) -> usize {
        self.timeouts.lock().len()
    }

    pub fn notices(&self) -> Vec<(ChannelId, String)> {
        self.notices.lock().clone()
    }
}

#[async_trait]
impl ModerationBackend for MockBackend {
    async fn timeout_facts(
        &self,
        _guild_id: GuildId,
        _user_id: UserId,
    ) -> Result<TimeoutFacts, ModerationError> {
        self.facts.lock().clone()
    }

    async fn apply_timeout(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        duration: Duration,
        reason: &str,
    ) -> Result<(), ModerationError> {
        let result = self.apply_result.lock().clone();
        if result.is_ok() {
            self.timeouts.lock().push(AppliedTimeout {
                guild_id,
                user_id,
                duration,
                reason: reason.to_owned(),
            });
        }
        result
    }

    async fn notify(&self, channel_id: ChannelId, content: &str) -> Result<(), ModerationError> {
        self.notices.lock().push((channel_id, content.to_owned()));
        Ok(())
    }
}

/// A member the bot is allowed to time out.
pub fn allowed_facts() -> TimeoutFacts {
    TimeoutFacts {
        target_in_guild: true,
        target_is_owner: false,
        target_permissions: Permissions::SEND_MESSAGES | Permissions::CONNECT,
        target_top_role: 2,
        bot_permissions: Permissions::MODERATE_MEMBERS | Permissions::CONNECT,
        bot_top_role: 5,
    }
}

pub fn monitor_info() -> MonitorInfo {
    MonitorInfo {
        guild_id: GUILD,
        user_id: TARGET,
        user_name: "Username".into(),
        started_by: MODERATOR,
        started_by_name: "Moderator".into(),
        started_at: Timestamp::now(),
        initial_channel: Some(VOICE_CHANNEL),
        command_channel: COMMAND_CHANNEL,
    }
}

pub fn session() -> Arc<MonitorSession> {
    MonitorSession::new(monitor_info())
}

/// Interleaved stereo sine wave in Discord's capture format.
pub fn stereo_sine(format: PcmFormat, freq: f32, seconds: f32, amplitude: f32) -> Vec<i16> {
    let frames = (format.sample_rate as f32 * seconds) as usize;
    let mut out = Vec::with_capacity(frames * format.channels as usize);
    for i in 0..frames {
        let t = i as f32 / format.sample_rate as f32;
        let value = (amplitude * (2.0 * PI * freq * t).sin() * 32767.0) as i16;
        for _ in 0..format.channels {
            out.push(value);
        }
    }
    out
}

pub fn segment(seconds: f32, captured_at: Instant) -> AudioSegment {
    AudioSegment {
        samples: stereo_sine(PcmFormat::DISCORD, 220.0, seconds, 0.5),
        format: PcmFormat::DISCORD,
        captured_at,
    }
}

/// Reads the fields of a canonical 44-byte WAV header.
pub struct WavHeader {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub data_len: u32,
}

pub fn parse_wav_header(bytes: &[u8]) -> WavHeader {
    assert!(bytes.len() >= 44, "WAV too short");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(&bytes[36..40], b"data");
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    assert_eq!(u16_at(20), 1, "expected PCM format");
    WavHeader {
        channels: u16_at(22),
        sample_rate: u32_at(24),
        bits_per_sample: u16_at(34),
        data_len: u32_at(40),
    }
}
