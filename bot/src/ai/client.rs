use std::{collections::BTreeMap, fmt, time::Duration};

use async_trait::async_trait;
use reqwest::{StatusCode, multipart};
use serde::Deserialize;

use crate::config::AiServiceConfig;

/// Emotion labels. Model-specific spellings (`ang`, `fearful`, `calm`, ...) are normalised so the
/// rest of the bot never depends on one particular model's label set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Emotion {
    Angry,
    Disgust,
    Fear,
    Happy,
    Neutral,
    Sad,
    Surprise,
    Other(String),
}

impl Emotion {
    pub fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "angry" | "anger" | "ang" => Self::Angry,
            "disgust" | "disgusted" | "dis" => Self::Disgust,
            "fear" | "fearful" | "fea" => Self::Fear,
            "happy" | "happiness" | "hap" | "joy" => Self::Happy,
            // Dpngtm/wav2vec2-emotion-recognition has no "neutral" class; its "calm" plays that role.
            "neutral" | "neu" | "calm" => Self::Neutral,
            "sad" | "sadness" => Self::Sad,
            "surprise" | "surprised" | "sur" => Self::Surprise,
            _ => Self::Other(label.trim().to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Angry => "Angry",
            Self::Disgust => "Disgust",
            Self::Fear => "Fear",
            Self::Happy => "Happy",
            Self::Neutral => "Neutral",
            Self::Sad => "Sad",
            Self::Surprise => "Surprise",
            Self::Other(label) => label,
        }
    }
}

impl fmt::Display for Emotion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A validated prediction for one audio segment.
#[derive(Debug, Clone, PartialEq)]
pub struct EmotionResult {
    /// Most likely emotion.
    pub emotion: Emotion,
    /// Probability of [`Self::emotion`], in `[0, 1]`.
    pub confidence: f32,
    /// Per-emotion probabilities, when the service provides them.
    pub scores: BTreeMap<Emotion, f32>,
}

#[derive(Debug, Deserialize)]
struct RawEmotionResponse {
    emotion: String,
    confidence: f64,
    #[serde(default)]
    scores: BTreeMap<String, f64>,
}

/// Rounding in the service can push a probability a hair past 1.0.
const PROBABILITY_TOLERANCE: f64 = 1e-3;

fn validate_probability(what: &str, value: f64) -> Result<f32, AiError> {
    if !value.is_finite() || value < 0.0 || value > 1.0 + PROBABILITY_TOLERANCE {
        return Err(AiError::InvalidResponse(format!(
            "{what} must be a probability in [0, 1], got {value}"
        )));
    }
    Ok(value.min(1.0) as f32)
}

impl EmotionResult {
    /// Parses and validates a JSON body from `POST /analyze`.
    pub fn from_json(body: &[u8]) -> Result<Self, AiError> {
        let raw: RawEmotionResponse = serde_json::from_slice(body)
            .map_err(|e| AiError::InvalidResponse(format!("malformed JSON: {e}")))?;

        if raw.emotion.trim().is_empty() {
            return Err(AiError::InvalidResponse("`emotion` is empty".into()));
        }
        let emotion = Emotion::from_label(&raw.emotion);
        let confidence = validate_probability("`confidence`", raw.confidence)?;

        let mut scores = BTreeMap::new();
        for (label, value) in raw.scores {
            let value = validate_probability(&format!("score for `{label}`"), value)?;
            let key = Emotion::from_label(&label);
            // Two raw labels may normalise to the same emotion; keep the larger probability.
            let slot = scores.entry(key).or_insert(value);
            *slot = slot.max(value);
        }

        Ok(Self {
            emotion,
            confidence,
            scores,
        })
    }

    /// Probability that the speaker is angry. Uses the full score table when available,
    /// otherwise the top prediction.
    pub fn angry_score(&self) -> f32 {
        self.anger_score(&[Emotion::Angry])
    }

    /// Combined probability of the given emotions, e.g. Angry + Disgust. Uses the full score
    /// table when available, otherwise the top prediction.
    pub fn anger_score(&self, emotions: &[Emotion]) -> f32 {
        if self.scores.is_empty() {
            return if emotions.contains(&self.emotion) {
                self.confidence
            } else {
                0.0
            };
        }
        emotions
            .iter()
            .filter_map(|e| self.scores.get(e))
            .sum::<f32>()
            .min(1.0)
    }
}

