//! Analysis pipeline: audio segment → 16 kHz WAV → AI service → detection engine → moderation.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{
    ai::{AiError, EmotionAnalyzer, EmotionResult},
    detection::{DetectionRules, Verdict},
    moderation::{ModerationOutcome, Moderator, TimeoutRequest},
    server_log::{LogEvent, ServerLog},
    state::{LastAnalysis, MonitorSession, SettingsStore},
    voice::{AudioSegment, segment_to_wav},
};

/// Minimum gap between repeated "AI service unavailable" warnings.
const AI_WARNING_INTERVAL: Duration = Duration::from_secs(30);

/// What happened to one segment.
#[derive(Debug, Clone, PartialEq)]
pub enum SegmentOutcome {
    Analyzed {
        result: EmotionResult,
        verdict: Verdict,
        moderation: Option<ModerationOutcome>,
    },
    /// The audio could not be converted; the segment was skipped.
    AudioRejected(String),
    /// The AI service failed; the segment was skipped.
    AiFailed(String),
    /// Monitoring stopped while the segment was in flight.
    Cancelled,
}

/// Per-session analysis state and dependencies.
pub struct AnalysisPipeline {
    analyzer: Arc<dyn EmotionAnalyzer>,
    moderator: Moderator,
    settings: Arc<SettingsStore>,
    server_log: ServerLog,
    last_ai_warning: Option<Instant>,
    ai_unhealthy: bool,
}

impl AnalysisPipeline {
    pub fn new(
        analyzer: Arc<dyn EmotionAnalyzer>,
        moderator: Moderator,
        settings: Arc<SettingsStore>,
        server_log: ServerLog,
    ) -> Self {
        Self {
            analyzer,
            moderator,
            settings,
            server_log,
            last_ai_warning: None,
            ai_unhealthy: false,
        }
    }

    pub async fn process(
        &mut self,
        session: &MonitorSession,
        segment: AudioSegment,
    ) -> SegmentOutcome {
        let user_id = session.info.user_id;
        let captured_at = segment.captured_at;
        info!(
            user_id = %user_id,
            duration = (segment.duration().as_secs_f64() * 100.0).round() / 100.0,
            "voice_segment_received"
        );

        // Resampling and encoding are CPU-bound: keep them off the async runtime. The raw
        // segment is moved in and dropped as soon as it has been converted.
        let wav = match tokio::task::spawn_blocking(move || segment_to_wav(&segment)).await {
            Ok(Ok(wav)) => wav,
            Ok(Err(e)) => {
                warn!(user_id = %user_id, error = %e, "segment_conversion_failed");
                return SegmentOutcome::AudioRejected(e.to_string());
            }
            Err(e) => {
                error!(user_id = %user_id, error = %e, "segment_conversion_panicked");
                return SegmentOutcome::AudioRejected(e.to_string());
            }
        };

        // The WAV buffer is moved into the request and freed once it has been sent.
        let analysis = tokio::select! {
            biased;
            _ = session.cancelled() => return SegmentOutcome::Cancelled,
            result = self.analyzer.analyze(wav) => result,
        };
        let result = match analysis {
            Ok(result) => {
                self.note_ai_success().await;
                result
            }
            Err(e) => {
                self.note_ai_failure(session, &e).await;
                return SegmentOutcome::AiFailed(e.to_string());
            }
        };

        let settings = self.settings.get(session.info.guild_id).await;
        let rules = DetectionRules::from(&settings.detection);
        // Angry + Disgust by default: the model often hears real anger as Disgust.
        let angry_score = result.anger_score(&settings.detection.anger_emotion_set());
        info!(
            user_id = %user_id,
            emotion = %result.emotion,
            confidence = %result.confidence,
            anger = %angry_score,
            "emotion_detected"
        );
        let verdict = session.with_state(|s| {
            s.segments_analyzed += 1;
            s.last_analysis = Some(LastAnalysis {
                emotion: result.emotion.clone(),
                confidence: result.confidence,
                angry_score,
                at: Instant::now(),
            });
            s.detector.observe(angry_score, captured_at, &rules)
        });

        let moderation = match &verdict {
            Verdict::NotAngry { .. } => None,
            Verdict::Counted { count, required } => {
                info!(user_id = %user_id, count, required, "anger_counter");
                self.server_log
                    .post(LogEvent::AngerDetected {
                        target: user_id,
                        confidence: angry_score,
                        count: *count,
                        required: *required,
                        window: rules.window,
                    })
                    .await;
                None
            }
            Verdict::Triggered(trigger) => {
                info!(
                    user_id = %user_id,
                    count = trigger.count(),
                    required = rules.required_detections,
                    "anger_counter"
                );
                if session.is_cancelled() {
                    return SegmentOutcome::Cancelled;
                }
                let request = TimeoutRequest {
                    guild_id: session.info.guild_id,
                    user_id,
                    user_name: session.info.user_name.clone(),
                    notify_channel: Some(
                        settings.log_channel.unwrap_or(session.info.command_channel),
                    ),
                    trigger: trigger.clone(),
                    required_detections: rules.required_detections,
                    window: rules.window,
                    duration: settings.detection.timeout(),
                };
                let outcome = self.moderator.handle_trigger(&request).await;
                session.with_state(|s| {
                    s.triggers += 1;
                    s.last_action = Some(outcome.summary());
                });
                Some(outcome)
            }
        };

        SegmentOutcome::Analyzed {
            result,
            verdict,
            moderation,
        }
    }

    async fn note_ai_success(&mut self) {
        if self.ai_unhealthy {
            info!("ai_service_recovered");
            self.ai_unhealthy = false;
            self.last_ai_warning = None;
            self.server_log.post(LogEvent::AiRecovered).await;
        }
    }

    async fn note_ai_failure(&mut self, session: &MonitorSession, err: &AiError) {
        session.with_state(|s| {
            s.ai_failures += 1;
            s.last_error = Some(err.to_string());
        });
        // Tell moderators once per outage, not once per skipped segment.
        if err.is_service_problem() && !self.ai_unhealthy {
            self.ai_unhealthy = true;
            self.server_log
                .post(LogEvent::AiUnavailable {
                    error: err.to_string(),
                })
                .await;
        }

        let now = Instant::now();
        let warn_now = self
            .last_ai_warning
            .is_none_or(|last| now.duration_since(last) >= AI_WARNING_INTERVAL);
        if warn_now {
            self.last_ai_warning = Some(now);
            warn!(user_id = %session.info.user_id, error = %err, "ai_analysis_failed: segment skipped");
        } else {
            debug!(user_id = %session.info.user_id, error = %err, "ai_analysis_failed: segment skipped");
        }
    }
}

/// Processes segments one at a time until the session ends. Sequential processing keeps
/// detections in capture order.
pub async fn run(
    session: Arc<MonitorSession>,
    mut segments: mpsc::Receiver<AudioSegment>,
    mut pipeline: AnalysisPipeline,
) {
    loop {
        let segment = tokio::select! {
            biased;
            _ = session.cancelled() => break,
            segment = segments.recv() => match segment {
                Some(segment) => segment,
                None => break,
            },
        };
        pipeline.process(&session, segment).await;
    }
    debug!(user_id = %session.info.user_id, "analysis worker stopped");
}
