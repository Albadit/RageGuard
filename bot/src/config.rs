//! Configuration loading.
//!
//! Secrets and deployment settings come from environment variables (usually a `.env` file);
//! detection behaviour comes from a TOML file (`rageguard.toml`). Both are validated up front so
//! the bot fails fast with a clear message instead of misbehaving at runtime.

use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use crate::ai::Emotion;

/// Default location of the detection config file, relative to the working directory.
pub const DEFAULT_CONFIG_PATH: &str = "rageguard.toml";
/// Default directory for state saved at runtime (e.g. the log channel chosen in Discord).
pub const DEFAULT_DATA_DIR: &str = "data";
/// Default address of the Python AI service.
pub const DEFAULT_AI_SERVICE_URL: &str = "http://127.0.0.1:8000";
/// Discord refuses timeouts longer than 28 days.
pub const MAX_TIMEOUT_MINUTES: u64 = 28 * 24 * 60;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "missing required environment variable `{0}`. Copy .env.example to .env and fill it in."
    )]
    MissingEnv(&'static str),
    #[error("environment variable `{name}` has an invalid value `{value}`: {reason}")]
    InvalidEnv {
        name: &'static str,
        value: String,
        reason: String,
    },
    #[error("could not read config file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse config file `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid setting `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

// ---------------------------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------------------------

/// Secrets and deployment settings read from the environment.
#[derive(Clone)]
pub struct EnvConfig {
    pub discord_token: String,
    pub ai_service_url: String,
    /// When true, RageGuard never applies a real timeout. Defaults to true.
    pub monitor_only: bool,
    pub log_level: String,
    pub config_path: PathBuf,
    /// Where runtime state is saved (the per-server log channel chosen in Discord).
    pub data_dir: PathBuf,
}

impl fmt::Debug for EnvConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvConfig")
            .field("discord_token", &"<redacted>")
            .field("ai_service_url", &self.ai_service_url)
            .field("monitor_only", &self.monitor_only)
            .field("log_level", &self.log_level)
            .field("config_path", &self.config_path)
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

impl EnvConfig {
    /// Reads the process environment. Call `dotenvy::dotenv()` first to load `.env`.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the config from an arbitrary key lookup. Empty values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let discord_token = get("DISCORD_TOKEN").ok_or(ConfigError::MissingEnv("DISCORD_TOKEN"))?;
        if discord_token.contains(char::is_whitespace) {
            return Err(ConfigError::InvalidEnv {
                name: "DISCORD_TOKEN",
                value: "<redacted>".into(),
                reason: "tokens never contain whitespace; paste the raw bot token without a `Bot ` prefix".into(),
            });
        }

        let ai_service_url = get("AI_SERVICE_URL").unwrap_or_else(|| DEFAULT_AI_SERVICE_URL.into());
        let ai_service_url = validate_url("AI_SERVICE_URL", &ai_service_url)?;

        let monitor_only = match get("MONITOR_ONLY") {
            None => true,
            Some(v) => parse_bool(&v).ok_or_else(|| ConfigError::InvalidEnv {
                name: "MONITOR_ONLY",
                value: v.clone(),
                reason: "expected true/false (refusing to guess, because a wrong guess could enable real timeouts)".into(),
            })?,
        };

        let log_level = get("LOG_LEVEL")
            .unwrap_or_else(|| "info".into())
            .to_ascii_lowercase();
        if !matches!(
            log_level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            return Err(ConfigError::InvalidEnv {
                name: "LOG_LEVEL",
                value: log_level,
                reason: "expected one of trace, debug, info, warn, error".into(),
            });
        }

        let config_path = get("RAGEGUARD_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
        let data_dir = get("RAGEGUARD_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));

        Ok(Self {
            discord_token,
            ai_service_url,
            monitor_only,
            log_level,
            config_path,
            data_dir,
        })
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn validate_url(name: &'static str, value: &str) -> Result<String, ConfigError> {
    let invalid = |reason: String| ConfigError::InvalidEnv {
        name,
        value: value.into(),
        reason,
    };
    let url = reqwest::Url::parse(value).map_err(|e| invalid(e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("expected an http:// or https:// URL".into()));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

// ---------------------------------------------------------------------------------------------
// Detection config file
// ---------------------------------------------------------------------------------------------

/// Contents of `rageguard.toml`. Every section and field is optional and falls back to defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub anger_detection: DetectionConfig,
    pub audio: AudioConfig,
    pub ai_service: AiServiceConfig,
}

/// Rules deciding when angry speech becomes a moderation action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DetectionConfig {
    /// Minimum "Angry" probability for a segment to count as an angry detection.
    pub threshold: f32,
    /// Angry detections needed inside the window to trigger moderation.
    pub required_detections: u32,
    /// Sliding window (seconds) in which the detections must occur.
    pub window_seconds: u64,
    /// Length of the Discord timeout.
    pub timeout_minutes: u64,
    /// Length of each audio segment sent for analysis.
    pub segment_seconds: f32,
    /// Emotions whose scores are added up as "anger". The model often labels real angry speech
    /// as Disgust, so both count by default.
    pub anger_emotions: Vec<String>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            threshold: 0.70,
            required_detections: 2,
            window_seconds: 15,
            timeout_minutes: 1,
            segment_seconds: 3.0,
            anger_emotions: vec!["Angry".into(), "Disgust".into()],
        }
    }
}

impl DetectionConfig {
    /// [`Self::anger_emotions`] as parsed emotions.
    pub fn anger_emotion_set(&self) -> Vec<Emotion> {
        self.anger_emotions
            .iter()
            .map(|label| Emotion::from_label(label))
            .collect()
    }

    pub fn window(&self) -> Duration {
        Duration::from_secs(self.window_seconds)
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_minutes * 60)
    }

    pub fn segment(&self) -> Duration {
        Duration::from_secs_f32(self.segment_seconds)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |field, reason: &str| {
            Err(ConfigError::Invalid {
                field,
                reason: reason.into(),
            })
        };
        if !self.threshold.is_finite() || self.threshold <= 0.0 || self.threshold > 1.0 {
            return invalid("anger_detection.threshold", "must be in (0.0, 1.0]");
        }
        if self.required_detections == 0 || self.required_detections > 100 {
            return invalid(
                "anger_detection.required_detections",
                "must be between 1 and 100",
            );
        }
        if self.window_seconds == 0 || self.window_seconds > 3600 {
            return invalid(
                "anger_detection.window_seconds",
                "must be between 1 and 3600",
            );
        }
        if self.timeout_minutes == 0 || self.timeout_minutes > MAX_TIMEOUT_MINUTES {
            return invalid(
                "anger_detection.timeout_minutes",
                "must be between 1 and 40320 (Discord's 28-day limit)",
            );
        }
        if !self.segment_seconds.is_finite() || !(1.0..=10.0).contains(&self.segment_seconds) {
            return invalid(
                "anger_detection.segment_seconds",
                "must be between 1 and 10 seconds (2–5 recommended)",
            );
        }
        if self.anger_emotions.is_empty() {
            return invalid(
                "anger_detection.anger_emotions",
                "must list at least one emotion",
            );
        }
        if let Some(unknown) = self
            .anger_emotions
            .iter()
            .find(|label| matches!(Emotion::from_label(label), Emotion::Other(_)))
        {
            return Err(ConfigError::Invalid {
                field: "anger_detection.anger_emotions",
                reason: format!(
                    "unknown emotion `{unknown}`; use Angry, Disgust, Fear, Happy, Neutral, Sad or Surprise"
                ),
            });
        }
        Ok(())
    }

    /// Non-fatal problems worth logging, e.g. rules that can never trigger.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        // Consecutive segments are `segment_seconds` apart, so N detections span (N-1) segments.
        let min_span =
            f64::from(self.required_detections.saturating_sub(1)) * f64::from(self.segment_seconds);
        if min_span > self.window_seconds as f64 {
            warnings.push(format!(
                "{} detections of {:.1}s segments need at least {:.1}s, but window_seconds is {}; moderation can never trigger",
                self.required_detections, self.segment_seconds, min_span, self.window_seconds
            ));
        }
        if self.threshold < 0.6 {
            warnings.push(format!(
                "threshold {:.2} is low; expect many false positives",
                self.threshold
            ));
        }
        if !(2.0..=5.0).contains(&self.segment_seconds) {
            warnings.push(format!(
                "segment_seconds {:.1} is outside the recommended 2–5 s range",
                self.segment_seconds
            ));
        }
        warnings
    }
}

