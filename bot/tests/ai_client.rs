//! AI response parsing and the HTTP client, against a mock server.

mod common;

use std::time::Duration;

use common::{ANGRY_91, NEUTRAL_92, emotion};
use rageguard::{
    ai::{AiError, Emotion, EmotionAnalyzer, EmotionResult, HttpEmotionClient},
    config::AiServiceConfig,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header_regex, method, path},
};

// --- parsing ----------------------------------------------------------------------------------

#[test]
fn parses_mocked_angry_response() {
    let result = emotion(ANGRY_91);
    assert_eq!(result.emotion, Emotion::Angry);
    assert!((result.confidence - 0.91).abs() < 1e-6);
    assert!((result.angry_score() - 0.91).abs() < 1e-6);
}

#[test]
fn parses_mocked_neutral_response() {
    let result = emotion(NEUTRAL_92);
    assert_eq!(result.emotion, Emotion::Neutral);
    assert_eq!(result.angry_score(), 0.0);
}

#[test]
fn angry_score_prefers_full_score_table() {
    let result = emotion(
        r#"{"emotion": "Neutral", "confidence": 0.55,
            "scores": {"Angry": 0.40, "Neutral": 0.55, "Sad": 0.05}}"#,
    );
    assert_eq!(result.emotion, Emotion::Neutral);
    assert!((result.angry_score() - 0.40).abs() < 1e-6);
    assert_eq!(result.scores.len(), 3);
}

#[test]
fn model_specific_labels_are_normalised() {
    for (label, expected) in [
        ("angry", Emotion::Angry),
        ("ANG", Emotion::Angry),
        ("calm", Emotion::Neutral),
        ("fearful", Emotion::Fear),
        ("surprised", Emotion::Surprise),
        ("disgust", Emotion::Disgust),
        ("happy", Emotion::Happy),
        ("sad", Emotion::Sad),
    ] {
        assert_eq!(Emotion::from_label(label), expected, "{label}");
    }
    assert_eq!(Emotion::from_label("Bored"), Emotion::Other("Bored".into()));
    assert_eq!(Emotion::Angry.to_string(), "Angry");
}

#[test]
fn raw_model_score_table_is_normalised() {
    let result = emotion(
        r#"{"emotion": "angry", "confidence": 0.8,
            "scores": {"angry": 0.8, "calm": 0.1, "fearful": 0.1}}"#,
    );
    assert_eq!(result.emotion, Emotion::Angry);
    assert!(result.scores.contains_key(&Emotion::Neutral));
    assert!(result.scores.contains_key(&Emotion::Fear));
}

#[test]
fn rounding_just_above_one_is_clamped() {
    let result = emotion(r#"{"emotion": "Angry", "confidence": 1.0004}"#);
    assert_eq!(result.confidence, 1.0);
}

#[test]
fn invalid_responses_are_rejected() {
    for body in [
        r#"{"emotion": "Angry", "confidence": 1.5}"#,
        r#"{"emotion": "Angry", "confidence": -0.1}"#,
        r#"{"emotion": "", "confidence": 0.5}"#,
        r#"{"emotion": "Angry"}"#,
        r#"{"confidence": 0.5}"#,
        r#"{"emotion": "Angry", "confidence": "high"}"#,
        r#"{"emotion": "Angry", "confidence": 0.9, "scores": {"Angry": 2.0}}"#,
        r#"not json"#,
        "",
    ] {
        assert!(
            matches!(
                EmotionResult::from_json(body.as_bytes()),
                Err(AiError::InvalidResponse(_))
            ),
            "{body}"
        );
    }
}

// --- HTTP client ------------------------------------------------------------------------------

fn client(url: &str) -> HttpEmotionClient {
    let config = AiServiceConfig {
        request_timeout_seconds: 1,
        connect_timeout_seconds: 1,
    };
    HttpEmotionClient::new(url, &config).unwrap()
}

fn fake_wav() -> Vec<u8> {
    rageguard::voice::wav::encode_pcm16_mono(&[0, 100, -100, 0], 16_000)
}

#[tokio::test]
async fn analyze_uploads_binary_multipart_wav() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .and(header_regex(
            "content-type",
            "^multipart/form-data; boundary=",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANGRY_91))
        .expect(1)
        .mount(&server)
        .await;

    let result = client(&server.uri()).analyze(fake_wav()).await.unwrap();
    assert_eq!(result.emotion, Emotion::Angry);

    let requests = server.received_requests().await.unwrap();
    let body = &requests[0].body;
    let contains = |needle: &[u8]| body.windows(needle.len()).any(|w| w == needle);
    assert!(contains(b"name=\"file\""));
    assert!(contains(b"filename=\"segment.wav\""));
    assert!(contains(b"Content-Type: audio/wav"));
    // Raw RIFF bytes, not base64.
    assert!(contains(b"RIFF"));
    assert!(contains(b"WAVE"));
}

