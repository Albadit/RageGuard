//! Application wiring: shared context, Discord client construction and graceful shutdown.

use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use serenity::{Client, all::GatewayIntents, gateway::GatewayError, http::HttpError};
use songbird::{SerenityInit, SongbirdKey};
use tracing::{info, warn};

use crate::{
    ai::{EmotionAnalyzer, HttpEmotionClient},
    config::AppConfig,
    discord::Handler,
    monitor::GuildVoiceManager,
    state::{GuildSettings, MonitorRegistry, SETTINGS_FILE_NAME, SettingsStore},
    voice::{VoiceLocks, songbird_config},
};

/// State shared by every event handler and session task.
pub struct AppContext {
    pub config: AppConfig,
    pub registry: MonitorRegistry,
    pub settings: Arc<SettingsStore>,
    pub analyzer: Arc<dyn EmotionAnalyzer>,
    pub voice_locks: VoiceLocks,
    /// One voice supervisor per server, shared by all monitored members there.
    pub voice: GuildVoiceManager,
}

impl AppContext {
    pub fn new(
        config: AppConfig,
        analyzer: Arc<dyn EmotionAnalyzer>,
        settings: SettingsStore,
    ) -> Self {
        Self {
            config,
            registry: MonitorRegistry::new(),
            settings: Arc::new(settings),
            analyzer,
            voice_locks: VoiceLocks::default(),
            voice: GuildVoiceManager::default(),
        }
    }

    /// Per-guild defaults from the config file and environment.
    pub fn default_guild_settings(config: &AppConfig) -> GuildSettings {
        GuildSettings {
            detection: config.file.anger_detection.clone(),
            // Chosen per server in Discord with /anger-setup and saved; no global default.
            log_channel: None,
        }
    }

    pub fn monitor_only(&self) -> bool {
        self.config.env.monitor_only
    }
}

/// Connects to Discord and runs until Ctrl+C or a fatal gateway error.
pub async fn run(config: AppConfig) -> anyhow::Result<()> {
    let analyzer = HttpEmotionClient::new(&config.env.ai_service_url, &config.file.ai_service)
        .context("failed to build the AI service HTTP client")?;
    let token = config.env.discord_token.clone();
    let settings_path = config.env.data_dir.join(SETTINGS_FILE_NAME);
    let settings = SettingsStore::load(AppContext::default_guild_settings(&config), settings_path)
        .context("failed to load saved server settings")?;
    if let Some(path) = settings.path() {
        info!(path = %path.display(), "server settings file");
    }
    let app = Arc::new(AppContext::new(config, Arc::new(analyzer), settings));

    // Neither intent is privileged, so no Developer Portal toggles are needed.
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let mut client = Client::builder(&token, intents)
        .event_handler(Handler::new(app.clone()))
        .register_songbird_from_config(songbird_config())
        .await
        .context("failed to create the Discord client")?;

    let voice = client.data.read().await.get::<SongbirdKey>().cloned();
    let shard_manager = client.shard_manager.clone();
    let shutdown_app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!(error = %e, "could not listen for Ctrl+C");
            return;
        }
        info!("shutting down: stopping monitoring and leaving voice channels");
        shutdown_app.voice.shutdown().await;
        for session in shutdown_app.registry.drain().await {
            if let Some(voice) = &voice {
                let _ = voice.remove(session.info.guild_id).await;
            }
        }
        shard_manager.shutdown_all().await;
    });

    match client.start().await {
        Ok(()) => Ok(()),
        Err(e) if is_invalid_token(&e) => Err(anyhow!(
            "Discord rejected DISCORD_TOKEN. Reset the token in the Developer Portal (Bot → Reset Token) and update .env"
        )),
        Err(e) => Err(e).context("the Discord client stopped with an error"),
    }
}

/// A bad token fails either on the first HTTP call (401) or during the gateway handshake.
fn is_invalid_token(err: &serenity::Error) -> bool {
    match err {
        serenity::Error::Gateway(GatewayError::InvalidAuthentication) => true,
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response)) => {
            response.status_code.as_u16() == 401
        }
        _ => false,
    }
}