/// Audio buffering behaviour.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Partial segments shorter than this are discarded when the speaker goes quiet.
    pub min_segment_seconds: f32,
    /// A partial segment is flushed after this much continuous silence.
    pub silence_flush_ms: u64,
    /// Segments queued for analysis before new ones are dropped (back-pressure).
    pub max_pending_segments: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            min_segment_seconds: 1.0,
            silence_flush_ms: 800,
            max_pending_segments: 2,
        }
    }
}

impl AudioConfig {
    pub fn validate(&self, segment_seconds: f32) -> Result<(), ConfigError> {
        if !self.min_segment_seconds.is_finite()
            || self.min_segment_seconds < 0.5
            || self.min_segment_seconds > segment_seconds
        {
            return Err(ConfigError::Invalid {
                field: "audio.min_segment_seconds",
                reason: "must be at least 0.5 and not longer than anger_detection.segment_seconds"
                    .into(),
            });
        }
        if !(100..=10_000).contains(&self.silence_flush_ms) {
            return Err(ConfigError::Invalid {
                field: "audio.silence_flush_ms",
                reason: "must be between 100 and 10000".into(),
            });
        }
        if !(1..=16).contains(&self.max_pending_segments) {
            return Err(ConfigError::Invalid {
                field: "audio.max_pending_segments",
                reason: "must be between 1 and 16".into(),
            });
        }
        Ok(())
    }
}

