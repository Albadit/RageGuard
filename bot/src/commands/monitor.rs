use std::sync::Arc;

use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption, GuildId,
    Permissions,
};

use super::{CommandError, Reply, display_name, user_option};
use crate::{
    app::AppContext,
    moderation::{format_duration, percent},
    monitor::{self, StartRequest},
    state::SessionStatus,
};

pub const NAME: &str = "anger-monitor";

pub fn definition() -> CreateCommand {
    CreateCommand::new(NAME)
        .description("Start monitoring a member's voice for repeated angry speech")
        .default_member_permissions(Permissions::MODERATE_MEMBERS)
        .dm_permission(false)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::User,
                "user",
                "Member to monitor (RageGuard joins their voice channel now, or when they join one)",
            )
            .required(true),
        )
}

pub async fn run(
    ctx: &Context,
    app: &Arc<AppContext>,
    command: &CommandInteraction,
    guild_id: GuildId,
) -> Result<Reply, CommandError> {
    let target = user_option(command, "user")?;
    let session = monitor::start(
        ctx,
        app,
        StartRequest {
            guild_id,
            target_id: target.user.id,
            target_name: target.display_name.clone(),
            target_is_bot: target.user.bot,
            moderator_id: command.user.id,
            moderator_name: display_name(&command.user),
            command_channel: command.channel_id,
        },
    )
    .await
    .map_err(|e| CommandError::new(e.to_string()))?;

    let settings = app.settings.get(guild_id).await;
    let rules = &settings.detection;
    let mode = if app.monitor_only() {
        "🧪 **Monitor-only mode**: detections are logged, but no timeouts are applied."
    } else {
        "⚠️ **Enforcement mode**: timeouts are applied automatically."
    };

    // Where the user is now, or a note that RageGuard will join when they arrive.
    let (where_now, mut waiting_note) = match session.info.initial_channel {
        Some(channel) => (format!(" in <#{channel}>"), String::new()),
        None => (
            String::new(),
            "\n⏳ They're not in a voice channel yet. I'll join automatically as soon as they do."
                .to_owned(),
        ),
    };

    // RageGuard can only be in one voice channel per server. Say so if it is listening to other
    // monitored members somewhere else.
    let listening_elsewhere = app
        .registry
        .sessions(guild_id)
        .await
        .iter()
        .filter(|s| s.info.user_id != target.user.id)
        .find_map(|s| {
            s.with_state(|st| {
                (st.status == SessionStatus::Listening)
                    .then_some(st.voice_channel)
                    .flatten()
            })
        });
    if let (Some(other), Some(mine)) = (listening_elsewhere, session.info.initial_channel)
        && other != mine
    {
        waiting_note = format!(
            "\nℹ️ I'm listening to other monitored members in <#{other}> and can only be in one voice \
             channel at a time. <@{}> is analysed while they're in the same channel, or when most \
             monitored members are in theirs.",
            target.user.id
        );
    }

    // The server log itself is posted by `monitor::start` and the session tasks.
    let log_note = match settings.log_channel {
        Some(channel) => format!("📋 Activity is logged in <#{channel}>."),
        None => {
            "📋 Tip: choose a log channel with `/anger-setup` to get a server log of detections."
                .to_owned()
        }
    };

    Ok(Reply::Text(format!(
        "🎙️ Now monitoring <@{user}>{where_now}.{waiting_note}\n{mode}\n{log_note}\n\n\
         **Trigger:** {required} angry detections at ≥ {threshold} within {window} → {timeout} timeout.\n\n\
         Only this member's audio is analysed, in short in-memory segments that are never stored. \
         Voice emotion detection is probabilistic and can be wrong. Use `/anger-stop` to stop.",
        user = target.user.id,
        required = rules.required_detections,
        threshold = percent(rules.threshold),
        window = format_duration(rules.window()),
        timeout = format_duration(rules.timeout()),
    )))
}