#[tokio::test]
async fn trailing_slash_in_base_url_is_handled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(200).set_body_string(NEUTRAL_92))
        .mount(&server)
        .await;
    let result = client(&format!("{}/", server.uri()))
        .analyze(fake_wav())
        .await
        .unwrap();
    assert_eq!(result.emotion, Emotion::Neutral);
}

#[tokio::test]
async fn model_loading_maps_to_not_ready() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(
            ResponseTemplate::new(503).set_body_string(r#"{"detail": "Model is still loading"}"#),
        )
        .mount(&server)
        .await;
    let err = client(&server.uri()).analyze(fake_wav()).await.unwrap_err();
    assert!(matches!(&err, AiError::NotReady(d) if d == "Model is still loading"));
    assert!(err.is_service_problem());
}

#[tokio::test]
async fn unsupported_audio_maps_to_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(
            ResponseTemplate::new(422).set_body_string(r#"{"detail": "Unsupported audio format"}"#),
        )
        .mount(&server)
        .await;
    let err = client(&server.uri()).analyze(fake_wav()).await.unwrap_err();
    assert!(
        matches!(&err, AiError::Rejected { status: 422, detail } if detail == "Unsupported audio format")
    );
    assert!(!err.is_service_problem());
}

#[tokio::test]
async fn server_error_maps_to_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;
    let err = client(&server.uri()).analyze(fake_wav()).await.unwrap_err();
    assert!(
        matches!(&err, AiError::Server { status: 500, detail } if detail == "Internal Server Error")
    );
}

#[tokio::test]
async fn malformed_success_body_is_invalid_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"label": "angry"}"#))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server.uri()).analyze(fake_wav()).await,
        Err(AiError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn slow_inference_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ANGRY_91)
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let err = client(&server.uri()).analyze(fake_wav()).await.unwrap_err();
    assert!(matches!(err, AiError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn offline_service_is_unavailable() {
    // Reserve a free port, then close it so nothing is listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = client(&format!("http://127.0.0.1:{port}"))
        .analyze(fake_wav())
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::Unavailable { .. }), "{err:?}");
    assert!(err.is_service_problem());
}

#[tokio::test]
async fn health_endpoint_is_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"status": "ok", "model_loaded": true}"#),
        )
        .mount(&server)
        .await;
    let health = client(&server.uri()).health().await.unwrap();
    assert_eq!(health.status, "ok");
    assert!(health.model_loaded);
}

#[tokio::test]
async fn health_while_loading_is_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string(r#"{"status": "loading", "model_loaded": false}"#),
        )
        .mount(&server)
        .await;
    let health = client(&server.uri()).health().await.unwrap();
    assert!(!health.model_loaded);
}

#[test]
fn anger_score_adds_up_the_chosen_emotions() {
    let result = emotion(
        r#"{"emotion": "Disgust", "confidence": 0.62,
            "scores": {"Disgust": 0.62, "Angry": 0.30, "Neutral": 0.08}}"#,
    );
    assert!((result.anger_score(&[Emotion::Angry]) - 0.30).abs() < 1e-6);
    assert!((result.anger_score(&[Emotion::Angry, Emotion::Disgust]) - 0.92).abs() < 1e-6);
    assert_eq!(result.anger_score(&[Emotion::Sad]), 0.0);
}

#[test]
fn anger_score_without_score_table_uses_top_emotion() {
    let disgust = emotion(r#"{"emotion": "Disgust", "confidence": 0.97}"#);
    assert!((disgust.anger_score(&[Emotion::Angry, Emotion::Disgust]) - 0.97).abs() < 1e-6);
    assert_eq!(disgust.anger_score(&[Emotion::Angry]), 0.0);
}
