use serenity::all::{Context, CreateCommand, GuildId, Permissions};

use super::{CommandError, Reply};
use crate::{
    app::AppContext,
    discord::setup::{
        NO_WRITABLE_CHANNELS, NOT_LISTED_HINT, cached_candidates, log_channel_picker,
    },
};

pub const NAME: &str = "anger-setup";

pub fn definition() -> CreateCommand {
    CreateCommand::new(NAME)
        .description("Choose the channel where RageGuard posts moderation notices")
        .default_member_permissions(Permissions::MODERATE_MEMBERS)
        .dm_permission(false)
}

pub async fn run(
    ctx: &Context,
    app: &AppContext,
    guild_id: GuildId,
) -> Result<Reply, CommandError> {
    let current = app.settings.get(guild_id).await.log_channel;
    let status = match current {
        Some(channel) => format!("Moderation notices currently go to <#{channel}>."),
        None => "No log channel chosen yet, so notices go to the channel where `/anger-monitor` was used."
            .to_owned(),
    };
    // Only channels RageGuard can post in are offered.
    let candidates = cached_candidates(ctx, guild_id);
    match log_channel_picker(&candidates, current) {
        Some(picker) => Ok(Reply::Components(
            format!(
                "⚙️ **RageGuard setup**\n{status}\n\nPick a channel below. A private moderators-only channel is best.\n-# {NOT_LISTED_HINT}"
            ),
            vec![picker],
        )),
        None => Ok(Reply::Text(format!(
            "⚙️ **RageGuard setup**\n{status}\n\n⚠️ {NO_WRITABLE_CHANNELS}"
        ))),
    }
}
