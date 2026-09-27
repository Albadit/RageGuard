//! Decides when repeated angry speech should become a moderation action.

mod engine;

pub use engine::{AngerDetection, AngerDetector, DetectionRules, Trigger, Verdict};
