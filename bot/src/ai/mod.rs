//! Client for the Python speech-emotion service.

mod client;

pub use client::{
    AiError, Emotion, EmotionAnalyzer, EmotionResult, HealthStatus, HttpEmotionClient,
};
