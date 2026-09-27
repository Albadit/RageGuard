//! Live end-to-end check against a running AI service. Ignored by default:
//!
//! ```text
//! docker compose up -d            # or: uvicorn app.main:app --port 8000
//! cargo test --test e2e_ai_service -- --ignored --nocapture
//! ```
//!
//! Environment:
//! * `RAGEGUARD_AI_URL` - service URL (default `http://127.0.0.1:8000`)
//! * `RAGEGUARD_E2E_WAV` - optional 16-bit PCM WAV of real speech to analyse

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use common::{GUILD, MockBackend, session};
use rageguard::{
    ai::{EmotionAnalyzer, HttpEmotionClient},
    config::{AiServiceConfig, DetectionConfig},
    moderation::Moderator,
    monitor::{AnalysisPipeline, SegmentOutcome},
    server_log::ServerLog,
    state::{GuildSettings, SettingsStore},
    voice::{AudioSegment, PcmFormat, Resampler},
};

fn client() -> HttpEmotionClient {
    let url = std::env::var("RAGEGUARD_AI_URL").unwrap_or_else(|_| "http://127.0.0.1:8000".into());
    let config = AiServiceConfig {
        request_timeout_seconds: 60,
        connect_timeout_seconds: 3,
    };
    HttpEmotionClient::new(&url, &config).unwrap()
}

/// Loads a PCM16 WAV and converts it to Discord's 48 kHz stereo capture format, so it goes
/// through exactly the same conversion as live voice.
fn load_as_discord_audio(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).expect("read RAGEGUARD_E2E_WAV");
    assert_eq!(&bytes[0..4], b"RIFF", "not a WAV file");
    let mut pos = 12;
    let (mut channels, mut rate, mut data) = (0u16, 0u32, None);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        match id {
            b"fmt " => {
                assert_eq!(
                    u16::from_le_bytes([body[0], body[1]]),
                    1,
                    "PCM WAV required"
                );
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                assert_eq!(
                    u16::from_le_bytes([body[14], body[15]]),
                    16,
                    "16-bit required"
                );
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }
    let samples: Vec<i16> = data
        .expect("data chunk")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect();
    let mono = rageguard::voice::resampler::downmix_to_mono(&samples, channels).unwrap();
    let at_48k = Resampler::new(rate, 48_000).unwrap().process(&mono);
    rageguard::voice::resampler::to_i16(&at_48k)
        .into_iter()
        .flat_map(|s| [s, s])
        .collect()
}

#[tokio::test]
#[ignore = "requires a running AI service"]
async fn health_reports_model_loaded() {
    let health = client().health().await.expect("AI service reachable");
    println!("health: {health:?}");
    assert!(health.model_loaded, "model not loaded yet: {health:?}");
}

#[tokio::test]
#[ignore = "requires a running AI service"]
async fn full_pipeline_against_live_service() {
    let pcm = match std::env::var("RAGEGUARD_E2E_WAV") {
        Ok(path) => load_as_discord_audio(&path),
        Err(_) => common::stereo_sine(PcmFormat::DISCORD, 180.0, 3.0, 0.3),
    };
    let segment_len = PcmFormat::DISCORD.samples_for(Duration::from_secs(3));

    let backend = MockBackend::allowed();
    let settings = Arc::new(SettingsStore::new(GuildSettings {
        detection: DetectionConfig::default(),
        log_channel: None,
    }));
    let mut pipeline = AnalysisPipeline::new(
        Arc::new(client()),
        Moderator::new(backend.clone(), true),
        settings.clone(),
        ServerLog::new(backend.clone(), settings, GUILD),
    );
    let session = session();
    let t0 = Instant::now();

    for (i, chunk) in pcm.chunks(segment_len).enumerate() {
        if chunk.len() < segment_len / 3 {
            break;
        }
        let segment = AudioSegment {
            samples: chunk.to_vec(),
            format: PcmFormat::DISCORD,
            captured_at: t0 + Duration::from_secs(3 * i as u64),
        };
        let started = Instant::now();
        let outcome = pipeline.process(&session, segment).await;
        let SegmentOutcome::Analyzed {
            result,
            verdict,
            moderation,
        } = outcome
        else {
            panic!("segment {i} failed: {outcome:?}");
        };
        println!(
            "segment {i}: {} ({:.0}%), angry {:.0}% -> {verdict:?} {moderation:?} [{:?}]",
            result.emotion,
            result.confidence * 100.0,
            result.angry_score() * 100.0,
            started.elapsed()
        );
        assert!((0.0..=1.0).contains(&result.confidence));
    }
    assert_eq!(
        backend.timeout_count(),
        0,
        "monitor-only must never time out"
    );
}
