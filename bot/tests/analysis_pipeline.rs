//! End-to-end pipeline without Discord:
//! captured audio → 16 kHz WAV → (mock) AI → detection engine → (mock) moderation.

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use common::{
    COMMAND_CHANNEL, GUILD, MockAnalyzer, MockBackend, TARGET, angry, neutral, parse_wav_header,
    segment, session,
};
use rageguard::{
    ai::{AiError, Emotion},
    config::DetectionConfig,
    detection::Verdict,
    moderation::{ModerationOutcome, Moderator},
    monitor::{AnalysisPipeline, SegmentOutcome},
    server_log::ServerLog,
    state::{GuildSettings, SettingsStore},
};
use serenity::model::id::ChannelId;

fn settings(log_channel: Option<ChannelId>) -> Arc<SettingsStore> {
    Arc::new(SettingsStore::new(GuildSettings {
        // Three detections, so the tests exercise counting across several clips.
        detection: DetectionConfig {
            required_detections: 3,
            ..DetectionConfig::default()
        },
        log_channel,
    }))
}

fn pipeline_with_log(
    analyzer: Arc<MockAnalyzer>,
    backend: Arc<MockBackend>,
    monitor_only: bool,
    log_channel: Option<ChannelId>,
) -> AnalysisPipeline {
    let settings = settings(log_channel);
    AnalysisPipeline::new(
        analyzer,
        Moderator::new(backend.clone(), monitor_only),
        settings.clone(),
        ServerLog::new(backend, settings, GUILD),
    )
}

/// No log channel chosen: only trigger notices are posted (to the command channel).
fn pipeline(
    analyzer: Arc<MockAnalyzer>,
    backend: Arc<MockBackend>,
    monitor_only: bool,
) -> AnalysisPipeline {
    pipeline_with_log(analyzer, backend, monitor_only, None)
}

fn verdict(outcome: &SegmentOutcome) -> &Verdict {
    match outcome {
        SegmentOutcome::Analyzed { verdict, .. } => verdict,
        other => panic!("segment was not analysed: {other:?}"),
    }
}

#[tokio::test]
async fn three_angry_segments_trigger_monitor_only_action() {
    let analyzer = MockAnalyzer::with([Ok(angry(0.86)), Ok(angry(0.89)), Ok(angry(0.91))]);
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline(analyzer.clone(), backend.clone(), true);
    let session = session();
    let t0 = Instant::now();

    let first = pipeline.process(&session, segment(3.0, t0)).await;
    assert_eq!(
        verdict(&first),
        &Verdict::Counted {
            count: 1,
            required: 3
        }
    );
    let second = pipeline
        .process(&session, segment(3.0, t0 + Duration::from_secs(3)))
        .await;
    assert_eq!(
        verdict(&second),
        &Verdict::Counted {
            count: 2,
            required: 3
        }
    );
    let third = pipeline
        .process(&session, segment(3.0, t0 + Duration::from_secs(6)))
        .await;

    let SegmentOutcome::Analyzed {
        result,
        verdict: Verdict::Triggered(trigger),
        moderation,
    } = third
    else {
        panic!("third angry segment should trigger: {third:?}");
    };
    assert_eq!(result.emotion, Emotion::Angry);
    assert_eq!(trigger.count(), 3);
    assert_eq!(
        moderation,
        Some(ModerationOutcome::Simulated { precheck: Ok(()) })
    );

    // Monitor-only: logged and announced, never applied.
    assert_eq!(backend.timeout_count(), 0);
    let notices = backend.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, COMMAND_CHANNEL);
    assert!(notices[0].1.contains("MONITOR ONLY"));

    // Every segment reached the AI service as a 3 s, 16 kHz mono WAV.
    let received = analyzer.received.lock();
    assert_eq!(received.len(), 3);
    for wav in received.iter() {
        let header = parse_wav_header(wav);
        assert_eq!((header.channels, header.sample_rate), (1, 16_000));
        assert_eq!(header.data_len, 96_000);
    }
    drop(received);

    // Session state reflects the trigger; history was cleared.
    session.with_state(|s| {
        assert_eq!(s.segments_analyzed, 3);
        assert_eq!(s.triggers, 1);
        assert_eq!(
            s.last_analysis.as_ref().map(|a| a.emotion.clone()),
            Some(Emotion::Angry)
        );
        assert!(s.last_action.as_deref().unwrap().contains("monitor-only"));
        assert_eq!(
            s.detector
                .count(t0 + Duration::from_secs(7), Duration::from_secs(15)),
            0
        );
    });
}

