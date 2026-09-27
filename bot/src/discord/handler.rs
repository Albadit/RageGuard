use std::sync::Arc;

use serenity::{
    all::{Command, Guild, Interaction, Ready, VoiceState},
    async_trait,
    client::{Context, EventHandler},
};
use tracing::{error, info, warn};

use super::{invite_url, setup};
use crate::{app::AppContext, commands, state::SessionSignal};

/// Serenity gateway event handler.
pub struct Handler {
    app: Arc<AppContext>,
}

impl Handler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(
            bot = %ready.user.name,
            guilds = ready.guilds.len(),
            monitor_only = self.app.monitor_only(),
            "connected to Discord"
        );
        info!("invite link: {}", invite_url(ready.application.id.get()));

        // Global commands work in every server the bot is in. Changes can take a few minutes to
        // show up; Ctrl+R in Discord refreshes them.
        match Command::set_global_commands(&ctx.http, commands::definitions()).await {
            Ok(registered) => info!("registered {} slash commands", registered.len()),
            Err(e) => error!(error = %e, "failed to register slash commands"),
        }
        // Older versions could register per-server copies too; Discord would then list every
        // command twice. Remove any that are left over.
        for guild in &ready.guilds {
            match guild.id.get_commands(&ctx.http).await {
                Ok(leftover) if leftover.is_empty() => {}
                Ok(leftover) => match guild.id.set_commands(&ctx.http, Vec::new()).await {
                    Ok(_) => info!(
                        guild_id = %guild.id,
                        "removed {} duplicate per-server slash commands",
                        leftover.len()
                    ),
                    Err(e) => {
                        warn!(guild_id = %guild.id, error = %e, "failed to remove duplicate slash commands")
                    }
                },
                Err(e) => {
                    warn!(guild_id = %guild.id, error = %e, "failed to list per-server slash commands")
                }
            }
        }

        let analyzer = self.app.analyzer.clone();
        tokio::spawn(async move {
            match analyzer.health().await {
                Ok(health) if health.model_loaded => info!("AI service is ready"),
                Ok(health) => {
                    warn!(status = %health.status, "AI service is up but the model is still loading")
                }
                Err(e) => {
                    warn!(error = %e, "AI service is not reachable yet; segments will be skipped until it is")
                }
            }
        });
    }

    /// Sent for every server at startup and when the bot is added to a new one.
    async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: Option<bool>) {
        setup::maybe_send_welcome(&ctx, &self.app, &guild).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => commands::dispatch(&ctx, &self.app, &command).await,
            Interaction::Component(component) => {
                setup::handle_component(&ctx, &self.app, &component).await
            }
            _ => {}
        }
    }

    async fn voice_state_update(&self, ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        let Some(guild_id) = new.guild_id else {
            return;
        };
        let reason = if new.user_id == ctx.cache.current_user().id {
            "bot's voice state changed"
        } else if self.app.voice.is_target(guild_id, new.user_id).await {
            "a monitored member's voice state changed"
        } else {
            return;
        };
        self.app
            .voice
            .signal(guild_id, SessionSignal::Reconcile(reason))
            .await;
    }
}
