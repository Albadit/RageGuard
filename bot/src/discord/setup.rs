//! Choosing the moderation log channel from inside Discord.
//!
//! The first time RageGuard sees a server without a log channel, it posts a setup message with a
//! channel picker. `/anger-setup` shows the same picker at any time. The choice is saved to disk.
//!
//! The picker lists only text channels RageGuard can actually post in, so a moderator can't pick
//! a channel that would fail with "Missing Access".

use serenity::{
    all::{
        ChannelType, ComponentInteraction, ComponentInteractionDataKind, CreateActionRow,
        CreateAllowedMentions, CreateInteractionResponse, CreateInteractionResponseMessage,
        CreateMessage, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption,
        EditInteractionResponse, Guild, Permissions,
    },
    client::Context,
    model::id::{ChannelId, GuildId, UserId},
};
use tracing::{error, info, warn};

use crate::{app::AppContext, moderation::has_moderation_permission};

/// `custom_id` of the log-channel picker.
pub const LOG_CHANNEL_SELECT_ID: &str = "rageguard:log-channel";

/// Discord allows at most 25 options in a dropdown.
pub const MAX_PICKER_OPTIONS: usize = 25;

/// Shown with the picker, because it deliberately hides channels RageGuard can't use.
pub const NOT_LISTED_HINT: &str = "Only channels RageGuard can post in are listed. Missing one? \
    Give the RageGuard role **View Channel** and **Send Messages** in it (Edit Channel → \
    Permissions), then run `/anger-setup` again.";

/// Shown instead of the picker when RageGuard can't post anywhere.
pub const NO_WRITABLE_CHANNELS: &str = "RageGuard can't post in any text channel yet. Give the \
    RageGuard role **View Channel** and **Send Messages** in the channel you want to use (Edit \
    Channel → Permissions), then run `/anger-setup` again.";

/// Text of the one-time setup message.
pub const WELCOME_TEXT: &str = "👋 **Thanks for adding RageGuard!**\n\n\
    Pick the channel where I should post moderation notices (timeouts, and what I *would* do in \
    monitor-only mode). A private moderators-only channel is best.\n\n\
    Only members with **Moderate Members** can choose. You can change it later with `/anger-setup`.";

/// A text channel in the server, and whether RageGuard can post in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCandidate {
    pub id: ChannelId,
    pub name: String,
    pub position: u16,
    /// RageGuard can view the channel and send messages in it.
    pub can_send: bool,
}

/// The server's text channels, with RageGuard's access to each.
pub fn channel_candidates(guild: &Guild, bot_id: UserId) -> Vec<ChannelCandidate> {
    let Some(me) = guild.members.get(&bot_id) else {
        return Vec::new();
    };
    let needed = Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES;
    guild
        .channels
        .values()
        .filter(|c| c.kind == ChannelType::Text)
        .map(|c| ChannelCandidate {
            id: c.id,
            name: c.name.clone(),
            position: c.position,
            can_send: guild.user_permissions_in(c, me).contains(needed),
        })
        .collect()
}

/// [`channel_candidates`] from the gateway cache (empty if the server isn't cached).
pub fn cached_candidates(ctx: &Context, guild_id: GuildId) -> Vec<ChannelCandidate> {
    let bot_id = ctx.cache.current_user().id;
    ctx.cache
        .guild(guild_id)
        .map(|guild| channel_candidates(&guild, bot_id))
        .unwrap_or_default()
}

/// Channels RageGuard can post in, in channel-list order, at most [`MAX_PICKER_OPTIONS`].
pub fn writable_channels(candidates: &[ChannelCandidate]) -> Vec<&ChannelCandidate> {
    let mut writable: Vec<_> = candidates.iter().filter(|c| c.can_send).collect();
    writable.sort_by_key(|c| (c.position, c.id));
    writable.truncate(MAX_PICKER_OPTIONS);
    writable
}

/// A dropdown of the channels RageGuard can post in, or `None` if there are none.
pub fn log_channel_picker(
    candidates: &[ChannelCandidate],
    current: Option<ChannelId>,
) -> Option<CreateActionRow> {
    let options: Vec<CreateSelectMenuOption> = writable_channels(candidates)
        .into_iter()
        .map(|c| {
            let label: String = format!("#{}", c.name).chars().take(100).collect();
            CreateSelectMenuOption::new(label, c.id.to_string())
                .default_selection(Some(c.id) == current)
        })
        .collect();
    if options.is_empty() {
        return None;
    }
    Some(CreateActionRow::SelectMenu(
        CreateSelectMenu::new(
            LOG_CHANNEL_SELECT_ID,
            CreateSelectMenuKind::String { options },
        )
        .placeholder("Choose the moderation log channel")
        .min_values(1)
        .max_values(1),
    ))
}

/// Where to post the setup message: the server's system channel if the bot may post there,
/// otherwise the top-most text channel it can post in.
pub fn choose_welcome_channel(
    system_channel: Option<ChannelId>,
    candidates: &[ChannelCandidate],
) -> Option<ChannelId> {
    if let Some(system) = system_channel
        && candidates.iter().any(|c| c.id == system && c.can_send)
    {
        return Some(system);
    }
    writable_channels(candidates).first().map(|c| c.id)
}