#[tokio::test]
async fn enforcement_mode_applies_configured_timeout() {
    let analyzer = MockAnalyzer::with([Ok(angry(0.9)), Ok(angry(0.9)), Ok(angry(0.9))]);
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline(analyzer, backend.clone(), false);
    let session = session();
    let t0 = Instant::now();
    let mut last = None;
    for i in 0..3 {
        last = Some(
            pipeline
                .process(&session, segment(3.0, t0 + Duration::from_secs(3 * i)))
                .await,
        );
    }
    assert!(matches!(
        last,
        Some(SegmentOutcome::Analyzed {
            moderation: Some(ModerationOutcome::TimedOut),
            ..
        })
    ));
    let timeouts = backend.timeouts.lock();
    assert_eq!(timeouts.len(), 1);
    assert_eq!(timeouts[0].duration, Duration::from_secs(60));
}

#[tokio::test]
async fn each_trigger_needs_fresh_detections() {
    let analyzer = MockAnalyzer::with((0..6).map(|_| Ok(angry(0.95))));
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline(analyzer, backend.clone(), true);
    let session = session();
    let t0 = Instant::now();
    let mut verdicts = Vec::new();
    for i in 0..6 {
        let outcome = pipeline
            .process(&session, segment(3.0, t0 + Duration::from_secs(3 * i)))
            .await;
        verdicts.push(verdict(&outcome).clone());
    }
    // Triggers on the 3rd and 6th angry segment; the ones in between start a new count.
    assert!(matches!(verdicts[2], Verdict::Triggered(_)));
    assert!(matches!(verdicts[3], Verdict::Counted { count: 1, .. }));
    assert!(matches!(verdicts[4], Verdict::Counted { count: 2, .. }));
    assert!(matches!(verdicts[5], Verdict::Triggered(_)));
    assert_eq!(backend.notices().len(), 2);
}

#[tokio::test]
async fn neutral_speech_takes_no_action() {
    let analyzer = MockAnalyzer::with((0..5).map(|_| Ok(neutral(0.92))));
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline(analyzer, backend.clone(), false);
    let session = session();
    for _ in 0..5 {
        let outcome = pipeline
            .process(&session, segment(3.0, Instant::now()))
            .await;
        assert!(matches!(verdict(&outcome), Verdict::NotAngry { .. }));
    }
    assert_eq!(backend.timeout_count(), 0);
    assert!(backend.notices().is_empty());
}

#[tokio::test]
async fn ai_failures_skip_segments_without_crashing() {
    let analyzer = MockAnalyzer::with([
        Err(AiError::Timeout(Duration::from_secs(10))),
        Err(AiError::NotReady("loading".into())),
        Ok(angry(0.9)),
    ]);
    let mut pipeline = pipeline(analyzer, MockBackend::allowed(), true);
    let session = session();

    assert!(matches!(
        pipeline
            .process(&session, segment(3.0, Instant::now()))
            .await,
        SegmentOutcome::AiFailed(_)
    ));
    assert!(matches!(
        pipeline
            .process(&session, segment(3.0, Instant::now()))
            .await,
        SegmentOutcome::AiFailed(_)
    ));
    let recovered = pipeline
        .process(&session, segment(3.0, Instant::now()))
        .await;
    assert!(matches!(
        verdict(&recovered),
        Verdict::Counted { count: 1, .. }
    ));
    session.with_state(|s| {
        assert_eq!(s.ai_failures, 2);
        assert_eq!(s.segments_analyzed, 1);
    });
}

