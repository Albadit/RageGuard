//! Threshold and sliding-window behaviour of the anger detection engine.

use std::time::{Duration, Instant};

use rageguard::{
    config::DetectionConfig,
    detection::{AngerDetector, DetectionRules, Verdict},
};

/// Three detections, so the tests exercise counting across several clips.
fn rules() -> DetectionRules {
    DetectionRules {
        threshold: 0.80,
        required_detections: 3,
        ..DetectionRules::from(&DetectionConfig::default())
    }
}

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s)
}

#[test]
fn rules_come_from_config() {
    let rules = DetectionRules::from(&DetectionConfig::default());
    assert_eq!(rules.threshold, 0.70);
    assert_eq!(rules.required_detections, 2);
    assert_eq!(rules.window, Duration::from_secs(15));
}

#[test]
fn single_prediction_never_triggers() {
    let mut detector = AngerDetector::new();
    let verdict = detector.observe(0.99, Instant::now(), &rules());
    assert_eq!(
        verdict,
        Verdict::Counted {
            count: 1,
            required: 3
        }
    );
}

#[test]
fn below_threshold_is_not_counted() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    for i in 0..10 {
        let verdict = detector.observe(0.79, t0 + secs(i as f32), &rules());
        assert!(matches!(verdict, Verdict::NotAngry { .. }));
    }
    assert_eq!(detector.count(t0 + secs(10.0), rules().window), 0);
}

#[test]
fn score_equal_to_threshold_counts() {
    let mut detector = AngerDetector::new();
    let verdict = detector.observe(0.80, Instant::now(), &rules());
    assert!(matches!(verdict, Verdict::Counted { count: 1, .. }));
}

#[test]
fn spec_example_triggers_on_third_detection() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    assert!(matches!(
        detector.observe(0.86, t0, &rules()),
        Verdict::Counted { count: 1, .. }
    ));
    assert!(matches!(
        detector.observe(0.89, t0 + secs(3.0), &rules()),
        Verdict::Counted { count: 2, .. }
    ));
    let Verdict::Triggered(trigger) = detector.observe(0.91, t0 + secs(6.0), &rules()) else {
        panic!("third detection should trigger");
    };
    assert_eq!(trigger.count(), 3);
    assert!((trigger.peak_confidence - 0.91).abs() < 1e-6);
    assert!((trigger.mean_confidence - (0.86 + 0.89 + 0.91) / 3.0).abs() < 1e-6);
    assert_eq!(trigger.span, secs(6.0));
}

#[test]
fn trigger_clears_history() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    for i in 0..3 {
        detector.observe(0.9, t0 + secs(i as f32), &rules());
    }
    assert_eq!(detector.count(t0 + secs(3.0), rules().window), 0);
}

#[test]
fn detections_outside_window_expire() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    detector.observe(0.9, t0, &rules());
    detector.observe(0.9, t0 + secs(10.0), &rules());
    // 20 s after the first detection: the first one has left the 15 s window.
    let verdict = detector.observe(0.9, t0 + secs(20.0), &rules());
    assert_eq!(
        verdict,
        Verdict::Counted {
            count: 2,
            required: 3
        }
    );
}

#[test]
fn detection_exactly_at_window_edge_still_counts() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    detector.observe(0.9, t0, &rules());
    detector.observe(0.9, t0 + secs(7.5), &rules());
    assert!(matches!(
        detector.observe(0.9, t0 + secs(15.0), &rules()),
        Verdict::Triggered(_)
    ));
}

#[test]
fn slow_steady_anger_never_triggers() {
    // One angry segment every 8 s: never three inside 15 s.
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    for i in 0..20 {
        let verdict = detector.observe(0.95, t0 + secs(i as f32 * 8.0), &rules());
        assert!(!matches!(verdict, Verdict::Triggered(_)), "iteration {i}");
    }
}

#[test]
fn neutral_speech_between_angry_segments_does_not_reset_count() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    detector.observe(0.9, t0, &rules());
    detector.observe(0.1, t0 + secs(3.0), &rules());
    detector.observe(0.9, t0 + secs(6.0), &rules());
    assert!(matches!(
        detector.observe(0.9, t0 + secs(9.0), &rules()),
        Verdict::Triggered(_)
    ));
}

#[test]
fn after_a_trigger_counting_starts_over() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    for i in 0..3 {
        detector.observe(0.9, t0 + secs(i as f32 * 3.0), &rules());
    }
    // The detections that caused the first trigger never count towards the next one.
    assert_eq!(
        detector.observe(0.95, t0 + secs(9.0), &rules()),
        Verdict::Counted {
            count: 1,
            required: 3
        }
    );
    assert_eq!(
        detector.observe(0.95, t0 + secs(12.0), &rules()),
        Verdict::Counted {
            count: 2,
            required: 3
        }
    );
    assert!(matches!(
        detector.observe(0.95, t0 + secs(15.0), &rules()),
        Verdict::Triggered(t) if t.count() == 3
    ));
}

#[test]
fn required_one_triggers_immediately() {
    let rules = DetectionRules {
        required_detections: 1,
        ..rules()
    };
    let mut detector = AngerDetector::new();
    assert!(matches!(
        detector.observe(0.85, Instant::now(), &rules),
        Verdict::Triggered(t) if t.count() == 1
    ));
}

#[test]
fn non_finite_scores_are_ignored() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    assert_eq!(
        detector.observe(f32::NAN, t0, &rules()),
        Verdict::NotAngry { score: 0.0 }
    );
    assert!(matches!(
        detector.observe(f32::INFINITY, t0, &rules()),
        Verdict::NotAngry { .. }
    ));
    assert_eq!(detector.count(t0, rules().window), 0);
}

#[test]
fn reset_clears_detections() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    detector.observe(0.9, t0, &rules());
    detector.observe(0.9, t0 + secs(3.0), &rules());
    detector.reset();
    assert_eq!(detector.count(t0 + secs(4.0), rules().window), 0);
    assert!(matches!(
        detector.observe(0.9, t0 + secs(5.0), &rules()),
        Verdict::Counted { count: 1, .. }
    ));
}

#[test]
fn count_does_not_include_expired_detections() {
    let mut detector = AngerDetector::new();
    let t0 = Instant::now();
    detector.observe(0.9, t0, &rules());
    detector.observe(0.9, t0 + secs(5.0), &rules());
    assert_eq!(detector.count(t0 + secs(10.0), rules().window), 2);
    assert_eq!(detector.count(t0 + secs(16.0), rules().window), 1);
    assert_eq!(detector.count(t0 + secs(21.0), rules().window), 0);
}
