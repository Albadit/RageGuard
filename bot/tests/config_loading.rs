//! Environment and TOML configuration loading and validation.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rageguard::config::{
    ConfigError, DEFAULT_AI_SERVICE_URL, DetectionConfig, EnvConfig, FileConfig,
};

fn env(pairs: &[(&str, &str)]) -> Result<EnvConfig, ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    EnvConfig::from_lookup(|key| map.get(key).cloned())
}

fn parse(toml: &str) -> Result<FileConfig, ConfigError> {
    FileConfig::from_toml_str(toml, Path::new("test.toml"))
}

// --- environment ------------------------------------------------------------------------------

#[test]
fn missing_token_is_a_clear_error() {
    let err = env(&[]).unwrap_err();
    assert!(matches!(err, ConfigError::MissingEnv("DISCORD_TOKEN")));
    let message = err.to_string();
    assert!(message.contains("DISCORD_TOKEN"));
    assert!(message.contains(".env.example"));
}

#[test]
fn empty_token_counts_as_missing() {
    // .env.example ships with `DISCORD_TOKEN=`.
    assert!(matches!(
        env(&[("DISCORD_TOKEN", "   ")]),
        Err(ConfigError::MissingEnv("DISCORD_TOKEN"))
    ));
}

#[test]
fn token_with_bot_prefix_is_rejected() {
    assert!(matches!(
        env(&[("DISCORD_TOKEN", "Bot abc.def")]),
        Err(ConfigError::InvalidEnv {
            name: "DISCORD_TOKEN",
            ..
        })
    ));
}

#[test]
fn defaults_are_safe() {
    let config = env(&[("DISCORD_TOKEN", "token")]).unwrap();
    assert!(config.monitor_only, "monitor-only must default to on");
    assert_eq!(config.ai_service_url, DEFAULT_AI_SERVICE_URL);
    assert_eq!(config.log_level, "info");
    assert_eq!(config.config_path, PathBuf::from("rageguard.toml"));
    assert_eq!(config.data_dir, PathBuf::from("data"));
}

#[test]
fn full_environment_is_parsed() {
    let config = env(&[
        ("DISCORD_TOKEN", "token"),
        ("AI_SERVICE_URL", "http://ai:8000/"),
        ("MONITOR_ONLY", "false"),
        ("LOG_LEVEL", "DEBUG"),
        ("RAGEGUARD_CONFIG", "custom.toml"),
        ("RAGEGUARD_DATA_DIR", "/var/lib/rageguard"),
    ])
    .unwrap();
    assert_eq!(
        config.ai_service_url, "http://ai:8000",
        "trailing slash trimmed"
    );
    assert!(!config.monitor_only);
    assert_eq!(config.log_level, "debug");
    assert_eq!(config.config_path, PathBuf::from("custom.toml"));
    assert_eq!(config.data_dir, PathBuf::from("/var/lib/rageguard"));
}

#[test]
fn removed_variables_are_ignored() {
    // These variables no longer exist; old .env files must still work.
    let config = env(&[
        ("DISCORD_TOKEN", "t"),
        ("DISCORD_CLIENT_ID", "abc"),
        ("DISCORD_GUILD_ID", "not even a number"),
        ("MOD_LOG_CHANNEL_ID", "123"),
    ]);
    assert!(config.is_ok());
}

#[test]
fn monitor_only_accepts_common_spellings() {
    for (value, expected) in [
        ("true", true),
        ("TRUE", true),
        ("1", true),
        ("yes", true),
        ("on", true),
        ("false", false),
        ("0", false),
        ("No", false),
        ("off", false),
    ] {
        let config = env(&[("DISCORD_TOKEN", "t"), ("MONITOR_ONLY", value)]).unwrap();
        assert_eq!(config.monitor_only, expected, "MONITOR_ONLY={value}");
    }
}

#[test]
fn ambiguous_monitor_only_is_rejected_instead_of_guessed() {
    let err = env(&[("DISCORD_TOKEN", "t"), ("MONITOR_ONLY", "maybe")]).unwrap_err();
    assert!(matches!(
        err,
        ConfigError::InvalidEnv {
            name: "MONITOR_ONLY",
            ..
        }
    ));
}

#[test]
fn invalid_ai_url_is_rejected() {
    for value in ["not a url", "ftp://host", "127.0.0.1:8000"] {
        assert!(
            matches!(
                env(&[("DISCORD_TOKEN", "t"), ("AI_SERVICE_URL", value)]),
                Err(ConfigError::InvalidEnv {
                    name: "AI_SERVICE_URL",
                    ..
                })
            ),
            "{value}"
        );
    }
}

#[test]
fn invalid_log_level_is_rejected() {
    assert!(matches!(
        env(&[("DISCORD_TOKEN", "t"), ("LOG_LEVEL", "verbose")]),
        Err(ConfigError::InvalidEnv {
            name: "LOG_LEVEL",
            ..
        })
    ));
}

#[test]
fn debug_output_redacts_token() {
    let config = env(&[("DISCORD_TOKEN", "super-secret-token")]).unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-token"));
    assert!(debug.contains("<redacted>"));
}

// --- TOML -------------------------------------------------------------------------------------

#[test]
fn defaults_match_specification() {
    let d = DetectionConfig::default();
    assert_eq!(d.threshold, 0.70);
    assert_eq!(d.required_detections, 2);
    assert_eq!(d.window_seconds, 15);
    assert_eq!(d.timeout_minutes, 1);
    assert_eq!(d.segment_seconds, 3.0);
    assert!(d.warnings().is_empty());
}

