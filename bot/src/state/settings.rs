use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serenity::model::id::{ChannelId, GuildId};
use tokio::sync::RwLock;

use crate::config::{ConfigError, DetectionConfig};

/// File (inside the data directory) holding choices made in Discord.
pub const SETTINGS_FILE_NAME: &str = "guild-settings.json";

/// Detection rules and notification channel for one guild.
#[derive(Debug, Clone, PartialEq)]
pub struct GuildSettings {
    pub detection: DetectionConfig,
    /// Where moderation notices go. `None` means "the channel `/anger-monitor` was used in".
    pub log_channel: Option<ChannelId>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(#[from] ConfigError),
    #[error("could not read {path}: {source}")]
    Load {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not a valid RageGuard settings file ({source}); fix or delete it")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not save {path}: {source}")]
    Save {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Per-guild choices that survive restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SavedGuild {
    #[serde(skip_serializing_if = "Option::is_none")]
    log_channel: Option<u64>,
    /// The "choose a log channel" message has been posted once.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    setup_prompted: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedFile {
    version: u32,
    guilds: BTreeMap<u64, SavedGuild>,
}

const FILE_VERSION: u32 = 1;

/// Per-guild settings.
///
/// * Detection rules start from `rageguard.toml`; `/anger-config` changes are kept in memory and
///   reset when the bot restarts.
/// * The log channel chosen in Discord (`/anger-setup`, the setup message, or
///   `/anger-config log_channel`) is saved to disk and survives restarts.
#[derive(Debug)]
pub struct SettingsStore {
    defaults: GuildSettings,
    detection_overrides: RwLock<HashMap<GuildId, DetectionConfig>>,
    saved: RwLock<BTreeMap<u64, SavedGuild>>,
    /// `None` keeps everything in memory (tests).
    path: Option<PathBuf>,
}

impl SettingsStore {
    /// A store that never touches the disk.
    pub fn new(defaults: GuildSettings) -> Self {
        Self {
            defaults,
            detection_overrides: RwLock::new(HashMap::new()),
            saved: RwLock::new(BTreeMap::new()),
            path: None,
        }
    }

    /// Loads saved choices from `path`. A missing file just means nothing has been saved yet.
    pub fn load(defaults: GuildSettings, path: PathBuf) -> Result<Self, SettingsError> {
        let saved = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: SavedFile =
                    serde_json::from_slice(&bytes).map_err(|source| SettingsError::Corrupt {
                        path: path.clone(),
                        source,
                    })?;
                file.guilds
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(SettingsError::Load { path, source }),
        };
        Ok(Self {
            defaults,
            detection_overrides: RwLock::new(HashMap::new()),
            saved: RwLock::new(saved),
            path: Some(path),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn defaults(&self) -> &GuildSettings {
        &self.defaults
    }

    pub async fn get(&self, guild_id: GuildId) -> GuildSettings {
        let detection = self
            .detection_overrides
            .read()
            .await
            .get(&guild_id)
            .cloned()
            .unwrap_or_else(|| self.defaults.detection.clone());
        let saved_channel = saved_log_channel(self.saved.read().await.get(&guild_id.get()));
        GuildSettings {
            detection,
            log_channel: saved_channel.or(self.defaults.log_channel),
        }
    }

    /// Applies `change` and validates the result. Invalid changes are rejected and nothing is
    /// stored. A changed log channel is saved to disk before the change takes effect.
    pub async fn update(
        &self,
        guild_id: GuildId,
        change: impl FnOnce(&mut GuildSettings),
    ) -> Result<GuildSettings, SettingsError> {
        let mut overrides = self.detection_overrides.write().await;
        let mut saved = self.saved.write().await;

        let current = GuildSettings {
            detection: overrides
                .get(&guild_id)
                .cloned()
                .unwrap_or_else(|| self.defaults.detection.clone()),
            log_channel: saved_log_channel(saved.get(&guild_id.get()))
                .or(self.defaults.log_channel),
        };
        let mut updated = current.clone();
        change(&mut updated);
        updated.detection.validate()?;

        if updated.log_channel != current.log_channel {
            let mut next = saved.clone();
            next.entry(guild_id.get()).or_default().log_channel =
                updated.log_channel.map(ChannelId::get);
            self.save(&next).await?;
            *saved = next;
        }
        overrides.insert(guild_id, updated.detection.clone());
        Ok(updated)
    }

    /// Saves the guild's moderation log channel.
    pub async fn set_log_channel(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Result<(), SettingsError> {
        self.modify_saved(guild_id, |g| g.log_channel = Some(channel_id.get()))
            .await
    }

    /// Drops this guild's detection overrides. The chosen log channel is kept.
    pub async fn reset(&self, guild_id: GuildId) -> GuildSettings {
        self.detection_overrides.write().await.remove(&guild_id);
        self.get(guild_id).await
    }

    /// True when the guild has no log channel (chosen or default) and has not been asked yet.
    pub async fn needs_setup_prompt(&self, guild_id: GuildId) -> bool {
        if self.defaults.log_channel.is_some() {
            return false;
        }
        let saved = self.saved.read().await;
        let guild = saved.get(&guild_id.get());
        saved_log_channel(guild).is_none() && !guild.is_some_and(|g| g.setup_prompted)
    }

    pub async fn mark_setup_prompted(&self, guild_id: GuildId) -> Result<(), SettingsError> {
        self.modify_saved(guild_id, |g| g.setup_prompted = true)
            .await
    }

    async fn modify_saved(
        &self,
        guild_id: GuildId,
        change: impl FnOnce(&mut SavedGuild),
    ) -> Result<(), SettingsError> {
        let mut saved = self.saved.write().await;
        let mut next = saved.clone();
        change(next.entry(guild_id.get()).or_default());
        if next == *saved {
            return Ok(());
        }
        self.save(&next).await?;
        *saved = next;
        Ok(())
    }

    /// Writes the file atomically (temp file + rename) so a crash never leaves it half-written.
    async fn save(&self, guilds: &BTreeMap<u64, SavedGuild>) -> Result<(), SettingsError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let save_error = |source| SettingsError::Save {
            path: path.clone(),
            source,
        };
        let json = serde_json::to_vec_pretty(&SavedFile {
            version: FILE_VERSION,
            guilds: guilds.clone(),
        })
        .expect("settings always serialise");
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(dir).await.map_err(save_error)?;
        }
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await.map_err(save_error)?;
        tokio::fs::rename(&tmp, path).await.map_err(save_error)
    }
}

fn saved_log_channel(guild: Option<&SavedGuild>) -> Option<ChannelId> {
    guild
        .and_then(|g| g.log_channel)
        .filter(|&id| id != 0)
        .map(ChannelId::new)
}
