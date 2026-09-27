//! Saved per-server settings: the log channel chosen in Discord survives restarts.

mod common;

use std::path::PathBuf;

use common::GUILD;
use rageguard::{
    config::DetectionConfig,
    state::{GuildSettings, SettingsError, SettingsStore},
};
use serenity::model::id::{ChannelId, GuildId};

/// A settings file path in a fresh temporary directory, removed on drop.
struct TempFile {
    dir: PathBuf,
}

impl TempFile {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("rageguard-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self { dir }
    }

    fn path(&self) -> PathBuf {
        // A nested directory proves the store creates missing parents.
        self.dir.join("data").join("guild-settings.json")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn defaults(log_channel: Option<ChannelId>) -> GuildSettings {
    GuildSettings {
        detection: DetectionConfig::default(),
        log_channel,
    }
}

const LOG: ChannelId = ChannelId::new(777);

#[tokio::test]
async fn missing_file_means_nothing_saved_yet() {
    let tmp = TempFile::new("missing");
    let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    assert_eq!(store.get(GUILD).await.log_channel, None);
    assert!(!tmp.path().exists(), "loading must not create the file");
}

#[tokio::test]
async fn chosen_log_channel_survives_a_restart() {
    let tmp = TempFile::new("restart");
    {
        let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
        store.set_log_channel(GUILD, LOG).await.unwrap();
        assert_eq!(store.get(GUILD).await.log_channel, Some(LOG));
    }
    let reloaded = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    assert_eq!(reloaded.get(GUILD).await.log_channel, Some(LOG));
    assert_eq!(reloaded.get(GuildId::new(2)).await.log_channel, None);
}

#[tokio::test]
async fn saved_channel_beats_the_env_fallback() {
    let tmp = TempFile::new("fallback");
    let env_default = ChannelId::new(111);
    let store = SettingsStore::load(defaults(Some(env_default)), tmp.path()).unwrap();
    assert_eq!(store.get(GUILD).await.log_channel, Some(env_default));
    store.set_log_channel(GUILD, LOG).await.unwrap();
    assert_eq!(store.get(GUILD).await.log_channel, Some(LOG));
    assert_eq!(
        store.get(GuildId::new(2)).await.log_channel,
        Some(env_default)
    );
}

#[tokio::test]
async fn config_command_log_channel_is_saved_but_rule_changes_are_not() {
    let tmp = TempFile::new("config");
    {
        let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
        store
            .update(GUILD, |s| {
                s.log_channel = Some(LOG);
                s.detection.threshold = 0.95;
            })
            .await
            .unwrap();
    }
    let reloaded = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    let settings = reloaded.get(GUILD).await;
    assert_eq!(settings.log_channel, Some(LOG));
    assert_eq!(
        settings.detection.threshold, 0.70,
        "rule changes reset to rageguard.toml on restart"
    );
}

#[tokio::test]
async fn reset_keeps_the_log_channel() {
    let store = SettingsStore::new(defaults(None));
    store.set_log_channel(GUILD, LOG).await.unwrap();
    store
        .update(GUILD, |s| s.detection.required_detections = 5)
        .await
        .unwrap();
    let after = store.reset(GUILD).await;
    assert_eq!(after.detection.required_detections, 2);
    assert_eq!(after.log_channel, Some(LOG));
}

#[tokio::test]
async fn setup_prompt_is_needed_once() {
    let tmp = TempFile::new("prompt");
    {
        let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
        assert!(store.needs_setup_prompt(GUILD).await);
        store.mark_setup_prompted(GUILD).await.unwrap();
        assert!(!store.needs_setup_prompt(GUILD).await);
        assert!(store.needs_setup_prompt(GuildId::new(2)).await);
    }
    let reloaded = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    assert!(
        !reloaded.needs_setup_prompt(GUILD).await,
        "must not ask again after a restart"
    );
}

#[tokio::test]
async fn no_setup_prompt_when_a_channel_exists() {
    let with_env_default = SettingsStore::new(defaults(Some(ChannelId::new(1))));
    assert!(!with_env_default.needs_setup_prompt(GUILD).await);

    let store = SettingsStore::new(defaults(None));
    store.set_log_channel(GUILD, LOG).await.unwrap();
    assert!(!store.needs_setup_prompt(GUILD).await);
}

#[tokio::test]
async fn saved_file_is_readable_json() {
    let tmp = TempFile::new("format");
    let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    store.set_log_channel(GUILD, LOG).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path()).unwrap()).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["guilds"][GUILD.get().to_string()]["log_channel"], 777);
    assert!(
        !tmp.path().with_extension("json.tmp").exists(),
        "temp file is renamed away"
    );
}

#[tokio::test]
async fn corrupted_file_is_a_clear_error() {
    let tmp = TempFile::new("corrupt");
    std::fs::create_dir_all(tmp.path().parent().unwrap()).unwrap();
    std::fs::write(tmp.path(), "{ not json").unwrap();
    let err = SettingsStore::load(defaults(None), tmp.path()).unwrap_err();
    assert!(matches!(err, SettingsError::Corrupt { .. }));
    assert!(err.to_string().contains("fix or delete it"));
}

#[tokio::test]
async fn invalid_rule_change_saves_nothing() {
    let tmp = TempFile::new("invalid");
    let store = SettingsStore::load(defaults(None), tmp.path()).unwrap();
    let result = store
        .update(GUILD, |s| {
            s.log_channel = Some(LOG);
            s.detection.threshold = 5.0;
        })
        .await;
    assert!(matches!(result, Err(SettingsError::Invalid(_))));
    assert_eq!(store.get(GUILD).await.log_channel, None);
    assert!(!tmp.path().exists());
}