/// Posts the setup message if this server has no log channel and has not been asked before.
pub async fn maybe_send_welcome(ctx: &Context, app: &AppContext, guild: &Guild) {
    if !app.settings.needs_setup_prompt(guild.id).await {
        return;
    }
    let candidates = channel_candidates(guild, ctx.cache.current_user().id);
    let (Some(channel), Some(picker)) = (
        choose_welcome_channel(guild.system_channel_id, &candidates),
        log_channel_picker(&candidates, None),
    ) else {
        warn!(
            guild_id = %guild.id,
            "no text channel I can post in; a moderator can run /anger-setup after fixing permissions"
        );
        return;
    };

    let message = CreateMessage::new()
        .content(format!("{WELCOME_TEXT}\n\n-# {NOT_LISTED_HINT}"))
        .components(vec![picker]);
    match channel.send_message(&ctx.http, message).await {
        Ok(_) => {
            info!(guild_id = %guild.id, channel_id = %channel, "setup_message_sent");
            if let Err(e) = app.settings.mark_setup_prompted(guild.id).await {
                warn!(guild_id = %guild.id, error = %e, "could not save that the setup message was sent");
            }
        }
        Err(e) => {
            warn!(guild_id = %guild.id, channel_id = %channel, error = %e, "setup_message_failed")
        }
    }
}

/// The channel picked in the dropdown. Also accepts the older channel-select dropdown, which may
/// still be on setup messages posted by earlier versions.
fn selected_channel(kind: &ComponentInteractionDataKind) -> Option<ChannelId> {
    match kind {
        ComponentInteractionDataKind::StringSelect { values } => values
            .first()?
            .parse::<u64>()
            .ok()
            .filter(|&id| id != 0)
            .map(ChannelId::new),
        ComponentInteractionDataKind::ChannelSelect { values } => values.first().copied(),
        _ => None,
    }
}

/// Handles a selection in the log-channel picker (from the setup message or `/anger-setup`).
pub async fn handle_component(ctx: &Context, app: &AppContext, component: &ComponentInteraction) {
    if component.data.custom_id != LOG_CHANNEL_SELECT_ID {
        return;
    }
    let Some(guild_id) = component.guild_id else {
        return;
    };

    let permissions = component
        .member
        .as_ref()
        .and_then(|m| m.permissions)
        .unwrap_or_else(Permissions::empty);
    if !has_moderation_permission(permissions) {
        let reply = CreateInteractionResponseMessage::new()
            .content("❌ Only members with the **Moderate Members** permission can choose RageGuard's log channel.")
            .ephemeral(true);
        if let Err(e) = component
            .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
            .await
        {
            warn!(error = %e, "failed to answer setup interaction");
        }
        return;
    }

    let Some(channel) = selected_channel(&component.data.kind) else {
        return;
    };

    // Acknowledge now: posting the test message below may take a moment.
    if let Err(e) = component
        .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
        .await
    {
        warn!(error = %e, "failed to acknowledge setup interaction");
        return;
    }

    let edit = match confirm_and_save(ctx, app, component, guild_id, channel).await {
        Ok(()) => EditInteractionResponse::new()
            .content(format!(
                "✅ RageGuard will post moderation notices in <#{channel}> (chosen by <@{}>). \
                 Change it any time with `/anger-setup`.",
                component.user.id
            ))
            .components(vec![]),
        Err(problem) => {
            let candidates = cached_candidates(ctx, guild_id);
            let current = app.settings.get(guild_id).await.log_channel;
            match log_channel_picker(&candidates, current) {
                Some(picker) => EditInteractionResponse::new()
                    .content(format!("⚠️ {problem}\n\n-# {NOT_LISTED_HINT}"))
                    .components(vec![picker]),
                None => EditInteractionResponse::new()
                    .content(format!("⚠️ {problem}\n\n{NO_WRITABLE_CHANNELS}"))
                    .components(vec![]),
            }
        }
    };
    if let Err(e) = component
        .edit_response(
            &ctx.http,
            edit.allowed_mentions(CreateAllowedMentions::new()),
        )
        .await
    {
        warn!(error = %e, "failed to update setup message");
    }
}

/// Posts a confirmation into `channel` (proving the bot can write there), then saves it.
async fn confirm_and_save(
    ctx: &Context,
    app: &AppContext,
    component: &ComponentInteraction,
    guild_id: GuildId,
    channel: ChannelId,
) -> Result<(), String> {
    let confirmation = CreateMessage::new()
        .content(format!(
            "🛡️ **RageGuard** will post moderation notices in this channel (set by <@{}>).",
            component.user.id
        ))
        .allowed_mentions(CreateAllowedMentions::new());
    if let Err(e) = channel.send_message(&ctx.http, confirmation).await {
        warn!(guild_id = %guild_id, channel_id = %channel, error = %e, "log_channel_not_writable");
        return Err(format!(
            "I can't post in <#{channel}> ({e}). Give the RageGuard role **View Channel** and \
             **Send Messages** there, then choose again."
        ));
    }
    app.settings
        .set_log_channel(guild_id, channel)
        .await
        .map_err(|e| {
            error!(guild_id = %guild_id, error = %e, "failed to save log channel");
            format!("Saving the choice failed: {e}")
        })?;
    info!(guild_id = %guild_id, channel_id = %channel, set_by = %component.user.id, "log_channel_set");
    Ok(())
}