#[tokio::test]
async fn stopped_session_does_not_analyse() {
    let analyzer = MockAnalyzer::with([Ok(angry(0.9))]);
    let mut pipeline = pipeline(analyzer.clone(), MockBackend::allowed(), true);
    let session = session();
    session.cancel();
    assert_eq!(
        pipeline
            .process(&session, segment(3.0, Instant::now()))
            .await,
        SegmentOutcome::Cancelled
    );
}

#[tokio::test]
async fn log_channel_gets_only_warnings_and_triggers() {
    let log = ChannelId::new(999);
    let analyzer = MockAnalyzer::with([
        Ok(neutral(0.9)),
        Ok(angry(0.86)),
        Ok(angry(0.89)),
        Ok(angry(0.91)),
    ]);
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline_with_log(analyzer, backend.clone(), true, Some(log));
    let session = session();
    let t0 = Instant::now();
    for i in 0..4 {
        pipeline
            .process(&session, segment(3.0, t0 + Duration::from_secs(3 * i)))
            .await;
    }

    let notices = backend.notices();
    assert!(notices.iter().all(|(channel, _)| *channel == log));
    let texts: Vec<&str> = notices.iter().map(|(_, text)| text.as_str()).collect();
    // Individual detections stay in the bot log; only the trigger notice reaches Discord.
    assert_eq!(texts.len(), 1, "{texts:#?}");
    assert!(texts[0].contains("MONITOR ONLY"));
    assert!(texts[0].contains(&format!("<@{TARGET}>")));
}

#[tokio::test]
async fn ai_outage_is_reported_once() {
    let log = ChannelId::new(999);
    let analyzer = MockAnalyzer::with([
        Err(AiError::Timeout(Duration::from_secs(10))),
        Err(AiError::Timeout(Duration::from_secs(10))),
        Err(AiError::NotReady("loading".into())),
        Ok(neutral(0.9)),
    ]);
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline_with_log(analyzer, backend.clone(), true, Some(log));
    let session = session();
    for _ in 0..4 {
        pipeline
            .process(&session, segment(3.0, Instant::now()))
            .await;
    }
    let texts: Vec<String> = backend.notices().into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts.len(), 1, "{texts:#?}");
    assert!(texts[0].contains("isn't responding"));
}

#[tokio::test]
async fn rejected_audio_is_not_an_outage() {
    let analyzer = MockAnalyzer::with([Err(AiError::Rejected {
        status: 422,
        detail: "too short".into(),
    })]);
    let backend = MockBackend::allowed();
    let mut pipeline = pipeline_with_log(analyzer, backend.clone(), true, Some(ChannelId::new(9)));
    pipeline
        .process(&session(), segment(3.0, Instant::now()))
        .await;
    assert!(backend.notices().is_empty());
}

#[tokio::test]
async fn disgust_counts_as_anger_by_default() {
    // Real angry shouting is often labelled "Disgust" by the model (seen in live testing).
    let disgust = || {
        common::emotion(
            r#"{"emotion": "Disgust", "confidence": 0.98,
                "scores": {"Disgust": 0.98, "Angry": 0.01, "Neutral": 0.01}}"#,
        )
    };
    let analyzer = MockAnalyzer::with([Ok(disgust()), Ok(disgust())]);
    let backend = MockBackend::allowed();
    let settings = Arc::new(SettingsStore::new(GuildSettings {
        detection: DetectionConfig::default(), // 2 detections, Angry + Disgust
        log_channel: None,
    }));
    let mut pipeline = AnalysisPipeline::new(
        analyzer,
        Moderator::new(backend.clone(), true),
        settings.clone(),
        ServerLog::new(backend.clone(), settings, GUILD),
    );
    let session = session();
    let t0 = Instant::now();
    let first = pipeline.process(&session, segment(1.7, t0)).await;
    assert!(matches!(
        verdict(&first),
        Verdict::Counted {
            count: 1,
            required: 2
        }
    ));
    let second = pipeline
        .process(&session, segment(1.5, t0 + Duration::from_secs(6)))
        .await;
    assert!(matches!(verdict(&second), Verdict::Triggered(_)));
    assert!(backend.notices()[0].1.contains("MONITOR ONLY"));
}