#[test]
fn spec_example_parses() {
    let config = parse(
        r#"
        [anger_detection]
        threshold = 0.70
        required_detections = 2
        window_seconds = 15
        timeout_minutes = 1
        segment_seconds = 3
        "#,
    )
    .unwrap();
    assert_eq!(config, FileConfig::default());
}

#[test]
fn removed_cooldown_setting_is_reported_clearly() {
    // The cooldown was removed; an old config file that still has it should say so.
    let err = parse("[anger_detection]\ncooldown_seconds = 120\n").unwrap_err();
    assert!(err.to_string().contains("cooldown_seconds"), "{err}");
}

#[test]
fn shipped_config_file_is_valid_and_matches_defaults() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rageguard.toml");
    let (config, found) = FileConfig::load(&path).unwrap().expect("file exists");
    assert_eq!(found, path);
    assert_eq!(config, FileConfig::default());
}

#[test]
fn partial_config_uses_defaults_for_the_rest() {
    let config = parse("[anger_detection]\nthreshold = 0.9\n").unwrap();
    assert_eq!(config.anger_detection.threshold, 0.9);
    assert_eq!(config.anger_detection.required_detections, 2);
    assert_eq!(config.audio, Default::default());
}

#[test]
fn empty_file_uses_defaults() {
    assert_eq!(parse("").unwrap(), FileConfig::default());
}

#[test]
fn unknown_keys_are_rejected_to_catch_typos() {
    let err = parse("[anger_detection]\nthreshhold = 0.9\n").unwrap_err();
    assert!(matches!(err, ConfigError::Parse { .. }));
    assert!(err.to_string().contains("threshhold"));
}

#[test]
fn wrong_types_are_rejected() {
    assert!(matches!(
        parse("[anger_detection]\nrequired_detections = \"three\"\n"),
        Err(ConfigError::Parse { .. })
    ));
}

#[test]
fn out_of_range_values_name_the_field() {
    let cases = [
        ("threshold = 1.5", "anger_detection.threshold"),
        ("threshold = 0.0", "anger_detection.threshold"),
        (
            "required_detections = 0",
            "anger_detection.required_detections",
        ),
        ("window_seconds = 0", "anger_detection.window_seconds"),
        ("timeout_minutes = 0", "anger_detection.timeout_minutes"),
        ("timeout_minutes = 40321", "anger_detection.timeout_minutes"),
        ("segment_seconds = 0.5", "anger_detection.segment_seconds"),
        ("segment_seconds = 30", "anger_detection.segment_seconds"),
    ];
    for (line, field) in cases {
        let err = parse(&format!("[anger_detection]\n{line}\n")).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { field: f, .. } if f == field),
            "{line} -> {err}"
        );
    }
}

#[test]
fn audio_and_ai_sections_are_validated() {
    assert!(matches!(
        parse("[audio]\nmin_segment_seconds = 4.0\n"),
        Err(ConfigError::Invalid {
            field: "audio.min_segment_seconds",
            ..
        })
    ));
    assert!(matches!(
        parse("[audio]\nmax_pending_segments = 0\n"),
        Err(ConfigError::Invalid {
            field: "audio.max_pending_segments",
            ..
        })
    ));
    assert!(matches!(
        parse("[ai_service]\nrequest_timeout_seconds = 0\n"),
        Err(ConfigError::Invalid {
            field: "ai_service.request_timeout_seconds",
            ..
        })
    ));
}

#[test]
fn impossible_rules_produce_a_warning() {
    let config = DetectionConfig {
        required_detections: 6,
        window_seconds: 10,
        ..DetectionConfig::default()
    };
    let warnings = config.warnings();
    assert!(
        warnings.iter().any(|w| w.contains("never trigger")),
        "{warnings:?}"
    );
}

#[test]
fn missing_file_returns_none() {
    let path = std::env::temp_dir().join("rageguard-definitely-missing-config.toml");
    assert!(FileConfig::load(&path).unwrap().is_none());
}

#[test]
fn file_on_disk_is_loaded() {
    let path = std::env::temp_dir().join(format!("rageguard-test-{}.toml", std::process::id()));
    std::fs::write(&path, "[anger_detection]\ntimeout_minutes = 10\n").unwrap();
    let loaded = FileConfig::load(&path);
    std::fs::remove_file(&path).ok();
    let (config, _) = loaded.unwrap().unwrap();
    assert_eq!(config.anger_detection.timeout_minutes, 10);
    assert_eq!(config.anger_detection.timeout().as_secs(), 600);
}

#[test]
fn anger_emotions_default_and_validation() {
    assert_eq!(
        DetectionConfig::default().anger_emotions,
        ["Angry", "Disgust"]
    );
    let only_angry = parse("[anger_detection]\nanger_emotions = [\"angry\"]\n").unwrap();
    assert_eq!(only_angry.anger_detection.anger_emotions, ["angry"]);
    assert!(matches!(
        parse("[anger_detection]\nanger_emotions = []\n"),
        Err(ConfigError::Invalid {
            field: "anger_detection.anger_emotions",
            ..
        })
    ));
    let err = parse("[anger_detection]\nanger_emotions = [\"Furious\"]\n").unwrap_err();
    assert!(err.to_string().contains("Furious"), "{err}");
}
