//! The server log posted to the log channel chosen in Discord.

mod common;

use std::{sync::Arc, time::Duration};

use common::{GUILD, MODERATOR, MockBackend, TARGET, VOICE_CHANNEL};
use rageguard::{
    config::DetectionConfig,
    server_log::{LogEvent, ServerLog},
    state::{GuildSettings, SettingsStore},
};
use serenity::model::id::ChannelId;

fn store(log_channel: Option<ChannelId>) -> Arc<SettingsStore> {
    Arc::new(SettingsStore::new(GuildSettings {
        detection: DetectionConfig::default(),
        log_channel,
    }))
}

#[tokio::test]
async fn nothing_is_posted_until_a_channel_is_chosen() {
    let backend = MockBackend::allowed();
    let settings = store(None);
    let log = ServerLog::new(backend.clone(), settings.clone(), GUILD);
    let warning = || LogEvent::AiUnavailable {
        error: "timed out".into(),
    };
    log.post(warning()).await;
    assert!(backend.notices().is_empty());

    let chosen = ChannelId::new(55);
    settings.set_log_channel(GUILD, chosen).await.unwrap();
    log.post(warning()).await;
    assert_eq!(backend.notices().len(), 1);
    assert_eq!(backend.notices()[0].0, chosen);
}

#[test]
fn messages_mention_people_and_channels() {
    let started = LogEvent::MonitoringStarted {
        by: MODERATOR,
        target: TARGET,
        channel: Some(VOICE_CHANNEL),
    }
    .message();
    assert!(started.contains(&format!("<@{MODERATOR}>")));
    assert!(started.contains(&format!("<@{TARGET}>")));
    assert!(started.contains(&format!("<#{VOICE_CHANNEL}>")));

    let waiting = LogEvent::MonitoringStarted {
        by: MODERATOR,
        target: TARGET,
        channel: None,
    }
    .message();
    assert!(waiting.contains("Waiting for them to join"));

    let stopped = LogEvent::MonitoringStopped {
        by: MODERATOR,
        target: TARGET,
        segments_analyzed: 42,
        triggers: 1,
    }
    .message();
    assert!(stopped.contains("stopped monitoring") && stopped.contains("42 segment"));
}

#[test]
fn anger_detection_reads_naturally() {
    let text = LogEvent::AngerDetected {
        target: TARGET,
        confidence: 0.873,
        count: 2,
        required: 3,
        window: Duration::from_secs(15),
    }
    .message();
    assert_eq!(
        text,
        format!("😠 Angry speech from <@{TARGET}> (87%). 2/3 within 15 seconds.")
    );
}

#[test]
fn voice_events_explain_what_happens_next() {
    let listening = LogEvent::Listening {
        target: TARGET,
        channel: VOICE_CHANNEL,
    }
    .message();
    assert!(listening.contains("Listening to"));

    let left = LogEvent::WaitingForVoice { target: TARGET }.message();
    assert!(left.contains("rejoin"));

    let problem = LogEvent::VoiceProblem {
        target: TARGET,
        channel: VOICE_CHANNEL,
        error: "Missing Access".into(),
    }
    .message();
    assert!(problem.contains("Missing Access") && problem.contains("keep retrying"));
}

#[tokio::test]
async fn only_warnings_are_posted() {
    let backend = MockBackend::allowed();
    let settings = store(Some(ChannelId::new(55)));
    let log = ServerLog::new(backend.clone(), settings, GUILD);
    for info in [
        LogEvent::MonitoringStarted {
            by: MODERATOR,
            target: TARGET,
            channel: Some(VOICE_CHANNEL),
        },
        LogEvent::Listening {
            target: TARGET,
            channel: VOICE_CHANNEL,
        },
        LogEvent::WaitingForVoice { target: TARGET },
        LogEvent::AiRecovered,
    ] {
        assert!(!info.is_warning());
        log.post(info).await;
    }
    assert!(backend.notices().is_empty());

    log.post(LogEvent::VoiceProblem {
        target: TARGET,
        channel: VOICE_CHANNEL,
        error: "Missing Access".into(),
    })
    .await;
    assert_eq!(backend.notices().len(), 1);
}