/// Response of `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub model_loaded: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("AI service is unreachable at {url} ({source})")]
    Unavailable {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("AI service did not answer within {0:?}")]
    Timeout(Duration),
    #[error("AI service is not ready yet: {0}")]
    NotReady(String),
    #[error("AI service rejected the audio (HTTP {status}): {detail}")]
    Rejected { status: u16, detail: String },
    #[error("AI service failed (HTTP {status}): {detail}")]
    Server { status: u16, detail: String },
    #[error("invalid response from AI service: {0}")]
    InvalidResponse(String),
}

impl AiError {
    /// Errors caused by the service being down or overloaded, as opposed to a bad segment.
    pub fn is_service_problem(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. } | Self::Timeout(_) | Self::NotReady(_) | Self::Server { .. }
        )
    }
}

/// Anything that can classify a WAV segment. Implemented by [`HttpEmotionClient`] and by test mocks.
#[async_trait]
pub trait EmotionAnalyzer: Send + Sync + 'static {
    /// Classifies a 16 kHz mono PCM WAV file held in memory.
    async fn analyze(&self, wav: Vec<u8>) -> Result<EmotionResult, AiError>;

    async fn health(&self) -> Result<HealthStatus, AiError>;
}

/// HTTP client for the FastAPI service. Audio is sent as a binary multipart upload.
#[derive(Debug, Clone)]
pub struct HttpEmotionClient {
    http: reqwest::Client,
    base_url: String,
    request_timeout: Duration,
}

impl HttpEmotionClient {
    pub fn new(base_url: &str, config: &AiServiceConfig) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout())
            .timeout(config.request_timeout())
            .user_agent(concat!("RageGuard/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            request_timeout: config.request_timeout(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn map_send_error(&self, err: reqwest::Error) -> AiError {
        if err.is_timeout() {
            AiError::Timeout(self.request_timeout)
        } else {
            AiError::Unavailable {
                url: self.base_url.clone(),
                source: err,
            }
        }
    }
}

#[async_trait]
impl EmotionAnalyzer for HttpEmotionClient {
    async fn analyze(&self, wav: Vec<u8>) -> Result<EmotionResult, AiError> {
        let part = multipart::Part::bytes(wav)
            .file_name("segment.wav")
            .mime_str("audio/wav")
            .expect("static MIME type is valid");
        let form = multipart::Form::new().part("file", part);

        let response = self
            .http
            .post(format!("{}/analyze", self.base_url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.map_send_error(e))?;

        let status = response.status();
        let body = response.bytes().await.map_err(|e| self.map_send_error(e))?;
        if status.is_success() {
            return EmotionResult::from_json(&body);
        }

        let detail = error_detail(&body);
        Err(match status {
            StatusCode::SERVICE_UNAVAILABLE => AiError::NotReady(detail),
            s if s.is_client_error() => AiError::Rejected {
                status: s.as_u16(),
                detail,
            },
            s => AiError::Server {
                status: s.as_u16(),
                detail,
            },
        })
    }

    async fn health(&self) -> Result<HealthStatus, AiError> {
        let response = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(|e| self.map_send_error(e))?;
        let status = response.status();
        let body = response.bytes().await.map_err(|e| self.map_send_error(e))?;
        if !status.is_success() && status != StatusCode::SERVICE_UNAVAILABLE {
            return Err(AiError::Server {
                status: status.as_u16(),
                detail: error_detail(&body),
            });
        }
        serde_json::from_slice(&body)
            .map_err(|e| AiError::InvalidResponse(format!("malformed health JSON: {e}")))
    }
}

/// Extracts FastAPI's `{"detail": ...}` message, falling back to a truncated body.
fn error_detail(body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Detail {
        detail: serde_json::Value,
    }
    if let Ok(Detail { detail }) = serde_json::from_slice::<Detail>(body) {
        return match detail {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        "<empty body>".into()
    } else {
        text.chars().take(200).collect()
    }
}
