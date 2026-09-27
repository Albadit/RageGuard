use serenity::all::{
    CommandInteraction, CommandOptionType, CreateCommand, CreateCommandOption, GuildId,
    Permissions, ResolvedValue,
};

use super::{CommandError, Reply};
use crate::{
    app::AppContext,
    config::MAX_TIMEOUT_MINUTES,
    moderation::{TIMEOUT_REASON, format_duration, percent},
    state::GuildSettings,
};

pub const NAME: &str = "anger-config";

pub fn definition() -> CreateCommand {
    CreateCommand::new(NAME)
        .description("View or change RageGuard's detection settings for this server")
        .default_member_permissions(Permissions::MODERATE_MEMBERS)
        .dm_permission(false)
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Number,
                "threshold",
                "Minimum anger score that counts as a detection, 0.5–1.0 (default 0.7)",
            )
            .min_number_value(0.5)
            .max_number_value(1.0),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Integer,
                "detections",
                "How many angry 3-second clips it takes to trigger a timeout (default 2)",
            )
            .min_int_value(1)
            .max_int_value(20),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Integer,
                "window_seconds",
                "Those angry clips must all happen within this many seconds (default 15)",
            )
            .min_int_value(5)
            .max_int_value(600),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Integer,
                "timeout_minutes",
                "Length of the Discord timeout in minutes (default 1)",
            )
            .min_int_value(1)
            .max_int_value(MAX_TIMEOUT_MINUTES),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::Boolean,
            "reset",
            "Restore the detection rules from rageguard.toml",
        ))
}

#[derive(Debug, Default)]
struct Changes {
    threshold: Option<f64>,
    detections: Option<i64>,
    window_seconds: Option<i64>,
    timeout_minutes: Option<i64>,
    reset: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.threshold.is_none()
            && self.detections.is_none()
            && self.window_seconds.is_none()
            && self.timeout_minutes.is_none()
            && !self.reset
    }

    fn apply(&self, settings: &mut GuildSettings) {
        let rules = &mut settings.detection;
        if let Some(v) = self.threshold {
            rules.threshold = v as f32;
        }
        // Discord enforces the option ranges, so these conversions cannot fail in practice;
        // out-of-range values would be caught by validation anyway.
        if let Some(v) = self.detections {
            rules.required_detections = u32::try_from(v).unwrap_or(0);
        }
        if let Some(v) = self.window_seconds {
            rules.window_seconds = u64::try_from(v).unwrap_or(0);
        }
        if let Some(v) = self.timeout_minutes {
            rules.timeout_minutes = u64::try_from(v).unwrap_or(0);
        }
    }
}

fn parse_changes(command: &CommandInteraction) -> Changes {
    let mut changes = Changes::default();
    for option in command.data.options() {
        match (option.name, option.value) {
            ("threshold", ResolvedValue::Number(v)) => changes.threshold = Some(v),
            ("detections", ResolvedValue::Integer(v)) => changes.detections = Some(v),
            ("window_seconds", ResolvedValue::Integer(v)) => changes.window_seconds = Some(v),
            ("timeout_minutes", ResolvedValue::Integer(v)) => changes.timeout_minutes = Some(v),
            ("reset", ResolvedValue::Boolean(v)) => changes.reset = v,
            _ => {}
        }
    }
    changes
}

pub async fn run(
    app: &AppContext,
    command: &CommandInteraction,
    guild_id: GuildId,
) -> Result<Reply, CommandError> {
    let changes = parse_changes(command);
    if changes.is_empty() {
        let current = app.settings.get(guild_id).await;
        return Ok(Reply::Text(format!(
            "⚙️ **RageGuard settings**\n{}",
            describe(&current, app.monitor_only())
        )));
    }

    if changes.reset {
        app.settings.reset(guild_id).await;
    }
    let updated = app
        .settings
        .update(guild_id, |settings| changes.apply(settings))
        .await
        .map_err(|e| CommandError::new(format!("Invalid configuration, nothing changed: {e}")))?;

    let mut reply = format!(
        "✅ **Settings updated** (applies now; resets to rageguard.toml on restart)\n{}",
        describe(&updated, app.monitor_only())
    );
    for warning in updated.detection.warnings() {
        reply.push_str(&format!("\n⚠️ {warning}"));
    }
    Ok(Reply::Text(reply))
}

fn describe(settings: &GuildSettings, monitor_only: bool) -> String {
    let rules = &settings.detection;
    format!(
        "• Threshold: **{}**\n\
         • Required detections: **{}** within **{}**\n\
         • Timeout duration: **{}**\n\
         • Timeout reason: *{}* (plus the detection details in the audit log)\n\
         • Segment length: **{:.1} s** (config file only)\n\
         • Monitor-only: **{}** (MONITOR_ONLY in .env only)",
        percent(rules.threshold),
        rules.required_detections,
        format_duration(rules.window()),
        format_duration(rules.timeout()),
        TIMEOUT_REASON,
        rules.segment_seconds,
        if monitor_only { "on" } else { "off" },
    )
}
