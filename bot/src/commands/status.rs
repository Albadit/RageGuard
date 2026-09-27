use std::time::{Duration, Instant};

use serenity::all::{Colour, CreateCommand, CreateEmbed, CreateEmbedFooter, GuildId, Permissions};

use super::{CommandError, Reply};
use crate::{
    app::AppContext,
    moderation::{format_duration, percent},
};

pub const NAME: &str = "anger-status";

/// Monitored members listed individually (the embed also has 4 settings fields; max 25).
const MAX_MEMBER_FIELDS: usize = 20;

pub fn definition() -> CreateCommand {
    CreateCommand::new(NAME)
        .description("Show RageGuard's monitoring status and detection settings")
        .default_member_permissions(Permissions::MODERATE_MEMBERS)
        .dm_permission(false)
}

pub async fn run(app: &AppContext, guild_id: GuildId) -> Result<Reply, CommandError> {
    let settings = app.settings.get(guild_id).await;
    let rules = &settings.detection;

    let ai_status = match tokio::time::timeout(Duration::from_secs(2), app.analyzer.health()).await
    {
        Ok(Ok(health)) if health.model_loaded => "🟢 Ready".to_owned(),
        Ok(Ok(health)) => format!("🟡 {}", health.status),
        Ok(Err(e)) => format!("🔴 {e}"),
        Err(_) => "🔴 No response".to_owned(),
    };

    let mut embed = CreateEmbed::new().title("RageGuard status");
    let sessions = app.registry.sessions(guild_id).await;
    if sessions.is_empty() {
        embed = embed
            .colour(Colour::LIGHT_GREY)
            .description("Not monitoring anyone. Use `/anger-monitor @user` to start.");
    } else {
        embed = embed.colour(if app.monitor_only() {
            Colour::GOLD
        } else {
            Colour::RED
        });
        // Embeds allow 25 fields; 4 are used for the settings below.
        let shown = sessions.len().min(MAX_MEMBER_FIELDS);
        for session in &sessions[..shown] {
            let snap = session.snapshot(Instant::now(), rules.window());
            let location = snap
                .voice_channel
                .map(|c| format!(" in <#{c}>"))
                .unwrap_or_default();
            let recent = match &snap.last_analysis {
                Some(last) => format!(
                    "{} {} ({} ago)",
                    last.emotion,
                    percent(last.confidence),
                    format_duration(Duration::from_secs(last.at.elapsed().as_secs()))
                ),
                None => "nothing analysed yet".to_owned(),
            };
            let mut value = format!(
                "<@{user}> · {status}{location}\n\
                 **Recent:** {recent}\n\
                 **Anger:** {count}/{required} in the last {window} · **Triggers:** {triggers}\n\
                 **Segments:** {analysed} analysed, {dropped} dropped, {ai_errors} AI errors\n\
                 Started by <@{by}> <t:{since}:R>",
                user = snap.info.user_id,
                status = snap.status.describe(),
                count = snap.anger_count,
                required = rules.required_detections,
                window = format_duration(rules.window()),
                triggers = snap.triggers,
                analysed = snap.segments_analyzed,
                dropped = snap.segments_dropped,
                ai_errors = snap.ai_failures,
                by = snap.info.started_by,
                since = snap.info.started_at.unix_timestamp(),
            );
            if let Some(action) = snap.last_action {
                value.push_str(&format!("\n**Last action:** {action}"));
            }
            if let Some(error) = snap.last_error {
                value.push_str(&format!("\n**Last error:** {error}"));
            }
            embed = embed.field(
                truncate(&snap.info.user_name, 256),
                truncate(&value, 1024),
                false,
            );
        }
        if sessions.len() > shown {
            embed = embed.description(format!(
                "Monitoring {} members; showing the first {shown}.",
                sessions.len()
            ));
        }
    }

    embed = embed
        .field("Current threshold", percent(rules.threshold), true)
        .field("Timeout duration", format_duration(rules.timeout()), true)
        .field(
            "Monitor-only",
            if app.monitor_only() {
                "✅ On (timeouts are only logged)"
            } else {
                "❌ Off (timeouts are enforced)"
            },
            true,
        )
        .field("AI service", truncate(&ai_status, 1000), true)
        .footer(CreateEmbedFooter::new(
            "Voice emotion detection is probabilistic and can be inaccurate.",
        ));
    Ok(Reply::Embed(Box::new(embed)))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    }
}
