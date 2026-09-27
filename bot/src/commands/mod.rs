//! Slash commands: `/anger-monitor`, `/anger-stop`, `/anger-status`, `/anger-config`,
//! `/anger-setup`.

mod configure;
mod monitor;
mod setup;
mod status;
mod stop;

use std::sync::Arc;

use serenity::all::{
    CommandInteraction, Context, CreateActionRow, CreateCommand, CreateEmbed,
    EditInteractionResponse, Permissions, ResolvedValue, User,
};
use tracing::{info, warn};

use crate::{app::AppContext, moderation::has_moderation_permission};

/// All command definitions, for registration on startup.
pub fn definitions() -> Vec<CreateCommand> {
    vec![
        monitor::definition(),
        stop::definition(),
        status::definition(),
        configure::definition(),
        setup::definition(),
    ]
}

/// Successful command output.
pub enum Reply {
    Text(String),
    Embed(Box<CreateEmbed>),
    /// Text with interactive components (e.g. a channel picker).
    Components(String, Vec<CreateActionRow>),
}

/// A message shown to the moderator when a command fails.
#[derive(Debug)]
pub struct CommandError(pub String);

impl CommandError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Handles one command interaction end to end. Never panics; all errors become a reply.
pub async fn dispatch(ctx: &Context, app: &Arc<AppContext>, command: &CommandInteraction) {
    // Acknowledge within Discord's 3-second deadline. Joining voice can take longer, so every
    // command defers first and edits the (private) response afterwards.
    if let Err(e) = command.defer_ephemeral(&ctx.http).await {
        warn!(command = %command.data.name, error = %e, "failed to acknowledge interaction");
        return;
    }

    let response = match run(ctx, app, command).await {
        Ok(Reply::Text(text)) => EditInteractionResponse::new().content(text),
        Ok(Reply::Embed(embed)) => EditInteractionResponse::new().embed(*embed),
        Ok(Reply::Components(text, rows)) => EditInteractionResponse::new()
            .content(text)
            .components(rows),
        Err(CommandError(message)) => {
            EditInteractionResponse::new().content(format!("❌ {message}"))
        }
    };
    if let Err(e) = command.edit_response(&ctx.http, response).await {
        warn!(command = %command.data.name, error = %e, "failed to send command response");
    }
}

async fn run(
    ctx: &Context,
    app: &Arc<AppContext>,
    command: &CommandInteraction,
) -> Result<Reply, CommandError> {
    let guild_id = command
        .guild_id
        .ok_or_else(|| CommandError::new("RageGuard commands only work inside a server."))?;

    // `default_member_permissions` hides the commands from non-moderators, but server admins
    // can override that in Integrations settings, so check again here.
    let permissions = command
        .member
        .as_ref()
        .and_then(|m| m.permissions)
        .unwrap_or_else(Permissions::empty);
    if !has_moderation_permission(permissions) {
        warn!(user_id = %command.user.id, command = %command.data.name, "permission_denied");
        return Err(CommandError::new(
            "You need the **Moderate Members** permission to use RageGuard.",
        ));
    }

    info!(
        command = %command.data.name,
        user_id = %command.user.id,
        guild_id = %guild_id,
        "command_invoked"
    );
    match command.data.name.as_str() {
        monitor::NAME => monitor::run(ctx, app, command, guild_id).await,
        stop::NAME => stop::run(ctx, app, command, guild_id).await,
        status::NAME => status::run(app, guild_id).await,
        configure::NAME => configure::run(app, command, guild_id).await,
        setup::NAME => setup::run(ctx, app, guild_id).await,
        other => Err(CommandError::new(format!("Unknown command `{other}`."))),
    }
}

/// A resolved user option plus the best name to show for them.
pub(crate) struct TargetUser<'a> {
    pub user: &'a User,
    pub display_name: String,
}

pub(crate) fn user_option<'a>(
    command: &'a CommandInteraction,
    name: &str,
) -> Result<TargetUser<'a>, CommandError> {
    command
        .data
        .options()
        .into_iter()
        .find(|option| option.name == name)
        .and_then(|option| match option.value {
            ResolvedValue::User(user, member) => Some(TargetUser {
                user,
                display_name: member
                    .and_then(|m| m.nick.clone())
                    .unwrap_or_else(|| display_name(user)),
            }),
            _ => None,
        })
        .ok_or_else(|| CommandError::new(format!("Missing `{name}` option.")))
}

pub(crate) fn display_name(user: &User) -> String {
    user.global_name
        .clone()
        .unwrap_or_else(|| user.name.clone())
}