/// HTTP behaviour when talking to the AI service.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiServiceConfig {
    pub request_timeout_seconds: u64,
    pub connect_timeout_seconds: u64,
}

impl Default for AiServiceConfig {
    fn default() -> Self {
        Self {
            request_timeout_seconds: 10,
            connect_timeout_seconds: 3,
        }
    }
}

impl AiServiceConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_seconds)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=120).contains(&self.request_timeout_seconds) {
            return Err(ConfigError::Invalid {
                field: "ai_service.request_timeout_seconds",
                reason: "must be between 1 and 120".into(),
            });
        }
        if !(1..=60).contains(&self.connect_timeout_seconds) {
            return Err(ConfigError::Invalid {
                field: "ai_service.connect_timeout_seconds",
                reason: "must be between 1 and 60".into(),
            });
        }
        Ok(())
    }
}

impl FileConfig {
    /// Parses and validates TOML text.
    pub fn from_toml_str(text: &str, origin: &Path) -> Result<Self, ConfigError> {
        let config: FileConfig = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: origin.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.anger_detection.validate()?;
        self.audio.validate(self.anger_detection.segment_seconds)?;
        self.ai_service.validate()
    }

    /// Loads the config file. Returns `Ok(None)` when no file exists so callers can fall back
    /// to defaults; a file that exists but is invalid is always an error.
    ///
    /// A relative path is looked up in the working directory first and then in `bot/`, so the
    /// bot works whether it is started from `bot/` or from the repository root.
    pub fn load(path: &Path) -> Result<Option<(Self, PathBuf)>, ConfigError> {
        let Some(found) = resolve_config_path(path) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&found).map_err(|source| ConfigError::Io {
            path: found.clone(),
            source,
        })?;
        Self::from_toml_str(&text, &found).map(|config| Some((config, found)))
    }
}

fn resolve_config_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.is_relative() {
        let nested = Path::new("bot").join(path);
        if nested.is_file() {
            return Some(nested);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Combined
// ---------------------------------------------------------------------------------------------

/// Everything the bot needs to start.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub env: EnvConfig,
    pub file: FileConfig,
    /// Where the file config came from, or `None` when defaults are used.
    pub file_source: Option<PathBuf>,
}

impl AppConfig {
    pub fn load(env: EnvConfig) -> Result<Self, ConfigError> {
        let (file, file_source) = match FileConfig::load(&env.config_path)? {
            Some((file, path)) => (file, Some(path)),
            None => (FileConfig::default(), None),
        };
        Ok(Self {
            env,
            file,
            file_source,
        })
    }
}
