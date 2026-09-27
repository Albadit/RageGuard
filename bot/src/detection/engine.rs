use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::config::DetectionConfig;

/// One segment that scored above the anger threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AngerDetection {
    /// When the audio was captured (not when analysis finished).
    pub timestamp: Instant,
    pub confidence: f32,
}

/// The subset of [`DetectionConfig`] the engine needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectionRules {
    pub threshold: f32,
    pub required_detections: usize,
    pub window: Duration,
}

impl From<&DetectionConfig> for DetectionRules {
    fn from(config: &DetectionConfig) -> Self {
        Self {
            threshold: config.threshold,
            required_detections: config.required_detections.max(1) as usize,
            window: config.window(),
        }
    }
}

/// Details of a threshold crossing.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    /// The detections that caused the trigger, oldest first.
    pub detections: Vec<AngerDetection>,
    pub peak_confidence: f32,
    pub mean_confidence: f32,
    /// Time between the first and last detection.
    pub span: Duration,
}

/// What the engine decided about one analysed segment.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Anger score below the threshold; nothing recorded.
    NotAngry { score: f32 },
    /// Angry detection recorded; not enough yet to act.
    Counted { count: usize, required: usize },
    /// Enough detections inside the window. History is cleared, so another trigger needs a
    /// fresh set of detections.
    Triggered(Trigger),
}

/// Sliding-window anger counter.
///
/// The engine never reads the clock itself; callers pass `now`, which keeps it deterministic
/// and easy to test.
#[derive(Debug, Default)]
pub struct AngerDetector {
    detections: VecDeque<AngerDetection>,
}

impl AngerDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one analysed segment into the engine.
    pub fn observe(&mut self, angry_score: f32, at: Instant, rules: &DetectionRules) -> Verdict {
        self.prune(at, rules.window);

        if !angry_score.is_finite() || angry_score < rules.threshold {
            return Verdict::NotAngry {
                score: if angry_score.is_finite() {
                    angry_score
                } else {
                    0.0
                },
            };
        }

        self.detections.push_back(AngerDetection {
            timestamp: at,
            confidence: angry_score,
        });

        let count = self.detections.len();
        if count < rules.required_detections {
            return Verdict::Counted {
                count,
                required: rules.required_detections,
            };
        }

        let detections: Vec<_> = self.detections.drain(..).collect();
        Verdict::Triggered(Trigger::from_detections(detections))
    }

    /// Angry detections currently inside the window.
    pub fn count(&self, now: Instant, window: Duration) -> usize {
        self.detections
            .iter()
            .filter(|d| now.saturating_duration_since(d.timestamp) <= window)
            .count()
    }

    /// Forgets all detections.
    pub fn reset(&mut self) {
        self.detections.clear();
    }

    /// Drops detections that fell out of the window.
    fn prune(&mut self, now: Instant, window: Duration) {
        self.detections
            .retain(|d| now.saturating_duration_since(d.timestamp) <= window);
    }
}

impl Trigger {
    fn from_detections(detections: Vec<AngerDetection>) -> Self {
        let peak_confidence = detections
            .iter()
            .map(|d| d.confidence)
            .fold(0.0_f32, f32::max);
        let mean_confidence = if detections.is_empty() {
            0.0
        } else {
            detections.iter().map(|d| d.confidence).sum::<f32>() / detections.len() as f32
        };
        let span = match (detections.first(), detections.last()) {
            (Some(first), Some(last)) => last.timestamp.saturating_duration_since(first.timestamp),
            _ => Duration::ZERO,
        };
        Self {
            detections,
            peak_confidence,
            mean_confidence,
            span,
        }
    }

    pub fn count(&self) -> usize {
        self.detections.len()
    }
}
