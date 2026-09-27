use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption, GuildId,
    Permissions,
};

use super::{CommandError, Reply, user_option};
use crate::{app::AppContext, monitor};

pub const NAME: &str = "anger-stop";

pub fn definition() -> CreateCommand {
    CreateCommand::new(NAME)
        .description("Stop monitoring a member")
        .default_member_permissions(Permissions::MODERATE_MEMBERS)
        .dm_permission(false)
        .add_option(
            CreateCommandOption::new(CommandOptionType::User, "user", "Member to stop monitoring")
                .required(true),
        )
}

pub async fn run(
    ctx: &Context,
    app: &AppContext,
    command: &CommandInteraction,
    guild_id: GuildId,
) -> Result<Reply, CommandError> {
    let target = user_option(command, "user")?;
    let session = monitor::stop(ctx, app, guild_id, target.user.id, command.user.id)
        .await
        .map_err(|e| CommandError::new(e.to_string()))?;

    let (analyzed, triggers) = session.with_state(|s| (s.segments_analyzed, s.triggers));
    Ok(Reply::Text(format!(
        "⏹️ Stopped monitoring <@{}>. {analyzed} segment(s) analysed, {triggers} trigger(s). \
         Detection history has been discarded.",
        target.user.id
    )))
}
